//! `timelapsable`: color/exposure matching and crop stabilization for timelapses.

mod analysis;
mod app;
mod atomic;
mod cache;
mod cli;
mod color;
mod decode;
mod error;
mod metadata;
mod plan;
mod render;
mod scan;
mod source;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    match app::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            error::exit_code(error::exit_code_for(&error))
        }
    }
}
