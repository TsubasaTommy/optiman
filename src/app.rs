//! Locations and actions shared by the CLI and the settings window.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::APP_NAME;
use crate::config::{self, Config, Quality};
use crate::daemon::resolve_roots;
use crate::platform::{AgentSpec, autostart, events, paths};

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

/// Opens (creating it if needed) the file the running agent holds an exclusive lock on.
pub fn open_lock(config_path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path_for(config_path))
}

/// True while an agent for this config runs (it holds the lock).
pub fn agent_running(config_path: &Path) -> bool {
    let Ok(f) = open_lock(config_path) else {
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

/// What registering the agent for this config takes.
pub fn agent_spec(config_path: &Path, cfg: &Config) -> Result<AgentSpec> {
    let log_path = log_path_for(config_path);
    Ok(AgentSpec {
        exe: autostart::agent_executable()?,
        config: config_path.to_path_buf(),
        log_dir: log_path.parent().map(Path::to_path_buf).unwrap_or_else(paths::log_dir),
        folders: resolve_roots(cfg),
        auto_update: cfg.auto_update,
        tray_icon: cfg.tray_icon,
    })
}

/// Registers (or re-registers) the agent for the folders in `cfg`, and (re)starts it if asked so
/// that it picks up the new settings.
pub fn register_agent(config_path: &Path, cfg: &Config, start_now: bool) -> Result<()> {
    let spec = agent_spec(config_path, cfg)?;
    if start_now {
        stop_agent(config_path)?;
    }
    autostart::install(&spec, start_now)
}

/// Settings edited in the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub effort: u8,
    pub quality: Quality,
    pub include: Vec<PathBuf>,
    pub exclude: Vec<PathBuf>,
    pub auto_update: bool,
    pub tray_icon: bool,
}

impl Settings {
    pub fn from_config(cfg: &Config) -> Settings {
        Settings {
            effort: cfg.effort,
            quality: cfg.quality,
            include: cfg.folder_paths(),
            exclude: cfg.exclude_folder_paths(),
            auto_update: cfg.auto_update,
            tray_icon: cfg.tray_icon,
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
    cfg.quality = settings.quality;
    cfg.folders = settings.include.iter().map(|f| config::contract_tilde(f)).collect();
    cfg.exclude_folders = settings.exclude.iter().map(|f| config::contract_tilde(f)).collect();
    cfg.auto_update = settings.auto_update;
    cfg.tray_icon = settings.tray_icon;
    cfg.save(config_path)?;
    register_agent(config_path, &cfg, true)?;
    Ok(cfg)
}

/// Text shared by the two settings windows (AppKit and Win32 lay it out separately).
pub mod ui_text {
    pub const TITLE: &str = "Optiman の設定";
    pub const EFFORT: &str = "圧縮率";
    pub const EFFORT_LOW: &str = "弱";
    pub const EFFORT_HIGH: &str = "強";
    pub const EFFORT_HINT: &str = "速度と容量が変わります ※画質は同じ";
    pub const QUALITY: &str = "画質";
    pub const INCLUDE: &str = "変換するフォルダ (サブフォルダも対象)";
    pub const EXCLUDE: &str = "除外するフォルダ (この中は変換しません)";
    pub const AUTO_UPDATE: &str = "自動でアップデートする";
    pub const TRAY_ICON: &str = if cfg!(target_os = "macos") {
        "メニューバーにアイコンを表示する"
    } else {
        "タスクトレイにアイコンを表示する"
    };
    pub const ADD: &str = "追加…";
    pub const REMOVE: &str = "削除";
    pub const CANCEL: &str = "キャンセル";
    pub const SAVE: &str = "保存";
    pub const SAVED: &str = "保存しました";
    pub const SAVED_DETAIL: &str = "設定を反映しました。フォルダに写真が追加されると自動で JPEG XL に変換されます。";
    pub const SAVE_FAILED: &str = "保存できませんでした";
}

/// The settings in one line: quality, effort, originals, existing photos and updates.
pub fn summary(cfg: &Config) -> String {
    format!(
        "{} / 既存の写真: {} / 自動アップデート: {}",
        encoding_summary(cfg),
        if cfg.scan_existing { "変換する" } else { "新規のみ" },
        if cfg.auto_update { "オン" } else { "オフ" }
    )
}

/// How photos are encoded and what happens to the originals.
pub fn encoding_summary(cfg: &Config) -> String {
    format!("画質: {} / 圧縮率: {} (1〜9) / 元ファイル: {}", mode_label(cfg), cfg.effort, cfg.original.describe())
}

/// One line describing how photos are encoded.
pub fn mode_label(cfg: &Config) -> String {
    match cfg.quality {
        Quality::Lossless => Quality::Lossless.name().into(),
        q => format!("{} (非可逆、JPEG は劣化なし)", q.name()),
    }
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
    fn mode_labels() {
        assert_eq!(mode_label(&Config::default()), "最高 (劣化なし)");
        assert_eq!(
            mode_label(&Config { quality: Quality::Medium, ..Config::default() }),
            "中 (非可逆、JPEG は劣化なし)"
        );
    }
}
