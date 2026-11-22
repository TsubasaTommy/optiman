//! Undo a conversion: rebuild the original file from a JPEG XL made by this tool.
//! - JPEG origin: the original JPEG, byte for byte (libjxl JPEG reconstruction).
//! - Lossless origin: a PNG with exactly the same pixels and colour profile (OS encoder).

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::jxl;
use crate::platform::{fsutil, image};

/// Picks `name.ext`, or `name-restored.ext`, `name-restored-2.ext` … if taken.
fn free_name(jxl_path: &Path, ext: &str) -> Result<PathBuf> {
    let dir = jxl_path.parent().context("フォルダが不明")?;
    let stem = jxl_path.file_stem().context("ファイル名が不明")?.to_string_lossy().into_owned();
    let candidates = [format!("{stem}.{ext}"), format!("{stem}-restored.{ext}")]
        .into_iter()
        .chain((2..100).map(|n| format!("{stem}-restored-{n}.{ext}")));
    candidates.map(|n| dir.join(n)).find(|p| !p.exists()).context("出力ファイル名を決められません")
}

pub fn restore_file(path: &Path, delete_jxl: bool) -> Result<PathBuf> {
    let data = fs::read(path).with_context(|| format!("読み込めません: {}", path.display()))?;
    let md = fs::metadata(path)?;
    let (bytes, ext) = match jxl::reconstruct_jpeg(&data).map_err(|e| anyhow!(e))? {
        Some(jpeg) => (jpeg, "jpg"),
        None => {
            let decoded = jxl::decode_exact(&data, false, true).map_err(|e| anyhow!(e))?;
            (image::encode_png(&decoded).map_err(|e| anyhow!(e))?, "png")
        }
    };
    if bytes.is_empty() {
        bail!("復元結果が空です");
    }
    let out = free_name(path, ext)?;
    fs::write(&out, &bytes).with_context(|| format!("書き込めません: {}", out.display()))?;
    if let Ok(f) = File::options().write(true).open(&out) {
        let _ = fsutil::copy_times(&f, &md);
    }
    if delete_jxl {
        fs::remove_file(path).with_context(|| format!("削除できません: {}", path.display()))?;
    }
    Ok(out)
}
