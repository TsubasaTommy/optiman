//! Updates through Homebrew, which owns the installed files. A daily launchd job (see
//! `autostart`) runs `optiman update --auto`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::update::Version;

const FORMULA: &str = "tsubasatommy/tap/optiman";

pub fn curl_command() -> Command {
    Command::new("/usr/bin/curl")
}

/// Whether Homebrew installed this binary (a copy put in place by hand is updated by hand).
pub fn can_install() -> bool {
    std::env::current_exe().is_ok_and(|exe| homebrew_prefix(&exe).is_some())
}

/// Runs `brew upgrade`. `quiet` (the scheduled run) captures Homebrew's output instead of
/// showing it.
pub fn install(latest: Version, quiet: bool) -> Result<String> {
    let exe = std::env::current_exe().context("実行ファイルのパスを取得できません")?;
    let prefix = homebrew_prefix(&exe)
        .context("自動で更新できるのは Homebrew で入れた場合だけです。新しい版をダウンロードして置き換えてください")?;
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", prefix.join("bin").display());
    for args in [&["update", "--quiet"][..], &["upgrade", FORMULA]] {
        let mut brew = Command::new(prefix.join("bin/brew"));
        brew.args(args)
            .env("PATH", &path)
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("HOMEBREW_NO_ENV_HINTS", "1")
            .stdin(Stdio::null());
        let (ok, detail) = if quiet {
            let out = brew.output().context("brew を実行できません")?;
            (out.status.success(), String::from_utf8_lossy(&out.stderr).trim().to_string())
        } else {
            (brew.status().context("brew を実行できません")?.success(), String::new())
        };
        if !ok {
            bail!("brew {} に失敗しました {detail}", args.join(" "));
        }
    }
    Ok(if prefix.join("Cellar/optiman").join(latest.to_string()).exists() {
        format!("v{latest} に更新しました")
    } else {
        format!("v{latest} はまだ Homebrew に届いていません。次の確認で更新します")
    })
}

/// Homebrew's prefix when Homebrew installed this binary (`<prefix>/Cellar/optiman/<version>/…`).
fn homebrew_prefix(exe: &Path) -> Option<PathBuf> {
    let s = exe.to_string_lossy();
    let (prefix, _) = s.split_once("/Cellar/")?;
    let prefix = PathBuf::from(prefix);
    prefix.join("bin/brew").exists().then_some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_homebrew_installs_update_themselves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/brew"), "").unwrap();
        let exe = dir.path().join("Cellar/optiman/0.5.0/bin/optiman");
        assert_eq!(homebrew_prefix(&exe), Some(dir.path().to_path_buf()));
        assert_eq!(homebrew_prefix(Path::new("/usr/local/bin/optiman")), None, "copied by hand");
        assert_eq!(homebrew_prefix(Path::new("/nonexistent/Cellar/optiman/0.5.0/bin/optiman")), None, "no brew");
    }
}
