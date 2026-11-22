//! launchd user agent (`~/Library/LaunchAgents`) that is started **on demand**:
//! `LaunchEvents` / `com.apple.fsevents.matching` makes launchd launch the agent when anything
//! below a watched folder changes, so no process of ours exists while nothing happens.
//! Also installs the settings app bundle (`~/Applications/Optiman.app`) and a second job that
//! runs the update check once a day (`update --auto`).

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

use super::APP_ICON;
use super::paths::home_dir;
use crate::platform::AgentSpec;
use crate::{APP_NAME, DISPLAY_NAME};

pub const LABEL: &str = "io.github.tsubasatommy.optiman";
const BUNDLE_ID: &str = "io.github.tsubasatommy.optiman.settings";
/// launchd's own output for the agent, next to our log (removed by `uninstall --purge`).
const LAUNCHD_LOG: &str = "launchd.log";
pub const LOG_FILES: &[&str] = &[LAUNCHD_LOG];

pub fn plist_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

pub fn app_bundle_path() -> PathBuf {
    home_dir().join("Applications").join(format!("{DISPLAY_NAME}.app"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

fn update_label() -> String {
    format!("{LABEL}.update")
}

pub fn update_plist_path() -> PathBuf {
    home_dir().join("Library/LaunchAgents").join(format!("{}.plist", update_label()))
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

fn plist(spec: &AgentSpec) -> String {
    let exe = xml_escape(&spec.exe.to_string_lossy());
    let config = xml_escape(&spec.config.to_string_lossy());
    let log = xml_escape(&spec.log_dir.join(LAUNCHD_LOG).to_string_lossy());
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
pub fn install(spec: &AgentSpec, start_now: bool) -> Result<()> {
    let path = plist_path();
    fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).context("LaunchAgents フォルダを作成できません")?;
    fs::create_dir_all(&spec.log_dir).ok();
    fs::write(&path, plist(spec)).with_context(|| format!("書き込めません: {}", path.display()))?;
    install_app_bundle(&app_bundle_path(), &spec.exe)?;
    sync_update_job(spec)?;
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
    remove_update_job()?;
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
fn install_app_bundle(app: &Path, exe: &Path) -> Result<()> {
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    for dir in [&macos, &resources] {
        fs::create_dir_all(dir).with_context(|| format!("作成できません: {}", dir.display()))?;
    }
    fs::write(resources.join("AppIcon.icns"), APP_ICON)?;
    fs::write(app.join("Contents/Info.plist"), info_plist())?;
    let link = macos.join(APP_NAME);
    let _ = fs::remove_file(&link);
    symlink(exe, &link).with_context(|| format!("リンクを作成できません: {}", link.display()))?;
    // Finder and Launchpad notice the new icon when the bundle itself looks modified.
    let _ = fs::File::open(app).and_then(|d| d.set_modified(SystemTime::now()));
    Ok(())
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>{APP_NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleName</key>
	<string>{DISPLAY_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>{DISPLAY_NAME}</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
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
    )
}

/// After an upgrade (`brew upgrade` replaces only the binary), rewrites the settings app and the
/// update job so they follow the binary. Only a registration that runs this binary is touched.
pub fn refresh(spec: &AgentSpec) -> Result<()> {
    let ours = fs::read_to_string(plist_path())
        .is_ok_and(|p| p.contains(&format!("<string>{}</string>", xml_escape(&spec.exe.to_string_lossy()))));
    if !ours {
        return Ok(());
    }
    let app = app_bundle_path();
    if needs_refresh(&app, &spec.exe) {
        install_app_bundle(&app, &spec.exe)?;
    }
    sync_update_job(spec)
}

/// A job that runs `update --auto` at noon and at login (a sleeping Mac runs a missed one when it
/// wakes); the command itself skips the check when the last one was recent.
fn update_plist(spec: &AgentSpec) -> String {
    let exe = xml_escape(&spec.exe.to_string_lossy());
    let config = xml_escape(&spec.config.to_string_lossy());
    let log = xml_escape(&spec.log_dir.join(LAUNCHD_LOG).to_string_lossy());
    let label = update_label();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--config</string>
		<string>{config}</string>
		<string>update</string>
		<string>--auto</string>
	</array>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>12</integer>
		<key>Minute</key>
		<integer>0</integer>
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

/// Registers the update job when automatic updates are on (and Homebrew can apply them), and
/// removes it otherwise.
fn sync_update_job(spec: &AgentSpec) -> Result<()> {
    if !spec.auto_update || !super::update::can_install() {
        return remove_update_job();
    }
    let path = update_plist_path();
    let want = update_plist(spec);
    if fs::read_to_string(&path).ok().as_deref() == Some(want.as_str()) {
        return Ok(());
    }
    fs::write(&path, want).with_context(|| format!("書き込めません: {}", path.display()))?;
    let _ = launchctl(&["bootout", &format!("{}/{}", domain(), update_label())]);
    if !launchctl(&["bootstrap", &domain(), &path.to_string_lossy()])? {
        bail!("launchctl bootstrap に失敗しました (自動アップデート)");
    }
    Ok(())
}

fn remove_update_job() -> Result<()> {
    let path = update_plist_path();
    if path.exists() {
        let _ = launchctl(&["bootout", &format!("{}/{}", domain(), update_label())]);
        fs::remove_file(&path).with_context(|| format!("削除できません: {}", path.display()))?;
    }
    Ok(())
}

fn needs_refresh(app: &Path, exe: &Path) -> bool {
    let ours = fs::read_link(app.join("Contents/MacOS").join(APP_NAME)).is_ok_and(|target| target == exe);
    ours && fs::read_to_string(app.join("Contents/Info.plist")).ok() != Some(info_plist())
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
        let spec = test_spec();
        let p = plist(&spec);
        assert!(p.contains("<key>com.apple.fsevents.matching</key>"));
        assert!(p.contains("<string>/Users/a/Pictures</string>"));
        assert!(p.contains("<string>/Volumes/x&amp;y/Photos</string>"));
        assert!(p.contains("<string>/c&lt;d&gt;/config.toml</string>"));
        assert!(p.contains("<string>--on-demand</string>"));
        assert!(!p.contains("KeepAlive"));
    }

    fn test_spec() -> AgentSpec {
        AgentSpec {
            exe: PathBuf::from("/opt/homebrew/bin/optiman"),
            config: PathBuf::from("/c<d>/config.toml"),
            log_dir: PathBuf::from("/logs"),
            folders: vec![PathBuf::from("/Users/a/Pictures"), PathBuf::from("/Volumes/x&y/Photos")],
            auto_update: true,
        }
    }

    #[test]
    fn update_job_runs_the_scheduled_check_daily() {
        let p = update_plist(&test_spec());
        assert!(p.contains("<string>io.github.tsubasatommy.optiman.update</string>"));
        assert!(p.contains("<string>update</string>\n\t\t<string>--auto</string>"));
        assert!(p.contains("<string>/c&lt;d&gt;/config.toml</string>"));
        assert!(p.contains("<key>StartCalendarInterval</key>"));
        assert!(!p.contains("KeepAlive"));
    }

    #[test]
    fn cellar_paths_map_to_the_stable_link() {
        let p = Path::new("/nonexistent-prefix/Cellar/optiman/0.2.0/bin/optiman");
        assert_eq!(stable_path(p), p, "falls back when the link does not exist");
    }

    #[test]
    fn app_bundle_carries_the_icon_and_follows_upgrades() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Optiman.app");
        let exe = Path::new("/opt/homebrew/bin/optiman");
        install_app_bundle(&app, exe).unwrap();
        assert!(fs::read_to_string(app.join("Contents/Info.plist")).unwrap().contains("<key>CFBundleIconFile</key>"));
        assert_eq!(fs::read(app.join("Contents/Resources/AppIcon.icns")).unwrap(), APP_ICON);
        assert!(!needs_refresh(&app, exe));

        fs::write(app.join("Contents/Info.plist"), "an older version").unwrap();
        assert!(needs_refresh(&app, exe));
        assert!(!needs_refresh(&app, Path::new("/elsewhere/optiman")), "another binary's bundle is left alone");
    }
}
