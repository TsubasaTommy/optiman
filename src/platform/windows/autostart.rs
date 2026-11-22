//! Per-user autostart through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. The agent
//! is a GUI-subsystem executable (no console window) and reads the folders from the config.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteValueW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

use super::{reg_string, spawn_detached, wide};

use crate::platform::AgentSpec;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("optiman");
pub const AGENT_EXE: &str = "optiman-agent.exe";
/// Logs written by the system for the agent (none on Windows).
pub const LOG_FILES: &[&str] = &[];

/// The windowless agent shipped next to the CLI (or this executable, if it is the agent).
pub fn agent_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("実行ファイルのパスを取得できません")?;
    let agent = exe.with_file_name(AGENT_EXE);
    if !agent.exists() {
        bail!("{AGENT_EXE} が見つかりません ({} と同じフォルダに置いてください)", exe.display());
    }
    Ok(agent)
}

fn command_line(exe: &Path, config: &Path) -> String {
    format!("\"{}\" --config \"{}\"", exe.display(), config.display())
}

/// Splits the command line we wrote ("exe" --config "path").
fn parse_command_line(s: &str) -> Option<(PathBuf, PathBuf)> {
    let mut parts = s.split('"').filter(|p| !p.trim().is_empty());
    let exe = parts.next()?;
    let flag = parts.next()?.trim();
    let config = parts.next()?;
    (flag == "--config").then(|| (PathBuf::from(exe), PathBuf::from(config)))
}

/// Opens the Run key, creating it if needed: a fresh profile may not have one yet.
fn with_run_key<T>(f: impl FnOnce(HKEY) -> Result<T>) -> Result<T> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    }
    .ok()
    .context("レジストリ (Run キー) を開けません")?;
    let _close = std::mem::DropGuard::new(key, |k| unsafe {
        let _ = RegCloseKey(k);
    });
    f(key)
}

fn registered_command() -> Option<String> {
    reg_string(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME)
}

pub fn is_registered() -> bool {
    registered_command().is_some()
}

/// Writes the Run entry and starts the agent if asked (the caller stops a running one first).
pub fn install(spec: &AgentSpec, start_now: bool) -> Result<()> {
    std::fs::create_dir_all(&spec.log_dir).ok();
    let cmd = command_line(&spec.exe, &spec.config);
    let data: Vec<u8> = wide(&cmd).iter().flat_map(|u| u.to_le_bytes()).collect();
    with_run_key(|key| {
        unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(&data)) }.ok().context("自動起動を登録できません")
    })?;
    if start_now {
        start()?;
    }
    Ok(())
}

pub fn start() -> Result<()> {
    let cmd = registered_command().context("自動起動が未登録です。先に `install` を実行してください")?;
    let (exe, config) = parse_command_line(&cmd).context("登録内容を解釈できません")?;
    spawn_detached(Command::new(&exe).arg("--config").arg(&config))
        .with_context(|| format!("起動できません: {}", exe.display()))?;
    Ok(())
}

/// Asks the registered agent to exit through its control pipe.
pub fn stop() -> Result<()> {
    if let Some((_, config)) = registered_command().as_deref().and_then(parse_command_line) {
        super::events::request_stop(&config);
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    stop()?;
    with_run_key(|key| {
        let r = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        if r.is_err() && r != ERROR_FILE_NOT_FOUND {
            bail!("自動起動を解除できません ({r:?})");
        }
        Ok(())
    })
}

pub fn describe() -> String {
    format!("HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\{}", crate::APP_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_roundtrip() {
        let exe = Path::new(r"C:\Users\a b\AppData\Local\Programs\Optiman\optiman-agent.exe");
        let cfg = Path::new(r"C:\Users\a b\AppData\Roaming\optiman\config.toml");
        let (e, c) = parse_command_line(&command_line(exe, cfg)).unwrap();
        assert_eq!(e, exe);
        assert_eq!(c, cfg);
    }
}
