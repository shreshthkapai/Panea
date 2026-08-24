use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant},
};

use image::{
    DynamicImage, ImageFormat, RgbaImage,
    imageops::{FilterType, crop_imm, overlay, resize},
};
use sha2::{Digest, Sha256};

use security::{
    AuthMethod, HostKey, HostKeyTrustAction, HostKeyTrustRequest, HostTrustProvider, KnownHosts,
    KnownHostsPolicy, Osc52ClipboardPolicy, Osc52ClipboardRequest, Osc52ClipboardTarget,
    SecretProvider, SecretRequest, SecretString, evaluate_osc52_clipboard_write,
};
use shell_integration::{ShellKind, script_for_shell};
use term_core::{TerminalCore, TerminalSize as CoreTerminalSize};
use term_parser::TerminalEmulator;
use transport_core::{TerminalSize, TerminalTransport, TransportLifecycleEvent, TransportState};
use transport_pty::{LocalPtyDiagnostics, LocalPtyTransport, LocalShellProfile};
use transport_ssh::{SshConnectionProfile, SshTransport};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("help") | None => {
            eprintln!(
                "usage: cargo xtask <fmt|clippy|test|build|check|layer-check|ci|branding|config-default|config-schema|bench|screenshot|fuzz-smoke|fuzz|doctor|bug-report|hardening|security-review|linux-compositor|compat|ssh-smoke|verify-os|package|package-plan|release-check|ios-readiness>"
            );
            ExitCode::SUCCESS
        }
        Some("fmt") => run("cargo", &["fmt", "--all"]),
        Some("clippy") => run("cargo", &["clippy", "--workspace", "--all-targets"]),
        Some("test") => run("cargo", &["test", "--workspace"]),
        Some("build") => run("cargo", &["build", "--workspace"]),
        Some("check") => run("cargo", &["check", "--workspace"]),
        Some("layer-check") => run_layer_check(),
        Some("branding") => run_branding(),
        Some("config-default") => print_config_default(),
        Some("config-schema") => print_config_schema(),
        Some("bench") => run_bench(),
        Some("screenshot") => run_screenshot(),
        Some("fuzz-smoke") => run_fuzz_smoke(),
        Some("fuzz") => run_fuzz(),
        Some("doctor") => run_doctor(),
        Some("bug-report") => run_bug_report(),
        Some("hardening") => run_hardening(),
        Some("security-review") => run_security_review(),
        Some("linux-compositor") => run_linux_compositor(),
        Some("compat") => run_compat(),
        Some("ssh-smoke") => run_ssh_smoke(),
        Some("verify-os") => run_verify_os(),
        Some("package") => run_package(),
        Some("package-plan") => run_package_plan(),
        Some("release-check") => run_release_check(),
        Some("ios-readiness") => run_ios_readiness(),
        Some("ci") => run_ci(),
        Some(command) => {
            eprintln!("unknown xtask command: {command}");
            ExitCode::from(2)
        }
    }
}

const BRAND_SOURCE: &str = "crates/assets/branding/panea-source.png";
const BRAND_OUTPUT_DIR: &str = "crates/assets/branding/generated";
const BRAND_MASTER_SIZE: u32 = 1024;
const BRAND_MARK_FILL: u32 = 768;

fn run_branding() -> ExitCode {
    match generate_brand_assets(Path::new(BRAND_SOURCE), Path::new(BRAND_OUTPUT_DIR)) {
        Ok(()) => {
            println!("generated Panea brand assets in {BRAND_OUTPUT_DIR}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("branding generation failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn generate_brand_assets(source: &Path, output_dir: &Path) -> Result<(), String> {
    let source_image = image::open(source)
        .map_err(|error| format!("failed to decode {}: {error}", source.display()))?
        .to_rgba8();
    let master = square_brand_master(&source_image)?;

    fs::create_dir_all(output_dir)
        .map_err(|error| format!("failed to create {}: {error}", output_dir.display()))?;
    save_png(&master, &output_dir.join("panea-icon-1024.png"))?;

    for size in [16, 24, 32, 48, 64, 128, 256, 512] {
        let icon = resize(&master, size, size, FilterType::Lanczos3);
        save_png(&icon, &output_dir.join(format!("panea-icon-{size}.png")))?;
    }

    let ico_sizes = [16, 24, 32, 48, 64, 128, 256];
    let ico_images = encoded_png_sizes(&master, &ico_sizes)?;
    fs::write(output_dir.join("panea.ico"), encode_ico(&ico_images))
        .map_err(|error| format!("failed to write Panea ICO: {error}"))?;

    let icns_sizes = [16, 32, 64, 128, 256, 512, 1024];
    let icns_images = encoded_png_sizes(&master, &icns_sizes)?;
    fs::write(output_dir.join("Panea.icns"), encode_icns(&icns_images)?)
        .map_err(|error| format!("failed to write Panea ICNS: {error}"))?;
    Ok(())
}

fn square_brand_master(source: &RgbaImage) -> Result<RgbaImage, String> {
    if source.width() == 0 || source.height() == 0 {
        return Err("brand source is empty".to_owned());
    }
    let background = *source.get_pixel(0, 0);
    let (mut min_x, mut min_y) = (source.width(), source.height());
    let (mut max_x, mut max_y) = (0, 0);
    let mut found = false;

    for (x, y, pixel) in source.enumerate_pixels() {
        let difference = pixel.0[..3]
            .iter()
            .zip(background.0[..3].iter())
            .map(|(left, right)| i32::from(*left) - i32::from(*right))
            .map(|delta| delta * delta)
            .sum::<i32>();
        if pixel[3] > 0 && difference > 64 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            found = true;
        }
    }
    if !found {
        return Err("brand source contains no mark distinct from its background".to_owned());
    }

    let mark = crop_imm(source, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1).to_image();
    let scale = (BRAND_MARK_FILL as f64 / f64::from(mark.width().max(mark.height()))).min(1.0);
    let width = (f64::from(mark.width()) * scale).round() as u32;
    let height = (f64::from(mark.height()) * scale).round() as u32;
    let mark = resize(&mark, width.max(1), height.max(1), FilterType::Lanczos3);
    let mut master = RgbaImage::from_pixel(BRAND_MASTER_SIZE, BRAND_MASTER_SIZE, background);
    overlay(
        &mut master,
        &mark,
        i64::from((BRAND_MASTER_SIZE - mark.width()) / 2),
        i64::from((BRAND_MASTER_SIZE - mark.height()) / 2),
    );
    Ok(master)
}

fn save_png(image: &RgbaImage, path: &Path) -> Result<(), String> {
    image
        .save_with_format(path, ImageFormat::Png)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn encoded_png_sizes(master: &RgbaImage, sizes: &[u32]) -> Result<Vec<(u32, Vec<u8>)>, String> {
    sizes
        .iter()
        .map(|size| {
            let image = resize(master, *size, *size, FilterType::Lanczos3);
            let mut bytes = Vec::new();
            DynamicImage::ImageRgba8(image)
                .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
                .map_err(|error| format!("failed to encode {size}px PNG: {error}"))?;
            Ok((*size, bytes))
        })
        .collect()
}

fn encode_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let header_size = 6 + images.len() * 16;
    let data_size = images.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
    let mut output = Vec::with_capacity(header_size + data_size);
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = header_size as u32;
    for (size, bytes) in images {
        output.push(if *size >= 256 { 0 } else { *size as u8 });
        output.push(if *size >= 256 { 0 } else { *size as u8 });
        output.extend_from_slice(&[0, 0]);
        output.extend_from_slice(&1_u16.to_le_bytes());
        output.extend_from_slice(&32_u16.to_le_bytes());
        output.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        output.extend_from_slice(&offset.to_le_bytes());
        offset += bytes.len() as u32;
    }
    for (_, bytes) in images {
        output.extend_from_slice(bytes);
    }
    output
}

fn encode_icns(images: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut chunks = Vec::new();
    for (size, bytes) in images {
        let kind = match size {
            16 => *b"icp4",
            32 => *b"icp5",
            64 => *b"icp6",
            128 => *b"ic07",
            256 => *b"ic08",
            512 => *b"ic09",
            1024 => *b"ic10",
            _ => return Err(format!("unsupported ICNS icon size: {size}")),
        };
        chunks.extend_from_slice(&kind);
        chunks.extend_from_slice(&((bytes.len() + 8) as u32).to_be_bytes());
        chunks.extend_from_slice(bytes);
    }
    let mut output = Vec::with_capacity(chunks.len() + 8);
    output.extend_from_slice(b"icns");
    output.extend_from_slice(&((chunks.len() + 8) as u32).to_be_bytes());
    output.extend_from_slice(&chunks);
    Ok(output)
}

fn run_linux_compositor() -> ExitCode {
    println!(
        "{}",
        diagnostics::linux_compositor_verification_report().render_text()
    );
    ExitCode::SUCCESS
}

fn run_ios_readiness() -> ExitCode {
    println!(
        "{}",
        diagnostics::ios_companion_readiness_report().render_text()
    );
    ExitCode::SUCCESS
}

fn run_compat() -> ExitCode {
    let mut args = std::env::args().skip(2).collect::<Vec<_>>();
    let command = args.first().map_or("plan", String::as_str);

    match command {
        "help" | "--help" | "-h" => {
            print_compat_help();
            ExitCode::SUCCESS
        }
        "plan" => {
            print_compat_plan();
            ExitCode::SUCCESS
        }
        "run" => {
            args.remove(0);
            let options = match CompatRunOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    print_compat_help();
                    return ExitCode::from(2);
                }
            };
            run_compat_cases(&options)
        }
        other => {
            eprintln!("unknown compat command: {other}");
            print_compat_help();
            ExitCode::from(2)
        }
    }
}

fn run_ssh_smoke() -> ExitCode {
    let mut args = std::env::args().skip(2).collect::<Vec<_>>();
    let command = args.first().map_or("plan", String::as_str);

    match command {
        "help" | "--help" | "-h" => {
            print_ssh_smoke_help();
            ExitCode::SUCCESS
        }
        "plan" => {
            print_ssh_smoke_plan();
            ExitCode::SUCCESS
        }
        "run" => {
            args.remove(0);
            let options = match SshSmokeOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    print_ssh_smoke_help();
                    return ExitCode::from(2);
                }
            };
            run_ssh_smoke_cases(options)
        }
        other => {
            eprintln!("unknown ssh-smoke command: {other}");
            print_ssh_smoke_help();
            ExitCode::from(2)
        }
    }
}

fn print_ssh_smoke_help() {
    eprintln!("usage: cargo xtask ssh-smoke <plan|run>");
    eprintln!(
        "usage: cargo xtask ssh-smoke run --host <host> [--port <port>] [--user <user>] [--auth <agent|public_key|password>] [--identity-file <path>] [--password-env <name>] [--passphrase-env <name>] [--timeout-ms <ms>] [--remote-kind <posix|powershell>] [--report-dir <path>]"
    );
    eprintln!(
        "env fallback: PANEA_SSH_SMOKE_HOST, PANEA_SSH_SMOKE_PORT, PANEA_SSH_SMOKE_USER, PANEA_SSH_SMOKE_AUTH, PANEA_SSH_SMOKE_IDENTITY_FILE, PANEA_SSH_SMOKE_PASSWORD, PANEA_SSH_SMOKE_PASSPHRASE"
    );
}

fn print_ssh_smoke_plan() {
    println!("Panea SSH real-server smoke suite");
    println!("host_platform={}", CompatPlatform::detect().label());
    println!("runner=Panea transport-ssh backend with bounded polling and explicit trust provider");
    println!(
        "server=provide --host or PANEA_SSH_SMOKE_HOST; the runner does not create a privileged sshd"
    );
    println!("cases:");
    for case in SSH_SMOKE_CASES {
        println!("- {case}");
    }
    println!("report=target/ssh-smoke/<platform>.md by default");
}

const SSH_SMOKE_CASES: &[&str] = &[
    "reject unknown host with default rejecting trust provider",
    "accept and persist unknown host only through explicit trust action",
    "authenticate and observe remote PTY output marker",
    "resize remote PTY through TerminalTransport::resize",
    "observe Unicode and large-output markers from the remote session",
    "reconnect using require_known against the persisted smoke known-hosts file",
    "detect changed host key from the smoke known-hosts file",
    "enforce remote OSC 52 clipboard denial by default",
];

#[derive(Debug, Clone)]
struct SshSmokeOptions {
    host: Option<String>,
    port: u16,
    username: Option<String>,
    auth_method: AuthMethod,
    identity_file: Option<PathBuf>,
    password_env: String,
    passphrase_env: String,
    timeout: Duration,
    report_dir: PathBuf,
    remote_kind: SshSmokeRemoteKind,
}

impl SshSmokeOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            host: std::env::var("PANEA_SSH_SMOKE_HOST").ok(),
            port: parse_env_u16("PANEA_SSH_SMOKE_PORT")?.unwrap_or(22),
            username: std::env::var("PANEA_SSH_SMOKE_USER").ok(),
            auth_method: parse_auth_method(
                &std::env::var("PANEA_SSH_SMOKE_AUTH").unwrap_or_else(|_| "agent".to_owned()),
            )?,
            identity_file: std::env::var_os("PANEA_SSH_SMOKE_IDENTITY_FILE").map(PathBuf::from),
            password_env: "PANEA_SSH_SMOKE_PASSWORD".to_owned(),
            passphrase_env: "PANEA_SSH_SMOKE_PASSPHRASE".to_owned(),
            timeout: Duration::from_secs(5),
            report_dir: PathBuf::from("target/ssh-smoke"),
            remote_kind: SshSmokeRemoteKind::Posix,
        };
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--host" => options.host = Some(required_arg(args, &mut index, "--host")?),
                "--port" => {
                    let value = required_arg(args, &mut index, "--port")?;
                    options.port = value
                        .parse::<u16>()
                        .map_err(|_| format!("invalid --port value: {value}"))?;
                }
                "--user" => options.username = Some(required_arg(args, &mut index, "--user")?),
                "--auth" => {
                    let value = required_arg(args, &mut index, "--auth")?;
                    options.auth_method = parse_auth_method(&value)?;
                }
                "--identity-file" => {
                    options.identity_file = Some(PathBuf::from(required_arg(
                        args,
                        &mut index,
                        "--identity-file",
                    )?));
                }
                "--password-env" => {
                    options.password_env = required_arg(args, &mut index, "--password-env")?;
                }
                "--passphrase-env" => {
                    options.passphrase_env = required_arg(args, &mut index, "--passphrase-env")?;
                }
                "--timeout-ms" => {
                    let value = required_arg(args, &mut index, "--timeout-ms")?;
                    let millis = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --timeout-ms value: {value}"))?;
                    if millis < 500 {
                        return Err("--timeout-ms must be at least 500".to_owned());
                    }
                    options.timeout = Duration::from_millis(millis);
                }
                "--remote-kind" => {
                    let value = required_arg(args, &mut index, "--remote-kind")?;
                    options.remote_kind = SshSmokeRemoteKind::parse(&value)
                        .ok_or_else(|| format!("unknown --remote-kind value: {value}"))?;
                }
                "--report-dir" => {
                    options.report_dir =
                        PathBuf::from(required_arg(args, &mut index, "--report-dir")?);
                }
                other => return Err(format!("unknown ssh-smoke option: {other}")),
            }
            index += 1;
        }

        if matches!(options.auth_method, AuthMethod::PublicKey) && options.identity_file.is_none() {
            return Err(
                "--auth public_key requires --identity-file or PANEA_SSH_SMOKE_IDENTITY_FILE"
                    .to_owned(),
            );
        }

        Ok(options)
    }

    fn profile(&self, name: &str, policy: KnownHostsPolicy, marker: &str) -> SshConnectionProfile {
        let mut profile =
            SshConnectionProfile::new(name.to_owned(), self.host.clone().unwrap_or_default());
        profile.port = self.port;
        profile.username = self.username.clone();
        profile.auth_method = self.auth_method.clone();
        profile.identity_file = self.identity_file.clone();
        profile.known_hosts_policy = policy;
        profile.remote_command = Some(self.remote_kind.command(marker));
        profile.shell_integration = false;
        profile.agent_forwarding = false;
        profile.connect_timeout = self.timeout;
        profile
    }
}

fn required_arg(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_env_u16(name: &str) -> Result<Option<u16>, String> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u16>()
            .map(Some)
            .map_err(|_| format!("invalid {name} value: {value}")),
        Err(_) => Ok(None),
    }
}

fn parse_auth_method(value: &str) -> Result<AuthMethod, String> {
    match value {
        "agent" => Ok(AuthMethod::Agent),
        "public_key" | "publickey" | "key" => Ok(AuthMethod::PublicKey),
        "password" => Ok(AuthMethod::Password),
        "keyboard_interactive" | "keyboard-interactive" => Ok(AuthMethod::KeyboardInteractive),
        other => Err(format!("unknown auth method: {other}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SshSmokeRemoteKind {
    Posix,
    Powershell,
}

impl SshSmokeRemoteKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "posix" => Some(Self::Posix),
            "powershell" | "pwsh" => Some(Self::Powershell),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::Powershell => "powershell",
        }
    }

    fn command(self, marker: &str) -> String {
        match self {
            Self::Posix => format!(
                "sleep 0.2; printf '%s\\n' {marker}; printf 'unicode: caf\\303\\251 \\346\\274\\242\\345\\255\\227 \\360\\237\\230\\200\\n'; stty size 2>/dev/null || true; i=0; while [ $i -lt 128 ]; do printf 'panea-ssh-large-%03d\\n' \"$i\"; i=$((i+1)); done"
            ),
            Self::Powershell => format!(
                "powershell -NoLogo -NoProfile -Command \"$u=[string]::Concat('unicode: caf',[char]0x00E9,' ',[char]0x6F22,[char]0x5B57,' ',[char]::ConvertFromUtf32(0x1F600)); Write-Output '{marker}'; Write-Output $u; 0..127 | ForEach-Object {{ Write-Output ('panea-ssh-large-' + $_.ToString('000')) }}\""
            ),
        }
    }
}

