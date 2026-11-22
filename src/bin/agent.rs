//! Agent entry point. On Windows this is a GUI-subsystem executable, so starting it from the
//! `Run` registry key or the Start menu never flashes a console window. It runs:
//! - the resident watcher (default),
//! - the settings window (`--settings`),
//! - the short-lived encoder process (`worker ...`).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "worker") {
        std::process::exit(optiman::cli::main());
    }
    let mut config = None;
    let mut settings = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--config" {
            config = it.next().map(PathBuf::from);
        } else if a == "--settings" {
            settings = true;
        }
    }
    if settings && config.is_none() {
        optiman::cli::migrate_from_legacy();
    }
    let config = config.unwrap_or_else(optiman::app::default_config_path);
    if settings {
        if let Err(e) = optiman::platform::ui::run_settings(&config) {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    std::process::exit(optiman::cli::run_agent(&config, false, false));
}
