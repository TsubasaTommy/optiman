//! TOML configuration.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::platform;

/// What to do with the original photo after a verified conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OriginalPolicy {
    /// Delete permanently.
    Delete,
    /// Move to the OS trash / recycle bin.
    Trash,
    /// Keep the original next to the AVIF.
    Keep,
}

impl OriginalPolicy {
    pub fn describe(self) -> &'static str {
        match self {
            OriginalPolicy::Delete => "完全に削除",
            OriginalPolicy::Trash => "ゴミ箱へ移動",
            OriginalPolicy::Keep => "そのまま残す",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum EncoderChoice {
    /// Best compression: rav1e (about 40% smaller than ImageIO at the same quality).
    Auto,
    /// OS encoder (macOS ImageIO): several times faster, larger files. Falls back to rav1e.
    Native,
    /// Built-in rav1e encoder.
    Rav1e,
}

pub const DEFAULT_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "bmp", "webp"];

/// RAW formats are never converted: re-encoding destroys sensor data.
pub const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "raw", "rwl",
    "3fr", "erf", "kdc", "mrw", "x3f", "iiq",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Folders to watch (recursively). `~` is expanded.
    pub folders: Vec<String>,
    pub original: OriginalPolicy,
    /// Convert photos that already exist when the agent starts (chosen at install time).
    pub scan_existing: bool,
    /// 1-100.
    pub quality: u8,
    /// rav1e speed preset, 1 (slowest/best) - 10 (fastest).
    pub speed: u8,
    pub encoder: EncoderChoice,
    /// Parallel encodes. 0 = automatic.
    pub jobs: usize,
    /// Seconds a file must stay unchanged before it is converted.
    pub settle_seconds: u64,
    /// Keep the original (and drop the AVIF) when the AVIF would not be smaller.
    pub skip_if_larger: bool,
    pub extensions: Vec<String>,
    /// Extra glob patterns (matched against each path component) to ignore.
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            folders: Vec::new(),
            original: OriginalPolicy::Delete,
            scan_existing: false,
            quality: 85,
            speed: 6,
            encoder: EncoderChoice::Auto,
            jobs: 0,
            settle_seconds: 2,
            skip_if_larger: true,
            extensions: DEFAULT_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
            exclude: Vec::new(),
        }
    }
}

const HEADER: &str = "\
# optimize-image-folder の設定ファイル
# 変更したら `optimize-image-folder restart` で反映されます。
#
# folders        監視するフォルダ (サブフォルダも対象)
# original       変換後の元ファイル: \"delete\"(完全削除) / \"trash\"(ゴミ箱) / \"keep\"(残す)
# scan_existing  起動時に既存の写真も変換するか
# quality        画質 1-100
# speed          rav1e の速度 1(高画質・遅い)-10(速い)
# encoder        \"auto\"(=rav1e, 高圧縮) / \"native\"(OS のエンコーダ, 高速だがサイズ大) / \"rav1e\"
# jobs           同時変換数 (0 = 自動)
# settle_seconds ファイルの書き込みが止まってから変換するまでの秒数
# skip_if_larger AVIF の方が大きくなる場合は元ファイルを残す
# extensions     対象の拡張子 (RAW は指定できません)
# exclude        除外するフォルダ/ファイル名のパターン (* と ? が使えます)