fn run_ssh_smoke_cases(options: SshSmokeOptions) -> ExitCode {
    let Some(host) = options
        .host
        .as_deref()
        .filter(|host| !host.trim().is_empty())
    else {
        eprintln!("missing SSH smoke server: provide --host or PANEA_SSH_SMOKE_HOST");
        return ExitCode::from(2);
    };

    if let Err(error) = fs::create_dir_all(&options.report_dir) {
        eprintln!(
            "failed to create SSH smoke report directory {}: {error}",
            options.report_dir.display()
        );
        return ExitCode::from(1);
    }

    let known_hosts_path = options.report_dir.join(format!(
        "known-hosts-{}.json",
        CompatPlatform::detect().label()
    ));
    let _ = fs::remove_file(&known_hosts_path);

    let results = vec![
        run_ssh_reject_unknown_host_case(&options, &known_hosts_path),
        run_ssh_accept_and_output_case(&options, &known_hosts_path),
        run_ssh_reconnect_case(&options, &known_hosts_path),
        run_ssh_changed_host_case(&options, &known_hosts_path),
        run_ssh_remote_osc52_policy_case(),
    ];

    for result in &results {
        println!("{}", result.render_line());
    }

    let report_path = options
        .report_dir
        .join(format!("{}.md", CompatPlatform::detect().label()));
    if let Err(error) = fs::write(
        &report_path,
        render_ssh_smoke_report(host, &options, &known_hosts_path, &results),
    ) {
        eprintln!("failed to write {}: {error}", report_path.display());
        return ExitCode::from(1);
    }
    println!("wrote SSH smoke report {}", report_path.display());

    if results
        .iter()
        .any(|result| result.status == SshSmokeStatus::Fail)
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn run_ssh_reject_unknown_host_case(
    options: &SshSmokeOptions,
    known_hosts_path: &Path,
) -> SshSmokeResult {
    let started = Instant::now();
    let mut profile = options.profile(
        "ssh-smoke-reject-unknown",
        KnownHostsPolicy::Ask,
        "panea-ssh-reject",
    );
    profile.auth_method = AuthMethod::None;
    let mut secrets = EnvSecretProvider::new(options);
    let mut trust = ScriptedHostTrustProvider::reject();

    match SshTransport::connect_with_security(
        profile,
        smoke_ssh_size(),
        known_hosts_path,
        &mut secrets,
        &mut trust,
    ) {
        Ok(mut transport) => {
            let _ = transport.shutdown();
            SshSmokeResult::fail(
                "reject-unknown-host",
                started.elapsed(),
                "unknown host unexpectedly connected".to_owned(),
            )
        }
        Err(error)
            if error.to_string().contains("unknown")
                || error.to_string().contains("explicit trust") =>
        {
            SshSmokeResult::pass(
                "reject-unknown-host",
                started.elapsed(),
                format!(
                    "unknown host rejected before auth; trust_requests={}",
                    trust.requests.len()
                ),
            )
        }
        Err(error) => SshSmokeResult::fail(
            "reject-unknown-host",
            started.elapsed(),
            format!("expected unknown-host rejection, got: {error}"),
        ),
    }
}

fn run_ssh_accept_and_output_case(
    options: &SshSmokeOptions,
    known_hosts_path: &Path,
) -> SshSmokeResult {
    run_ssh_output_case(
        "accept-store-output",
        options,
        known_hosts_path,
        KnownHostsPolicy::Ask,
        ScriptedHostTrustProvider::trust_and_store(),
        "panea-ssh-smoke",
    )
}

fn run_ssh_reconnect_case(options: &SshSmokeOptions, known_hosts_path: &Path) -> SshSmokeResult {
    run_ssh_output_case(
        "require-known-reconnect",
        options,
        known_hosts_path,
        KnownHostsPolicy::RequireKnown,
        ScriptedHostTrustProvider::reject(),
        "panea-ssh-reconnect",
    )
}

fn run_ssh_output_case(
    name: &'static str,
    options: &SshSmokeOptions,
    known_hosts_path: &Path,
    policy: KnownHostsPolicy,
    mut trust_provider: ScriptedHostTrustProvider,
    marker: &'static str,
) -> SshSmokeResult {
    let started = Instant::now();
    let profile = options.profile(name, policy, marker);
    let mut secrets = EnvSecretProvider::new(options);
    let mut lifecycle = Vec::new();
    let mut output = Vec::new();

    let mut transport = match SshTransport::connect_with_security(
        profile,
        smoke_ssh_size(),
        known_hosts_path,
        &mut secrets,
        &mut trust_provider,
    ) {
        Ok(transport) => transport,
        Err(error) => {
            return SshSmokeResult::fail(
                name,
                started.elapsed(),
                format!("connect failed: {error}"),
            );
        }
    };

    let resize_result = transport.resize(TerminalSize::new(100, 40, 800, 640));
    if let Err(error) = resize_result {
        let _ = transport.shutdown();
        return SshSmokeResult::fail(name, started.elapsed(), format!("resize failed: {error}"));
    }

    let deadline = Instant::now() + options.timeout;
    let marker_bytes = marker.as_bytes();
    let unicode_marker = b"unicode:";
    let large_marker = b"panea-ssh-large-127";
    let mut saw_marker = false;
    let mut saw_unicode = false;
    let mut saw_large = false;

    while Instant::now() < deadline {
        match transport.poll_output() {
            Ok(poll) => {
                lifecycle.extend(poll.lifecycle);
                output.extend(poll.bytes);
                saw_marker = saw_marker
                    || output
                        .windows(marker_bytes.len())
                        .any(|w| w == marker_bytes);
                saw_unicode = saw_unicode
                    || output
                        .windows(unicode_marker.len())
                        .any(|w| w == unicode_marker);
                saw_large = saw_large
                    || output
                        .windows(large_marker.len())
                        .any(|w| w == large_marker);
                if saw_marker && saw_unicode && saw_large && poll.closed {
                    break;
                }
            }
            Err(error) => {
                let _ = transport.shutdown();
                return SshSmokeResult {
                    name,
                    status: SshSmokeStatus::Fail,
                    duration: started.elapsed(),
                    detail: format!("poll failed: {error}"),
                    bytes_received: output.len(),
                    preview: preview_bytes(&output),
                    lifecycle,
                };
            }
        }
        thread::sleep(Duration::from_millis(10));
    }

    let shutdown = transport.shutdown();
    let persisted = KnownHosts::load(known_hosts_path)
        .ok()
        .and_then(|known_hosts| {
            options
                .host
                .as_deref()
                .and_then(|host| known_hosts.entry_for(host, options.port))
                .cloned()
        })
        .is_some();

    if !saw_marker || !saw_unicode || !saw_large {
        return SshSmokeResult {
            name,
            status: SshSmokeStatus::Fail,
            duration: started.elapsed(),
            detail: format!(
                "missing output marker(s): marker={saw_marker} unicode={saw_unicode} large={saw_large} shutdown={:?}",
                shutdown.map_err(|error| error.to_string())
            ),
            bytes_received: output.len(),
            preview: preview_bytes(&output),
            lifecycle,
        };
    }

    if let Err(error) = shutdown {
        return SshSmokeResult {
            name,
            status: SshSmokeStatus::Fail,
            duration: started.elapsed(),
            detail: format!("output observed, but shutdown failed: {error}"),
            bytes_received: output.len(),
            preview: preview_bytes(&output),
            lifecycle,
        };
    }

    SshSmokeResult {
        name,
        status: SshSmokeStatus::Pass,
        duration: started.elapsed(),
        detail: format!(
            "remote PTY output, Unicode marker, large-output marker, resize event, and shutdown passed; known_host_persisted={persisted}; trust_requests={}",
            trust_provider.requests.len()
        ),
        bytes_received: output.len(),
        preview: preview_bytes(&output),
        lifecycle,
    }
}

fn run_ssh_changed_host_case(options: &SshSmokeOptions, known_hosts_path: &Path) -> SshSmokeResult {
    let started = Instant::now();
    let Some(host) = options.host.clone() else {
        return SshSmokeResult::fail(
            "changed-host-key",
            started.elapsed(),
            "host is not configured".to_owned(),
        );
    };

    let bogus_key = HostKey::from_raw(host, options.port, "panea-test-host-key", b"changed-key");
    let mut known_hosts = KnownHosts::empty();
    known_hosts.trust(&bogus_key);
    if let Err(error) = known_hosts.save(known_hosts_path) {
        return SshSmokeResult::fail(
            "changed-host-key",
            started.elapsed(),
            format!("failed to write changed-host fixture: {error}"),
        );
    }

    let mut profile = options.profile(
        "ssh-smoke-changed-host",
        KnownHostsPolicy::RequireKnown,
        "panea-ssh-changed",
    );
    profile.auth_method = AuthMethod::None;
    let mut secrets = EnvSecretProvider::new(options);
    let mut trust = ScriptedHostTrustProvider::reject();

    match SshTransport::connect_with_security(
        profile,
        smoke_ssh_size(),
        known_hosts_path,
        &mut secrets,
        &mut trust,
    ) {
        Ok(mut transport) => {
            let _ = transport.shutdown();
            SshSmokeResult::fail(
                "changed-host-key",
                started.elapsed(),
                "changed host key unexpectedly connected".to_owned(),
            )
        }
        Err(error)
            if error.to_string().contains("changed host key")
                || error.to_string().contains("blocked") =>
        {
            SshSmokeResult::pass(
                "changed-host-key",
                started.elapsed(),
                "changed host key was detected and blocked".to_owned(),
            )
        }
        Err(error) => SshSmokeResult::fail(
            "changed-host-key",
            started.elapsed(),
            format!("expected changed-host-key rejection, got: {error}"),
        ),
    }
}

fn run_ssh_remote_osc52_policy_case() -> SshSmokeResult {
    let started = Instant::now();
    let request = Osc52ClipboardRequest {
        target: Osc52ClipboardTarget::Clipboard,
        payload_base64: "cGFuZWE=".to_owned(),
        remote: true,
    };
    let decision = evaluate_osc52_clipboard_write(&request, &Osc52ClipboardPolicy::default());
    if decision.is_allowed() {
        SshSmokeResult::fail(
            "remote-osc52-policy",
            started.elapsed(),
            "remote OSC 52 write was allowed by default".to_owned(),
        )
    } else {
        SshSmokeResult::pass(
            "remote-osc52-policy",
            started.elapsed(),
            format!("remote OSC 52 write denied by default: {decision:?}"),
        )
    }
}

fn smoke_ssh_size() -> TerminalSize {
    TerminalSize::new(80, 24, 640, 384)
}

#[derive(Debug)]
struct EnvSecretProvider {
    password_env: String,
    passphrase_env: String,
}

impl EnvSecretProvider {
    fn new(options: &SshSmokeOptions) -> Self {
        Self {
            password_env: options.password_env.clone(),
            passphrase_env: options.passphrase_env.clone(),
        }
    }
}

impl SecretProvider for EnvSecretProvider {
    fn request_secret(
        &mut self,
        request: SecretRequest,
    ) -> security::SecurityResult<Option<SecretString>> {
        let env_name = match request {
            SecretRequest::SshPassword { .. } => &self.password_env,
            SecretRequest::SshKeyPassphrase { .. } => &self.passphrase_env,
        };
        Ok(std::env::var(env_name).ok().map(SecretString::new))
    }
}

#[derive(Debug)]
struct ScriptedHostTrustProvider {
    unknown_action: HostKeyTrustAction,
    changed_action: HostKeyTrustAction,
    requests: Vec<HostKeyTrustRequest>,
}

impl ScriptedHostTrustProvider {
    fn reject() -> Self {
        Self {
            unknown_action: HostKeyTrustAction::Reject,
            changed_action: HostKeyTrustAction::Reject,
            requests: Vec::new(),
        }
    }

    fn trust_and_store() -> Self {
        Self {
            unknown_action: HostKeyTrustAction::TrustAndStore,
            changed_action: HostKeyTrustAction::Reject,
            requests: Vec::new(),
        }
    }
}

impl HostTrustProvider for ScriptedHostTrustProvider {
    fn decide_host_trust(
        &mut self,
        request: HostKeyTrustRequest,
    ) -> security::SecurityResult<HostKeyTrustAction> {
        let action = match request.reason {
            security::HostKeyTrustReason::UnknownHost => self.unknown_action,
            security::HostKeyTrustReason::ChangedHostKey
            | security::HostKeyTrustReason::PinnedFingerprintMismatch => self.changed_action,
        };
        self.requests.push(request);
        Ok(action)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SshSmokeStatus {
    Pass,
    Fail,
}

impl SshSmokeStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone)]
struct SshSmokeResult {
    name: &'static str,
    status: SshSmokeStatus,
    duration: Duration,
    detail: String,
    bytes_received: usize,
    preview: String,
    lifecycle: Vec<TransportLifecycleEvent>,
}

impl SshSmokeResult {
    fn pass(name: &'static str, duration: Duration, detail: String) -> Self {
        Self {
            name,
            status: SshSmokeStatus::Pass,
            duration,
            detail,
            bytes_received: 0,
            preview: String::new(),
            lifecycle: Vec::new(),
        }
    }

    fn fail(name: &'static str, duration: Duration, detail: String) -> Self {
        Self {
            name,
            status: SshSmokeStatus::Fail,
            duration,
            detail,
            bytes_received: 0,
            preview: String::new(),
            lifecycle: Vec::new(),
        }
    }

    fn render_line(&self) -> String {
        format!(
            "[{}] {} duration_ms={} bytes={} {}",
            self.status.label(),
            self.name,
            self.duration.as_millis(),
            self.bytes_received,
            self.detail
        )
    }
}

fn render_ssh_smoke_report(
    host: &str,
    options: &SshSmokeOptions,
    known_hosts_path: &Path,
    results: &[SshSmokeResult],
) -> String {
    let mut lines = vec![
        "# Panea SSH Smoke Report".to_owned(),
        String::new(),
        format!("- Host platform: {}", CompatPlatform::detect().label()),
        format!("- SSH target: {}:{}", host, options.port),
        format!("- Auth method: {:?}", options.auth_method),
        format!("- Identity file configured: {}", options.identity_file.is_some()),
        format!("- Remote command kind: {}", options.remote_kind.label()),
        format!("- Timeout ms: {}", options.timeout.as_millis()),
        format!("- Smoke known-hosts path: {}", known_hosts_path.display()),
        "- Secrets are read only from named environment variables and are not written to this report"
            .to_owned(),
        "- Cross-OS status: this report verifies only the current host; macOS/Linux/Windows reports must be collected separately"
            .to_owned(),
        String::new(),
        "| Status | Case | Duration ms | Bytes | Detail |".to_owned(),
        "| --- | --- | ---: | ---: | --- |".to_owned(),
    ];

    for result in results {
        lines.push(format!(
            "| {} | `{}` | {} | {} | {} |",
            result.status.label(),
            result.name,
            result.duration.as_millis(),
            result.bytes_received,
            result.detail.replace('|', "\\|")
        ));
    }

    lines.push(String::new());
    lines.push("## Output Preview And Lifecycle".to_owned());
    for result in results {
        lines.push(String::new());
        lines.push(format!("### `{}`", result.name));
        lines.push(format!("- status: {}", result.status.label()));
        if !result.lifecycle.is_empty() {
            lines.push(format!("- lifecycle: {:?}", result.lifecycle));
        }
        if !result.preview.is_empty() {
            lines.push("- preview:".to_owned());
            lines.push("```text".to_owned());
            lines.push(result.preview.clone());
            lines.push("```".to_owned());
        }
    }

    lines.join("\n")
}

fn print_compat_help() {
    eprintln!("usage: cargo xtask compat <plan|run>");
    eprintln!(
        "usage: cargo xtask compat run [--required-only] [--category <name>] [--case <key>] [--timeout-ms <ms>] [--report-dir <path>]"
    );
    eprintln!("categories: shells, editors, tuis, multiplexers, ssh, protocol");
}

fn print_compat_plan() {
    println!("Panea app compatibility suite");
    println!("host_platform={}", CompatPlatform::detect().label());
    println!("runner=bounded real-process and real-PTY smoke checks");
    println!("manual_checks=recorded separately; they are not treated as pass");
    println!("cases:");
    for case in compat_cases() {
        println!(
            "- {} category={} required={} platforms={} mode={} verifies={}",
            case.key,
            case.category,
            case.required,
            case.platforms.label(),
            case.mode_label(),
            case.verifies
        );
    }
}

#[derive(Debug, Clone)]
struct CompatRunOptions {
    category: Option<CompatCategory>,
    case_key: Option<String>,
    required_only: bool,
    timeout: Duration,
    report_dir: PathBuf,
}

impl CompatRunOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut category = None;
        let mut case_key = None;
        let mut required_only = false;
        let mut timeout = Duration::from_secs(5);
        let mut report_dir = PathBuf::from("target/compatibility");
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--category" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--category requires a value".to_owned());
                    };
                    category = Some(
                        CompatCategory::parse(value)
                            .ok_or_else(|| format!("unknown compatibility category: {value}"))?,
                    );
                }
                "--case" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--case requires a value".to_owned());
                    };
                    case_key = Some(value.clone());
                }
                "--required-only" => required_only = true,
                "--timeout-ms" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--timeout-ms requires a value".to_owned());
                    };
                    let millis = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --timeout-ms value: {value}"))?;
                    if millis < 100 {
                        return Err("--timeout-ms must be at least 100".to_owned());
                    }
                    timeout = Duration::from_millis(millis);
                }
                "--report-dir" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--report-dir requires a value".to_owned());
                    };
                    report_dir = PathBuf::from(value);
                }
                other => return Err(format!("unknown compat option: {other}")),
            }
            index += 1;
        }

        Ok(Self {
            category,
            case_key,
            required_only,
            timeout,
            report_dir,
        })
    }
}

fn run_compat_cases(options: &CompatRunOptions) -> ExitCode {
    let host = CompatPlatform::detect();
    let cases = compat_cases();
    let mut selected = cases
        .iter()
        .filter(|case| case.platforms.matches(host))
        .filter(|case| {
            options
                .category
                .is_none_or(|category| category == case.category)
        })
        .filter(|case| {
            options
                .case_key
                .as_deref()
                .is_none_or(|key| key == case.key)
        })
        .filter(|case| !options.required_only || case.required)
        .collect::<Vec<_>>();

    selected.sort_by_key(|case| (case.category.sort_key(), case.key));

    if selected.is_empty() {
        eprintln!("no compatibility cases matched current filters");
        return ExitCode::from(2);
    }

    let mut results = Vec::new();
    for case in selected {
        let result = run_compat_case(case, options.timeout);
        println!("{}", result.render_line());
        results.push(result);
    }

    if let Err(error) = fs::create_dir_all(&options.report_dir) {
        eprintln!(
            "failed to create compatibility report directory {}: {error}",
            options.report_dir.display()
        );
        return ExitCode::from(1);
    }

    let report_path = options.report_dir.join(format!("{}.md", host.label()));
    if let Err(error) = fs::write(&report_path, render_compat_report(host, &results)) {
        eprintln!("failed to write {}: {error}", report_path.display());
        return ExitCode::from(1);
    }
    println!("wrote compatibility report {}", report_path.display());

    if results
        .iter()
        .any(|result| result.status == CompatResultStatus::Fail)
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn run_compat_case(case: &CompatCase, timeout: Duration) -> CompatResult {
    let started = Instant::now();
    let outcome = match &case.kind {
        CompatCaseKind::Process {
            program,
            args,
            marker,
        } => run_process_compat(program, args, *marker, timeout),
        CompatCaseKind::Pty { profile, marker } => run_pty_compat(profile.clone(), marker, timeout),
        CompatCaseKind::Manual { instructions } => Ok(CompatExecution {
            status: CompatResultStatus::Manual,
            detail: instructions.to_string(),
            bytes_received: 0,
            preview: String::new(),
            lifecycle: Vec::new(),
            diagnostics: None,
        }),
    };

    match outcome {
        Ok(mut execution) => {
            execution.detail = format!("{}; {}", execution.detail, case.verifies);
            CompatResult {
                key: case.key,
                category: case.category,
                required: case.required,
                status: execution.status,
                duration: started.elapsed(),
                detail: execution.detail,
                bytes_received: execution.bytes_received,
                preview: execution.preview,
                lifecycle: execution.lifecycle,
                diagnostics: execution.diagnostics,
            }
        }
        Err(error) => CompatResult {
            key: case.key,
            category: case.category,
            required: case.required,
            status: if error.missing_program {
                if case.required {
                    CompatResultStatus::Fail
                } else {
                    CompatResultStatus::Skip
                }
            } else {
                CompatResultStatus::Fail
            },
            duration: started.elapsed(),
            detail: error.message,
            bytes_received: error.bytes_received,
            preview: error.preview,
            lifecycle: error.lifecycle,
            diagnostics: error.diagnostics,
        },
    }
}

fn run_process_compat(
    program: &str,
    args: &[String],
    marker: Option<&str>,
    timeout: Duration,
) -> Result<CompatExecution, CompatRunError> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            CompatRunError::missing_or_failed(
                error.kind() == std::io::ErrorKind::NotFound,
                format!("failed to spawn {program}: {error}"),
            )
        })?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child.wait_with_output().map_err(|error| {
                    CompatRunError::failed(format!("failed to read {program} output: {error}"))
                })?;
                let mut bytes = output.stdout;
                bytes.extend_from_slice(&output.stderr);
                let preview = preview_bytes(&bytes);

                if !output.status.success() {
                    return Err(CompatRunError {
                        missing_program: false,
                        message: format!("{program} exited with status {:?}", output.status.code()),
                        bytes_received: bytes.len(),
                        preview,
                        lifecycle: Vec::new(),
                        diagnostics: None,
                    });
                }

                if let Some(marker) = marker
                    && !preview.contains(marker)
                    && !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                {
                    return Err(CompatRunError {
                        missing_program: false,
                        message: format!("{program} did not emit marker {marker:?}"),
                        bytes_received: bytes.len(),
                        preview,
                        lifecycle: Vec::new(),
                        diagnostics: None,
                    });
                }

                return Ok(CompatExecution {
                    status: CompatResultStatus::Pass,
                    detail: format!(
                        "process command completed: {}",
                        command_label(program, args)
                    ),
                    bytes_received: bytes.len(),
                    preview,
                    lifecycle: Vec::new(),
                    diagnostics: None,
                });
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CompatRunError::failed(format!(
                    "timed out after {:?}: {}",
                    timeout,
                    command_label(program, args)
                )));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CompatRunError::failed(format!(
                    "failed while waiting for {program}: {error}"
                )));
            }
        }
    }
}

fn run_pty_compat(
    profile: LocalShellProfile,
    marker: &'static [u8],
    timeout: Duration,
) -> Result<CompatExecution, CompatRunError> {
    let mut transport = LocalPtyTransport::spawn(profile, TerminalSize::new(80, 24, 640, 384))
        .map_err(|error| {
            CompatRunError::missing_or_failed(
                error.to_string().contains("failed to spawn"),
                format!("failed to spawn PTY case: {error}"),
            )
        })?;

    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut lifecycle = Vec::new();
    let mut saw_marker = false;
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));

    while Instant::now() < deadline {
        match transport.poll_output() {
            Ok(poll) => {
                answer_terminal_queries(&mut transport, &mut terminal, &poll.bytes)?;
                lifecycle.extend(poll.lifecycle);
                output.extend(poll.bytes);
                saw_marker =
                    saw_marker || output.windows(marker.len()).any(|window| window == marker);
                let closed =
                    poll.closed || matches!(transport.state(), TransportState::Closed { .. });
                if closed {
                    break;
                }
            }
            Err(error) => {
                let diagnostics = transport.diagnostics();
                let _ = transport.shutdown();
                return Err(CompatRunError {
                    missing_program: false,
                    message: format!("PTY poll failed: {error}"),
                    bytes_received: output.len(),
                    preview: preview_bytes(&output),
                    lifecycle,
                    diagnostics: Some(format_pty_diagnostics(&diagnostics)),
                });
            }
        }

        thread::sleep(Duration::from_millis(10));
    }

    let before_shutdown = transport.diagnostics();
    let shutdown_result = transport.shutdown();
    let after_shutdown = transport.diagnostics();
    let diagnostics = format!(
        "before_shutdown:\n{}\nafter_shutdown:\n{}",
        format_pty_diagnostics(&before_shutdown),
        format_pty_diagnostics(&after_shutdown)
    );

    if !saw_marker {
        let runtime_unavailable = pty_runtime_unavailable(&output);
        return Err(CompatRunError {
            missing_program: runtime_unavailable,
            message: if runtime_unavailable {
                "PTY launcher exists, but its optional runtime is unavailable on this host"
                    .to_owned()
            } else {
                format!(
                    "PTY case did not observe marker {:?}; shutdown={:?}",
                    String::from_utf8_lossy(marker),
                    shutdown_result.map_err(|error| error.to_string())
                )
            },
            bytes_received: output.len(),
            preview: preview_bytes(&output),
            lifecycle,
            diagnostics: Some(diagnostics),
        });
    }

    if let Err(error) = shutdown_result {
        return Err(CompatRunError {
            missing_program: false,
            message: format!("PTY marker observed, but bounded shutdown failed: {error}"),
            bytes_received: output.len(),
            preview: preview_bytes(&output),
            lifecycle,
            diagnostics: Some(diagnostics),
        });
    }

    if after_shutdown.shutdown_timed_out {
        return Err(CompatRunError {
            missing_program: false,
            message: "PTY marker observed, but shutdown timed out".to_owned(),
            bytes_received: output.len(),
            preview: preview_bytes(&output),
            lifecycle,
            diagnostics: Some(diagnostics),
        });
    }

    Ok(CompatExecution {
        status: CompatResultStatus::Pass,
        detail: "PTY command emitted marker and shut down cleanly".to_owned(),
        bytes_received: output.len(),
        preview: preview_bytes(&output),
        lifecycle,
        diagnostics: Some(diagnostics),
    })
}

