use std::process::ExitCode;

mod checkout_setup;
mod cli;
mod native_client;
mod runtime;

fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("reviewq: {error}");
            ExitCode::FAILURE
        }
    }
}
