//! Moves an installation made under the former name (`optimize-image-folder`, up to v0.2.0) to
//! Optiman: the old agent is stopped and unregistered, the settings, state and logs move to the
//! new folders, and the agent is registered again under the new name.
//!
//! Cheap when there is nothing to do (a few `stat` calls), so it runs before every user command.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::platform::{autostart, paths};
use crate::{APP_NAME, LEGACY_NAME, app};

/// What [`from_legacy`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Migrated {
    pub settings: bool,
    pub agent: bool,
}

impl Migrated {
    pub fn any(self) -> bool {
        self.settings || self.agent
    }
}

/// Migrates the standard installation. A custom config (`--config`, `OPTIMAN_HOME`) is left alone.
pub fn from_legacy() -> Migrated {
    if std::env::var_os("OPTIMAN_HOME").is_some() {
        return Migrated::default();
    }
    let legacy_config = paths::config_dir_for(LEGACY_NAME).join("config.toml");
    let agent = autostart::remove_legacy();
    if agent {
        // Give the old agent a moment to exit and release its files.
        for _ in 0..50 {
            if !legacy_config.exists() || !app::agent_running(&legacy_config) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let settings = move_dir(&paths::config_dir_for(LEGACY_NAME), &paths::config_dir());
    move_dir(&paths::log_root_for(LEGACY_NAME), &paths::log_root_for(APP_NAME));
    if agent && !autostart::is_registered() {
        let config_path = app::default_config_path();
        if let Ok(cfg) = Config::load(&config_path)
            && let Err(e) = app::register_agent(&config_path, &cfg, true)
        {
            eprintln!("自動起動を Optiman の名前で登録し直せませんでした: {e:#}");
        }
    }
    Migrated { settings, agent }
}

/// Renames `from` to `to` when only `from` exists. When that fails (on Windows a file inside may
/// still be open), copies the files at its top level instead and leaves `from` in place.
fn move_dir(from: &Path, to: &Path) -> bool {
    if !from.is_dir() || to.exists() {
        return false;
    }
    if let Some(parent) = to.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::rename(from, to).is_ok() {
        return true;
    }
    let Ok(entries) = fs::read_dir(from) else {
        return false;
    };
    if fs::create_dir_all(to).is_err() {
        return false;
    }
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = fs::copy(entry.path(), to.join(entry.file_name()));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_only_when_the_new_folder_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("optimize-image-folder");
        let new = dir.path().join("optiman");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("config.toml"), "effort = 3\n").unwrap();

        assert!(move_dir(&old, &new));
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(new.join("config.toml")).unwrap(), "effort = 3\n");

        fs::create_dir(&old).unwrap();
        fs::write(old.join("config.toml"), "effort = 9\n").unwrap();
        assert!(!move_dir(&old, &new), "an existing new folder is never overwritten");
        assert_eq!(fs::read_to_string(new.join("config.toml")).unwrap(), "effort = 3\n");
    }

    #[test]
    fn nothing_to_move() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!move_dir(&dir.path().join("missing"), &dir.path().join("optiman")));
    }
}
