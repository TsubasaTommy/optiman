//! TOML configuration.

use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::platform;

/// What to do with the original photo after a verified conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OriginalPolicy {
    /// 検証に成功したら完全に削除する
    Delete,
    /// 検証に成功したらゴミ箱へ移動する
    Trash,
    /// JPEG XL の隣にそのまま残す
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

/// Compression strength = libjxl effort. Higher is smaller and slower; quality is never affected.
pub const EFFORT_RANGE: RangeInclusive<u8> = 1..=9;
pub const DEFAULT_EFFORT: u8 = 7;

/// Image quality for PNG, TIFF, BMP and lossless WebP. JPEG is always recompressed losslessly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// 最高: 劣化なし (元とまったく同じ。いつでも元に戻せる)
    #[default]
    Lossless,
    /// 高: 見た目ではほぼ区別できない
    High,
    /// 中: 拡大すると、細かい模様がわずかになめらかに見える
    Medium,
    /// 低: 細かい模様 (岩肌や木の葉など) がなめらかになる。拡大するとはっきり分かる
    Low,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Lossless, Quality::High, Quality::Medium, Quality::Low];

    /// libjxl's JPEG-style quality for a lossy encode (`None` = lossless). All three stay inside
    /// libjxl's recommended range (distance 1.0, 1.9 and 2.8).
    pub fn jxl_quality(self) -> Option<u8> {
        match self {
            Quality::Lossless => None,
            Quality::High => Some(90),
            Quality::Medium => Some(80),
            Quality::Low => Some(70),
        }
    }

    /// Name shown to people.
    pub fn name(self) -> &'static str {
        match self {
            Quality::Lossless => "最高 (劣化なし)",
            Quality::High => "高",
            Quality::Medium => "中",
            Quality::Low => "低",
        }
    }

    /// One line explaining what the choice means.
    pub fn describe(self) -> &'static str {
        match self {
            Quality::Lossless => "元とまったく同じ画質で、いつでも元に戻せます。",
            Quality::High => "見た目ではほぼ区別できません。元の画質には戻せません。",
            Quality::Medium => "拡大すると、細かい模様がわずかになめらかに見えます。元の画質には戻せません。",
            Quality::Low => {
                "岩肌や木の葉のような細かい模様がなめらかになり、拡大するとはっきり分かります。元の画質には戻せません。"
            }
        }
    }
}

/// Formats that convert without loss. (HEIC and lossy WebP are lossy sources and are skipped.)
pub const DEFAULT_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp"];

/// RAW formats are never converted: re-encoding destroys sensor data.
pub const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "raw", "rwl",
    "3fr", "erf", "kdc", "mrw", "x3f", "iiq",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Folders to watch, recursively (whitelist). `~` is expanded.
    pub folders: Vec<String>,
    /// Folders inside `folders` that are never touched (blacklist).
    pub exclude_folders: Vec<String>,
    /// Compression strength 1-9 (libjxl effort). Changes size and time, never quality.
    pub effort: u8,
    /// Image quality for PNG/TIFF/BMP/lossless WebP. JPEG stays lossless either way.
    pub quality: Quality,
    pub original: OriginalPolicy,
    /// Convert photos that already exist when the agent starts.
    pub scan_existing: bool,
    /// Parallel encoder processes. 0 = automatic (1).
    pub jobs: usize,
    /// Seconds a file must stay unchanged before it is converted.
    pub settle_seconds: u64,
    /// Keep the original (and drop the JPEG XL) when the JPEG XL would not be smaller.
    pub skip_if_larger: bool,
    pub extensions: Vec<String>,
    /// Extra glob patterns (matched against each path component) to ignore.
    pub exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            folders: vec![contract_tilde(&platform::paths::pictures_dir())],
            exclude_folders: Vec::new(),
            effort: DEFAULT_EFFORT,
            quality: Quality::Lossless,
            original: OriginalPolicy::Delete,
            scan_existing: false,
            jobs: 0,
            settle_seconds: 2,
            skip_if_larger: true,
            extensions: DEFAULT_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
            exclude: Vec::new(),
        }
    }
}

const HEADER: &str = "\
# Optiman の設定ファイル (設定画面からも変更できます)
# 手で編集したら `optiman restart` で反映されます。
#
# folders         変換するフォルダ (サブフォルダも対象)
# exclude_folders 変換しないフォルダ (folders の中で除外したい場所)
# effort          圧縮の強さ 1(速い)-9(小さい) 既定 7。変わるのはサイズと時間だけで、画質は変わらない
# quality         PNG、TIFF、BMP、ロスレス WebP の画質: \"lossless\"(最高・劣化なし) / \"high\"(高) / \"medium\"(中) / \"low\"(低)
#                 高・中・低は非可逆で、元の画質には戻せない。JPEG はどれでも劣化なし
# original        変換後の元ファイル: \"delete\"(完全削除) / \"trash\"(ゴミ箱) / \"keep\"(残す)
# scan_existing   起動時に既存の写真も変換するか
# jobs            同時変換数 (0 = 自動)
# settle_seconds  ファイルの書き込みが止まってから変換するまでの秒数
# skip_if_larger  JPEG XL の方が大きくなる場合は元ファイルを残す
# extensions      対象の拡張子 (RAW は指定できません)
# exclude         除外するフォルダ/ファイル名のパターン (* と ? が使えます)