";

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = fs::read_to_string(path).with_context(|| format!("設定ファイルを読めません: {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("設定ファイルの形式が不正です: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let body = toml::to_string_pretty(self).context("設定のシリアライズに失敗")?;
        write_atomic(path, format!("{HEADER}{body}").as_bytes())
    }

    pub fn validate(&self) -> Result<()> {
        if !(1..=100).contains(&self.quality) {
            bail!("quality は 1〜100 で指定してください (現在 {})", self.quality);
        }
        if !(1..=10).contains(&self.speed) {
            bail!("speed は 1〜10 で指定してください (現在 {})", self.speed);
        }
        for ext in &self.extensions {
            let e = ext.trim_start_matches('.').to_ascii_lowercase();
            if RAW_EXTENSIONS.contains(&e.as_str()) {
                bail!("RAW 形式 (.{e}) は変換対象にできません");
            }
            if e == "avif" {
                bail!(".avif は変換対象にできません");
            }
        }
        Ok(())
    }

    pub fn folder_paths(&self) -> Vec<PathBuf> {
        self.folders.iter().map(|f| expand_tilde(f)).collect()
    }

    pub fn effective_jobs(&self) -> usize {
        if self.jobs > 0 {
            return self.jobs;
        }
        // Each rav1e encode of a 12 MP photo peaks around 0.7 GB, so stay modest by default.
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        (cpus / 2).clamp(1, 2)
    }
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return platform::paths::home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return platform::paths::home_dir().join(rest);
    }
    PathBuf::from(s)
}

/// Store a path with `~` when it is under the home directory, for a portable config file.
pub fn contract_tilde(p: &Path) -> String {
    let home = platform::paths::home_dir();
    if let Ok(rest) = p.strip_prefix(&home) {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        let sep = std::path::MAIN_SEPARATOR;
        return format!("~{sep}{}", rest.display());
    }
    p.display().to_string()
}

/// Refuse folders where deleting originals would be catastrophic.
pub fn check_folder_safe(path: &Path) -> Result<()> {
    let canon =
        platform::paths::canonical(path).with_context(|| format!("フォルダが見つかりません: {}", path.display()))?;
    if !canon.is_dir() {
        bail!("フォルダではありません: {}", path.display());
    }
    if canon.parent().is_none() {
        bail!("ドライブ/ボリュームのルートは監視できません: {}", path.display());
    }
    let home = platform::paths::canonical(&platform::paths::home_dir()).unwrap_or_else(|_| platform::paths::home_dir());
    if canon == home {
        bail!("ホームフォルダ全体は監視できません。写真フォルダを指定してください: {}", path.display());
    }
    if home.starts_with(&canon) {
        bail!("ホームフォルダを含むフォルダは監視できません: {}", path.display());
    }
    for sys in platform::paths::system_dirs() {
        let sys = platform::paths::canonical(&sys).unwrap_or(sys);
        if canon.starts_with(&sys) {
            bail!("システムフォルダは監視できません: {}", path.display());
        }
    }
    Ok(())
}

/// Write via a temporary file + rename so readers never see a partial file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("フォルダを作成できません: {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data).with_context(|| format!("書き込みに失敗: {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("書き込みに失敗: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config { folders: vec!["~/Pictures".into()], scan_existing: true, ..Config::default() };
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(cfg, loaded);

        fs::write(&path, "folders = [\"/x\"]\n").unwrap();
        let partial = Config::load(&path).unwrap();
        assert_eq!(partial.original, OriginalPolicy::Delete);
        assert_eq!(partial.quality, 85);
        assert!(!partial.scan_existing);
    }

    #[test]
    fn rejects_raw_and_bad_ranges() {
        let mut cfg = Config::default();
        cfg.extensions.push("CR3".into());
        assert!(cfg.validate().is_err());
        let cfg = Config { quality: 0, ..Config::default() };
        assert!(cfg.validate().is_err());
        let cfg = Config { speed: 11, ..Config::default() };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "qualty = 3\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn tilde_expansion() {
        let home = platform::paths::home_dir();
        assert_eq!(expand_tilde("~/Pictures"), home.join("Pictures"));
        assert_eq!(contract_tilde(&home.join("Pictures")), format!("~{}Pictures", std::path::MAIN_SEPARATOR));
    }

    #[test]
    fn unsafe_folders_rejected() {
        assert!(check_folder_safe(&platform::paths::home_dir()).is_err());
        let root = if cfg!(windows) { PathBuf::from("C:\\") } else { PathBuf::from("/") };
        assert!(check_folder_safe(&root).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(check_folder_safe(dir.path()).is_ok());
    }
}
