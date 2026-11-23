use std::process::ExitCode;

mod checkout_setup;
mod cli;
mod install;
mod json_edit;
mod native_client;
mod profile_config;
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