fn pty_runtime_unavailable(output: &[u8]) -> bool {
    let output = String::from_utf8_lossy(output).to_ascii_lowercase();
    output.contains("windows subsystem for linux has no installed distributions")
        || output.contains("there are no distributions installed")
}

fn answer_terminal_queries(
    transport: &mut LocalPtyTransport,
    terminal: &mut TerminalEmulator,
    bytes: &[u8],
) -> Result<(), CompatRunError> {
    let responses = terminal_responses_for(terminal, bytes).map_err(CompatRunError::failed)?;
    if !responses.is_empty() {
        transport.write_input(&responses).map_err(|error| {
            CompatRunError::failed(format!("failed to write terminal query response: {error}"))
        })?;
    }

    Ok(())
}

fn terminal_responses_for(
    terminal: &mut TerminalEmulator,
    bytes: &[u8],
) -> Result<Vec<u8>, String> {
    terminal
        .apply_bytes(bytes)
        .map_err(|error| format!("failed to parse PTY output: {error}"))?;
    Ok(terminal.state_mut().take_pending_output())
}

fn command_label(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn preview_bytes(bytes: &[u8]) -> String {
    const LIMIT: usize = 320;
    let start = bytes.len().saturating_sub(LIMIT);
    String::from_utf8_lossy(&bytes[start..])
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn format_pty_diagnostics(diagnostics: &LocalPtyDiagnostics) -> String {
    [
        format!("command={}", diagnostics.command),
        format!("pid={:?}", diagnostics.process_id),
        format!("state={:?}", diagnostics.state),
        format!("bytes_received={}", diagnostics.bytes_received),
        format!("read_events={}", diagnostics.read_events),
        format!(
            "last_reader_preview={:?}",
            String::from_utf8_lossy(&diagnostics.last_bytes_preview)
        ),
        format!("reader_started={}", diagnostics.reader_started),
        format!("reader_stopped={}", diagnostics.reader_stopped),
        format!("reader_error={:?}", diagnostics.reader_error),
        format!("child_exited={}", diagnostics.child_exited),
        format!("kill_attempted={}", diagnostics.kill_attempted),
        format!("shutdown_timed_out={}", diagnostics.shutdown_timed_out),
    ]
    .join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatPlatform {
    Windows,
    Macos,
    LinuxX11,
    LinuxWayland,
    Any,
    Unix,
}

impl CompatPlatform {
    fn detect() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "linux") {
            match std::env::var("XDG_SESSION_TYPE") {
                Ok(value) if value.eq_ignore_ascii_case("wayland") => Self::LinuxWayland,
                _ => Self::LinuxX11,
            }
        } else {
            Self::Any
        }
    }

    fn matches(self, host: Self) -> bool {
        match self {
            Self::Any => true,
            Self::Unix => !matches!(host, Self::Windows),
            Self::LinuxX11 => host == Self::LinuxX11,
            Self::LinuxWayland => host == Self::LinuxWayland,
            Self::Windows => host == Self::Windows,
            Self::Macos => host == Self::Macos,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::LinuxX11 => "linux-x11",
            Self::LinuxWayland => "linux-wayland",
            Self::Any => "any",
            Self::Unix => "macos/linux",
        }
    }

    fn parse_target(value: &str) -> Option<Self> {
        match value {
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::Macos),
            "linux-x11" => Some(Self::LinuxX11),
            "linux-wayland" => Some(Self::LinuxWayland),
            _ => None,
        }
    }

    fn screenshot_key(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::LinuxX11 => "linux-x11",
            Self::LinuxWayland => "linux-wayland",
            Self::Any | Self::Unix => "windows",
        }
    }

    fn is_linux(self) -> bool {
        matches!(self, Self::LinuxX11 | Self::LinuxWayland)
    }
}

fn run_verify_os() -> ExitCode {
    let mut args = std::env::args().skip(2).collect::<Vec<_>>();
    let command = args.first().map_or("plan", String::as_str);

    match command {
        "help" | "--help" | "-h" => {
            print_verify_os_help();
            ExitCode::SUCCESS
        }
        "plan" => {
            print_verify_os_plan();
            ExitCode::SUCCESS
        }
        "run" => {
            args.remove(0);
            let options = match VerifyOsOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    print_verify_os_help();
                    return ExitCode::from(2);
                }
            };
            run_verify_os_suite(options)
        }
        "report" => {
            print_verify_os_report_hint();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown verify-os command: {other}");
            print_verify_os_help();
            ExitCode::from(2)
        }
    }
}

fn print_verify_os_help() {
    eprintln!("usage: cargo xtask verify-os <plan|run|report>");
    eprintln!(
        "usage: cargo xtask verify-os run [--target-platform <windows|macos|linux-x11|linux-wayland>] [--suite <smoke|ci|full>] [--report-dir <path>] [--timeout-ms <ms>] [--allow-missing-screenshot-baseline] [--with-ssh]"
    );
}

fn print_verify_os_plan() {
    println!("Panea real cross-OS verification runners");
    println!("required runners:");
    println!("- windows: native Windows runner");
    println!("- macos: macOS runner");
    println!("- linux-x11: Linux runner with XDG_SESSION_TYPE=x11");
    println!("- linux-wayland: Linux runner with XDG_SESSION_TYPE=wayland");
    println!("default report root: target/cross-os/<platform>/");
    println!("runner command:");
    println!("  cargo xtask verify-os run --target-platform <platform> --suite ci");
    println!("step coverage:");
    for step in verify_os_steps(&VerifyOsOptions::default_for(CompatPlatform::detect())) {
        println!("- {} [{}]", step.key, step.category);
    }
    println!(
        "SSH real-server tests run only when --with-ssh is passed or PANEA_SSH_SMOKE_HOST is set."
    );
}

fn print_verify_os_report_hint() {
    println!("Cross-OS verification reports are written by:");
    println!("  cargo xtask verify-os run --target-platform <platform>");
    println!("default markdown: target/cross-os/<platform>/report.md");
    println!("default JSON:     target/cross-os/<platform>/report.json");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifySuite {
    Smoke,
    Ci,
    Full,
}

impl VerifySuite {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "smoke" => Some(Self::Smoke),
            "ci" => Some(Self::Ci),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Ci => "ci",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone)]
struct VerifyOsOptions {
    target_platform: CompatPlatform,
    suite: VerifySuite,
    report_root: PathBuf,
    timeout: Duration,
    allow_missing_screenshot_baseline: bool,
    with_ssh: bool,
}

impl VerifyOsOptions {
    fn default_for(target_platform: CompatPlatform) -> Self {
        Self {
            target_platform,
            suite: VerifySuite::Smoke,
            report_root: PathBuf::from("target/cross-os"),
            timeout: Duration::from_secs(120),
            allow_missing_screenshot_baseline: false,
            with_ssh: std::env::var_os("PANEA_SSH_SMOKE_HOST").is_some(),
        }
    }

    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default_for(CompatPlatform::detect());
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--target-platform" | "--platform" => {
                    let value = required_arg(args, &mut index, "--target-platform")?;
                    options.target_platform = CompatPlatform::parse_target(&value)
                        .ok_or_else(|| format!("unsupported target platform: {value}"))?;
                }
                "--suite" => {
                    let value = required_arg(args, &mut index, "--suite")?;
                    options.suite = VerifySuite::parse(&value)
                        .ok_or_else(|| format!("unsupported verify suite: {value}"))?;
                }
                "--report-dir" => {
                    options.report_root =
                        PathBuf::from(required_arg(args, &mut index, "--report-dir")?);
                }
                "--timeout-ms" => {
                    let value = required_arg(args, &mut index, "--timeout-ms")?;
                    let millis = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --timeout-ms value: {value}"))?;
                    if millis < 500 {
                        return Err("--timeout-ms must be at least 500".to_owned());
                    }
                    options.timeout = Duration::from_millis(millis);
                }
                "--allow-missing-screenshot-baseline" => {
                    options.allow_missing_screenshot_baseline = true;
                }
                "--with-ssh" => {
                    options.with_ssh = true;
                }
                other => return Err(format!("unknown verify-os option: {other}")),
            }
            index += 1;
        }

        Ok(options)
    }

    fn report_dir(&self) -> PathBuf {
        self.report_root.join(self.target_platform.label())
    }
}

#[derive(Debug, Clone)]
enum VerifyStepKind {
    Command {
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone)]
struct VerifyStep {
    key: &'static str,
    category: &'static str,
    description: &'static str,
    required: bool,
    kind: VerifyStepKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyStepStatus {
    Passed,
    Failed,
    Skipped,
    Blocked,
    TimedOut,
}

impl VerifyStepStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone)]
struct VerifyStepResult {
    key: String,
    category: String,
    description: String,
    required: bool,
    status: VerifyStepStatus,
    duration: Duration,
    exit_code: Option<i32>,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    stdout_preview: String,
    stderr_preview: String,
    note: String,
}

fn run_verify_os_suite(options: VerifyOsOptions) -> ExitCode {
    let report_dir = options.report_dir();
    let logs_dir = report_dir.join("logs");
    if let Err(error) = fs::create_dir_all(&logs_dir) {
        eprintln!(
            "failed to create cross-OS verification report directory {}: {error}",
            logs_dir.display()
        );
        return ExitCode::from(1);
    }

    let started = Instant::now();
    let steps = verify_os_steps(&options);
    let mut results = Vec::new();
    for step in &steps {
        println!("verify-os: running {} ({})", step.key, step.category);
        let result = run_verify_step(step, &options, &logs_dir);
        println!(
            "verify-os: {} -> {} ({:.2}s)",
            result.key,
            result.status.label(),
            result.duration.as_secs_f64()
        );
        results.push(result);
    }

    let report = VerifyOsReport {
        target_platform: options.target_platform,
        detected_platform: CompatPlatform::detect(),
        suite: options.suite,
        started_duration: started.elapsed(),
        results,
    };

    let markdown_path = report_dir.join("report.md");
    let json_path = report_dir.join("report.json");
    if let Err(error) = fs::write(&markdown_path, report.render_markdown()) {
        eprintln!("failed to write {}: {error}", markdown_path.display());
        return ExitCode::from(1);
    }
    if let Err(error) = fs::write(&json_path, report.render_json()) {
        eprintln!("failed to write {}: {error}", json_path.display());
        return ExitCode::from(1);
    }
    println!(
        "wrote cross-OS verification report {}",
        markdown_path.display()
    );
    println!("wrote cross-OS verification JSON {}", json_path.display());

    if report.has_hard_failure() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn verify_os_steps(options: &VerifyOsOptions) -> Vec<VerifyStep> {
    let target = options.target_platform;
    let mut steps = Vec::new();

    if matches!(options.suite, VerifySuite::Ci | VerifySuite::Full) {
        steps.push(command_step(
            "format",
            "unit",
            "cargo formatting gate",
            true,
            "cargo",
            ["fmt", "--all", "--check"],
            target_env(target),
        ));
    }

    steps.extend([
        xtask_step(
            "layer-boundaries",
            "architecture",
            "workspace dependency boundary validation",
            true,
            ["layer-check"],
            target,
        ),
        command_step(
            "unit-workspace",
            "unit",
            "workspace unit tests",
            true,
            "cargo",
            ["test", "--workspace"],
            target_env(target),
        ),
        command_step(
            "parser-tests",
            "parser",
            "parser-focused tests",
            true,
            "cargo",
            ["test", "-p", "term-parser"],
            target_env(target),
        ),
        command_step(
            "unicode-tests",
            "unicode",
            "terminal core Unicode and grapheme tests",
            true,
            "cargo",
            ["test", "-p", "term-core", "unicode"],
            target_env(target),
        ),
        xtask_step(
            "fuzz-regressions",
            "fuzz",
            "fuzz regression/property smoke tests",
            true,
            ["fuzz-smoke"],
            target,
        ),
        command_step(
            "renderer-tests",
            "renderer",
            "renderer contract and WGPU tests",
            true,
            "cargo",
            ["test", "-p", "render-core", "-p", "render-wgpu"],
            target_env(target),
        ),
        command_step(
            "config-tests",
            "config",
            "portable config and TOML tests",
            true,
            "cargo",
            ["test", "-p", "config-core", "-p", "config-toml"],
            target_env(target),
        ),
        command_step(
            "clipboard-policy-tests",
            "clipboard",
            "clipboard and OSC 52 policy tests",
            true,
            "cargo",
            ["test", "-p", "security", "osc52"],
            target_env(target),
        ),
        command_step(
            "shell-tests",
            "shell",
            "shell integration parser and activation tests",
            true,
            "cargo",
            ["test", "-p", "shell-integration"],
            target_env(target),
        ),
        command_step(
            "pty-tests",
            "pty",
            "transport-core and local PTY tests",
            true,
            "cargo",
            ["test", "-p", "transport-core", "-p", "transport-pty"],
            target_env(target),
        ),
        xtask_step(
            "screenshot-tests",
            "screenshot",
            "deterministic renderer screenshot verification",
            true,
            [
                "screenshot",
                "verify",
                "--platform",
                target.screenshot_key(),
                "--report-dir",
                "target/cross-os/screenshots",
            ],
            target,
        ),
        xtask_step(
            "compat-required",
            "compatibility",
            "required shell and protocol compatibility smoke tests",
            true,
            [
                "compat",
                "run",
                "--required-only",
                "--report-dir",
                "target/cross-os/compatibility",
            ],
            target,
        ),
        xtask_step(
            "doctor-json",
            "diagnostics",
            "installed diagnostics model JSON smoke",
            true,
            ["doctor", "--json"],
            target,
        ),
    ]);

    if target.is_linux() {
        steps.push(xtask_step(
            "linux-compositor-diagnostics",
            "platform",
            "Linux X11/Wayland compositor diagnostics",
            true,
            ["linux-compositor"],
            target,
        ));
    } else {
        steps.push(skipped_step(
            "linux-compositor-diagnostics",
            "platform",
            "Linux X11/Wayland compositor diagnostics",
            "not a Linux target",
        ));
    }

    if options.with_ssh {
        steps.push(xtask_step(
            "ssh-smoke",
            "ssh",
            "real SSH server smoke tests",
            true,
            [
                "ssh-smoke",
                "run",
                "--report-dir",
                "target/cross-os/ssh-smoke",
            ],
            target,
        ));
    } else {
        steps.push(skipped_step(
            "ssh-smoke",
            "ssh",
            "real SSH server smoke tests",
            "PANEA_SSH_SMOKE_HOST is not configured and --with-ssh was not passed",
        ));
    }

    steps.push(xtask_step(
        "packaging-smoke",
        "packaging",
        "packaged artifact build and doctor smoke",
        true,
        [
            "package",
            "smoke",
            "--profile",
            "dev",
            "--build",
            "--timeout-ms",
            "10000",
        ],
        target,
    ));

    if matches!(options.suite, VerifySuite::Full) {
        steps.push(xtask_step(
            "compat-optional",
            "compatibility",
            "optional app compatibility probes for installed tools",
            false,
            [
                "compat",
                "run",
                "--report-dir",
                "target/cross-os/compatibility-full",
            ],
            target,
        ));
    }

    steps
}

fn command_step<const N: usize>(
    key: &'static str,
    category: &'static str,
    description: &'static str,
    required: bool,
    program: &'static str,
    args: [&'static str; N],
    env: Vec<(String, String)>,
) -> VerifyStep {
    VerifyStep {
        key,
        category,
        description,
        required,
        kind: VerifyStepKind::Command {
            program: program.to_owned(),
            args: args.into_iter().map(str::to_owned).collect(),
            env,
        },
    }
}

fn xtask_step<const N: usize>(
    key: &'static str,
    category: &'static str,
    description: &'static str,
    required: bool,
    args: [&'static str; N],
    target: CompatPlatform,
) -> VerifyStep {
    let mut cargo_args = vec![
        "run".to_owned(),
        "-p".to_owned(),
        "xtask".to_owned(),
        "--".to_owned(),
    ];
    cargo_args.extend(args.into_iter().map(str::to_owned));
    VerifyStep {
        key,
        category,
        description,
        required,
        kind: VerifyStepKind::Command {
            program: "cargo".to_owned(),
            args: cargo_args,
            env: target_env(target),
        },
    }
}

fn skipped_step(
    key: &'static str,
    category: &'static str,
    description: &'static str,
    reason: &'static str,
) -> VerifyStep {
    VerifyStep {
        key,
        category,
        description,
        required: false,
        kind: VerifyStepKind::Skipped {
            reason: reason.to_owned(),
        },
    }
}

fn target_env(target: CompatPlatform) -> Vec<(String, String)> {
    match target {
        CompatPlatform::LinuxX11 => vec![("XDG_SESSION_TYPE".to_owned(), "x11".to_owned())],
        CompatPlatform::LinuxWayland => {
            vec![("XDG_SESSION_TYPE".to_owned(), "wayland".to_owned())]
        }
        _ => Vec::new(),
    }
}

fn run_verify_step(
    step: &VerifyStep,
    options: &VerifyOsOptions,
    logs_dir: &Path,
) -> VerifyStepResult {
    let started = Instant::now();
    match &step.kind {
        VerifyStepKind::Skipped { reason } => VerifyStepResult {
            key: step.key.to_owned(),
            category: step.category.to_owned(),
            description: step.description.to_owned(),
            required: step.required,
            status: VerifyStepStatus::Skipped,
            duration: started.elapsed(),
            exit_code: None,
            stdout_log: None,
            stderr_log: None,
            stdout_preview: String::new(),
            stderr_preview: String::new(),
            note: reason.clone(),
        },
        VerifyStepKind::Command { program, args, env } => {
            let stdout_log = logs_dir.join(format!("{}.stdout.log", step.key));
            let stderr_log = logs_dir.join(format!("{}.stderr.log", step.key));
            let stdout = match fs::File::create(&stdout_log) {
                Ok(file) => file,
                Err(error) => {
                    return verify_spawn_failure(step, started.elapsed(), error.to_string());
                }
            };
            let stderr = match fs::File::create(&stderr_log) {
                Ok(file) => file,
                Err(error) => {
                    return verify_spawn_failure(step, started.elapsed(), error.to_string());
                }
            };

            let mut command = Command::new(program);
            command
                .args(args)
                .envs(env.iter().map(|(key, value)| (key, value)))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    return verify_spawn_failure(step, started.elapsed(), error.to_string());
                }
            };

            let mut timed_out = false;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) if started.elapsed() >= options.timeout => {
                        timed_out = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(50)),
                    Err(error) => {
                        return verify_spawn_failure(step, started.elapsed(), error.to_string());
                    }
                }
            };

            let stdout_preview = read_log_tail(&stdout_log);
            let stderr_preview = read_log_tail(&stderr_log);
            let exit_code = status.and_then(|status| status.code());
            let mut verify_status = match status {
                Some(status) if status.success() => VerifyStepStatus::Passed,
                Some(_) => VerifyStepStatus::Failed,
                None if timed_out => VerifyStepStatus::TimedOut,
                None => VerifyStepStatus::Failed,
            };
            let mut note = if timed_out {
                format!("step exceeded {} ms", options.timeout.as_millis())
            } else {
                format!("command: {} {}", program, args.join(" "))
            };

            if verify_status == VerifyStepStatus::Failed
                && options.allow_missing_screenshot_baseline
                && step.key == "screenshot-tests"
                && stderr_preview.contains("missing screenshot baselines")
            {
                verify_status = VerifyStepStatus::Blocked;
                note = "missing screenshot baseline on this platform; capture baselines on the target host before treating screenshot parity as verified".to_owned();
            }

            VerifyStepResult {
                key: step.key.to_owned(),
                category: step.category.to_owned(),
                description: step.description.to_owned(),
                required: step.required,
                status: verify_status,
                duration: started.elapsed(),
                exit_code,
                stdout_log: Some(stdout_log),
                stderr_log: Some(stderr_log),
                stdout_preview,
                stderr_preview,
                note,
            }
        }
    }
}

fn verify_spawn_failure(
    step: &VerifyStep,
    duration: Duration,
    message: String,
) -> VerifyStepResult {
    VerifyStepResult {
        key: step.key.to_owned(),
        category: step.category.to_owned(),
        description: step.description.to_owned(),
        required: step.required,
        status: VerifyStepStatus::Failed,
        duration,
        exit_code: None,
        stdout_log: None,
        stderr_log: None,
        stdout_preview: String::new(),
        stderr_preview: String::new(),
        note: message,
    }
}

