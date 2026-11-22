//! Locations and actions shared by the CLI and the settings window.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::APP_NAME;
use crate::config::{self, Config};
use crate::daemon::resolve_roots;
use crate::platform::{autostart, events, paths};

pub fn default_config_path() -> PathBuf {
    match std::env::var_os("OPTIMAN_HOME") {
        Some(home) => PathBuf::from(home).join("config.toml"),
        None => paths::config_dir().join("config.toml"),
    }
}

/// The standard log folder for the standard config; otherwise next to the config file. Derived
/// from the config path only, so the CLI and the agent always agree.
pub fn log_path_for(config_path: &Path) -> PathBuf {
    if config_path == paths::config_dir().join("config.toml") {
        paths::log_dir().join(format!("{APP_NAME}.log"))
    } else {
        config_path.with_file_name(format!("{APP_NAME}.log"))
    }
}

pub fn lock_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("agent.lock")
}

/// The running agent holds an exclusive lock on this file.
pub fn agent_running(config_path: &Path) -> bool {
    let Ok(f) =
        fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path_for(config_path))
    else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Stops the agent and waits (up to 10 s) until it has released its lock.
pub fn stop_agent(config_path: &Path) -> Result<()> {
    autostart::stop()?;
    if agent_running(config_path) {
        events::request_stop(config_path);
        for _ in 0..100 {
            if !agent_running(config_path) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// Registers (or re-registers) the agent for the folders in `cfg`, and (re)starts it if asked so
/// that it picks up the new settings.
pub fn register_agent(config_path: &Path, cfg: &Config, start_now: bool) -> Result<()> {
    let exe = autostart::agent_executable()?;
    if start_now {
        stop_agent(config_path)?;
    }
    let log_path = log_path_for(config_path);
    let log_dir = log_path.parent().map(Path::to_path_buf).unwrap_or_else(paths::log_dir);
    let folders = resolve_roots(cfg);
    autostart::install(
        &autostart::AgentSpec { exe: &exe, config: config_path, log_dir: &log_dir, folders: &folders },
        start_now,
    )
}

/// Settings edited in the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub effort: u8,
    pub lossy: bool,
    pub quality: u8,
    pub include: Vec<PathBuf>,
    pub exclude: Vec<PathBuf>,
}

impl Settings {
    pub fn from_config(cfg: &Config) -> Settings {
        Settings {
            effort: cfg.effort,
            lossy: cfg.lossy,
            quality: cfg.quality,
            include: cfg.folder_paths(),
            exclude: cfg.exclude_folder_paths(),
        }
    }
}

/// Validates, saves and applies settings (the agent is re-registered with the new folders).
pub fn save_settings(config_path: &Path, settings: &Settings) -> Result<Config> {
    if settings.include.is_empty() {
        bail!("変換するフォルダを 1 つ以上指定してください");
    }
    for f in &settings.include {
        config::check_folder_safe(f)?;
    }
    let mut cfg = Config::load(config_path).unwrap_or_default();
    cfg.effort = settings.effort;
    cfg.lossy = settings.lossy;
    cfg.quality = settings.quality;
    cfg.folders = settings.include.iter().map(|f| config::contract_tilde(f)).collect();
    cfg.exclude_folders = settings.exclude.iter().map(|f| config::contract_tilde(f)).collect();
    cfg.save(config_path)?;
    register_agent(config_path, &cfg, true)?;
    Ok(cfg)
}

/// A short description for an effort value (shown next to the slider).
pub fn effort_label(effort: u8) -> String {
    let hint = match effort {
        1..=3 => "速い",
        4..=6 => "やや小さい",
        7 => "標準",
        8 => "小さい",
        _ => "最小 (時間がかかる)",
    };
    format!("{effort} ({hint})")
}

/// A short description for a lossy quality value (shown next to its slider).
pub fn quality_label(quality: u8) -> String {
    let hint = match quality {
        90.. => "最高画質",
        80..=89 => "高画質",
        70..=79 => "標準",
        _ => "小さい",
    };
    format!("{quality} ({hint})")
}

/// One line describing how photos are encoded.
pub fn mode_label(cfg: &Config) -> String {
    if cfg.lossy { format!("非可逆 品質 {} (JPEG は劣化なし)", cfg.quality) } else { "劣化なし".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_location_follows_the_config() {
        let custom = Path::new("/tmp/somewhere/config.toml");
        assert_eq!(log_path_for(custom), Path::new("/tmp/somewhere/optiman.log"));
    }

    #[test]
    fn quality_labels() {
        assert_eq!(quality_label(90), "90 (最高画質)");
        assert_eq!(quality_label(60), "60 (小さい)");
    }

    #[test]
    fn effort_labels() {
        assert_eq!(effort_label(7), "7 (標準)");
        assert!(effort_label(9).starts_with("9 "));
    }
}
