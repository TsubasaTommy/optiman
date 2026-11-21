//! Encoder selection, encoding and output verification.

pub mod av1;

use std::fmt;

use crate::config::EncoderChoice;
use crate::platform::{self, image as native};
use crate::{exif, metadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    /// OS encoder (macOS ImageIO `public.avif`).
    Native,
    /// Built-in rav1e.
    Rav1e,
}

impl fmt::Display for EncoderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EncoderKind::Native => "native (OS)",
            EncoderKind::Rav1e => "rav1e",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The file cannot be handled (no decoder, multi-frame, corrupt). Not retried until it changes.
    Unsupported(String),
    /// Something went wrong that may succeed later.
    Failed(String),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::Unsupported(s) | EncodeError::Failed(s) => f.write_str(s),
        }
    }
}

pub struct Encoded {
    pub avif: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// `auto` picks rav1e: at equal SSIM it produced files ~40% smaller than ImageIO's encoder in
/// our measurements, which matters more than speed for a background job that deletes originals.
pub fn resolve(choice: EncoderChoice) -> EncoderKind {
    match choice {
        EncoderChoice::Auto | EncoderChoice::Rav1e => EncoderKind::Rav1e,
        EncoderChoice::Native if native::native_encoder_available() => EncoderKind::Native,
        EncoderChoice::Native => {
            crate::warn!("OS の AVIF エンコーダが使えないため rav1e を使います");
            EncoderKind::Rav1e
        }
    }
}

pub fn encode(kind: EncoderKind, data: &[u8], quality: u8, speed: u8) -> Result<Encoded, EncodeError> {
    match kind {
        EncoderKind::Native => native::native_encode(data, quality),
        EncoderKind::Rav1e => {
            let img = native::decode(data)?;
            let exif = metadata::extract_exif(data).map(|mut e| {
                // The decoder already applied the orientation to the pixels.
                exif::normalize_orientation(&mut e);
                e
            });
            av1::encode(&img, exif, quality, speed)
        }
    }
}

/// Checks the AVIF before the original is touched: container structure, AV1 sequence header,
/// dimensions, and (where the OS has an AVIF decoder) a full decode.
pub fn verify(avif: &[u8], width: u32, height: u32) -> Result<(), String> {
    let structurally_checked = match avif_parse::read_avif(&mut &avif[..]) {
        Ok(parsed) => {
            let meta = parsed.primary_item_metadata().map_err(|e| format!("AV1 ヘッダが不正: {e:?}"))?;
            // `still_picture` is not checked: ImageIO's encoder leaves it unset, which is valid AVIF.
            let (w, h) = (meta.max_frame_width.get(), meta.max_frame_height.get());
            if (w, h) != (width, height) {
                return Err(format!("AVIF のサイズが一致しません: {w}x{h} (期待値 {width}x{height})"));
            }
            true
        }
        // ImageIO tiles large images into a grid (valid MIAF) that avif-parse cannot read;
        // those must pass the full OS decode below instead.
        Err(avif_parse::Error::Unsupported(m)) if m.contains("Grid") => false,
        Err(e) => return Err(format!("AVIF の構造が不正: {e:?}")),
    };
    match native::verify_decode(avif) {
        None if structurally_checked => Ok(()),
        None => Err("グリッド形式の AVIF を検証できるデコーダがありません".into()),
        Some(Ok((dw, dh))) if (dw, dh) == (width, height) => Ok(()),
        Some(Ok((dw, dh))) => Err(format!("デコード結果のサイズが一致しません: {dw}x{dh}")),
        Some(Err(e)) => Err(format!("AVIF をデコードできません: {e}")),
    }
}

/// Rows of an RGBA8 image (straight alpha), oriented for display.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub primaries: platform::Primaries,
}

impl DecodedImage {
    pub fn has_alpha(&self) -> bool {
        self.rgba.chunks_exact(4).any(|p| p[3] != 255)
    }
}