fn read_log_tail(path: &Path) -> String {
    const LIMIT: usize = 4096;
    let Ok(bytes) = fs::read(path) else {
        return String::new();
    };
    let start = bytes.len().saturating_sub(LIMIT);
    String::from_utf8_lossy(&bytes[start..])
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

struct VerifyOsReport {
    target_platform: CompatPlatform,
    detected_platform: CompatPlatform,
    suite: VerifySuite,
    started_duration: Duration,
    results: Vec<VerifyStepResult>,
}

impl VerifyOsReport {
    fn has_hard_failure(&self) -> bool {
        self.results.iter().any(|result| {
            result.required
                && matches!(
                    result.status,
                    VerifyStepStatus::Failed | VerifyStepStatus::TimedOut
                )
        })
    }

    fn render_markdown(&self) -> String {
        let mut lines = Vec::new();
        lines.push("# Panea Cross-OS Verification Report".to_owned());
        lines.push(String::new());
        lines.push(format!(
            "- Target platform: `{}`",
            self.target_platform.label()
        ));
        lines.push(format!(
            "- Detected host: `{}`",
            self.detected_platform.label()
        ));
        lines.push(format!("- Suite: `{}`", self.suite.label()));
        lines.push(format!(
            "- Duration: `{:.2}s`",
            self.started_duration.as_secs_f64()
        ));
        lines.push(format!(
            "- Result: `{}`",
            if self.has_hard_failure() {
                "failed"
            } else {
                "completed"
            }
        ));
        lines.push(String::new());
        lines.push("| Step | Category | Required | Status | Seconds | Exit | Note |".to_owned());
        lines.push("| --- | --- | --- | --- | ---: | ---: | --- |".to_owned());
        for result in &self.results {
            lines.push(format!(
                "| `{}` | `{}` | {} | `{}` | {:.2} | {} | {} |",
                result.key,
                result.category,
                result.required,
                result.status.label(),
                result.duration.as_secs_f64(),
                result
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                markdown_escape(&result.note)
            ));
        }
        lines.push(String::new());
        lines.push("## Failure Previews".to_owned());
        lines.push(String::new());
        let mut wrote_preview = false;
        for result in &self.results {
            if matches!(
                result.status,
                VerifyStepStatus::Failed | VerifyStepStatus::TimedOut | VerifyStepStatus::Blocked
            ) {
                wrote_preview = true;
                lines.push(format!("### {}", result.key));
                lines.push(String::new());
                if let Some(path) = &result.stderr_log {
                    lines.push(format!("- stderr log: `{}`", path.display()));
                }
                if let Some(path) = &result.stdout_log {
                    lines.push(format!("- stdout log: `{}`", path.display()));
                }
                if !result.stderr_preview.is_empty() {
                    lines.push(String::new());
                    lines.push("```text".to_owned());
                    lines.push(result.stderr_preview.clone());
                    lines.push("```".to_owned());
                }
                if !result.stdout_preview.is_empty() {
                    lines.push(String::new());
                    lines.push("```text".to_owned());
                    lines.push(result.stdout_preview.clone());
                    lines.push("```".to_owned());
                }
                lines.push(String::new());
            }
        }
        if !wrote_preview {
            lines.push("No failed, timed-out, or blocked steps.".to_owned());
            lines.push(String::new());
        }
        lines.join("\n")
    }

    fn render_json(&self) -> String {
        let mut json = String::new();
        json.push_str("{\n");
        json.push_str(&format!(
            "  \"target_platform\": \"{}\",\n",
            json_escape(self.target_platform.label())
        ));
        json.push_str(&format!(
            "  \"detected_platform\": \"{}\",\n",
            json_escape(self.detected_platform.label())
        ));
        json.push_str(&format!(
            "  \"suite\": \"{}\",\n",
            json_escape(self.suite.label())
        ));
        json.push_str(&format!(
            "  \"duration_ms\": {},\n",
            self.started_duration.as_millis()
        ));
        json.push_str(&format!(
            "  \"hard_failure\": {},\n",
            self.has_hard_failure()
        ));
        json.push_str("  \"steps\": [\n");
        for (index, result) in self.results.iter().enumerate() {
            if index > 0 {
                json.push_str(",\n");
            }
            json.push_str("    {\n");
            json.push_str(&format!(
                "      \"key\": \"{}\",\n",
                json_escape(&result.key)
            ));
            json.push_str(&format!(
                "      \"category\": \"{}\",\n",
                json_escape(&result.category)
            ));
            json.push_str(&format!(
                "      \"description\": \"{}\",\n",
                json_escape(&result.description)
            ));
            json.push_str(&format!("      \"required\": {},\n", result.required));
            json.push_str(&format!(
                "      \"status\": \"{}\",\n",
                json_escape(result.status.label())
            ));
            json.push_str(&format!(
                "      \"duration_ms\": {},\n",
                result.duration.as_millis()
            ));
            match result.exit_code {
                Some(code) => json.push_str(&format!("      \"exit_code\": {},\n", code)),
                None => json.push_str("      \"exit_code\": null,\n"),
            }
            json.push_str(&format!(
                "      \"note\": \"{}\"",
                json_escape(&result.note)
            ));
            json.push_str("\n    }");
        }
        json.push_str("\n  ]\n");
        json.push_str("}\n");
        json
    }
}

fn markdown_escape(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatCategory {
    Shells,
    Editors,
    Tuis,
    Multiplexers,
    Ssh,
    Protocol,
}

impl fmt::Display for CompatCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shells => "shells",
            Self::Editors => "editors",
            Self::Tuis => "tuis",
            Self::Multiplexers => "multiplexers",
            Self::Ssh => "ssh",
            Self::Protocol => "protocol",
        })
    }
}

impl CompatCategory {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "shells" => Some(Self::Shells),
            "editors" => Some(Self::Editors),
            "tuis" => Some(Self::Tuis),
            "multiplexers" => Some(Self::Multiplexers),
            "ssh" => Some(Self::Ssh),
            "protocol" => Some(Self::Protocol),
            _ => None,
        }
    }

    fn sort_key(self) -> u8 {
        match self {
            Self::Shells => 0,
            Self::Editors => 1,
            Self::Tuis => 2,
            Self::Multiplexers => 3,
            Self::Ssh => 4,
            Self::Protocol => 5,
        }
    }
}

#[derive(Debug, Clone)]
enum CompatCaseKind {
    Process {
        program: &'static str,
        args: Vec<String>,
        marker: Option<&'static str>,
    },
    Pty {
        profile: LocalShellProfile,
        marker: &'static [u8],
    },
    Manual {
        instructions: &'static str,
    },
}

#[derive(Debug, Clone)]
struct CompatCase {
    key: &'static str,
    category: CompatCategory,
    platforms: CompatPlatform,
    required: bool,
    kind: CompatCaseKind,
    verifies: &'static str,
}

impl CompatCase {
    fn mode_label(&self) -> &'static str {
        match self.kind {
            CompatCaseKind::Process { .. } => "process",
            CompatCaseKind::Pty { .. } => "pty",
            CompatCaseKind::Manual { .. } => "manual",
        }
    }
}

fn compat_cases() -> Vec<CompatCase> {
    let mut cases = Vec::new();

    cases.extend(shell_compat_cases());
    cases.extend(process_compat_cases(
        CompatCategory::Editors,
        [
            ("editor-vim", "vim", &["--version"][..]),
            ("editor-neovim", "nvim", &["--version"][..]),
            ("editor-nano", "nano", &["--version"][..]),
            ("editor-helix", "hx", &["--version"][..]),
        ],
        "editor binary starts and reports a version; full-screen editing remains a manual PTY check",
    ));
    cases.extend(process_compat_cases(
        CompatCategory::Tuis,
        [
            ("tui-htop", "htop", &["--version"][..]),
            ("tui-btop", "btop", &["--version"][..]),
            ("tui-lazygit", "lazygit", &["--version"][..]),
            ("tui-fzf", "fzf", &["--version"][..]),
            ("tool-git", "git", &["--version"][..]),
            ("tool-cargo", "cargo", &["--version"][..]),
            ("tool-node", "node", &["--version"][..]),
            ("tool-npm", "npm", &["--version"][..]),
            ("tool-pnpm", "pnpm", &["--version"][..]),
            ("tool-yarn", "yarn", &["--version"][..]),
            ("tool-python", "python", &["--version"][..]),
        ],
        "tool starts and reports version; interactive output protocol remains covered by manual checklist",
    ));
    cases.extend(process_compat_cases(
        CompatCategory::Multiplexers,
        [
            ("mux-tmux", "tmux", &["-V"][..]),
            ("mux-screen", "screen", &["-v"][..]),
            ("mux-zellij", "zellij", &["--version"][..]),
        ],
        "external multiplexer binary starts; nested session behavior remains a manual PTY check",
    ));

    cases.push(process_case(ProcessCaseSpec {
        key: "ssh-client",
        category: CompatCategory::Ssh,
        platforms: CompatPlatform::Any,
        required: false,
        program: "ssh",
        args: &["-V"],
        marker: None,
        verifies: "SSH client exists; Panea transport verification lives in cargo xtask ssh-smoke",
    }));
    cases.push(manual_case(
        "ssh-local-server",
        CompatCategory::Ssh,
        CompatPlatform::Any,
        false,
        "start a controlled local SSH server, open remote PTY, resize, emit Unicode, test OSC 52 policy, then disconnect cleanly",
        "remote PTY, resize, Unicode, clipboard policy, disconnect handling",
    ));

    cases.extend(protocol_compat_cases());
    cases
}

fn shell_compat_cases() -> Vec<CompatCase> {
    vec![
        pty_case(
            "shell-powershell",
            CompatCategory::Shells,
            CompatPlatform::Windows,
            true,
            LocalShellProfile::powershell().with_args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                "Write-Output panea-compat-powershell",
            ]),
            b"panea-compat-powershell",
            "PowerShell PTY output, lifecycle, and bounded teardown",
        ),
        pty_case(
            "shell-cmd",
            CompatCategory::Shells,
            CompatPlatform::Windows,
            true,
            LocalShellProfile::cmd().with_args(["/D", "/C", "echo panea-compat-cmd"]),
            b"panea-compat-cmd",
            "cmd.exe PTY output, lifecycle, and bounded teardown",
        ),
        pty_case(
            "shell-pwsh",
            CompatCategory::Shells,
            CompatPlatform::Any,
            false,
            LocalShellProfile::custom("pwsh", "pwsh").with_args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                "Write-Output panea-compat-pwsh",
            ]),
            b"panea-compat-pwsh",
            "PowerShell Core PTY output when installed",
        ),
        pty_case(
            "shell-sh",
            CompatCategory::Shells,
            CompatPlatform::Unix,
            true,
            LocalShellProfile::custom("sh", "sh")
                .with_args(["-lc", "printf '%s\\n' panea-compat-sh"]),
            b"panea-compat-sh",
            "POSIX shell PTY output, lifecycle, and bounded teardown",
        ),
        pty_case(
            "shell-bash",
            CompatCategory::Shells,
            CompatPlatform::Any,
            false,
            LocalShellProfile::custom("bash", "bash")
                .with_args(["-lc", "printf '%s\\n' panea-compat-bash"]),
            b"panea-compat-bash",
            "bash PTY output when installed",
        ),
        pty_case(
            "shell-zsh",
            CompatCategory::Shells,
            CompatPlatform::Any,
            false,
            LocalShellProfile::custom("zsh", "zsh")
                .with_args(["-lc", "printf '%s\\n' panea-compat-zsh"]),
            b"panea-compat-zsh",
            "zsh PTY output when installed",
        ),
        pty_case(
            "shell-fish",
            CompatCategory::Shells,
            CompatPlatform::Any,
            false,
            LocalShellProfile::custom("fish", "fish").with_args(["-c", "echo panea-compat-fish"]),
            b"panea-compat-fish",
            "fish PTY output when installed",
        ),
        pty_case(
            "shell-wsl",
            CompatCategory::Shells,
            CompatPlatform::Windows,
            false,
            LocalShellProfile::wsl(None).with_args([
                "--exec",
                "sh",
                "-lc",
                "printf '%s\\n' panea-compat-wsl",
            ]),
            b"panea-compat-wsl",
            "WSL shell PTY output when WSL is installed",
        ),
    ]
}

fn protocol_compat_cases() -> Vec<CompatCase> {
    if cfg!(windows) {
        vec![pty_case(
            "protocol-ansi-marker",
            CompatCategory::Protocol,
            CompatPlatform::Windows,
            true,
            LocalShellProfile::powershell().with_args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                "$e=[char]27; $b=[char]7; Write-Output \"$e[38;2;255;0;0mpanea-compat-truecolor$e[0m\"; Write-Output \"$e]0;panea-title$b\"",
            ]),
            b"panea-compat-truecolor",
            "truecolor SGR and OSC title bytes can traverse the PTY output path",
        )]
    } else {
        vec![pty_case(
            "protocol-ansi-marker",
            CompatCategory::Protocol,
            CompatPlatform::Unix,
            true,
            LocalShellProfile::custom("sh", "sh").with_args([
                "-lc",
                "printf '\\033[38;2;255;0;0mpanea-compat-truecolor\\033[0m\\n\\033]0;panea-title\\007\\n'",
            ]),
            b"panea-compat-truecolor",
            "truecolor SGR and OSC title bytes can traverse the PTY output path",
        )]
    }
}

fn process_compat_cases<const N: usize>(
    category: CompatCategory,
    commands: [(&'static str, &'static str, &[&'static str]); N],
    verifies: &'static str,
) -> Vec<CompatCase> {
    commands
        .into_iter()
        .map(|(key, program, args)| {
            process_case(ProcessCaseSpec {
                key,
                category,
                platforms: CompatPlatform::Any,
                required: false,
                program,
                args,
                marker: None,
                verifies,
            })
        })
        .collect()
}

fn pty_case(
    key: &'static str,
    category: CompatCategory,
    platforms: CompatPlatform,
    required: bool,
    profile: LocalShellProfile,
    marker: &'static [u8],
    verifies: &'static str,
) -> CompatCase {
    CompatCase {
        key,
        category,
        platforms,
        required,
        kind: CompatCaseKind::Pty { profile, marker },
        verifies,
    }
}

#[derive(Debug, Clone, Copy)]
struct ProcessCaseSpec<'a> {
    key: &'static str,
    category: CompatCategory,
    platforms: CompatPlatform,
    required: bool,
    program: &'static str,
    args: &'a [&'static str],
    marker: Option<&'static str>,
    verifies: &'static str,
}

fn process_case(spec: ProcessCaseSpec<'_>) -> CompatCase {
    CompatCase {
        key: spec.key,
        category: spec.category,
        platforms: spec.platforms,
        required: spec.required,
        kind: CompatCaseKind::Process {
            program: spec.program,
            args: spec.args.iter().map(|arg| (*arg).to_owned()).collect(),
            marker: spec.marker,
        },
        verifies: spec.verifies,
    }
}

fn manual_case(
    key: &'static str,
    category: CompatCategory,
    platforms: CompatPlatform,
    required: bool,
    instructions: &'static str,
    verifies: &'static str,
) -> CompatCase {
    CompatCase {
        key,
        category,
        platforms,
        required,
        kind: CompatCaseKind::Manual { instructions },
        verifies,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatResultStatus {
    Pass,
    Fail,
    Skip,
    Manual,
}

impl CompatResultStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
            Self::Manual => "MANUAL",
        }
    }
}

#[derive(Debug, Clone)]
struct CompatExecution {
    status: CompatResultStatus,
    detail: String,
    bytes_received: usize,
    preview: String,
    lifecycle: Vec<TransportLifecycleEvent>,
    diagnostics: Option<String>,
}

#[derive(Debug, Clone)]
struct CompatRunError {
    missing_program: bool,
    message: String,
    bytes_received: usize,
    preview: String,
    lifecycle: Vec<TransportLifecycleEvent>,
    diagnostics: Option<String>,
}

impl CompatRunError {
    fn failed(message: String) -> Self {
        Self {
            missing_program: false,
            message,
            bytes_received: 0,
            preview: String::new(),
            lifecycle: Vec::new(),
            diagnostics: None,
        }
    }

    fn missing_or_failed(missing_program: bool, message: String) -> Self {
        Self {
            missing_program,
            message,
            bytes_received: 0,
            preview: String::new(),
            lifecycle: Vec::new(),
            diagnostics: None,
        }
    }
}

#[derive(Debug, Clone)]
struct CompatResult {
    key: &'static str,
    category: CompatCategory,
    required: bool,
    status: CompatResultStatus,
    duration: Duration,
    detail: String,
    bytes_received: usize,
    preview: String,
    lifecycle: Vec<TransportLifecycleEvent>,
    diagnostics: Option<String>,
}

impl CompatResult {
    fn render_line(&self) -> String {
        format!(
            "[{}] {} category={} required={} duration_ms={} bytes={} {}",
            self.status.label(),
            self.key,
            self.category,
            self.required,
            self.duration.as_millis(),
            self.bytes_received,
            self.detail
        )
    }
}

fn render_compat_report(host: CompatPlatform, results: &[CompatResult]) -> String {
    let mut lines = vec![
        "# Panea Compatibility Report".to_owned(),
        String::new(),
        format!("- Host platform: {}", host.label()),
        "- Scope: bounded app/process/PTY smoke checks; manual-required rows are not passes"
            .to_owned(),
        "- Cross-OS status: not cross-OS verified until Windows, macOS, Linux X11, and Linux Wayland reports exist"
            .to_owned(),
        String::new(),
        "| Status | Case | Category | Required | Duration ms | Bytes | Detail |".to_owned(),
        "| --- | --- | --- | --- | ---: | ---: | --- |".to_owned(),
    ];

    for result in results {
        lines.push(format!(
            "| {} | `{}` | {} | {} | {} | {} | {} |",
            result.status.label(),
            result.key,
            result.category,
            result.required,
            result.duration.as_millis(),
            result.bytes_received,
            result.detail.replace('|', "\\|")
        ));
    }

    lines.push(String::new());
    lines.push("## Output Preview And Diagnostics".to_owned());
    for result in results {
        lines.push(String::new());
        lines.push(format!("### `{}`", result.key));
        lines.push(format!("- status: {}", result.status.label()));
        if !result.lifecycle.is_empty() {
            lines.push(format!("- lifecycle: {:?}", result.lifecycle));
        }
        if !result.preview.is_empty() {
            lines.push("- preview:".to_owned());
            lines.push("```text".to_owned());
            lines.push(result.preview.clone());
            lines.push("```".to_owned());
        }
        if let Some(diagnostics) = &result.diagnostics {
            lines.push("- diagnostics:".to_owned());
            lines.push("```text".to_owned());
            lines.push(diagnostics.clone());
            lines.push("```".to_owned());
        }
    }

    lines.join("\n")
}

fn run_hardening() -> ExitCode {
    let input = match doctor_input() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    println!(
        "{}",
        diagnostics::stability_hardening_report(&input).render_text()
    );
    ExitCode::SUCCESS
}

fn run_security_review() -> ExitCode {
    let input = match doctor_input() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    println!(
        "{}",
        diagnostics::security_review_report(&input).render_text()
    );
    ExitCode::SUCCESS
}

fn run_package_plan() -> ExitCode {
    println!("{}", diagnostics::packaging_plan().render_text());
    ExitCode::SUCCESS
}

fn run_package() -> ExitCode {
    let mut args = std::env::args().skip(2).collect::<Vec<_>>();
    let command = args.first().map_or("plan", String::as_str);

    match command {
        "help" | "--help" | "-h" => {
            print_package_help();
            ExitCode::SUCCESS
        }
        "plan" => {
            print_package_plan();
            ExitCode::SUCCESS
        }
        "build" => {
            args.remove(0);
            let options = match PackageOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    print_package_help();
                    return ExitCode::from(2);
                }
            };
            build_package(&options)
        }
        "smoke" => {
            args.remove(0);
            let mut options = match PackageOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    print_package_help();
                    return ExitCode::from(2);
                }
            };
            options.build_before_smoke = options.build_before_smoke || options.smoke_build_flag;
            smoke_package(&options)
        }
        other => {
            eprintln!("unknown package command: {other}");
            print_package_help();
            ExitCode::from(2)
        }
    }
}

fn print_package_help() {
    eprintln!("usage: cargo xtask package <plan|build|smoke>");
    eprintln!(
        "usage: cargo xtask package build [--target-platform <windows|macos|linux-x11|linux-wayland>] [--profile <dev|release>] [--out-dir <path>] [--skip-cargo-build]"
    );
    eprintln!(
        "usage: cargo xtask package smoke [--target-platform <windows|macos|linux-x11|linux-wayland>] [--profile <dev|release>] [--out-dir <path>] [--build] [--timeout-ms <ms>]"
    );
}

fn print_package_plan() {
    println!("{}", diagnostics::packaging_plan().render_text());
    println!();
    println!("Implemented package artifacts:");
    println!("- windows: staged directory, portable ZIP, and per-user installer EXE");
    println!("- macos: Panea.app bundle, distribution ZIP, and DMG");
    println!("- linux: staged directory, portable tar.gz, deb, AppImage, and rpm packages");
    println!();
    println!("Build on each target OS:");
    println!("  cargo xtask package build --profile release");
    println!("Smoke the packaged doctor command:");
    println!("  cargo xtask package smoke --profile release --build");
    println!("The smoke also runs packaged headless shell and GUI startup/input commands:");
    println!("  panea shell-smoke --json");
    println!("  panea gui-smoke --startup --json");
    println!("  panea gui-smoke --terminal-io --json");
    println!();
    println!("Signing and notarization activate through documented release credentials.");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageProfile {
    Dev,
    Release,
}

impl PackageProfile {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "dev" | "debug" => Some(Self::Dev),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Release => "release",
        }
    }

    fn target_dir(self) -> &'static str {
        match self {
            Self::Dev => "debug",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Clone)]
