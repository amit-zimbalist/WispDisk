mod args;
mod driver;
mod payload;
mod protocol;

use std::process::ExitCode;

use args::{Cli, normalize_windows_args};
use clap::Parser;

fn main() -> ExitCode {
    let cli = Cli::parse_from(normalize_windows_args(std::env::args_os()));
    let dry_run = cli.dry_run;
    let command = match cli.into_command() {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    if dry_run {
        println!("validated request: {command}");
        return ExitCode::SUCCESS;
    }

    match driver::execute(&command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
