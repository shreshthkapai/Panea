use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("help") | None => {
            eprintln!(
                "usage: cargo xtask <fmt|clippy|test|build|check|layer-check|ci|config-default|config-schema|bench|fuzz-smoke|fuzz|doctor|bug-report|hardening|security-review|package-plan|release-check|ios-readiness>"
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
        Some("fuzz-smoke") => run_fuzz_smoke(),
        Some("fuzz") => run_fuzz(),
        Some("doctor") => run_doctor(),
        Some("bug-report") => run_bug_report(),
        Some("hardening") => run_hardening(),
        Some("security-review") => run_security_review(),
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

fn run_ios_readiness() -> ExitCode {
    println!(
        "{}",
        diagnostics::ios_companion_readiness_report().render_text()
    );
    ExitCode::SUCCESS
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
        ("xtask", allowed(["config-toml", "diagnostics"])),
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
}
