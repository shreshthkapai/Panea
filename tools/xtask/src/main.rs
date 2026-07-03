use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant},
};

use transport_core::{TerminalSize, TerminalTransport, TransportLifecycleEvent, TransportState};
use transport_pty::{LocalPtyDiagnostics, LocalPtyTransport, LocalShellProfile};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("help") | None => {
            eprintln!(
                "usage: cargo xtask <fmt|clippy|test|build|check|layer-check|ci|config-default|config-schema|bench|screenshot|fuzz-smoke|fuzz|doctor|bug-report|hardening|security-review|linux-compositor|compat|package-plan|release-check|ios-readiness>"
            );
            ExitCode::SUCCESS
        }
        Some("fmt") => run("cargo", &["fmt", "--all"]),
        Some("clippy") => run("cargo", &["clippy", "--workspace", "--all-targets"]),
        Some("test") => run("cargo", &["test", "--workspace"]),
        Some("build") => run("cargo", &["build", "--workspace"]),
        Some("check") => run("cargo", &["check", "--workspace"]),
        Some("layer-check") => run_layer_check(),
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

    while Instant::now() < deadline {
        match transport.poll_output() {
            Ok(poll) => {
                answer_terminal_queries(&mut transport, &poll.bytes)?;
                lifecycle.extend(poll.lifecycle);
                output.extend(poll.bytes);
                saw_marker =
                    saw_marker || output.windows(marker.len()).any(|window| window == marker);
                let closed =
                    poll.closed || matches!(transport.state(), TransportState::Closed { .. });
                if saw_marker && closed {
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
        return Err(CompatRunError {
            missing_program: false,
            message: format!(
                "PTY case did not observe marker {:?}; shutdown={:?}",
                String::from_utf8_lossy(marker),
                shutdown_result.map_err(|error| error.to_string())
            ),
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

fn answer_terminal_queries(
    transport: &mut LocalPtyTransport,
    bytes: &[u8],
) -> Result<(), CompatRunError> {
    if bytes
        .windows(b"\x1b[6n".len())
        .any(|window| window == b"\x1b[6n")
    {
        transport.write_input(b"\x1b[1;1R").map_err(|error| {
            CompatRunError::failed(format!("failed to answer CPR query: {error}"))
        })?;
    }

    Ok(())
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
        verifies: "SSH client exists; real SSH server smoke is deferred to the SSH server phase",
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
    let topic = std::env::args().nth(2).as_deref().map_or(
        Some(diagnostics::DoctorTopic::All),
        diagnostics::DoctorTopic::parse,
    );
    let Some(topic) = topic else {
        eprintln!(
            "unknown doctor topic; expected renderer, config, platform, shell-integration, performance, ssh, or window"
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

    println!(
        "{}",
        diagnostics::doctor_report(&input, topic).render_text()
    );
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
        ("platform-core", allowed([])),
        ("platform-winit", allowed(["platform-core"])),
        ("render-core", allowed([])),
        ("render-wgpu", allowed(["font-system", "render-core"])),
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
                "config-toml",
                "diagnostics",
                "render-wgpu",
                "transport-core",
                "transport-pty",
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
}