struct PackageOptions {
    target_platform: CompatPlatform,
    profile: PackageProfile,
    out_dir: PathBuf,
    skip_cargo_build: bool,
    build_before_smoke: bool,
    smoke_build_flag: bool,
    timeout: Duration,
}

impl PackageOptions {
    fn default_for(target_platform: CompatPlatform) -> Self {
        Self {
            target_platform,
            profile: PackageProfile::Release,
            out_dir: PathBuf::from("target/packages"),
            skip_cargo_build: false,
            build_before_smoke: false,
            smoke_build_flag: false,
            timeout: Duration::from_secs(10),
        }
    }

    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default_for(CompatPlatform::detect());
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--target-platform" | "--platform" => {
                    let value = required_arg(args, &mut index, "--target-platform")?;
                    options.target_platform = CompatPlatform::parse_target(&value)
                        .ok_or_else(|| format!("unsupported target platform: {value}"))?;
                }
                "--profile" => {
                    let value = required_arg(args, &mut index, "--profile")?;
                    options.profile = PackageProfile::parse(&value)
                        .ok_or_else(|| format!("unsupported package profile: {value}"))?;
                }
                "--out-dir" => {
                    options.out_dir = PathBuf::from(required_arg(args, &mut index, "--out-dir")?);
                }
                "--skip-cargo-build" => {
                    options.skip_cargo_build = true;
                }
                "--build" => {
                    options.smoke_build_flag = true;
                }
                "--timeout-ms" => {
                    let value = required_arg(args, &mut index, "--timeout-ms")?;
                    let millis = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --timeout-ms value: {value}"))?;
                    if millis < 500 {
                        return Err("--timeout-ms must be at least 500".to_owned());
                    }
                    options.timeout = Duration::from_millis(millis);
                }
                other => return Err(format!("unknown package option: {other}")),
            }
            index += 1;
        }

        Ok(options)
    }
}

#[derive(Debug, Clone)]
struct PackageLayout {
    package_dir: PathBuf,
    binary_path: PathBuf,
    resource_dir: PathBuf,
    manifest_path: PathBuf,
}

fn build_package(options: &PackageOptions) -> ExitCode {
    if options.target_platform != CompatPlatform::detect() {
        eprintln!(
            "cannot build {} package on {} host; run this command on the target OS runner",
            options.target_platform.label(),
            CompatPlatform::detect().label()
        );
        return ExitCode::from(2);
    }

    if !options.skip_cargo_build {
        let code = build_desktop_binary(options.profile, options.target_platform);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }

    match stage_package(options) {
        Ok(layout) => {
            println!("wrote package artifact {}", layout.package_dir.display());
            println!("binary {}", layout.binary_path.display());
            println!("manifest {}", layout.manifest_path.display());
            match emit_distribution_artifacts(options, &layout) {
                Ok(artifacts) => {
                    for artifact in artifacts {
                        println!("distribution {} {}", artifact.kind, artifact.path.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("distribution artifact build failed: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Err(error) => {
            eprintln!("package build failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn smoke_package(options: &PackageOptions) -> ExitCode {
    if options.build_before_smoke {
        let code = build_package(options);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }

    let layout = package_layout(options);
    if let Err(error) = verify_distribution_checksums(options) {
        eprintln!("package checksum smoke failed: {error}");
        return ExitCode::from(1);
    }
    match verify_package_contents(&layout, options.target_platform) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("package smoke failed before launch: {error}");
            return ExitCode::from(1);
        }
    }

    let doctor_smoke = run_packaged_doctor(&layout.binary_path, options.timeout);
    if doctor_smoke.status != PackageSmokeStatus::Passed {
        eprintln!("{}", doctor_smoke.render_line());
        return ExitCode::from(1);
    }

    println!("{}", doctor_smoke.render_line());

    let shell_smoke = run_packaged_shell_smoke(&layout.binary_path, options.timeout);
    if shell_smoke.status != PackageSmokeStatus::Passed {
        eprintln!("{}", shell_smoke.render_line());
        return ExitCode::from(1);
    }

    println!("{}", shell_smoke.render_line());
    let gui_smoke = run_packaged_gui_smoke(&layout.binary_path, options.timeout);
    if gui_smoke.status != PackageSmokeStatus::Passed {
        eprintln!("{}", gui_smoke.render_line());
        return ExitCode::from(1);
    }
    println!("{}", gui_smoke.render_line());
    if options.target_platform == CompatPlatform::Windows {
        let installer_smoke = run_windows_installer_smoke(options);
        if installer_smoke.status != PackageSmokeStatus::Passed {
            eprintln!("{}", installer_smoke.render_line());
            return ExitCode::from(1);
        }
        println!("{}", installer_smoke.render_line());
    }
    ExitCode::SUCCESS
}

fn build_desktop_binary(profile: PackageProfile, target_platform: CompatPlatform) -> ExitCode {
    let mut args = vec!["build", "-p", "panea-desktop"];
    if target_platform == CompatPlatform::Windows {
        args.extend(["--features", "windows-gui"]);
    }
    if profile == PackageProfile::Release {
        args.push("--release");
    }
    run("cargo", &args)
}

fn stage_package(options: &PackageOptions) -> Result<PackageLayout, String> {
    let layout = package_layout(options);
    if layout.package_dir.exists() {
        let package_dir = layout
            .package_dir
            .canonicalize()
            .map_err(|error| format!("failed to resolve existing package directory: {error}"))?;
        let out_dir = options
            .out_dir
            .canonicalize()
            .map_err(|error| format!("failed to resolve package output directory: {error}"))?;
        if !package_dir.starts_with(&out_dir) || package_dir == out_dir {
            return Err(format!(
                "refusing to clear package path outside output directory: {}",
                package_dir.display()
            ));
        }
        fs::remove_dir_all(&package_dir)
            .map_err(|error| format!("failed to clear {}: {error}", package_dir.display()))?;
    }
    fs::create_dir_all(&layout.resource_dir).map_err(|error| {
        format!(
            "failed to create {}: {error}",
            layout.resource_dir.display()
        )
    })?;
    if let Some(parent) = layout.binary_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    let built_binary = built_desktop_binary(options.profile);
    fs::copy(&built_binary, &layout.binary_path).map_err(|error| {
        format!(
            "failed to copy binary {} to {}: {error}",
            built_binary.display(),
            layout.binary_path.display()
        )
    })?;
    if options.target_platform == CompatPlatform::Windows {
        let source = built_windows_gui_binary(options.profile);
        let destination = windows_gui_binary_path(&layout.binary_path);
        fs::copy(&source, &destination).map_err(|error| {
            format!(
                "failed to copy Windows GUI binary {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    }

    write_package_resources(options, &layout)?;
    Ok(layout)
}

#[derive(Debug, Clone)]
struct DistributionArtifact {
    kind: &'static str,
    path: PathBuf,
}

fn emit_distribution_artifacts(
    options: &PackageOptions,
    layout: &PackageLayout,
) -> Result<Vec<DistributionArtifact>, String> {
    fs::create_dir_all(&options.out_dir).map_err(|error| {
        format!(
            "failed to create artifact directory {}: {error}",
            options.out_dir.display()
        )
    })?;
    let mut artifacts = match options.target_platform {
        CompatPlatform::Windows => emit_windows_artifacts(options, layout),
        CompatPlatform::Macos => emit_macos_artifacts(options, layout),
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => {
            emit_linux_artifacts(options, layout)
        }
        CompatPlatform::Any | CompatPlatform::Unix => {
            Err("distribution artifacts require a concrete target platform".to_owned())
        }
    }?;
    let checksums = write_artifact_checksums(options, &artifacts)?;
    artifacts.push(DistributionArtifact {
        kind: "sha256-checksums",
        path: checksums,
    });
    Ok(artifacts)
}

fn write_artifact_checksums(
    options: &PackageOptions,
    artifacts: &[DistributionArtifact],
) -> Result<PathBuf, String> {
    let path = checksum_manifest_path(options);
    let mut contents = String::new();
    for artifact in artifacts {
        let bytes = fs::read(&artifact.path)
            .map_err(|error| format!("failed to hash {}: {error}", artifact.path.display()))?;
        let digest = Sha256::digest(bytes);
        let name = artifact
            .path
            .file_name()
            .ok_or_else(|| format!("artifact has no file name: {}", artifact.path.display()))?;
        contents.push_str(&format!("{digest:x}  {}\n", name.to_string_lossy()));
    }
    write_file(&path, &contents)?;
    Ok(path)
}

fn checksum_manifest_path(options: &PackageOptions) -> PathBuf {
    let platform = match options.target_platform {
        CompatPlatform::Windows => "windows",
        CompatPlatform::Macos => "macos",
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => "linux",
        CompatPlatform::Any | CompatPlatform::Unix => "unknown",
    };
    options.out_dir.join(format!(
        "{}-SHA256SUMS.txt",
        artifact_stem(options, platform)
    ))
}

fn verify_distribution_checksums(options: &PackageOptions) -> Result<(), String> {
    let manifest_path = checksum_manifest_path(options);
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let mut checked = 0usize;
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let (expected, file_name) = line
            .split_once("  ")
            .ok_or_else(|| format!("invalid checksum line: {line}"))?;
        if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
            return Err(format!("unsafe checksum artifact name: {file_name}"));
        }
        let artifact = options.out_dir.join(file_name);
        let bytes = fs::read(&artifact)
            .map_err(|error| format!("failed to read {}: {error}", artifact.display()))?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != expected {
            return Err(format!("checksum mismatch for {}", artifact.display()));
        }
        checked += 1;
    }
    if checked < 2 {
        return Err("checksum manifest did not cover all distribution artifacts".to_owned());
    }
    Ok(())
}

fn artifact_stem(options: &PackageOptions, platform: &str) -> String {
    format!(
        "panea-{}-{platform}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        options.profile.label()
    )
}

fn emit_windows_artifacts(
    options: &PackageOptions,
    layout: &PackageLayout,
) -> Result<Vec<DistributionArtifact>, String> {
    sign_windows_file(&layout.binary_path)?;
    sign_windows_file(&windows_gui_binary_path(&layout.binary_path))?;
    let archive = options.out_dir.join(format!(
        "{}.zip",
        artifact_stem(options, "windows-portable")
    ));
    remove_file_if_exists(&archive)?;
    let package_name = layout
        .package_dir
        .file_name()
        .ok_or_else(|| "Windows package directory has no file name".to_owned())?;
    run_checked(
        Command::new("tar.exe")
            .arg("-a")
            .arg("-c")
            .arg("-f")
            .arg(&archive)
            .arg("-C")
            .arg(
                layout
                    .package_dir
                    .parent()
                    .ok_or_else(|| "Windows package directory has no parent".to_owned())?,
            )
            .arg(package_name),
        "Windows portable ZIP",
    )?;

    let installer = options.out_dir.join(format!(
        "{}.exe",
        artifact_stem(options, "windows-installer")
    ));
    let package_root = layout
        .package_dir
        .canonicalize()
        .map_err(|error| format!("failed to resolve installer payload: {error}"))?;
    let mut command = Command::new("cargo");
    command.args(["build", "-p", "panea-windows-installer"]);
    if options.profile == PackageProfile::Release {
        command.arg("--release");
    }
    command.env("PANEA_PACKAGE_ROOT", package_root);
    run_checked(&mut command, "Windows installer build")?;
    fs::copy(
        PathBuf::from("target")
            .join(options.profile.target_dir())
            .join("panea-installer.exe"),
        &installer,
    )
    .map_err(|error| format!("failed to copy Windows installer: {error}"))?;
    sign_windows_file(&installer)?;

    Ok(vec![
        DistributionArtifact {
            kind: "windows-portable-zip",
            path: archive,
        },
        DistributionArtifact {
            kind: "windows-per-user-installer",
            path: installer,
        },
    ])
}

fn emit_macos_artifacts(
    options: &PackageOptions,
    layout: &PackageLayout,
) -> Result<Vec<DistributionArtifact>, String> {
    let app = layout.package_dir.join("Panea.app");
    sign_macos_app(&app)?;
    let zip = options
        .out_dir
        .join(format!("{}.zip", artifact_stem(options, "macos")));
    remove_file_if_exists(&zip)?;
    run_checked(
        Command::new("ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(&app)
            .arg(&zip),
        "macOS application ZIP",
    )?;
    notarize_macos_app_archive(&zip, &app)?;
    let dmg = options
        .out_dir
        .join(format!("{}.dmg", artifact_stem(options, "macos")));
    remove_file_if_exists(&dmg)?;
    run_checked(
        Command::new("hdiutil")
            .args(["create", "-volname", "Panea", "-srcfolder"])
            .arg(&app)
            .args(["-ov", "-format", "UDZO"])
            .arg(&dmg),
        "macOS DMG",
    )?;
    sign_macos_disk_image(&dmg)?;
    notarize_macos_artifact(&dmg)?;
    Ok(vec![
        DistributionArtifact {
            kind: "macos-app-zip",
            path: zip,
        },
        DistributionArtifact {
            kind: "macos-dmg",
            path: dmg,
        },
    ])
}

fn emit_linux_artifacts(
    options: &PackageOptions,
    layout: &PackageLayout,
) -> Result<Vec<DistributionArtifact>, String> {
    let tarball = options.out_dir.join(format!(
        "{}.tar.gz",
        artifact_stem(options, "linux-portable")
    ));
    remove_file_if_exists(&tarball)?;
    let package_name = layout
        .package_dir
        .file_name()
        .ok_or_else(|| "Linux package directory has no file name".to_owned())?;
    run_checked(
        Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(
                layout
                    .package_dir
                    .parent()
                    .ok_or_else(|| "Linux package directory has no parent".to_owned())?,
            )
            .arg(package_name),
        "Linux portable tarball",
    )?;

    let deb_root = options
        .out_dir
        .join(format!(".{}-deb", artifact_stem(options, "linux")));
    if deb_root.exists() {
        fs::remove_dir_all(&deb_root)
            .map_err(|error| format!("failed to clear deb staging: {error}"))?;
    }
    let usr = deb_root.join("usr");
    copy_dir_recursive(&layout.package_dir.join("bin"), &usr.join("bin"))?;
    copy_dir_recursive(&layout.package_dir.join("share"), &usr.join("share"))?;
    write_file(
        &deb_root.join("DEBIAN").join("control"),
        &format!(
            "Package: panea\nVersion: {}\nSection: utils\nPriority: optional\nArchitecture: {}\nMaintainer: Panea contributors\nDescription: GPU-first cross-platform terminal emulator\n",
            env!("CARGO_PKG_VERSION"),
            deb_architecture()
        ),
    )?;
    let deb = options
        .out_dir
        .join(format!("{}.deb", artifact_stem(options, "linux")));
    remove_file_if_exists(&deb)?;
    run_checked(
        Command::new("dpkg-deb")
            .args(["--build", "--root-owner-group"])
            .arg(&deb_root)
            .arg(&deb),
        "Debian package",
    )?;
    fs::remove_dir_all(&deb_root)
        .map_err(|error| format!("failed to clear deb staging: {error}"))?;

    let appimage = emit_linux_appimage(options, layout)?;
    let rpm = emit_linux_rpm(options, layout)?;

    Ok(vec![
        DistributionArtifact {
            kind: "linux-portable-tarball",
            path: tarball,
        },
        DistributionArtifact {
            kind: "linux-deb",
            path: deb,
        },
        DistributionArtifact {
            kind: "linux-appimage",
            path: appimage,
        },
        DistributionArtifact {
            kind: "linux-rpm",
            path: rpm,
        },
    ])
}

fn signing_required() -> bool {
    std::env::var("PANEA_REQUIRE_SIGNING").is_ok_and(|value| value == "1")
}

fn sign_windows_file(path: &Path) -> Result<(), String> {
    let Some(certificate) = std::env::var("PANEA_WINDOWS_SIGN_CERTIFICATE")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return if signing_required() {
            Err("PANEA_REQUIRE_SIGNING=1 but PANEA_WINDOWS_SIGN_CERTIFICATE is unset".to_owned())
        } else {
            Ok(())
        };
    };
    let signtool =
        std::env::var("PANEA_WINDOWS_SIGNTOOL").unwrap_or_else(|_| "signtool.exe".to_owned());
    let timestamp = std::env::var("PANEA_WINDOWS_TIMESTAMP_URL")
        .unwrap_or_else(|_| "http://timestamp.digicert.com".to_owned());
    let mut command = Command::new(signtool);
    command
        .args(["sign", "/fd", "SHA256", "/td", "SHA256", "/tr"])
        .arg(timestamp)
        .args(["/f"])
        .arg(certificate);
    if let Ok(password) = std::env::var("PANEA_WINDOWS_SIGN_PASSWORD") {
        command.args(["/p", &password]);
    }
    command.arg(path);
    run_checked(&mut command, "Windows Authenticode signing")?;
    let signtool =
        std::env::var("PANEA_WINDOWS_SIGNTOOL").unwrap_or_else(|_| "signtool.exe".to_owned());
    run_checked(
        Command::new(signtool)
            .args(["verify", "/pa", "/all"])
            .arg(path),
        "Windows Authenticode verification",
    )
}

fn sign_macos_app(app: &Path) -> Result<(), String> {
    let Some(identity) = std::env::var("PANEA_MACOS_SIGN_IDENTITY")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return if signing_required() {
            Err("PANEA_REQUIRE_SIGNING=1 but PANEA_MACOS_SIGN_IDENTITY is unset".to_owned())
        } else {
            Ok(())
        };
    };
    run_checked(
        Command::new("codesign")
            .args([
                "--force",
                "--deep",
                "--options",
                "runtime",
                "--timestamp",
                "--sign",
            ])
            .arg(identity)
            .arg(app),
        "macOS application signing",
    )?;
    run_checked(
        Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app),
        "macOS signature verification",
    )
}

fn notarize_macos_artifact(dmg: &Path) -> Result<(), String> {
    let Some(profile) = std::env::var("PANEA_MACOS_NOTARY_PROFILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return if signing_required() {
            Err("PANEA_REQUIRE_SIGNING=1 but PANEA_MACOS_NOTARY_PROFILE is unset".to_owned())
        } else {
            Ok(())
        };
    };
    run_checked(
        Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(dmg)
            .args(["--keychain-profile", &profile, "--wait"]),
        "macOS notarization",
    )?;
    run_checked(
        Command::new("xcrun").args(["stapler", "staple"]).arg(dmg),
        "macOS notarization staple",
    )
}

fn notarize_macos_app_archive(zip: &Path, app: &Path) -> Result<(), String> {
    let Some(profile) = std::env::var("PANEA_MACOS_NOTARY_PROFILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return if signing_required() {
            Err("PANEA_REQUIRE_SIGNING=1 but PANEA_MACOS_NOTARY_PROFILE is unset".to_owned())
        } else {
            Ok(())
        };
    };
    run_checked(
        Command::new("xcrun")
            .args(["notarytool", "submit"])
            .arg(zip)
            .args(["--keychain-profile", &profile, "--wait"]),
        "macOS application notarization",
    )?;
    run_checked(
        Command::new("xcrun").args(["stapler", "staple"]).arg(app),
        "macOS application notarization staple",
    )?;
    remove_file_if_exists(zip)?;
    run_checked(
        Command::new("ditto")
            .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
            .arg(app)
            .arg(zip),
        "macOS stapled application ZIP",
    )
}

fn sign_macos_disk_image(dmg: &Path) -> Result<(), String> {
    let Some(identity) = std::env::var("PANEA_MACOS_SIGN_IDENTITY")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return if signing_required() {
            Err("PANEA_REQUIRE_SIGNING=1 but PANEA_MACOS_SIGN_IDENTITY is unset".to_owned())
        } else {
            Ok(())
        };
    };
    run_checked(
        Command::new("codesign")
            .args(["--force", "--timestamp", "--sign"])
            .arg(identity)
            .arg(dmg),
        "macOS disk image signing",
    )
}

fn emit_linux_appimage(
    options: &PackageOptions,
    layout: &PackageLayout,
) -> Result<PathBuf, String> {
    let app_dir = options
        .out_dir
        .join(format!(".{}-AppDir", artifact_stem(options, "linux")));
    if app_dir.exists() {
        fs::remove_dir_all(&app_dir).map_err(|error| format!("failed to clear AppDir: {error}"))?;
    }
    copy_dir_recursive(&layout.package_dir.join("bin"), &app_dir.join("usr/bin"))?;
    copy_dir_recursive(
        &layout.package_dir.join("share"),
        &app_dir.join("usr/share"),
    )?;
    fs::copy(
        app_dir.join("usr/share/applications/panea.desktop"),
        app_dir.join("panea.desktop"),
    )
    .map_err(|error| format!("failed to stage AppImage desktop file: {error}"))?;
    fs::copy(
        app_dir.join("usr/share/icons/hicolor/512x512/apps/panea.png"),
        app_dir.join("panea.png"),
    )
    .map_err(|error| format!("failed to stage AppImage icon: {error}"))?;
    let app_run = app_dir.join("AppRun");
    write_file(
        &app_run,
        "#!/bin/sh\nHERE=\"$(dirname \"$(readlink -f \"$0\")\")\"\nexec \"$HERE/usr/bin/panea\" \"$@\"\n",
    )?;
    make_executable(&app_run)?;

    let output = options
        .out_dir
        .join(format!("{}.AppImage", artifact_stem(options, "linux")));
    remove_file_if_exists(&output)?;
    let tool = std::env::var("PANEA_APPIMAGETOOL").unwrap_or_else(|_| "appimagetool".to_owned());
    let mut command = Command::new(tool);
    command
        .env("ARCH", appimage_architecture())
        .arg(&app_dir)
        .arg(&output);
    run_checked(&mut command, "Linux AppImage build")?;
    fs::remove_dir_all(&app_dir).map_err(|error| format!("failed to clear AppDir: {error}"))?;
    Ok(output)
}

fn emit_linux_rpm(options: &PackageOptions, layout: &PackageLayout) -> Result<PathBuf, String> {
    let top = options
        .out_dir
        .join(format!(".{}-rpm", artifact_stem(options, "linux")));
    if top.exists() {
        fs::remove_dir_all(&top)
            .map_err(|error| format!("failed to clear rpm staging: {error}"))?;
    }
    for directory in ["BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"] {
        fs::create_dir_all(top.join(directory))
            .map_err(|error| format!("failed to create rpm staging: {error}"))?;
    }
    let source_name = format!("panea-{}", env!("CARGO_PKG_VERSION"));
    let source_root = top.join("SOURCES").join(&source_name);
    copy_dir_recursive(&layout.package_dir.join("bin"), &source_root.join("bin"))?;
    copy_dir_recursive(
        &layout.package_dir.join("share"),
        &source_root.join("share"),
    )?;
    let source_archive = top.join("SOURCES").join(format!("{source_name}.tar.gz"));
    run_checked(
        Command::new("tar")
            .arg("-czf")
            .arg(&source_archive)
            .arg("-C")
            .arg(top.join("SOURCES"))
            .arg(&source_name),
        "RPM source archive",
    )?;
    fs::remove_dir_all(&source_root)
        .map_err(|error| format!("failed to clear rpm source tree: {error}"))?;
    let spec = top.join("SPECS/panea.spec");
    write_file(
        &spec,
        &format!(
            "Name: panea\nVersion: {}\nRelease: 1%{{?dist}}\nSummary: GPU-first cross-platform terminal emulator\nLicense: MIT OR Apache-2.0\nURL: https://github.com/shreshthkapai/Panea\nSource0: %{{name}}-%{{version}}.tar.gz\nBuildArch: {}\n\n%description\nPanea terminal emulator.\n\n%prep\n%setup -q\n\n%build\n\n%install\nmkdir -p %{{buildroot}}/usr\ncp -a bin share %{{buildroot}}/usr/\n\n%files\n/usr/bin/panea\n/usr/share/panea\n/usr/share/applications/panea.desktop\n/usr/share/icons/hicolor/512x512/apps/panea.png\n\n%changelog\n* Thu Jan 01 1970 Panea contributors - {}-1\n- Automated reproducible package\n",
            env!("CARGO_PKG_VERSION"),
            rpm_architecture(),
            env!("CARGO_PKG_VERSION")
        ),
    )?;
    let canonical_top = top
        .canonicalize()
        .map_err(|error| format!("failed to resolve rpm staging: {error}"))?;
    run_checked(
        Command::new("rpmbuild")
            .args(["-bb", "--define"])
            .arg(format!("_topdir {}", canonical_top.display()))
            .arg(&spec),
        "Linux RPM build",
    )?;
    let built = find_file_with_extension(&top.join("RPMS"), "rpm")?
        .ok_or_else(|| "rpmbuild completed without producing an rpm".to_owned())?;
    let output = options
        .out_dir
        .join(format!("{}.rpm", artifact_stem(options, "linux")));
    remove_file_if_exists(&output)?;
    fs::copy(&built, &output).map_err(|error| format!("failed to collect rpm: {error}"))?;
    fs::remove_dir_all(&top).map_err(|error| format!("failed to clear rpm staging: {error}"))?;
    Ok(output)
}

fn find_file_with_extension(root: &Path, extension: &str) -> Result<Option<PathBuf>, String> {
    for entry in
        fs::read_dir(root).map_err(|error| format!("failed to read {}: {error}", root.display()))?
    {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            if let Some(found) = find_file_with_extension(&path, extension)? {
                return Ok(Some(found));
            }
        } else if path.extension().is_some_and(|value| value == extension) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn appimage_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "i686",
        other => other,
    }
}

fn rpm_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86" => "i686",
        other => other,
    }
}

