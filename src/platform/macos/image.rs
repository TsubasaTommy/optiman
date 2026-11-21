//! ImageIO / CoreGraphics: decode, native AVIF encode, and verification decode.

use std::sync::OnceLock;

use super::ffi::*;
use crate::encode::{DecodedImage, EncodeError, Encoded};
use crate::platform::Primaries;

const AVIF_UTI: &str = "public.avif";

/// ImageIO gained an AVIF *encoder* in recent macOS releases; detect it at runtime.
pub fn native_encoder_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| unsafe {
        Cf::from_create(CGImageDestinationCopyTypeIdentifiers())
            .is_some_and(|a| cf_array_contains_str(a.ptr(), AVIF_UTI))
    })
}

/// AVIF *decoding* exists since macOS 13.
pub fn native_decoder_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| unsafe {
        Cf::from_create(CGImageSourceCopyTypeIdentifiers()).is_some_and(|a| cf_array_contains_str(a.ptr(), AVIF_UTI))
    })
}

struct Source {
    src: Cf,
    props: Cf,
    width: usize,
    height: usize,
    orientation: i64,
}

fn open_source(data: &[u8]) -> Result<Source, EncodeError> {
    let unsupported = |m: &str| EncodeError::Unsupported(m.to_string());
    unsafe {
        let cfdata = cf_data(data);
        let no_cache = cf_dict(&[(kCGImageSourceShouldCache, kCFBooleanFalse)]);
        let src = Cf::from_create(CGImageSourceCreateWithData(cfdata.ptr(), no_cache.ptr()))
            .ok_or_else(|| unsupported("画像として読み込めません (未対応の形式)"))?;
        if CGImageSourceGetStatus(src.mut_ptr()) < 0 {
            return Err(unsupported("画像が壊れているか未対応の形式です"));
        }
        match CGImageSourceGetCount(src.mut_ptr()) {
            0 => return Err(unsupported("画像が含まれていません")),
            1 => {}
            _ => return Err(unsupported("複数フレームの画像 (アニメーション等) は対象外です")),
        }
        let props = Cf::from_create(CGImageSourceCopyPropertiesAtIndex(src.mut_ptr(), 0, std::ptr::null()))
            .ok_or_else(|| unsupported("画像情報を読めません"))?;
        let width = cf_dict_i64(props.ptr(), kCGImagePropertyPixelWidth).unwrap_or(0);
        let height = cf_dict_i64(props.ptr(), kCGImagePropertyPixelHeight).unwrap_or(0);
        if width <= 0 || height <= 0 {
            return Err(unsupported("画像サイズを取得できません"));
        }
        let orientation =
            cf_dict_i64(props.ptr(), kCGImagePropertyOrientation).filter(|o| (1..=8).contains(o)).unwrap_or(1);
        Ok(Source { src, props, width: width as usize, height: height as usize, orientation })
    }
}

/// Full-resolution image with the EXIF orientation applied to the pixels.
fn oriented_image(s: &Source) -> Result<(Cf, usize, usize), EncodeError> {
    unsafe {
        let max = cf_i64(s.width.max(s.height) as i64);
        let opts = cf_dict(&[
            (kCGImageSourceCreateThumbnailFromImageAlways, kCFBooleanTrue),
            (kCGImageSourceCreateThumbnailWithTransform, kCFBooleanTrue),
            (kCGImageSourceThumbnailMaxPixelSize, max.ptr()),
            (kCGImageSourceShouldCacheImmediately, kCFBooleanTrue),
        ]);
        let img = Cf::from_create(CGImageSourceCreateThumbnailAtIndex(s.src.mut_ptr(), 0, opts.ptr()))
            .ok_or_else(|| EncodeError::Unsupported("画像をデコードできません".into()))?;
        let (w, h) = (CGImageGetWidth(img.mut_ptr()), CGImageGetHeight(img.mut_ptr()));
        let expected = if (5..=8).contains(&s.orientation) { (s.height, s.width) } else { (s.width, s.height) };
        if (w, h) != expected {
            return Err(EncodeError::Failed(format!(
                "デコード結果のサイズが想定外です: {w}x{h} (期待値 {}x{})",
                expected.0, expected.1
            )));
        }
        Ok((img, w, h))
    }
}

