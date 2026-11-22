//! Converting a single photo to JPEG XL, with the checks that make deleting the original safe:
//! encode in memory → verify (JPEG: rebuilt file is byte-identical; others: decoded pixels are
//! identical, or close to the source in the optional lossy mode) → hidden temp file → fsync →
//! rename → re-check the source → only then apply the original-file policy.

use std::fs::{self, File};
use std::io::Write;
use std::mem::DropGuard;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::{Config, OriginalPolicy};
use crate::filter::TEMP_SUFFIX;
use crate::jxl::{self, EncodeError, EncodeOptions};
use crate::metadata::{self, Container};
use crate::platform::{self, fsutil};
use crate::state::mtime_ns;
use crate::{exif, image::SourceImage};

pub const OUTPUT_EXTENSION: &str = "jxl";

#[derive(Debug, Clone)]
pub struct ConvertCtx {
    pub effort: u8,
    /// `Some(quality)`: encode non-JPEG sources lossily. JPEG is always lossless.
    pub quality: Option<u8>,
    pub threads: usize,
    pub original: OriginalPolicy,
    pub skip_if_larger: bool,
    pub dry_run: bool,
}

/// libjxl worker threads per encode: half the cores (the encode runs at background priority).
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(1).max(1)
}

impl ConvertCtx {
    pub fn new(cfg: &Config) -> ConvertCtx {
        ConvertCtx {
            effort: cfg.effort,
            quality: cfg.lossy.then_some(cfg.quality),
            threads: default_threads(),
            original: cfg.original,
            skip_if_larger: cfg.skip_if_larger,
            dry_run: false,
        }
    }

    fn options(&self) -> EncodeOptions {
        EncodeOptions { effort: self.effort, threads: self.threads, quality: self.quality }
    }
}

/// Identifies the exact version of a file that was skipped, so it is retried only if it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: i128,
}

impl Fingerprint {
    pub fn of(md: &fs::Metadata) -> Fingerprint {
        Fingerprint { size: md.len(), mtime_ns: mtime_ns(md.modified().unwrap_or(SystemTime::UNIX_EPOCH)) }
    }

    pub fn of_path(path: &Path) -> Option<Fingerprint> {
        fs::symlink_metadata(path).ok().map(|m| Fingerprint::of(&m))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Converted {
        output: PathBuf,
        before: u64,
        after: u64,
        original: OriginalPolicy,
        note: Option<String>,
    },
    /// Dry run: would be converted to this path.
    WouldConvert(PathBuf),
    /// Keep mode: a JPEG XL for this exact source already exists.
    AlreadyDone(PathBuf),
    /// Not converted on purpose. `remember`: don't retry this version of the file.
    Skipped {
        reason: String,
        remember: Option<Fingerprint>,
    },
    /// Not converted, nothing worth logging (vanished, not downloaded, hidden...).
    Ignored,
    /// The file changed while we worked on it; try again after it settles.
    Requeue(String),
    Failed {
        reason: String,
        remember: Option<Fingerprint>,
    },
}

impl Outcome {
    fn skipped(reason: impl Into<String>, fp: Fingerprint) -> Outcome {
        Outcome::Skipped { reason: reason.into(), remember: Some(fp) }
    }

    fn failed(reason: impl Into<String>) -> Outcome {
        Outcome::Failed { reason: reason.into(), remember: None }
    }

    /// The fingerprint to add to the skip list, if any.
    pub fn remember(&self) -> Option<(Fingerprint, &str)> {
        match self {
            Outcome::Skipped { reason, remember: Some(fp) } | Outcome::Failed { reason, remember: Some(fp) } => {
                Some((*fp, reason))
            }
            _ => None,
        }
    }

