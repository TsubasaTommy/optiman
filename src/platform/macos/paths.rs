//! Standard locations on macOS.

use std::ffi::CStr;
use std::path::PathBuf;

use crate::APP_NAME;

pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return h;
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_dir.is_null() {
            return PathBuf::from(CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned());
        }
    }
    PathBuf::from("/")
}

pub fn config_dir() -> PathBuf {
    config_dir_for(APP_NAME)
}

pub fn log_dir() -> PathBuf {
    log_root_for(APP_NAME)
}

/// Settings and state of an app called `name` (also used to find the former name's folder).
pub fn config_dir_for(name: &str) -> PathBuf {
    home_dir().join("Library/Application Support").join(name)
}

/// The folder that holds everything log-related for `name`.
pub fn log_root_for(name: &str) -> PathBuf {
    home_dir().join("Library/Logs").join(name)
}

pub fn pictures_dir() -> PathBuf {
    home_dir().join("Pictures")
}

/// Never watch these (or anything inside them).
pub fn system_dirs() -> Vec<PathBuf> {
    [
        "/System",
        "/Library",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/private/etc",
        "/Applications",
        "/opt",
        "/cores",
        "/dev",
        "/private/var/db",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// Canonical form used for watch roots (FSEvents reports fully resolved paths).
pub fn normalize(p: PathBuf) -> PathBuf {
    p
}

pub fn canonical(p: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p)
}
