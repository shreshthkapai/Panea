use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("help") | None => {
            eprintln!(
                "usage: cargo xtask <fmt|clippy|test|build|check|ci|config-default|config-schema|bench|doctor|bug-report>"
            );
            ExitCode::SUCCESS
        }
        Some("fmt") => run("cargo", &["fmt", "--all"]),
        Some("clippy") => run("cargo", &["clippy", "--workspace", "--all-targets"]),
        Some("test") => run("cargo", &["test", "--workspace"]),
        Some("build") => run("cargo", &["build", "--workspace"]),
        Some("check") => run("cargo", &["check", "--workspace"]),
        Some("config-default") => print_config_default(),
        Some("config-schema") => print_config_schema(),
        Some("bench") => run_bench(),
        Some("doctor") => run_doctor(),
        Some("bug-report") => run_bug_report(),
        Some("ci") => run_ci(),
        Some(command) => {
            eprintln!("unknown xtask command: {command}");
            ExitCode::from(2)
        }
    }
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
        "-p".to_owned(),
        "panea-bench".to_owned(),
        "--".to_owned(),
    ];
    cargo_args.extend(args);
    let refs = cargo_args.iter().map(String::as_str).collect::<Vec<_>>();
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
    for (program, args) in [
        ("cargo", &["fmt", "--all", "--check"][..]),
        ("cargo", &["clippy", "--workspace", "--all-targets"][..]),
        ("cargo", &["test", "--workspace"][..]),
        ("cargo", &["build", "--workspace"][..]),
    ] {
        let code = run(program, args);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }

    ExitCode::SUCCESS
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
