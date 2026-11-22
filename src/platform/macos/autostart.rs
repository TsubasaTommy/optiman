//! launchd user agent (`~/Library/LaunchAgents`) that is started **on demand**:
//! `LaunchEvents` / `com.apple.fsevents.matching` makes launchd launch the agent when anything
//! below a watched folder changes, so no process of ours exists while nothing happens.
//! Also installs the settings app bundle (`~/Applications/Optiman.app`).

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::paths::home_dir;

pub const LABEL: &str = "io.github.tsubasatommy.optiman";
pub const APP_NAME: &str = crate::DISPLAY_NAME;
const BUNDLE_ID: &str = "io.github.tsubasatommy.optiman.settings";

/// What the agent needs to know to be registered.
pub struct AgentSpec<'a> {
    pub exe: &'a Path,
    pub config: &'a Path,
    pub log_dir: &'a Path,
    pub folders: &'a [PathBuf],
}

pub fn plist_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

pub fn app_bundle_path() -> PathBuf {
    home_dir().join("Applications").join(format!("{APP_NAME}.app"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

/// The binary launchd should run. A Homebrew Cellar path changes on every upgrade, so use the
/// stable `<prefix>/bin` link instead when there is one.
pub fn agent_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("実行ファイルのパスを取得できません")?;
    Ok(stable_path(&exe))
}

fn stable_path(exe: &Path) -> PathBuf {
    let s = exe.to_string_lossy();
    if let Some((prefix, _)) = s.split_once("/Cellar/")
        && let Some(name) = exe.file_name()
    {
        let linked = Path::new(prefix).join("bin").join(name);
        if linked.exists() {
            return linked;
        }
    }
    exe.to_path_buf()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn plist(spec: &AgentSpec<'_>) -> String {
    let exe = xml_escape(&spec.exe.to_string_lossy());
    let config = xml_escape(&spec.config.to_string_lossy());
    let log = xml_escape(&spec.log_dir.join("launchd.log").to_string_lossy());
    let events: String = spec
        .folders
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                "\t\t\t<key>folder{i}</key>\n\t\t\t<dict>\n\t\t\t\t<key>Path</key>\n\t\t\t\t<string>{}</string>\n\t\t\t</dict>\n",
                xml_escape(&f.to_string_lossy())
            )
        })
        .collect();
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
		<string>--on-demand</string>
	</array>
	<key>LaunchEvents</key>
	<dict>
		<key>com.apple.fsevents.matching</key>
		<dict>
{events}		</dict>
	</dict>
	<key>RunAtLoad</key>
	<true/>
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

/// Writes (or rewrites) the LaunchAgent and the settings app, and (re)loads the agent.
pub fn install(spec: &AgentSpec<'_>, start_now: bool) -> Result<()> {
    let path = plist_path();
    fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).context("LaunchAgents フォルダを作成できません")?;
    fs::create_dir_all(spec.log_dir).ok();
    fs::write(&path, plist(spec)).with_context(|| format!("書き込めません: {}", path.display()))?;
    install_app_bundle(spec.exe)?;
    let _ = launchctl(&["bootout", &service()]);
    if start_now {
        start()?;
    }
    Ok(())
}

/// Loads the agent; launchd runs it once now (RunAtLoad) and then only on folder changes.
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
    let app = app_bundle_path();
    if app.join("Contents/Info.plist").exists() {
        fs::remove_dir_all(&app).with_context(|| format!("削除できません: {}", app.display()))?;
    }
    Ok(())
}

pub fn describe() -> String {
    format!("LaunchAgent (フォルダの変更時だけ起動): {}", plist_path().display())
}

/// A minimal app bundle whose executable is a symlink to the CLI; launched from the bundle, the
/// binary opens the settings window. Created locally, so Gatekeeper does not quarantine it.
fn install_app_bundle(exe: &Path) -> Result<()> {
    let app = app_bundle_path();
    let macos = app.join("Contents/MacOS");
    fs::create_dir_all(&macos).with_context(|| format!("作成できません: {}", macos.display()))?;
    let info = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>optiman</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleName</key>
	<string>{APP_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>{APP_NAME}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
"#,
        crate::VERSION
    );
    fs::write(app.join("Contents/Info.plist"), info)?;
    let link = macos.join("optiman");
    let _ = fs::remove_file(&link);
    symlink(exe, &link).with_context(|| format!("リンクを作成できません: {}", link.display()))?;
    Ok(())
}

/// True when this process was started from the settings app bundle.
pub fn launched_from_app_bundle() -> bool {
    std::env::args_os().next().is_some_and(|a| a.to_string_lossy().contains(".app/Contents/MacOS/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_has_launch_events_per_folder_and_escapes() {
        let folders = [PathBuf::from("/Users/a/Pictures"), PathBuf::from("/Volumes/x&y/Photos")];
        let spec = AgentSpec {
            exe: Path::new("/opt/homebrew/bin/optiman"),
            config: Path::new("/c<d>/config.toml"),
            log_dir: Path::new("/logs"),
            folders: &folders,
        };
        let p = plist(&spec);
        assert!(p.contains("<key>com.apple.fsevents.matching</key>"));
        assert!(p.contains("<string>/Users/a/Pictures</string>"));
        assert!(p.contains("<string>/Volumes/x&amp;y/Photos</string>"));
        assert!(p.contains("<string>/c&lt;d&gt;/config.toml</string>"));
        assert!(p.contains("<string>--on-demand</string>"));
        assert!(!p.contains("KeepAlive"));
    }

    #[test]
    fn cellar_paths_map_to_the_stable_link() {
        let p = Path::new("/nonexistent-prefix/Cellar/optiman/0.2.0/bin/optiman");
        assert_eq!(stable_path(p), p, "falls back when the link does not exist");
    }
}