    pub fn describe(&self, path: &Path) -> Option<String> {
        let p = path.display();
        Some(match self {
            Outcome::Converted { output, before, after, original, note } => {
                let pct = if *before > 0 { 100.0 - (*after as f64 * 100.0 / *before as f64) } else { 0.0 };
                let name = output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut s = format!(
                    "変換: {p} → {name} ({} → {}, -{pct:.0}%) 元ファイル: {}",
                    human(*before),
                    human(*after),
                    original.describe()
                );
                if let Some(n) = note {
                    s.push_str(&format!(" ({n})"));
                }
                s
            }
            Outcome::WouldConvert(out) => format!("変換予定: {p} → {}", out.display()),
            Outcome::AlreadyDone(out) => format!("変換済み: {p} ({})", out.display()),
            Outcome::Skipped { reason, .. } => format!("スキップ: {p} ({reason})"),
            Outcome::Ignored => return None,
            Outcome::Requeue(r) => format!("再試行待ち: {p} ({r})"),
            Outcome::Failed { reason, .. } => format!("失敗: {p} ({reason})"),
        })
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

enum Target {
    New(PathBuf),
    Done(PathBuf),
}

/// `IMG_1.jpg` → `IMG_1.jxl`; on a name clash `IMG_1-jpg.jxl`, then `IMG_1-jpg-2.jxl` ...
fn choose_output(src: &Path, src_mtime: SystemTime, keep_mode: bool) -> Option<Target> {
    let dir = src.parent()?;
    let stem = src.file_stem()?.to_string_lossy().into_owned();
    let ext = src.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let names = [format!("{stem}.{OUTPUT_EXTENSION}"), format!("{stem}-{ext}.{OUTPUT_EXTENSION}")]
        .into_iter()
        .chain((2..100).map(|n| format!("{stem}-{ext}-{n}.{OUTPUT_EXTENSION}")));
    for name in names {
        let candidate = dir.join(name);
        match fs::symlink_metadata(&candidate) {
            Err(_) => return Some(Target::New(candidate)),
            // We copy the source mtime onto our output, so a match means "already converted".
            Ok(md) if keep_mode && md.modified().ok() == Some(src_mtime) => return Some(Target::Done(candidate)),
            Ok(_) => continue,
        }
    }
    None
}

fn same_file_state(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// Encodes in memory and verifies the result (lossless, or close in lossy mode). Returns the bytes.
fn encode_verified(ctx: &ConvertCtx, data: Vec<u8>, fp: Fingerprint) -> Result<Vec<u8>, Outcome> {
    let size = data.len() as u64;
    let too_big = |jxl: &[u8]| {
        (ctx.skip_if_larger && jxl.len() as u64 >= size).then(|| {
            Outcome::skipped(
                format!("JPEG XL の方が大きいため元ファイルを維持 ({} → {})", human(size), human(jxl.len() as u64)),
                fp,
            )
        })
    };
    let encode_err = |e: EncodeError| match e {
        EncodeError::Unsupported(m) => Outcome::skipped(m, fp),
        EncodeError::Failed(m) => Outcome::failed(m),
    };
    let verify_err = |lossy: bool| {
        move |m: String| {
            let what = if lossy { "非可逆変換の検証に失敗" } else { "劣化なしの検証に失敗" };
            Outcome::Failed { reason: format!("{what}: {m}"), remember: Some(fp) }
        }
    };

    if let Some(reason) = metadata::lossy_extras(&data) {
        return Err(Outcome::skipped(reason, fp));
    }
    match metadata::sniff(&data) {
        Container::Jpeg => {
            let jxl = jxl::transcode_jpeg(&data, &ctx.options()).map_err(encode_err)?;
            if let Some(skip) = too_big(&jxl) {
                return Err(skip);
            }
            jxl::verify_jpeg(&jxl, &data).map_err(verify_err(false))?;
            Ok(jxl)
        }
        Container::Heif => Err(Outcome::skipped("HEIC/HEIF は対象外です (非可逆形式)", fp)),
        Container::WebP if !metadata::webp_is_lossless(&data) => Err(Outcome::skipped("非可逆 WebP は対象外です", fp)),
        _ => {
            let meta = jxl::Metadata {
                exif: metadata::extract_exif(&data).map(|mut e| {
                    // The codestream carries the orientation; keep EXIF consistent with it.
                    exif::normalize_orientation(&mut e);
                    e
                }),
                xmp: metadata::extract_xmp(&data).map(|x| metadata::normalize_xmp_orientation(&x)),
            };
            let img: SourceImage = platform::image::decode(&data).map_err(encode_err)?;
            drop(data);
            let mut jxl = jxl::encode_pixels(&img, &meta, &ctx.options()).map_err(encode_err)?;
            let mut lossy = ctx.quality.is_some();
            if lossy && jxl.len() as u64 * 2 > size {
                // Lossy barely helped: screenshots and graphics are often smaller lossless (and
                // exact). Photos shrink far below half and never pay for this second encode.
                let lossless = EncodeOptions { quality: None, ..ctx.options() };
                let exact = jxl::encode_pixels(&img, &meta, &lossless).map_err(encode_err)?;
                if exact.len() <= jxl.len() {
                    jxl = exact;
                    lossy = false;
                }
            }
            if let Some(skip) = too_big(&jxl) {
                return Err(skip);
            }
            if lossy {
                jxl::verify_lossy(&jxl, &img).map_err(verify_err(true))?;
            } else {
                jxl::verify_lossless(&jxl, &img).map_err(verify_err(false))?;
            }
            Ok(jxl)
        }
    }
}

pub fn convert_file(ctx: &ConvertCtx, path: &Path) -> Outcome {
    let Ok(md) = fs::symlink_metadata(path) else { return Outcome::Ignored };
    if !md.is_file() || md.len() == 0 {
        return Outcome::Ignored;
    }
    if fsutil::is_placeholder(&md) || fsutil::is_hidden(&md) {
        return Outcome::Ignored; // not downloaded from the cloud (converting would download it), or hidden
    }
    let size = md.len();
    let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let fp = Fingerprint::of(&md);
    let target = match choose_output(path, mtime, ctx.original == OriginalPolicy::Keep) {
        None => return Outcome::failed("出力ファイル名を決められません"),
        Some(Target::Done(out)) => return Outcome::AlreadyDone(out),
        Some(Target::New(out)) => out,
    };
    if ctx.dry_run {
        return Outcome::WouldConvert(target);
    }

    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => return Outcome::failed(format!("読み込めません: {e}")),
    };
    match fs::symlink_metadata(path) {
        Ok(now) if same_file_state(&md, &now) => {}
        _ => return Outcome::Requeue("読み込み中に変更されました".into()),
    }
    let encoded = match encode_verified(ctx, data, fp) {
        Ok(e) => e,
        Err(outcome) => return outcome,
    };
    let after = encoded.len() as u64;

    let Some(dir) = target.parent() else { return Outcome::failed("出力先フォルダが不明") };
    let file_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Removed on every early return; dismissed once the file has been renamed into place.
    let tmp = DropGuard::new(dir.join(format!(".{file_name}{TEMP_SUFFIX}")), |p| {
        let _ = fs::remove_file(p);
    });
    if let Err(e) = write_temp(&tmp, &encoded, &md) {
        return Outcome::failed(format!("書き込めません: {e}"));
    }
    drop(encoded);
    fsutil::copy_extra_metadata(path, &tmp);

    if target.exists() {
        return Outcome::Requeue("出力先に同名ファイルが作成されました".into());
    }
    if let Err(e) = fs::rename(&*tmp, &target) {
        return Outcome::failed(format!("出力ファイルを配置できません: {e}"));
    }
    DropGuard::dismiss(tmp);
    fsutil::sync_dir(dir);

    // The source must be exactly what we encoded before we touch it.
    match fs::symlink_metadata(path) {
        Ok(now) if same_file_state(&md, &now) => {}
        _ => {
            let _ = fs::remove_file(&target);
            return Outcome::Requeue("変換中に元ファイルが変更されました".into());
        }
    }

    let note = match ctx.original {
        OriginalPolicy::Keep => None,
        OriginalPolicy::Delete => fs::remove_file(path).err().map(|e| format!("元ファイルを削除できませんでした: {e}")),
        OriginalPolicy::Trash => fsutil::trash(path).err().map(|e| format!("ゴミ箱へ移動できませんでした: {e}")),
    };
    Outcome::Converted { output: target, before: size, after, original: ctx.original, note }
}

fn write_temp(tmp: &Path, data: &[u8], src: &fs::Metadata) -> std::io::Result<()> {
    let mut f = File::create(tmp)?;
    f.write_all(data)?;
    fsutil::copy_times(&f, src)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_naming() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("IMG_1.JPG");
        fs::write(&src, b"x").unwrap();
        let t = fs::metadata(&src).unwrap().modified().unwrap();
        let name = |t: Option<Target>| match t {
            Some(Target::New(p)) => format!("new:{}", p.file_name().unwrap().to_string_lossy()),
            Some(Target::Done(p)) => format!("done:{}", p.file_name().unwrap().to_string_lossy()),
            None => "none".into(),
        };
        assert_eq!(name(choose_output(&src, t, false)), "new:IMG_1.jxl");
        fs::write(dir.path().join("IMG_1.jxl"), b"other").unwrap();
        assert_eq!(name(choose_output(&src, t, false)), "new:IMG_1-jpg.jxl");
        fs::write(dir.path().join("IMG_1-jpg.jxl"), b"other").unwrap();
        assert_eq!(name(choose_output(&src, t, false)), "new:IMG_1-jpg-2.jxl");

        // keep mode recognises an earlier output by its copied mtime
        let f = File::options().write(true).open(dir.path().join("IMG_1.jxl")).unwrap();
        f.set_modified(t).unwrap();
        assert_eq!(name(choose_output(&src, t, true)), "done:IMG_1.jxl");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(10), "10 B");
        assert_eq!(human(2048), "2.0 KB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
    }
}