/// Encodes with ImageIO's AVIF encoder. EXIF/GPS/ICC are carried over from the source
/// properties; the orientation is baked into the pixels (ImageIO does not write `irot`).
pub fn native_encode(data: &[u8], quality: u8) -> Result<Encoded, EncodeError> {
    let s = open_source(data)?;
    let (img, w, h) = oriented_image(&s)?;
    unsafe {
        let props = Cf::from_create(CFDictionaryCreateMutableCopy(kCFAllocatorDefault, 0, s.props.ptr()))
            .ok_or_else(|| EncodeError::Failed("メモリ不足".into()))?;
        let one = cf_i64(1);
        CFDictionarySetValue(props.mut_ptr(), kCGImagePropertyOrientation, one.ptr());
        CFDictionaryRemoveValue(props.mut_ptr(), kCGImagePropertyPixelWidth);
        CFDictionaryRemoveValue(props.mut_ptr(), kCGImagePropertyPixelHeight);
        let tiff = CFDictionaryGetValue(s.props.ptr(), kCGImagePropertyTIFFDictionary);
        let tiff_copy = if !tiff.is_null() && CFGetTypeID(tiff) == CFDictionaryGetTypeID() {
            Cf::from_create(CFDictionaryCreateMutableCopy(kCFAllocatorDefault, 0, tiff))
        } else {
            None
        };
        if let Some(t) = &tiff_copy {
            CFDictionarySetValue(t.mut_ptr(), kCGImagePropertyTIFFOrientation, one.ptr());
            CFDictionarySetValue(props.mut_ptr(), kCGImagePropertyTIFFDictionary, t.ptr());
        }
        let q = cf_f64(f64::from(quality.clamp(1, 100)) / 100.0);
        CFDictionarySetValue(props.mut_ptr(), kCGImageDestinationLossyCompressionQuality, q.ptr());

        let out = Cf::from_create(CFDataCreateMutable(kCFAllocatorDefault, 0))
            .ok_or_else(|| EncodeError::Failed("メモリ不足".into()))?;
        let uti = cf_string(AVIF_UTI);
        let dest = Cf::from_create(CGImageDestinationCreateWithData(out.mut_ptr(), uti.ptr(), 1, std::ptr::null()))
            .ok_or_else(|| EncodeError::Failed("ImageIO の AVIF エンコーダを開けません".into()))?;
        CGImageDestinationAddImage(dest.mut_ptr(), img.mut_ptr(), props.ptr());
        if !CGImageDestinationFinalize(dest.mut_ptr()) {
            return Err(EncodeError::Failed("ImageIO で AVIF を書き出せませんでした".into()));
        }
        drop(dest);
        let avif = cf_data_bytes(out.ptr()).to_vec();
        Ok(Encoded { avif, width: w as u32, height: h as u32 })
    }
}

/// Decodes to oriented RGBA8 (straight alpha) in sRGB, or Display P3 for wide-gamut sources.
pub fn decode(data: &[u8]) -> Result<DecodedImage, EncodeError> {
    let s = open_source(data)?;
    let (img, w, h) = oriented_image(&s)?;
    unsafe {
        let space = CGImageGetColorSpace(img.mut_ptr());
        let wide = !space.is_null() && CGColorSpaceIsWideGamutRGB(space);
        let (space_name, primaries) =
            if wide { (kCGColorSpaceDisplayP3, Primaries::DisplayP3) } else { (kCGColorSpaceSRGB, Primaries::Srgb) };
        let target = Cf::from_create(CGColorSpaceCreateWithName(space_name))
            .ok_or_else(|| EncodeError::Failed("色空間を作成できません".into()))?;
        let alpha_info = CGImageGetAlphaInfo(img.mut_ptr()) & 0x1F;
        let has_alpha =
            ![kCGImageAlphaNone, kCGImageAlphaNoneSkipLast, kCGImageAlphaNoneSkipFirst].contains(&alpha_info);
        let bitmap_info = if has_alpha { kCGImageAlphaPremultipliedLast } else { kCGImageAlphaNoneSkipLast };
        let mut rgba = vec![0u8; w * h * 4];
        let ctx = Cf::from_create(CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            target.mut_ptr(),
            bitmap_info,
        ))
        .ok_or_else(|| EncodeError::Failed("描画コンテキストを作成できません".into()))?;
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } };
        CGContextDrawImage(ctx.mut_ptr(), rect, img.mut_ptr());
        drop(ctx);
        if has_alpha {
            unpremultiply(&mut rgba);
        } else {
            rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        }
        Ok(DecodedImage { width: w as u32, height: h as u32, rgba, primaries })
    }
}

fn unpremultiply(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        match a {
            255 => {}
            0 => p[..3].fill(0),
            _ => {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }
}

/// Fully decodes the AVIF with ImageIO. `None` when the OS has no AVIF decoder.
pub fn verify_decode(avif: &[u8]) -> Option<Result<(u32, u32), String>> {
    if !native_decoder_available() {
        return None;
    }
    Some(unsafe {
        (|| {
            let data = cf_data(avif);
            let src = Cf::from_create(CGImageSourceCreateWithData(data.ptr(), std::ptr::null()))
                .ok_or("ImageIO が AVIF を開けません")?;
            let opts = cf_dict(&[(kCGImageSourceShouldCacheImmediately, kCFBooleanTrue)]);
            let img = Cf::from_create(CGImageSourceCreateImageAtIndex(src.mut_ptr(), 0, opts.ptr()))
                .ok_or("ImageIO が AVIF をデコードできません")?;
            Ok((CGImageGetWidth(img.mut_ptr()) as u32, CGImageGetHeight(img.mut_ptr()) as u32))
        })()
        .map_err(|e: &str| e.to_string())
    })
}
