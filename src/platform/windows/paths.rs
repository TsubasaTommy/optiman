//! Known folders via SHGetKnownFolderPath.

use std::path::{Path, PathBuf};

use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_RoamingAppData, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath,
};
use windows::core::GUID;

use super::take_co_string;
use crate::APP_NAME;

fn known(id: &GUID) -> Option<PathBuf> {
    unsafe { take_co_string(SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?) }.map(PathBuf::from)
}

pub fn home_dir() -> PathBuf {
    known(&FOLDERID_Profile)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

pub fn config_dir() -> PathBuf {
    known(&FOLDERID_RoamingAppData).unwrap_or_else(|| home_dir().join("AppData\\Roaming")).join(APP_NAME)
}

pub fn log_dir() -> PathBuf {
    known(&FOLDERID_LocalAppData).unwrap_or_else(|| home_dir().join("AppData\\Local")).join(APP_NAME).join("logs")
}

pub fn pictures_dir() -> PathBuf {
    known(&FOLDERID_Pictures).unwrap_or_else(|| home_dir().join("Pictures"))
}

pub fn system_dirs() -> Vec<PathBuf> {
    let windir = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let mut v = vec![windir];
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
        if let Some(p) = std::env::var_os(var) {
            v.push(PathBuf::from(p));
        }
    }
    v
}

/// `fs::canonicalize` returns `\\?\C:\...`; strip the verbatim prefix for drive paths so
/// paths stay readable and work with Shell APIs.
fn normalize(p: PathBuf) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            return PathBuf::from(rest);
        }
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
    }
    p
}

pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p).map(normalize)
}
