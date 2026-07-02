use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("help") | None => {
            eprintln!(
                "usage: cargo xtask <fmt|clippy|test|build|check|ci|config-default|config-schema>"
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
        Some("ci") => run_ci(),
        Some(command) => {
            eprintln!("unknown xtask command: {command}");
            ExitCode::from(2)
        }
    }
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
