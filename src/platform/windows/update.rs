//! Updates through the setup program, which installs per user (no UAC prompt), stops the agent,
//! replaces the files, keeps the settings and starts the new agent. The resident agent checks
//! from a thread that sleeps between checks.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use super::spawn_detached;
use crate::platform::AgentSpec;
use crate::update::{self, Version};

const SETUP: &str = "Optiman-Setup.exe";
/// The first check waits until login has settled and the network is up.
const FIRST_CHECK: Duration = Duration::from_secs(10 * 60);
/// How often the thread wakes to see whether a day has passed since the last check.
const POLL: Duration = Duration::from_secs(60 * 60);

pub fn curl_command() -> Command {
    let system = std::env::var_os("SystemRoot").map(|root| PathBuf::from(root).join(r"System32\curl.exe"));
    let mut curl = Command::new(system.filter(|p| p.exists()).unwrap_or_else(|| "curl.exe".into()));
    curl.creation_flags(CREATE_NO_WINDOW.0);
    curl
}

/// Starts the update thread when automatic updates are on and the setup program installed us.
pub fn on_agent_start(spec: &AgentSpec) {
    if !spec.auto_update || !can_install() {
        return;
    }
    let config = spec.config.clone();
    let spawned = thread::Builder::new().name("update".into()).spawn(move || {
        thread::sleep(FIRST_CHECK);
        loop {
            update::log_result(update::check(&config, true));
            thread::sleep(POLL);
        }
    });
    if let Err(e) = spawned {
        crate::warn!("アップデートの確認を始められません: {e}");
    }
}

/// Whether the setup program installed us: it leaves its uninstaller next to the executables.
/// A copy unpacked from the zip has none and is updated by hand.
pub fn can_install() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.with_file_name("unins000.exe").exists())
}

/// Downloads the setup program, checks it against the release's SHA256SUMS.txt and starts it.
/// `quiet` (the scheduled run) shows nothing; otherwise the setup program shows its progress.
pub fn install(latest: Version, quiet: bool) -> Result<String> {
    if !can_install() {
        bail!(
            "自動で更新できるのはインストーラーで入れた場合だけです。新しい版の zip をダウンロードして置き換えてください"
        );
    }
    let sums = update::curl(&["-fsSL", "--max-time", "60", &update::asset_url(latest, "SHA256SUMS.txt")])?;
    let expected = expected_sha256(&String::from_utf8_lossy(&sums), SETUP)
        .with_context(|| format!("SHA256SUMS.txt に {SETUP} がありません"))?;
    let dir = std::env::temp_dir().join("optiman-update");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).with_context(|| format!("作成できません: {}", dir.display()))?;
    let setup = dir.join(SETUP);
    update::curl(&["-fsSL", "--max-time", "900", "-o", &setup.to_string_lossy(), &update::asset_url(latest, SETUP)])?;
    if hex(&sha256(&fs::read(&setup)?)?) != expected {
        let _ = fs::remove_file(&setup);
        bail!("ダウンロードしたインストーラーが壊れています (SHA-256 が一致しません)");
    }
    // The JPEG XL viewer was offered at the first install; an update does not ask again.
    let mut command = Command::new(&setup);
    command.args([
        if quiet { "/VERYSILENT" } else { "/SILENT" },
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
        "/MERGETASKS=!jxlviewer",
    ]);
    spawn_detached(&mut command).with_context(|| format!("起動できません: {}", setup.display()))?;
    Ok(format!("v{latest} をインストールしています (終わると監視が新しい版で再開します)"))
}

/// The lowercase hex SHA-256 listed for `file` in a `sha256sum`-style list.
fn expected_sha256(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256(data: &[u8]) -> Result<[u8; 32]> {
    let mut digest = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut digest) }
        .ok()
        .context("SHA-256 を計算できません")?;
    Ok(digest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_setup_checksum() {
        let sums = "cb33  optiman-macos-universal.tar.gz\nBA22  Optiman-Setup.exe\n5c8f *optiman-windows-x64.zip\n";
        assert_eq!(expected_sha256(sums, SETUP).as_deref(), Some("ba22"));
        assert_eq!(expected_sha256(sums, "optiman-windows-x64.zip").as_deref(), Some("5c8f"));
        assert_eq!(expected_sha256(sums, "missing.exe"), None);
    }

    #[test]
    fn sha256_matches_the_standard_vector() {
        assert_eq!(hex(&sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
