//! Resident agent entry point. On Windows this is a GUI-subsystem executable so that starting
//! it from the `Run` registry key does not flash a console window. It also serves as the
//! short-lived encoder process (`worker`), which then has no console either.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() {
    if std::env::args_os().nth(1).is_some_and(|a| a == "worker") {
        std::process::exit(oif::cli::main());
    }
    let mut args = std::env::args_os().skip(1);
    let mut config = None;
    while let Some(a) = args.next() {
        if a == "--config" {
            config = args.next().map(PathBuf::from);
        }
    }
    let config = config.unwrap_or_else(oif::cli::default_config_path);
    std::process::exit(oif::cli::run_agent(&config, false));
}
