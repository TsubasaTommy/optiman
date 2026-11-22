//! Automatic updates. A newer GitHub release is installed the way the app was installed:
//! `brew upgrade` on macOS, the setup program (per user, silent, no UAC prompt) on Windows.
//! Automatic checks run about once a day, from a daily launchd job (macOS) or a sleeping thread
//! of the resident agent (Windows); between checks nothing runs for them.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::platform;

pub const RELEASES: &str = "https://github.com/TsubasaTommy/optiman/releases";
/// Minimum time between automatic checks: a little under a day, so a daily job never skips one.
const CHECK_INTERVAL: Duration = Duration::from_secs(20 * 60 * 60);
const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// A release version, `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u32; 3]);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok());
        let v = Version([parts.next()??, parts.next()??, parts.next()??]);
        parts.next().is_none().then_some(v)
    }

    pub fn current() -> Version {
        Version::parse(crate::VERSION).expect("the package version is major.minor.patch")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.0;
        write!(f, "{major}.{minor}.{patch}")
    }
}

/// Records the time of the last check (its modification time), next to the config.
pub fn stamp_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("update-check")
}

/// Looks for a newer release and starts installing it. `auto` is the scheduled run: it does
/// nothing (`Ok(None)`) when this copy cannot update itself, automatic updates are off or the
/// last check was recent.
pub fn check(config_path: &Path, auto: bool) -> Result<Option<String>> {
    let stamp = stamp_path(config_path);
    if auto
        && (!Config::load(config_path).unwrap_or_default().auto_update
            || !platform::update::can_install()
            || !due(&stamp))
    {
        return Ok(None);
    }
    let latest = latest()?;
    let _ = fs::write(&stamp, latest.to_string());
    let current = Version::current();
    if latest <= current {
        return Ok(Some(format!("最新版です (v{current})")));
    }
    platform::update::install(latest, auto).map(Some)
}

/// Logs what a scheduled check did.
pub fn log_result(result: Result<Option<String>>) {
    match result {
        Ok(Some(message)) => crate::info!("アップデート: {message}"),
        Ok(None) => {}
        Err(e) => crate::warn!("アップデートを確認できません: {e:#}"),
    }
}

fn due(stamp: &Path) -> bool {
    let age = fs::metadata(stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    age.is_none_or(|age| age >= CHECK_INTERVAL)
}

/// The newest published release: GitHub redirects `releases/latest` to its tag.
fn latest() -> Result<Version> {
    let latest = format!("{RELEASES}/latest");
    let out = curl(&["-fsS", "--max-time", "30", "-o", NULL_DEVICE, "-w", "%{redirect_url}", &latest])?;
    let url = String::from_utf8_lossy(&out);
    url.rsplit_once("/tag/")
        .and_then(|(_, tag)| Version::parse(tag))
        .with_context(|| format!("最新版を判別できません ({url})"))
}

/// A file attached to a release.
pub fn asset_url(version: Version, file: &str) -> String {
    format!("{RELEASES}/download/v{version}/{file}")
}

/// Runs the curl that ships with the OS (macOS, Windows 10 1803 and later) and returns what it
/// printed. `-q` ignores the user's curlrc.
pub fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let out = platform::update::curl_command()
        .arg("-q")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("curl を実行できません")?;
    if !out.status.success() {
        bail!("通信できません ({})", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(Version::parse("v0.4.1"), Some(Version([0, 4, 1])));
        assert_eq!(Version::parse("1.10.0").map(|v| v.to_string()).as_deref(), Some("1.10.0"));
        assert!(Version::parse("1.10.0") > Version::parse("1.9.9"));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "v1.2.3-beta"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::current().to_string(), crate::VERSION);
    }

    #[test]
    fn checks_are_spaced_out() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = stamp_path(&dir.path().join("config.toml"));
        assert!(due(&stamp), "never checked");
        fs::write(&stamp, "").unwrap();
        assert!(!due(&stamp), "just checked");
        let old = std::time::SystemTime::now() - CHECK_INTERVAL;
        fs::File::options().write(true).open(&stamp).unwrap().set_modified(old).unwrap();
        assert!(due(&stamp));
    }

    #[test]
    fn scheduled_checks_respect_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        Config { auto_update: false, ..Config::default() }.save(&config).unwrap();
        assert_eq!(check(&config, true).unwrap(), None);
        assert!(!stamp_path(&config).exists(), "no network access when switched off");
    }
}