fn deb_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "i386",
        other => other,
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("failed to create {}: {error}", destination.display()))?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let output = destination.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &output)?;
        } else {
            fs::copy(&path, &output).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    path.display(),
                    output.display()
                )
            })?;
        }
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("failed to replace {}: {error}", path.display()))?;
    }
    Ok(())
}

fn run_checked(command: &mut Command, label: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("failed to start {label}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{label} exited with {status}"))
    }
}

fn package_layout(options: &PackageOptions) -> PackageLayout {
    let base_name = format!(
        "panea-{}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        package_platform_label(options.target_platform),
        options.profile.label()
    );
    let package_dir = options.out_dir.join(base_name);

    match options.target_platform {
        CompatPlatform::Macos => {
            let app_dir = package_dir.join("Panea.app");
            let resource_dir = app_dir.join("Contents").join("Resources");
            PackageLayout {
                package_dir,
                binary_path: app_dir.join("Contents").join("MacOS").join("panea"),
                manifest_path: resource_dir.join("package-manifest.json"),
                resource_dir,
            }
        }
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => {
            let resource_dir = package_dir.join("share").join("panea");
            PackageLayout {
                binary_path: package_dir.join("bin").join("panea"),
                manifest_path: resource_dir.join("package-manifest.json"),
                resource_dir,
                package_dir,
            }
        }
        CompatPlatform::Windows | CompatPlatform::Any | CompatPlatform::Unix => {
            let resource_dir = package_dir.join("share").join("panea");
            PackageLayout {
                binary_path: package_dir.join(package_binary_name(options.target_platform)),
                manifest_path: resource_dir.join("package-manifest.json"),
                resource_dir,
                package_dir,
            }
        }
    }
}

fn write_package_resources(options: &PackageOptions, layout: &PackageLayout) -> Result<(), String> {
    write_file(
        &layout.resource_dir.join("config").join("default.toml"),
        &config_toml::default_config_toml().map_err(|error| error.to_string())?,
    )?;
    write_file(
        &layout.resource_dir.join("config").join("schema.json"),
        &config_toml::schema_json().map_err(|error| error.to_string())?,
    )?;

    for example in assets::CONFIG_EXAMPLES {
        write_file(
            &layout
                .resource_dir
                .join("config")
                .join("examples")
                .join(example.name),
            example.contents,
        )?;
    }
    for example in assets::PROGRAMMABLE_CONFIG_EXAMPLES {
        write_file(
            &layout
                .resource_dir
                .join("config")
                .join("examples")
                .join(example.name),
            example.contents,
        )?;
    }
    for theme in assets::THEMES {
        write_file(
            &layout.resource_dir.join("themes").join(theme.name),
            theme.contents,
        )?;
    }
    for profile in assets::CURSOR_PROFILES {
        write_file(
            &layout
                .resource_dir
                .join("cursor-profiles")
                .join(profile.name),
            profile.contents,
        )?;
    }
    for asset in assets::CURSOR_VECTOR_ASSETS {
        write_file(
            &layout.resource_dir.join("cursor-vectors").join(asset.name),
            asset.contents,
        )?;
    }

    for shell in [
        ShellKind::Bash,
        ShellKind::Zsh,
        ShellKind::Fish,
        ShellKind::PowerShell,
    ] {
        let script = script_for_shell(shell).expect("baseline shell script");
        write_file(
            &layout
                .resource_dir
                .join("shell-integration")
                .join(script.file_name),
            script.contents,
        )?;
    }

    copy_repo_file("README.md", &layout.resource_dir.join("README.md"))?;
    copy_repo_file("LICENSE", &layout.resource_dir.join("LICENSE"))?;
    copy_repo_file(
        "LICENSE-APACHE",
        &layout.resource_dir.join("LICENSE-APACHE"),
    )?;
    copy_repo_file("LICENSE-MIT", &layout.resource_dir.join("LICENSE-MIT"))?;
    for doc in [
        "docs/getting-started.md",
        "docs/config.md",
        "docs/compatibility.md",
        "docs/cursor-customization.md",
        "docs/programmable-config.md",
        "docs/doctor.md",
        "docs/shell-integration.md",
        "docs/desktop-ux.md",
        "docs/platform-support.md",
        "docs/troubleshooting.md",
        "docs/packaging.md",
        "docs/alpha-scope.md",
        "docs/notifications.md",
    ] {
        let file_name = Path::new(doc)
            .file_name()
            .ok_or_else(|| format!("invalid doc path: {doc}"))?;
        copy_repo_file(doc, &layout.resource_dir.join("docs").join(file_name))?;
    }

    match options.target_platform {
        CompatPlatform::Macos => write_macos_bundle_files(layout)?,
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => {
            write_linux_package_files(layout)?
        }
        CompatPlatform::Windows | CompatPlatform::Any | CompatPlatform::Unix => {
            write_windows_package_files(layout)?
        }
    }

    write_file(
        &layout.manifest_path,
        &render_package_manifest(options, layout),
    )?;
    write_file(
        &layout.resource_dir.join("INSTALL.md"),
        &package_install_notes(options),
    )?;
    Ok(())
}

fn write_macos_bundle_files(layout: &PackageLayout) -> Result<(), String> {
    let contents_dir = layout
        .binary_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "invalid macOS bundle layout".to_owned())?;
    write_file(
        &contents_dir.join("Info.plist"),
        &format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>panea</string>
  <key>CFBundleIdentifier</key><string>dev.panea.terminal</string>
  <key>CFBundleName</key><string>Panea</string>
  <key>CFBundleIconFile</key><string>Panea.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{}</string>
  <key>CFBundleVersion</key><string>{}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
"#,
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION"),
        ),
    )?;
    write_bytes(
        &layout.resource_dir.join("Panea.icns"),
        assets::PANEA_ICON_ICNS,
    )
}

fn write_linux_package_files(layout: &PackageLayout) -> Result<(), String> {
    let share_dir = layout
        .resource_dir
        .parent()
        .ok_or_else(|| "invalid Linux package layout".to_owned())?;
    write_file(
        &share_dir.join("applications").join("panea.desktop"),
        "[Desktop Entry]\nType=Application\nName=Panea\nComment=GPU-first cross-platform terminal\nExec=panea\nTerminal=false\nCategories=System;TerminalEmulator;\nIcon=panea\n",
    )?;
    write_bytes(
        &share_dir
            .join("icons")
            .join("hicolor")
            .join("512x512")
            .join("apps")
            .join("panea.png"),
        assets::PANEA_ICON_PNG_512,
    )
}

fn write_windows_package_files(layout: &PackageLayout) -> Result<(), String> {
    write_file(
        &layout.resource_dir.join("WINDOWS.txt"),
        "Panea Windows portable package.\n\nRun panea-gui.exe for the normal desktop application without a console window. Use panea.exe for CLI diagnostics such as `panea doctor`. The release pipeline also emits a per-user installer that adds Start menu entries and the CLI binary directory to the user PATH.\n",
    )?;
    write_bytes(
        &layout.resource_dir.join("icons").join("panea.ico"),
        assets::PANEA_ICON_ICO,
    )
}

fn verify_package_contents(
    layout: &PackageLayout,
    target_platform: CompatPlatform,
) -> Result<(), String> {
    for path in [
        &layout.binary_path,
        &layout.manifest_path,
        &layout.resource_dir.join("README.md"),
        &layout.resource_dir.join("LICENSE"),
        &layout.resource_dir.join("LICENSE-APACHE"),
        &layout.resource_dir.join("LICENSE-MIT"),
        &layout.resource_dir.join("config").join("default.toml"),
        &layout.resource_dir.join("config").join("schema.json"),
        &layout
            .resource_dir
            .join("shell-integration")
            .join("panea.bash"),
        &layout
            .resource_dir
            .join("shell-integration")
            .join("panea.zsh"),
        &layout
            .resource_dir
            .join("shell-integration")
            .join("panea.fish"),
        &layout
            .resource_dir
            .join("shell-integration")
            .join("panea.ps1"),
        &layout
            .resource_dir
            .join("config")
            .join("examples")
            .join("advanced.panea"),
        &layout.resource_dir.join("themes").join("panea-dark.toml"),
        &layout.resource_dir.join("themes").join("panea-light.toml"),
        &layout
            .resource_dir
            .join("cursor-profiles")
            .join("static.toml"),
        &layout
            .resource_dir
            .join("cursor-profiles")
            .join("motion.toml"),
        &layout
            .resource_dir
            .join("cursor-vectors")
            .join("chevron.panea-cursor.json"),
    ] {
        if !path.exists() {
            return Err(format!("missing packaged file {}", path.display()));
        }
    }
    for path in package_icon_paths(layout, target_platform) {
        if !path.exists() {
            return Err(format!("missing packaged icon {}", path.display()));
        }
    }
    if target_platform == CompatPlatform::Windows {
        let gui_binary = windows_gui_binary_path(&layout.binary_path);
        if !gui_binary.exists() {
            return Err(format!(
                "missing packaged Windows GUI entrypoint {}",
                gui_binary.display()
            ));
        }
    }
    Ok(())
}

fn package_icon_paths(layout: &PackageLayout, target_platform: CompatPlatform) -> Vec<PathBuf> {
    match target_platform {
        CompatPlatform::Macos => vec![layout.resource_dir.join("Panea.icns")],
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => {
            let share_dir = layout.resource_dir.parent().unwrap_or(&layout.resource_dir);
            vec![
                share_dir
                    .join("icons")
                    .join("hicolor")
                    .join("512x512")
                    .join("apps")
                    .join("panea.png"),
            ]
        }
        CompatPlatform::Windows | CompatPlatform::Any | CompatPlatform::Unix => {
            vec![layout.resource_dir.join("icons").join("panea.ico")]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageSmokeStatus {
    Passed,
    Failed,
    TimedOut,
}

impl PackageSmokeStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone)]
struct PackageSmokeResult {
    name: &'static str,
    status: PackageSmokeStatus,
    duration: Duration,
    detail: String,
}

impl PackageSmokeResult {
    fn render_line(&self) -> String {
        format!(
            "[{}] package {} smoke duration_ms={} {}",
            self.status.label(),
            self.name,
            self.duration.as_millis(),
            self.detail
        )
    }
}

fn run_packaged_doctor(binary_path: &Path, timeout: Duration) -> PackageSmokeResult {
    let started = Instant::now();
    let mut child = match Command::new(binary_path)
        .args(["doctor", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return PackageSmokeResult {
                name: "doctor",
                status: PackageSmokeStatus::Failed,
                duration: started.elapsed(),
                detail: format!("failed to spawn {}: {error}", binary_path.display()),
            };
        }
    };

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return PackageSmokeResult {
                    name: "doctor",
                    status: PackageSmokeStatus::TimedOut,
                    duration: started.elapsed(),
                    detail: format!("{} doctor --json exceeded timeout", binary_path.display()),
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return PackageSmokeResult {
                    name: "doctor",
                    status: PackageSmokeStatus::Failed,
                    duration: started.elapsed(),
                    detail: format!("failed while waiting for doctor smoke: {error}"),
                };
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("\"name\":\"doctor\"")
                && stdout.contains("\"topic\"")
                && stdout.contains("\"lines\"")
            {
                PackageSmokeResult {
                    name: "doctor",
                    status: PackageSmokeStatus::Passed,
                    duration: started.elapsed(),
                    detail: format!("{} doctor --json succeeded", binary_path.display()),
                }
            } else {
                PackageSmokeResult {
                    name: "doctor",
                    status: PackageSmokeStatus::Failed,
                    duration: started.elapsed(),
                    detail: "doctor output did not look like Panea diagnostics JSON".to_owned(),
                }
            }
        }
        Ok(output) => PackageSmokeResult {
            name: "doctor",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!(
                "doctor exited {:?}: {}",
                output.status.code(),
                preview_bytes(&output.stderr)
            ),
        },
        Err(error) => PackageSmokeResult {
            name: "doctor",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!("failed to collect doctor output: {error}"),
        },
    }
}

fn run_packaged_shell_smoke(binary_path: &Path, timeout: Duration) -> PackageSmokeResult {
    let started = Instant::now();
    let timeout_ms = timeout.as_millis().to_string();
    let mut child = match Command::new(binary_path)
        .args(["shell-smoke", "--json", "--timeout-ms", timeout_ms.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return PackageSmokeResult {
                name: "shell-launch",
                status: PackageSmokeStatus::Failed,
                duration: started.elapsed(),
                detail: format!(
                    "failed to spawn {} shell-smoke: {error}",
                    binary_path.display()
                ),
            };
        }
    };

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return PackageSmokeResult {
                    name: "shell-launch",
                    status: PackageSmokeStatus::TimedOut,
                    duration: started.elapsed(),
                    detail: format!("{} shell-smoke exceeded timeout", binary_path.display()),
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return PackageSmokeResult {
                    name: "shell-launch",
                    status: PackageSmokeStatus::Failed,
                    duration: started.elapsed(),
                    detail: format!("failed while waiting for shell smoke: {error}"),
                };
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("\"name\":\"shell-smoke\"")
                && stdout.contains("\"passed\":true")
                && stdout.contains("\"marker_observed\":true")
            {
                PackageSmokeResult {
                    name: "shell-launch",
                    status: PackageSmokeStatus::Passed,
                    duration: started.elapsed(),
                    detail: format!(
                        "{} shell-smoke observed a PTY marker",
                        binary_path.display()
                    ),
                }
            } else {
                PackageSmokeResult {
                    name: "shell-launch",
                    status: PackageSmokeStatus::Failed,
                    duration: started.elapsed(),
                    detail: format!(
                        "shell-smoke output did not report success: {}",
                        preview_bytes(&output.stdout)
                    ),
                }
            }
        }
        Ok(output) => PackageSmokeResult {
            name: "shell-launch",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!(
                "shell-smoke exited {:?}: stdout={} stderr={}",
                output.status.code(),
                preview_bytes(&output.stdout),
                preview_bytes(&output.stderr)
            ),
        },
        Err(error) => PackageSmokeResult {
            name: "shell-launch",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!("failed to collect shell-smoke output: {error}"),
        },
    }
}

fn run_packaged_gui_smoke(binary_path: &Path, timeout: Duration) -> PackageSmokeResult {
    let started = Instant::now();
    let startup = run_packaged_gui_smoke_mode(binary_path, timeout, "--startup");
    if startup.status != PackageSmokeStatus::Passed {
        return startup;
    }
    let terminal_io = run_packaged_gui_smoke_mode(binary_path, timeout, "--terminal-io");
    if terminal_io.status != PackageSmokeStatus::Passed {
        return terminal_io;
    }
    PackageSmokeResult {
        name: "gui-launch",
        status: PackageSmokeStatus::Passed,
        duration: started.elapsed(),
        detail: format!(
            "{} rendered exactly one startup prompt without input, then rendered input echo and command output",
            packaged_gui_binary(binary_path).display()
        ),
    }
}

