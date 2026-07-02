use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("help") | None => {
            eprintln!("usage: cargo xtask <fmt|clippy|test|build|check|ci>");
            ExitCode::SUCCESS
        }
        Some("fmt") => run("cargo", &["fmt", "--all"]),
        Some("clippy") => run("cargo", &["clippy", "--workspace", "--all-targets"]),
        Some("test") => run("cargo", &["test", "--workspace"]),
        Some("build") => run("cargo", &["build", "--workspace"]),
        Some("check") => run("cargo", &["check", "--workspace"]),
        Some("ci") => run_ci(),
        Some(command) => {
            eprintln!("unknown xtask command: {command}");
            ExitCode::from(2)
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
