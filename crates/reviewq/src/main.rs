use std::process::ExitCode;

mod checkout_setup;
mod cli;
mod install;
mod native_client;
mod runtime;
mod runtime_settings;
mod service;

fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("reviewq: {error}");
            ExitCode::FAILURE
        }
    }
}