fn run_packaged_gui_smoke_mode(
    binary_path: &Path,
    timeout: Duration,
    mode: &'static str,
) -> PackageSmokeResult {
    let started = Instant::now();
    let timeout_ms = timeout.as_millis().to_string();
    let gui_binary = packaged_gui_binary(binary_path);
    let mut child = match Command::new(&gui_binary)
        .args([
            "gui-smoke",
            mode,
            "--json",
            "--timeout-ms",
            timeout_ms.as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return PackageSmokeResult {
                name: "gui-launch",
                status: PackageSmokeStatus::Failed,
                duration: started.elapsed(),
                detail: format!(
                    "failed to spawn {} gui-smoke: {error}",
                    gui_binary.display()
                ),
            };
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() >= timeout.saturating_add(Duration::from_secs(2)) => {
                let _ = child.kill();
                let _ = child.wait();
                return PackageSmokeResult {
                    name: "gui-launch",
                    status: PackageSmokeStatus::TimedOut,
                    duration: started.elapsed(),
                    detail: format!("{} gui-smoke exceeded timeout", gui_binary.display()),
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return PackageSmokeResult {
                    name: "gui-launch",
                    status: PackageSmokeStatus::Failed,
                    duration: started.elapsed(),
                    detail: format!("failed while waiting for gui smoke: {error}"),
                };
            }
        }
    }
    match child.wait_with_output() {
        Ok(output)
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("\"status\":\"passed\"") =>
        {
            PackageSmokeResult {
                name: "gui-launch",
                status: PackageSmokeStatus::Passed,
                duration: started.elapsed(),
                detail: format!("{} passed {mode}", gui_binary.display(),),
            }
        }
        Ok(output) => PackageSmokeResult {
            name: "gui-launch",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!(
                "gui-smoke exited {:?}: stdout={} stderr={}",
                output.status.code(),
                preview_bytes(&output.stdout),
                preview_bytes(&output.stderr)
            ),
        },
        Err(error) => PackageSmokeResult {
            name: "gui-launch",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!("failed to collect gui-smoke output: {error}"),
        },
    }
}

fn run_windows_installer_smoke(options: &PackageOptions) -> PackageSmokeResult {
    let started = Instant::now();
    let installer = options.out_dir.join(format!(
        "{}.exe",
        artifact_stem(options, "windows-installer")
    ));
    let install_dir = options
        .out_dir
        .join(format!(".panea-installer-smoke-{}", std::process::id()));
    if install_dir.exists()
        && let Err(error) = fs::remove_dir_all(&install_dir)
    {
        return PackageSmokeResult {
            name: "windows-installer",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!("failed to clear installer smoke directory: {error}"),
        };
    }

    let install = run_bounded_command(
        Command::new(&installer)
            .args(["install", "--install-dir"])
            .arg(&install_dir)
            .args(["--no-shortcuts", "--no-path", "--no-register"]),
        options.timeout,
    );
    if let Err(error) = install {
        return PackageSmokeResult {
            name: "windows-installer",
            status: PackageSmokeStatus::Failed,
            duration: started.elapsed(),
            detail: format!("installer launch failed: {error}"),
        };
    }

    let installed_binary = install_dir.join("panea.exe");
    let doctor = run_packaged_doctor(&installed_binary, options.timeout);
    let shell = run_packaged_shell_smoke(&installed_binary, options.timeout);
    let gui = run_packaged_gui_smoke(&installed_binary, options.timeout);
    let uninstall = run_bounded_command(
        Command::new(&installer)
            .args(["uninstall", "--install-dir"])
            .arg(&install_dir)
            .args(["--no-shortcuts", "--no-path", "--no-register"]),
        options.timeout,
    );

    let passed = doctor.status == PackageSmokeStatus::Passed
        && shell.status == PackageSmokeStatus::Passed
        && gui.status == PackageSmokeStatus::Passed
        && uninstall.is_ok()
        && !install_dir.exists();
    PackageSmokeResult {
        name: "windows-installer",
        status: if passed {
            PackageSmokeStatus::Passed
        } else {
            PackageSmokeStatus::Failed
        },
        duration: started.elapsed(),
        detail: format!(
            "install={} doctor={} shell={} gui={} uninstall={} cleaned={}",
            installer.display(),
            doctor.status.label(),
            shell.status.label(),
            gui.status.label(),
            uninstall.as_ref().map_or("failed", |_| "passed"),
            !install_dir.exists()
        ),
    }
}

fn run_bounded_command(command: &mut Command, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("command exited with {status}"));
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("command exceeded {} ms", timeout.as_millis()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

fn built_desktop_binary(profile: PackageProfile) -> PathBuf {
    PathBuf::from("target")
        .join(profile.target_dir())
        .join(host_binary_name())
}

fn built_windows_gui_binary(profile: PackageProfile) -> PathBuf {
    PathBuf::from("target")
        .join(profile.target_dir())
        .join("panea-gui.exe")
}

fn windows_gui_binary_path(binary_path: &Path) -> PathBuf {
    binary_path.with_file_name("panea-gui.exe")
}

fn packaged_gui_binary(binary_path: &Path) -> PathBuf {
    let candidate = windows_gui_binary_path(binary_path);
    if candidate.is_file() {
        candidate
    } else {
        binary_path.to_path_buf()
    }
}

fn host_binary_name() -> &'static str {
    if cfg!(windows) { "panea.exe" } else { "panea" }
}

fn package_binary_name(platform: CompatPlatform) -> &'static str {
    match platform {
        CompatPlatform::Windows => "panea.exe",
        CompatPlatform::Macos
        | CompatPlatform::LinuxX11
        | CompatPlatform::LinuxWayland
        | CompatPlatform::Any
        | CompatPlatform::Unix => "panea",
    }
}

fn package_platform_label(platform: CompatPlatform) -> &'static str {
    match platform {
        CompatPlatform::Windows => "windows-portable",
        CompatPlatform::Macos => "macos-app",
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => "linux-portable",
        CompatPlatform::Any => "unknown",
        CompatPlatform::Unix => "unix",
    }
}

fn render_package_manifest(options: &PackageOptions, layout: &PackageLayout) -> String {
    let gui_binary = if options.target_platform == CompatPlatform::Windows {
        windows_gui_binary_path(&layout.binary_path)
    } else {
        layout.binary_path.clone()
    };
    format!(
        concat!(
            "{{\n",
            "  \"name\": \"panea\",\n",
            "  \"version\": \"{}\",\n",
            "  \"target_platform\": \"{}\",\n",
            "  \"artifact_kind\": \"{}\",\n",
            "  \"profile\": \"{}\",\n",
            "  \"binary\": \"{}\",\n",
            "  \"gui_binary\": \"{}\",\n",
            "  \"resources\": \"{}\",\n",
            "  \"doctor_smoke\": \"panea doctor --json\",\n",
            "  \"shell_launch_smoke\": \"panea shell-smoke --json\",\n",
            "  \"gui_startup_smoke\": \"panea gui-smoke --startup --json\",\n",
            "  \"gui_launch_smoke\": \"panea gui-smoke --terminal-io --json\",\n",
            "  \"contains\": [\"binary\", \"gui_entrypoint\", \"application_icon\", \"default_config\", \"config_schema\", \"config_examples\", \"programmable_config_examples\", \"themes\", \"cursor_profiles\", \"cursor_vector_assets\", \"shell_integration_scripts\", \"doctor_command\", \"shell_smoke_command\", \"gui_smoke_command\", \"license\", \"readme\"]\n",
            "}}\n"
        ),
        env!("CARGO_PKG_VERSION"),
        json_escape(options.target_platform.label()).as_str(),
        json_escape(package_platform_label(options.target_platform)).as_str(),
        json_escape(options.profile.label()).as_str(),
        json_escape(&relative_or_display(
            &layout.package_dir,
            &layout.binary_path
        ))
        .as_str(),
        json_escape(&relative_or_display(&layout.package_dir, &gui_binary)).as_str(),
        json_escape(&relative_or_display(
            &layout.package_dir,
            &layout.resource_dir
        ))
        .as_str(),
    )
}

fn package_install_notes(options: &PackageOptions) -> String {
    let common = "Panea package artifact\n\nRun `panea doctor --json` first to verify diagnostics. Run `panea shell-smoke --json` to verify a bounded local PTY session, `panea gui-smoke --startup --json` to verify one settled prompt with no input, then `panea gui-smoke --terminal-io --json` to verify input echo and command output.\n\n";
    match options.target_platform {
        CompatPlatform::Windows => format!(
            "{common}Windows delivery:\n- Run `panea-gui.exe` from the portable ZIP for a console-free desktop launch.\n- Use `panea.exe` for CLI diagnostics and smoke commands.\n- Or run the Panea installer EXE for a per-user install, Start menu shortcuts, and user PATH registration.\n- Uninstall from the Start menu shortcut or `panea-uninstall.exe uninstall`.\n"
        ),
        CompatPlatform::Macos => format!(
            "{common}macOS delivery:\n- Open `Panea.app` or run `Panea.app/Contents/MacOS/panea doctor --json`.\n- Release builds emit ZIP and DMG artifacts. Signing and notarization require distribution credentials.\n"
        ),
        CompatPlatform::LinuxX11 | CompatPlatform::LinuxWayland => format!(
            "{common}Linux delivery:\n- Extract the portable tarball and run `bin/panea`.\n- Run the AppImage, install the deb on Debian-compatible distributions, or install the RPM on RPM-compatible distributions.\n- Desktop metadata and the application icon are included under `share/`.\n"
        ),
        CompatPlatform::Any | CompatPlatform::Unix => common.to_owned(),
    }
}

fn copy_repo_file(source: &str, dest: &Path) -> Result<(), String> {
    let contents =
        fs::read_to_string(source).map_err(|error| format!("failed to read {source}: {error}"))?;
    write_file(dest, &contents)
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(path, contents)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn write_bytes(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(path, contents)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn relative_or_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn run_release_check() -> ExitCode {
    let input = match doctor_input() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    println!(
        "{}",
        diagnostics::release_validation_report(&input).render_text()
    );
    ExitCode::SUCCESS
}

fn run_doctor() -> ExitCode {
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let json = args.iter().any(|arg| arg == "--json");
    let topic_arg = args
        .iter()
        .find(|arg| arg.as_str() != "--json")
        .map(String::as_str);
    let topic = topic_arg.map_or(
        Some(diagnostics::DoctorTopic::All),
        diagnostics::DoctorTopic::parse,
    );
    let Some(topic) = topic else {
        eprintln!(
            "unknown doctor topic; expected renderer, config, platform, shell, performance, ssh, window, fonts, or clipboard"
        );
        return ExitCode::from(2);
    };

    let input = match doctor_input() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    let report = diagnostics::doctor_report(&input, topic);
    if json {
        println!("{}", report.render_json());
    } else {
        println!("{}", report.render_text());
    }
    ExitCode::SUCCESS
}

fn run_bug_report() -> ExitCode {
    let input = match doctor_input() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    println!(
        "{}",
        diagnostics::BugReportSnapshot::from_doctor_input(&input).render_text()
    );
    ExitCode::SUCCESS
}

fn doctor_input() -> Result<diagnostics::DoctorInput, config_toml::ConfigTomlError> {
    let loaded = config_toml::load(config_toml::ConfigLoadOptions::default())?;
    Ok(diagnostics::DoctorInput {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_source: config_source_text(&loaded.source),
        config: loaded.config,
        config_diagnostics: loaded.diagnostics,
        platform: diagnostics::PlatformSnapshot::detect(),
        runtime: diagnostics::DoctorRuntimeSnapshot::default(),
        recent_errors: Vec::new(),
    })
}

fn config_source_text(source: &config_toml::ConfigSource) -> String {
    match source {
        config_toml::ConfigSource::Default => "default".to_owned(),
        config_toml::ConfigSource::File(path) => path.display().to_string(),
        config_toml::ConfigSource::ExplicitFile(path) => {
            format!("explicit:{}", path.display())
        }
    }
}

fn run_bench() -> ExitCode {
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let mut cargo_args = vec![
        "run".to_owned(),
        "--release".to_owned(),
        "-p".to_owned(),
        "panea-bench".to_owned(),
        "--".to_owned(),
    ];
    cargo_args.extend(args);
    let refs = cargo_args.iter().map(String::as_str).collect::<Vec<_>>();
    run("cargo", &refs)
}

fn run_screenshot() -> ExitCode {
    let mut args = std::env::args().skip(2).collect::<Vec<_>>();
    let command = args.first().map_or("help", String::as_str);

    match command {
        "help" | "--help" | "-h" => {
            print_screenshot_help();
            ExitCode::SUCCESS
        }
        "capture" => {
            args.remove(0);
            let options = match ScreenshotOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(2);
                }
            };
            capture_screenshot_baselines(&options)
        }
        "verify" => {
            args.remove(0);
            let options = match ScreenshotOptions::parse(&args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(2);
                }
            };
            verify_screenshot_baselines(&options)
        }
        "report" => {
            print_screenshot_report();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown screenshot command: {other}");
            print_screenshot_help();
            ExitCode::from(2)
        }
    }
}

fn print_screenshot_help() {
    eprintln!(
        "usage: cargo xtask screenshot <capture|verify|report> [--platform <key>] [--baseline-dir <path>] [--report-dir <path>]"
    );
    eprintln!("platform keys: windows, macos, linux-x11, linux-wayland");
}

#[derive(Debug, Clone)]
struct ScreenshotOptions {
    platform_key: String,
    baseline_root: PathBuf,
    report_root: PathBuf,
}

impl ScreenshotOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut platform_key = render_wgpu::detect_screenshot_platform_key().to_owned();
        let mut baseline_root = PathBuf::from("tools/conformance/screenshots/baselines");
        let mut report_root = PathBuf::from("target/screenshots");
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--platform" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--platform requires a value".to_owned());
                    };
                    if !matches!(
                        value.as_str(),
                        "windows" | "macos" | "linux-x11" | "linux-wayland" | "linux"
                    ) {
                        return Err(format!("unsupported screenshot platform key: {value}"));
                    }
                    platform_key = value.clone();
                }
                "--baseline-dir" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--baseline-dir requires a value".to_owned());
                    };
                    baseline_root = PathBuf::from(value);
                }
                "--report-dir" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Err("--report-dir requires a value".to_owned());
                    };
                    report_root = PathBuf::from(value);
                }
                other => return Err(format!("unknown screenshot option: {other}")),
            }
            index += 1;
        }

        Ok(Self {
            platform_key,
            baseline_root,
            report_root,
        })
    }

    fn baseline_dir(&self) -> PathBuf {
        self.baseline_root.join(&self.platform_key)
    }

    fn report_dir(&self) -> PathBuf {
        self.report_root.join(&self.platform_key)
    }
}