";

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = fs::read_to_string(path).with_context(|| format!("設定ファイルを読めません: {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("設定ファイルの形式が不正です: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Loads the file, or returns (and writes) the defaults when it does not exist yet.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if path.exists() {
            return Config::load(path);
        }
        let cfg = Config::default();
        cfg.save(path)?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let body = toml::to_string_pretty(self).context("設定のシリアライズに失敗")?;
        write_atomic(path, format!("{HEADER}{body}").as_bytes())
    }

    pub fn validate(&self) -> Result<()> {
        if !EFFORT_RANGE.contains(&self.effort) {
            bail!(
                "effort (圧縮の強さ) は {}〜{} で指定してください (現在 {})",
                EFFORT_RANGE.start(),
                EFFORT_RANGE.end(),
                self.effort
            );
        }
        for ext in &self.extensions {
            let e = ext.trim_start_matches('.').to_ascii_lowercase();
            if RAW_EXTENSIONS.contains(&e.as_str()) {
                bail!("RAW 形式 (.{e}) は変換対象にできません");
            }
            if matches!(e.as_str(), "jxl" | "heic" | "heif" | "avif") {
                bail!(".{e} は変換対象にできません (劣化なしに変換できない、または変換済みの形式)");
            }
        }
        Ok(())
    }

    pub fn folder_paths(&self) -> Vec<PathBuf> {
        self.folders.iter().map(|f| expand_tilde(f)).collect()
    }

    pub fn exclude_folder_paths(&self) -> Vec<PathBuf> {
        self.exclude_folders.iter().map(|f| expand_tilde(f)).collect()
    }

    /// One encoder process at a time by default: libaom already uses several threads, and a
    /// single process keeps peak memory low.
    pub fn effective_jobs(&self) -> usize {
        self.jobs.max(1)
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
    match p.strip_prefix(&home) {
        Ok(rest) if rest.is_empty() => "~".into(),
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => p.display().to_string(),
    }
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
        let cfg = Config {
            folders: vec!["~/Pictures".into()],
            exclude_folders: vec!["~/Pictures/private".into()],
            scan_existing: true,
            ..Config::default()
        };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);

        fs::write(&path, "folders = [\"/x\"]\n").unwrap();
        let partial = Config::load(&path).unwrap();
        assert_eq!(partial.original, OriginalPolicy::Delete);
        assert_eq!(partial.effort, DEFAULT_EFFORT);
        assert!(partial.exclude_folders.is_empty());
        assert!(!partial.scan_existing);
    }

    #[test]
    fn default_watches_the_pictures_folder() {
        let cfg = Config::default();
        assert_eq!(cfg.folder_paths(), vec![platform::paths::pictures_dir()]);
        assert_eq!(cfg.effort, 7);
    }

    #[test]
    fn effort_is_limited_to_1_through_9() {
        for (e, ok) in [(0, false), (1, true), (7, true), (9, true), (10, false)] {
            assert_eq!(Config { effort: e, ..Config::default() }.validate().is_ok(), ok, "effort {e}");
        }
    }

    #[test]
    fn rejects_raw_and_lossy_sources() {
        for ext in ["CR3", "heic", "jxl"] {
            let mut cfg = Config::default();
            cfg.extensions.push(ext.into());
            assert!(cfg.validate().is_err(), "{ext}");
        }
    }

    #[test]
    fn quality_is_lossless_by_default_and_named_in_the_file() {
        assert_eq!(Config::default().quality, Quality::Lossless);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (text, q) in [
            ("lossless", Quality::Lossless),
            ("high", Quality::High),
            ("medium", Quality::Medium),
            ("low", Quality::Low),
        ] {
            fs::write(&path, format!("quality = \"{text}\"\n")).unwrap();
            assert_eq!(Config::load(&path).unwrap().quality, q);
        }
        fs::write(&path, "quality = 90\n").unwrap();
        assert!(Config::load(&path).is_err(), "a number is not a quality level");
        let levels: Vec<_> = Quality::ALL.iter().map(|q| q.jxl_quality()).collect();
        assert_eq!(levels, [None, Some(90), Some(80), Some(70)]);
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
