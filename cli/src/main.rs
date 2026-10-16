mod args;
mod driver;
mod logging;
mod payload;
mod protocol;

use std::process::ExitCode;

use args::{Cli, normalize_windows_args};
use clap::Parser;
use tracing::{error, info};

fn main() -> ExitCode {
    let cli = Cli::parse_from(normalize_windows_args(std::env::args_os()));
    let log_level = cli
        .log
        .filter(|level| *level != tracing::level_filters::LevelFilter::OFF);
    let command = match cli.into_command() {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    let logging = match log_level.map(logging::Logging::start).transpose() {
        Ok(logging) => logging,
        Err(error) => {
            eprintln!("error: failed to initialize logging: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    info!(%command, "Command started");
    match driver::execute(&command) {
        Ok(()) => {
            info!("Command completed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            if logging.is_some() {
                error!(error = %format_args!("{error:#}"), "Command failed");
            } else {
                eprintln!("error: {error:#}");
            }
            ExitCode::from(driver::error_exit_code(&error))
        }
    }
}