fn capture_screenshot_baselines(options: &ScreenshotOptions) -> ExitCode {
    let captures = match render_wgpu::capture_all_screenshot_fixtures() {
        Ok(captures) => captures,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    let baseline_dir = options.baseline_dir();
    if let Err(error) = fs::create_dir_all(&baseline_dir) {
        eprintln!(
            "failed to create screenshot baseline directory {}: {error}",
            baseline_dir.display()
        );
        return ExitCode::from(1);
    }

    for capture in &captures {
        let path = baseline_dir.join(format!("{}.ppm", capture.fixture_name));
        if let Err(error) = fs::write(&path, capture.frame.encode_ppm()) {
            eprintln!("failed to write {}: {error}", path.display());
            return ExitCode::from(1);
        }
        println!(
            "captured fixture={} platform={} path={} hash={:016x}",
            capture.fixture_name,
            options.platform_key,
            path.display(),
            capture.frame.snapshot_hash()
        );
    }

    ExitCode::SUCCESS
}

fn verify_screenshot_baselines(options: &ScreenshotOptions) -> ExitCode {
    let baseline_dir = options.baseline_dir();
    let report_dir = options.report_dir();
    let captures = match render_wgpu::capture_all_screenshot_fixtures() {
        Ok(captures) => captures,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = fs::create_dir_all(&report_dir) {
        eprintln!(
            "failed to create screenshot report directory {}: {error}",
            report_dir.display()
        );
        return ExitCode::from(1);
    }

    let mut diffs = BTreeMap::new();
    let mut missing = Vec::new();
    for capture in captures {
        let baseline_path = baseline_dir.join(format!("{}.ppm", capture.fixture_name));
        let expected = match fs::read(&baseline_path) {
            Ok(bytes) => match render_wgpu::CpuFrame::decode_ppm(&bytes) {
                Ok(frame) => frame,
                Err(error) => {
                    eprintln!("failed to parse {}: {error}", baseline_path.display());
                    return ExitCode::from(1);
                }
            },
            Err(error) => {
                missing.push(format!("{} ({error})", baseline_path.display()));
                continue;
            }
        };
        let diff = render_wgpu::compare_screenshots(
            &expected,
            &capture.frame,
            render_wgpu::ScreenshotTolerance::default(),
        );
        println!("{}", diff.render_summary(&capture.fixture_name));
        let actual_path = report_dir.join(format!("{}-actual.ppm", capture.fixture_name));
        if let Err(error) = fs::write(&actual_path, capture.frame.encode_ppm()) {
            eprintln!("failed to write {}: {error}", actual_path.display());
            return ExitCode::from(1);
        }
        diffs.insert(capture.fixture_name, diff);
    }

    if !missing.is_empty() {
        eprintln!("missing screenshot baselines:");
        for path in missing {
            eprintln!("  {path}");
        }
        eprintln!(
            "run `cargo xtask screenshot capture --platform {}` on the target host to create baselines",
            options.platform_key
        );
        return ExitCode::from(1);
    }

    let report = render_wgpu::ScreenshotReport {
        platform_key: options.platform_key.clone(),
        diffs,
    };
    let report_path = report_dir.join("report.md");
    if let Err(error) = fs::write(&report_path, report.render_markdown()) {
        eprintln!("failed to write {}: {error}", report_path.display());
        return ExitCode::from(1);
    }
    println!("wrote screenshot report {}", report_path.display());

    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_screenshot_report() {
    let platform = render_wgpu::detect_screenshot_platform_key();
    println!("Panea screenshot verification");
    println!("current_platform={platform}");
    println!("fixtures:");
    for fixture in render_wgpu::screenshot_fixtures() {
        println!(
            "- {} kind={:?} description={}",
            fixture.name, fixture.kind, fixture.description
        );
    }
    println!("baseline_root=tools/conformance/screenshots/baselines/<platform>");
    println!("report_root=target/screenshots/<platform>");
    println!("verified_platforms=none until each platform runner captures and verifies baselines");
}

const FUZZ_TARGETS: &[&str] = &[
    "parser_input",
    "grid_actions",
    "resize",
    "unicode",
    "selection_ranges",
    "osc_dcs",
    "shell_markers",
];

fn run_fuzz_smoke() -> ExitCode {
    for (program, args) in [
        ("cargo", &["test", "-p", "term-core", "fuzz"][..]),
        ("cargo", &["test", "-p", "term-parser", "fuzz"][..]),
        ("cargo", &["test", "-p", "shell-integration", "fuzz"][..]),
    ] {
        let code = run(program, args);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }

    ExitCode::SUCCESS
}

fn run_fuzz() -> ExitCode {
    let mut args = std::env::args().skip(2).collect::<Vec<_>>();
    let Some(target) = args.first().cloned() else {
        eprintln!("usage: cargo xtask fuzz <target> [-- <libfuzzer args>]");
        eprintln!("known targets: {}", FUZZ_TARGETS.join(", "));
        return ExitCode::from(2);
    };

    if !FUZZ_TARGETS.contains(&target.as_str()) {
        eprintln!("unknown fuzz target: {target}");
        eprintln!("known targets: {}", FUZZ_TARGETS.join(", "));
        return ExitCode::from(2);
    }

    let mut cargo_args = vec!["+nightly".to_owned(), "fuzz".to_owned(), "run".to_owned()];
    cargo_args.append(&mut args);
    let refs = cargo_args.iter().map(String::as_str).collect::<Vec<_>>();

    #[cfg(windows)]
    if let Some(asan_dir) = windows_asan_runtime_dir() {
        return run_with_extra_path("cargo", &refs, &asan_dir);
    }

    #[cfg(windows)]
    eprintln!(
        "warning: clang_rt.asan_dynamic-x86_64.dll was not found in common Visual Studio/LLVM locations; cargo-fuzz may fail at runtime"
    );

    run("cargo", &refs)
}

fn print_config_default() -> ExitCode {
    match config_toml::default_config_toml() {
        Ok(config) => {
            print!("{config}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn print_config_schema() -> ExitCode {
    match config_toml::schema_json() {
        Ok(schema) => {
            println!("{schema}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run_ci() -> ExitCode {
    let layer_check = run_layer_check();
    if layer_check != ExitCode::SUCCESS {
        return layer_check;
    }

    for (program, args) in [
        ("cargo", &["fmt", "--all", "--check"][..]),
        ("cargo", &["check", "-p", "term-core"][..]),
        ("cargo", &["test", "-p", "render-core"][..]),
        ("cargo", &["clippy", "--workspace", "--all-targets"][..]),
        ("cargo", &["test", "--workspace"][..]),
        ("cargo", &["build", "--workspace", "--exclude", "xtask"][..]),
    ] {
        let code = run(program, args);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }

    ExitCode::SUCCESS
}

const WORKSPACE_PACKAGES: &[&str] = &[
    "assets",
    "config-core",
    "config-lua",
    "config-toml",
    "diagnostics",
    "font-system",
    "mux",
    "panea-bench",
    "panea-desktop",
    "panea-fuzz",
    "panea-ios",
    "panea-windows-installer",
    "platform-core",
    "platform-winit",
    "render-core",
    "render-wgpu",
    "security",
    "semantics",
    "shell-integration",
    "term-core",
    "term-parser",
    "transport-core",
    "transport-pty",
    "transport-ssh",
    "xtask",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestInfo {
    package_name: Option<String>,
    workspace_dependencies: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LayerViolation {
    manifest_path: PathBuf,
    package_name: String,
    dependency: String,
    message: String,
}

fn run_layer_check() -> ExitCode {
    match validate_layer_boundaries(Path::new(".")) {
        Ok(()) => {
            println!("architecture layer check passed");
            ExitCode::SUCCESS
        }
        Err(violations) => {
            eprintln!("architecture layer check failed:");
            for violation in violations {
                eprintln!(
                    "  {}: {} must not depend on {} ({})",
                    violation.manifest_path.display(),
                    violation.package_name,
                    violation.dependency,
                    violation.message
                );
            }
            ExitCode::from(1)
        }
    }
}

fn validate_layer_boundaries(root: &Path) -> Result<(), Vec<LayerViolation>> {
    let mut manifests = Vec::new();
    collect_manifests(root, &mut manifests).map_err(|error| {
        vec![LayerViolation {
            manifest_path: root.join("Cargo.toml"),
            package_name: "workspace".to_owned(),
            dependency: "filesystem".to_owned(),
            message: error.to_string(),
        }]
    })?;

    let mut violations = Vec::new();
    for manifest_path in manifests {
        let contents = match fs::read_to_string(&manifest_path) {
            Ok(contents) => contents,
            Err(error) => {
                violations.push(LayerViolation {
                    manifest_path,
                    package_name: "unknown".to_owned(),
                    dependency: "filesystem".to_owned(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let manifest = parse_manifest(&contents);
        let Some(package_name) = manifest.package_name else {
            continue;
        };
        violations.extend(violations_for_manifest(
            &manifest_path,
            &package_name,
            &manifest.workspace_dependencies,
        ));
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn collect_manifests(dir: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();

        if file_name == ".git" || file_name == "target" {
            continue;
        }

        if path.is_dir() {
            collect_manifests(&path, output)?;
        } else if file_name == "Cargo.toml" {
            output.push(path);
        }
    }

    Ok(())
}

fn parse_manifest(contents: &str) -> ManifestInfo {
    let workspace_packages = WORKSPACE_PACKAGES.iter().copied().collect::<BTreeSet<_>>();
    let mut section = String::new();
    let mut package_name = None;
    let mut workspace_dependencies = BTreeSet::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(['[', ']']).to_owned();
            continue;
        }

        if section == "package" && package_name.is_none() && line.starts_with("name") {
            package_name = parse_quoted_value(line);
            continue;
        }

        if section.ends_with("dependencies")
            && let Some(dependency) = parse_dependency_name(line)
            && workspace_packages.contains(dependency.as_str())
        {
            workspace_dependencies.insert(dependency);
        }
    }

    ManifestInfo {
        package_name,
        workspace_dependencies,
    }
}

fn parse_quoted_value(line: &str) -> Option<String> {
    let value = line.split_once('=')?.1.trim();
    Some(value.trim_matches('"').to_owned())
}

fn parse_dependency_name(line: &str) -> Option<String> {
    let key = line.split_once('=')?.0.trim();
    let key = key.split_once('.').map_or(key, |(name, _)| name).trim();
    let key = key.trim_matches('"');

    if key.is_empty() {
        None
    } else {
        Some(key.to_owned())
    }
}

fn violations_for_manifest(
    manifest_path: &Path,
    package_name: &str,
    workspace_dependencies: &BTreeSet<String>,
) -> Vec<LayerViolation> {
    let rules = dependency_rules();
    let Some(allowed) = rules.get(package_name) else {
        return vec![LayerViolation {
            manifest_path: manifest_path.to_owned(),
            package_name: package_name.to_owned(),
            dependency: "*".to_owned(),
            message: "package has no architecture dependency rule".to_owned(),
        }];
    };

    workspace_dependencies
        .iter()
        .filter(|dependency| !allowed.contains(dependency.as_str()))
        .map(|dependency| LayerViolation {
            manifest_path: manifest_path.to_owned(),
            package_name: package_name.to_owned(),
            dependency: dependency.clone(),
            message: boundary_message(package_name).to_owned(),
        })
        .collect()
}

fn dependency_rules() -> BTreeMap<&'static str, BTreeSet<&'static str>> {
    BTreeMap::from([
        ("assets", allowed([])),
        ("config-core", allowed([])),
        ("config-lua", allowed(["config-core"])),
        ("config-toml", allowed(["config-core"])),
        (
            "diagnostics",
            allowed([
                "config-core",
                "platform-core",
                "render-core",
                "security",
                "semantics",
                "term-core",
                "transport-core",
            ]),
        ),
        ("font-system", allowed([])),
        ("mux", allowed(["transport-core"])),
        (
            "panea-bench",
            allowed([
                "config-core",
                "diagnostics",
                "font-system",
                "render-core",
                "render-wgpu",
                "semantics",
                "term-core",
                "term-parser",
            ]),
        ),
        (
            "panea-desktop",
            allowed([
                "config-core",
                "config-lua",
                "config-toml",
                "diagnostics",
                "font-system",
                "mux",
                "platform-core",
                "platform-winit",
                "render-core",
                "render-wgpu",
                "security",
                "semantics",
                "shell-integration",
                "term-core",
                "term-parser",
                "transport-core",
                "transport-pty",
                "transport-ssh",
            ]),
        ),
        (
            "panea-fuzz",
            allowed(["semantics", "shell-integration", "term-core", "term-parser"]),
        ),
        (
            "panea-ios",
            allowed([
                "config-core",
                "font-system",
                "render-core",
                "security",
                "semantics",
                "term-core",
                "term-parser",
                "transport-core",
                "transport-ssh",
            ]),
        ),
        ("panea-windows-installer", allowed([])),
        ("platform-core", allowed([])),
        ("platform-winit", allowed(["platform-core"])),
        ("render-core", allowed([])),
        (
            "render-wgpu",
            allowed(["assets", "font-system", "render-core"]),
        ),
        ("security", allowed([])),
        ("semantics", allowed(["term-core"])),
        ("shell-integration", allowed(["semantics"])),
        ("term-core", allowed([])),
        ("term-parser", allowed(["term-core"])),
        ("transport-core", allowed([])),
        ("transport-pty", allowed(["transport-core"])),
        ("transport-ssh", allowed(["security", "transport-core"])),
        (
            "xtask",
            allowed([
                "assets",
                "config-core",
                "config-toml",
                "diagnostics",
                "render-wgpu",
                "security",
                "shell-integration",
                "term-core",
                "term-parser",
                "transport-core",
                "transport-pty",
                "transport-ssh",
            ]),
        ),
    ])
}

fn allowed<const N: usize>(items: [&'static str; N]) -> BTreeSet<&'static str> {
    items.into_iter().collect()
}

fn boundary_message(package_name: &str) -> &'static str {
    match package_name {
        "term-core" => {
            "terminal core must not know about renderer, platform, transport, SSH, or app crates"
        }
        "render-core" => {
            "render-core must stay renderer-independent and must not know about PTY, SSH, shell, platform, or app crates"
        }
        "render-wgpu" => {
            "render-wgpu may use renderer/font contracts but must not know about shells, PTY, SSH, app runtime, or platform adapters"
        }
        "platform-core" => {
            "platform-core must expose capability contracts without depending on renderer or transport crates"
        }
        "platform-winit" => {
            "platform-winit must hide windowing internals behind platform-core contracts"
        }
        "config-core" => {
            "config-core defines portable user intent and must not import runtime layer crates"
        }
        "transport-core" => {
            "transport-core must model bytes and lifecycle without renderer, platform, or config dependencies"
        }
        "transport-pty" => {
            "transport-pty must stay behind transport-core and avoid renderer or app dependencies"
        }
        "transport-ssh" => {
            "transport-ssh must stay behind transport-core/security and avoid renderer or app dependencies"
        }
        _ => "dependency is not allowed by the layer boundary matrix",
    }
}

fn run(program: &str, args: &[&str]) -> ExitCode {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("failed to run {program}: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run_with_extra_path(program: &str, args: &[&str], path: &Path) -> ExitCode {
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![path.to_owned()];
    paths.extend(std::env::split_paths(&old_path));
    let joined = match std::env::join_paths(paths) {
        Ok(joined) => joined,
        Err(error) => {
            eprintln!("failed to prepare PATH for {program}: {error}");
            return ExitCode::from(1);
        }
    };

    match Command::new(program)
        .args(args)
        .env("PATH", joined)
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("failed to run {program}: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn windows_asan_runtime_dir() -> Option<PathBuf> {
    let dll = "clang_rt.asan_dynamic-x86_64.dll";
    for dir in windows_asan_candidate_dirs() {
        let direct = dir.join(dll);
        if direct.exists() {
            return Some(dir);
        }
        if let Some(found) = find_file_bounded(&dir, dll, 16) {
            return found.parent().map(Path::to_owned);
        }
    }
    None
}

#[cfg(windows)]
fn windows_asan_candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
        dirs.push(
            PathBuf::from(program_files_x86)
                .join("Microsoft Visual Studio")
                .join("2022"),
        );
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let program_files = PathBuf::from(program_files);
        dirs.push(program_files.join("Microsoft Visual Studio").join("2022"));
        dirs.push(program_files.join("LLVM").join("bin"));
    }
    dirs.push(PathBuf::from(
        r"C:\Program Files (x86)\Microsoft Visual Studio\2022",
    ));
    dirs.push(PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\2022",
    ));
    dirs.push(PathBuf::from(r"C:\Program Files\LLVM\bin"));
    dirs
}

#[cfg(windows)]
fn find_file_bounded(dir: &Path, file_name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path.file_name().is_some_and(|candidate| {
                candidate.to_string_lossy().eq_ignore_ascii_case(file_name)
            })
        {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file_bounded(&path, file_name, depth - 1)
        {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_collects_workspace_dependencies() {
        let manifest = parse_manifest(
            r#"
            [package]
            name = "term-parser"

            [dependencies]
            term-core = { path = "../term-core" }
            serde.workspace = true
            "#,
        );

        assert_eq!(manifest.package_name.as_deref(), Some("term-parser"));
        assert!(manifest.workspace_dependencies.contains("term-core"));
        assert!(!manifest.workspace_dependencies.contains("serde"));
    }

    #[test]
    fn lower_layer_dependency_is_rejected() {
        let violations = violations_for_manifest(
            Path::new("crates/term-core/Cargo.toml"),
            "term-core",
            &BTreeSet::from(["render-core".to_owned()]),
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].dependency, "render-core");
    }

    #[test]
    fn renderer_backend_cannot_import_transport() {
        let violations = violations_for_manifest(
            Path::new("crates/render-wgpu/Cargo.toml"),
            "render-wgpu",
            &BTreeSet::from(["render-core".to_owned(), "transport-pty".to_owned()]),
        );

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].dependency, "transport-pty");
    }

    #[test]
    fn app_crate_can_compose_layers() {
        let violations = violations_for_manifest(
            Path::new("apps/desktop/Cargo.toml"),
            "panea-desktop",
            &BTreeSet::from([
                "term-core".to_owned(),
                "platform-winit".to_owned(),
                "render-wgpu".to_owned(),
                "transport-pty".to_owned(),
            ]),
        );

        assert!(violations.is_empty());
    }

    #[test]
    fn compatibility_cases_cover_required_shell_and_protocol_smoke() {
        let cases = compat_cases();

        assert!(cases.iter().any(|case| case.key == "shell-powershell"
            && case.required
            && case.category == CompatCategory::Shells));
        assert!(cases.iter().any(|case| case.key == "shell-cmd"
            && case.required
            && case.category == CompatCategory::Shells));
        assert!(cases.iter().any(|case| case.key == "protocol-ansi-marker"
            && case.required
            && case.category == CompatCategory::Protocol));
        assert!(cases.iter().any(|case| case.key == "mux-tmux"
            && !case.required
            && case.category == CompatCategory::Multiplexers));
    }

    #[test]
    fn compatibility_terminal_probe_answers_queries_split_across_reads() {
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));

        assert!(
            terminal_responses_for(&mut terminal, b"\x1b[")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            terminal_responses_for(&mut terminal, b"6n").unwrap(),
            b"\x1b[1;1R"
        );
    }

    #[test]
    fn compatibility_options_parse_filters_and_timeout() {
        let options = CompatRunOptions::parse(&[
            "--required-only".to_owned(),
            "--category".to_owned(),
            "shells".to_owned(),
            "--case".to_owned(),
            "shell-cmd".to_owned(),
            "--timeout-ms".to_owned(),
            "250".to_owned(),
        ])
        .expect("compat options");

        assert!(options.required_only);
        assert_eq!(options.category, Some(CompatCategory::Shells));
        assert_eq!(options.case_key.as_deref(), Some("shell-cmd"));
        assert_eq!(options.timeout, Duration::from_millis(250));
    }

    #[test]
    fn compatibility_classifies_missing_wsl_distribution_as_unavailable_runtime() {
        assert!(pty_runtime_unavailable(
            b"Windows Subsystem for Linux has no installed distributions."
        ));
        assert!(!pty_runtime_unavailable(b"bash: printf: command not found"));
    }

    #[test]
    fn ssh_smoke_options_parse_without_secret_values() {
        let options = SshSmokeOptions::parse(&[
            "--host".to_owned(),
            "127.0.0.1".to_owned(),
            "--port".to_owned(),
            "2222".to_owned(),
            "--user".to_owned(),
            "panea".to_owned(),
            "--auth".to_owned(),
            "public_key".to_owned(),
            "--identity-file".to_owned(),
            "target/test-id".to_owned(),
            "--password-env".to_owned(),
            "PASSWORD_ENV_NAME".to_owned(),
            "--passphrase-env".to_owned(),
            "PASSPHRASE_ENV_NAME".to_owned(),
            "--timeout-ms".to_owned(),
            "750".to_owned(),
            "--remote-kind".to_owned(),
            "posix".to_owned(),
        ])
        .expect("ssh smoke options");

        assert_eq!(options.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(options.port, 2222);
        assert_eq!(options.username.as_deref(), Some("panea"));
        assert_eq!(options.auth_method, AuthMethod::PublicKey);
        assert_eq!(options.password_env, "PASSWORD_ENV_NAME");
        assert_eq!(options.passphrase_env, "PASSPHRASE_ENV_NAME");
        assert_eq!(options.timeout, Duration::from_millis(750));
        assert_eq!(options.remote_kind, SshSmokeRemoteKind::Posix);
    }

    #[test]
    fn verify_os_options_parse_target_suite_and_timeout() {
        let options = VerifyOsOptions::parse(&[
            "--target-platform".to_owned(),
            "linux-wayland".to_owned(),
            "--suite".to_owned(),
            "ci".to_owned(),
            "--report-dir".to_owned(),
            "target/custom-cross-os".to_owned(),
            "--timeout-ms".to_owned(),
            "750".to_owned(),
            "--allow-missing-screenshot-baseline".to_owned(),
        ])
        .expect("verify-os options");

        assert_eq!(options.target_platform, CompatPlatform::LinuxWayland);
        assert_eq!(options.suite, VerifySuite::Ci);
        assert_eq!(options.report_root, PathBuf::from("target/custom-cross-os"));
        assert_eq!(options.timeout, Duration::from_millis(750));
        assert!(options.allow_missing_screenshot_baseline);
    }

    #[test]
    fn verify_os_steps_cover_required_categories() {
        let options = VerifyOsOptions {
            target_platform: CompatPlatform::LinuxX11,
            suite: VerifySuite::Ci,
            report_root: PathBuf::from("target/cross-os"),
            timeout: Duration::from_secs(1),
            allow_missing_screenshot_baseline: true,
            with_ssh: false,
        };
        let steps = verify_os_steps(&options);

        assert!(steps.iter().any(|step| step.key == "format"));
        assert!(steps.iter().any(|step| step.key == "parser-tests"));
        assert!(steps.iter().any(|step| step.key == "unicode-tests"));
        assert!(steps.iter().any(|step| step.key == "screenshot-tests"));
        assert!(
            steps
                .iter()
                .any(|step| step.key == "linux-compositor-diagnostics" && step.required)
        );
        assert!(steps.iter().any(|step| {
            step.key == "ssh-smoke" && matches!(step.kind, VerifyStepKind::Skipped { .. })
        }));
        assert!(steps.iter().any(|step| {
            step.key == "packaging-smoke"
                && step.required
                && matches!(step.kind, VerifyStepKind::Command { .. })
        }));
    }

    #[test]
    fn verify_os_target_env_marks_linux_backend() {
        assert_eq!(
            target_env(CompatPlatform::LinuxWayland),
            vec![("XDG_SESSION_TYPE".to_owned(), "wayland".to_owned())]
        );
        assert_eq!(
            target_env(CompatPlatform::LinuxX11),
            vec![("XDG_SESSION_TYPE".to_owned(), "x11".to_owned())]
        );
        assert!(target_env(CompatPlatform::Windows).is_empty());
    }

    #[test]
    fn cross_os_workflow_provisions_real_linux_sessions_and_packaging_tools() {
        let workflow = include_str!("../../../.github/workflows/cross-os-verification.yml");
        let release_workflow = include_str!("../../../.github/workflows/release-artifacts.yml");

        assert!(workflow.contains("xvfb-run -a"));
        assert!(workflow.contains("libxkbcommon-x11-0"));
        assert!(workflow.contains("--backend=headless-backend.so"));
        assert!(workflow.contains("WAYLAND_DISPLAY"));
        assert!(workflow.contains("PANEA_APPIMAGETOOL"));
        assert!(workflow.contains("APPIMAGE_EXTRACT_AND_RUN"));
        assert!(workflow.contains("--timeout-ms 600000"));
        assert!(
            workflow.contains("a6d71e2b6cd66f8e8d16c37ad164658985e0cf5fcaa950c90a482890cb9d13e0")
        );
        assert!(release_workflow.contains("libxkbcommon-x11-0"));
    }

    #[test]
    fn verify_os_report_json_escapes_step_notes() {
        let report = VerifyOsReport {
            target_platform: CompatPlatform::Windows,
            detected_platform: CompatPlatform::Windows,
            suite: VerifySuite::Smoke,
            started_duration: Duration::from_millis(12),
            results: vec![VerifyStepResult {
                key: "doctor-json".to_owned(),
                category: "diagnostics".to_owned(),
                description: "diagnostic report".to_owned(),
                required: true,
                status: VerifyStepStatus::Passed,
                duration: Duration::from_millis(5),
                exit_code: Some(0),
                stdout_log: None,
                stderr_log: None,
                stdout_preview: String::new(),
                stderr_preview: String::new(),
                note: "path C:\\panea\nok".to_owned(),
            }],
        };

        let json = report.render_json();
        assert!(json.contains("\"target_platform\": \"windows\""));
        assert!(json.contains("path C:\\\\panea\\nok"));
        assert!(!report.has_hard_failure());
    }

    #[test]
    fn package_options_parse_target_profile_and_timeout() {
        let options = PackageOptions::parse(&[
            "--target-platform".to_owned(),
            "macos".to_owned(),
            "--profile".to_owned(),
            "dev".to_owned(),
            "--out-dir".to_owned(),
            "target/custom-packages".to_owned(),
            "--skip-cargo-build".to_owned(),
            "--timeout-ms".to_owned(),
            "750".to_owned(),
        ])
        .expect("package options");

        assert_eq!(options.target_platform, CompatPlatform::Macos);
        assert_eq!(options.profile, PackageProfile::Dev);
        assert_eq!(options.out_dir, PathBuf::from("target/custom-packages"));
        assert!(options.skip_cargo_build);
        assert_eq!(options.timeout, Duration::from_millis(750));
    }

    #[test]
    fn package_layouts_are_platform_specific() {
        let mut options = PackageOptions::default_for(CompatPlatform::Windows);
        options.profile = PackageProfile::Dev;
        let windows = package_layout(&options);
        assert!(
            windows.binary_path.ends_with("panea.exe") || windows.binary_path.ends_with("panea")
        );
        assert!(
            windows
                .resource_dir
                .ends_with(Path::new("share").join("panea"))
        );

        options.target_platform = CompatPlatform::Macos;
        let macos = package_layout(&options);
        assert!(
            macos
                .binary_path
                .ends_with(Path::new("Contents").join("MacOS").join("panea"))
        );
        assert!(
            macos
                .resource_dir
                .ends_with(Path::new("Contents").join("Resources"))
        );

        options.target_platform = CompatPlatform::LinuxWayland;
        let linux = package_layout(&options);
        assert!(linux.binary_path.ends_with(Path::new("bin").join("panea")));
        assert!(
            linux
                .resource_dir
                .ends_with(Path::new("share").join("panea"))
        );
    }

    #[test]
    fn package_manifest_lists_required_resources() {
        let options = PackageOptions::default_for(CompatPlatform::Windows);
        let layout = package_layout(&options);
        let manifest = render_package_manifest(&options, &layout);

        assert!(manifest.contains("\"default_config\""));
        assert!(manifest.contains("\"shell_integration_scripts\""));
        assert!(manifest.contains("\"doctor_command\""));
        assert!(manifest.contains("\"shell_smoke_command\""));
        assert!(manifest.contains("\"gui_smoke_command\""));
        assert!(manifest.contains("\"gui_binary\": \"panea-gui.exe\""));
        assert!(manifest.contains("\"gui_entrypoint\""));
        assert!(manifest.contains("\"cursor_vector_assets\""));
        assert!(manifest.contains("\"programmable_config_examples\""));
        assert!(manifest.contains("\"themes\""));
        assert!(manifest.contains("\"cursor_profiles\""));
        assert!(manifest.contains("\"license\""));
        assert!(manifest.contains("\"application_icon\""));
    }

    #[test]
    fn windows_gui_entrypoint_is_sibling_of_console_binary() {
        let binary = Path::new("package").join("panea.exe");
        assert_eq!(
            windows_gui_binary_path(&binary),
            Path::new("package").join("panea-gui.exe")
        );
    }

    #[test]
    fn package_icon_paths_follow_platform_conventions() {
        let windows_options = PackageOptions::default_for(CompatPlatform::Windows);
        let windows = package_layout(&windows_options);
        assert!(package_icon_paths(&windows, CompatPlatform::Windows)[0].ends_with("panea.ico"));

        let macos_options = PackageOptions::default_for(CompatPlatform::Macos);
        let macos = package_layout(&macos_options);
        assert!(package_icon_paths(&macos, CompatPlatform::Macos)[0].ends_with("Panea.icns"));

        let linux_options = PackageOptions::default_for(CompatPlatform::LinuxWayland);
        let linux = package_layout(&linux_options);
        assert!(
            package_icon_paths(&linux, CompatPlatform::LinuxWayland)[0]
                .ends_with(Path::new("512x512").join("apps").join("panea.png"))
        );
    }

    #[test]
    fn distribution_artifact_names_are_versioned_and_arch_specific() {
        let mut options = PackageOptions::default_for(CompatPlatform::Windows);
        options.profile = PackageProfile::Release;
        let name = artifact_stem(&options, "windows-installer");

        assert!(name.contains(env!("CARGO_PKG_VERSION")));
        assert!(name.contains(std::env::consts::ARCH));
        assert!(name.ends_with("-release"));
    }

    #[test]
    fn packaged_theme_and_cursor_profiles_parse_as_portable_config() {
        for asset in assets::THEMES.iter().chain(assets::CURSOR_PROFILES) {
            config_toml::parse_str(asset.contents, None, config_core::ConfigPlatform::Unknown)
                .unwrap_or_else(|error| panic!("{} must parse: {error}", asset.name));
        }
    }
}
