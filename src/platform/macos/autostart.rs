//! launchd user agent (`~/Library/LaunchAgents`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::paths::home_dir;

pub const LABEL: &str = "io.github.tsubasatommy.optimize-image-folder";

pub fn plist_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

/// The binary launchd should run (the CLI itself, with `run`).
pub fn agent_executable() -> Result<PathBuf> {
    std::env::current_exe().context("実行ファイルのパスを取得できません")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn plist(exe: &Path, config: &Path, log_dir: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    let config = xml_escape(&config.to_string_lossy());
    let log = xml_escape(&log_dir.join("launchd.log").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--config</string>
		<string>{config}</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>60</integer>
	<key>ProcessType</key>
	<string>Background</string>
	<key>LowPriorityIO</key>
	<true/>
	<key>Nice</key>
	<integer>10</integer>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

fn launchctl(args: &[&str]) -> Result<bool> {
    let status = Command::new("/bin/launchctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("launchctl を実行できません")?;
    Ok(status.success())
}

pub fn is_registered() -> bool {
    plist_path().exists()
}

pub fn is_loaded() -> bool {
    launchctl(&["print", &service()]).unwrap_or(false)
}

/// Writes the LaunchAgent and (re)loads it.
pub fn install(exe: &Path, config: &Path, log_dir: &Path, start_now: bool) -> Result<()> {
    let path = plist_path();
    fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).context("LaunchAgents フォルダを作成できません")?;
    fs::create_dir_all(log_dir).ok();
    fs::write(&path, plist(exe, config, log_dir)).with_context(|| format!("書き込めません: {}", path.display()))?;
    let _ = launchctl(&["bootout", &service()]);
    if start_now {
        start()?;
    }
    Ok(())
}

pub fn start() -> Result<()> {
    let path = plist_path();
    if !path.exists() {
        bail!("自動起動が未登録です。先に `install` を実行してください");
    }
    if is_loaded() {
        launchctl(&["kickstart", &service()])?;
        return Ok(());
    }
    if !launchctl(&["bootstrap", &domain(), &path.to_string_lossy()])? {
        bail!("launchctl bootstrap に失敗しました");
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    if is_loaded() {
        launchctl(&["bootout", &service()])?;
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    stop()?;
    let path = plist_path();
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("削除できません: {}", path.display()))?;
    }
    Ok(())
}

pub fn describe() -> String {
    format!("LaunchAgent: {}", plist_path().display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_paths() {
        let p = plist(Path::new("/a&b/oif"), Path::new("/c<d>/config.toml"), Path::new("/logs"));
        assert!(p.contains("<string>/a&amp;b/oif</string>"));
        assert!(p.contains("<string>/c&lt;d&gt;/config.toml</string>"));
        assert!(p.contains("<string>Background</string>"));
    }
}
