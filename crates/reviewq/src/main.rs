use std::{env, process::ExitCode};

fn main() -> ExitCode {
    if env::args_os().skip(1).eq(["--version"]) {
        println!("reviewq {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    eprintln!("Usage: reviewq --version");
    ExitCode::FAILURE
}
