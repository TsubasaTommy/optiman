//! ImageIO decoding without colour conversion or rotation, PNG writing for `restore`, and
//! detection of JPEG XL support (macOS 14+).

use std::sync::OnceLock;

use super::ffi::*;
use crate::image::{ColorInfo, SourceImage};
use crate::jxl::EncodeError;

// CGColorSpaceModel
const MODEL_MONOCHROME: i32 = 0;
const MODEL_RGB: i32 = 1;
// CGBitmapInfo
const ALPHA_MASK: u32 = 0x1F;
const FLOAT_COMPONENTS: u32 = 1 << 8;
const BYTE_ORDER_MASK: u32 = 0x7000;
const BYTE_ORDER_16_LITTLE: u32 = 1 << 12;
const BYTE_ORDER_32_LITTLE: u32 = 2 << 12;
const BYTE_ORDER_16_BIG: u32 = 3 << 12;
const BYTE_ORDER_32_BIG: u32 = 4 << 12;
// CGImageAlphaInfo
const ALPHA_NONE: u32 = 0;
const ALPHA_PREMUL_LAST: u32 = 1;
const ALPHA_PREMUL_FIRST: u32 = 2;
const ALPHA_LAST: u32 = 3;
const ALPHA_FIRST: u32 = 4;
#[cfg(test)]
const ALPHA_SKIP_LAST: u32 = 5;
const ALPHA_SKIP_FIRST: u32 = 6;

fn unsupported(m: impl Into<String>) -> EncodeError {
    EncodeError::Unsupported(m.into())
}

/// Colour information of a CGColorSpace: CICP for sRGB/Display P3, otherwise the ICC bytes.
unsafe fn color_info(space: CGColorSpaceRef) -> ColorInfo {
    unsafe {
        if let Some(name) = Cf::from_create(CGColorSpaceCopyName(space)) {
            if CFEqual(name.ptr(), kCGColorSpaceSRGB) != 0 {
                return ColorInfo::Srgb;
            }
            if CFEqual(name.ptr(), kCGColorSpaceDisplayP3) != 0 {
                return ColorInfo::DisplayP3;
            }
        }
        match Cf::from_create(CGColorSpaceCopyICCData(space)) {
            Some(icc) => ColorInfo::Icc(cf_data_bytes(icc.ptr()).to_vec()),
            None => ColorInfo::Srgb, // device/uncalibrated: displayed as sRGB anyway
        }
    }
}

/// Decodes the first image exactly as stored: original orientation, colour space and bit depth.
pub fn decode(data: &[u8]) -> Result<SourceImage, EncodeError> {
    unsafe {
        // ImageIO reads the file bytes in place; `data` outlives every CF object made here.
        let cfdata = cf_data_borrowed(data);
        let src = Cf::from_create(CGImageSourceCreateWithData(cfdata.ptr(), std::ptr::null()))
            .ok_or_else(|| unsupported("画像として読み込めません (未対応の形式)"))?;
        if CGImageSourceGetStatus(src.mut_ptr()) < 0 {
            return Err(unsupported("画像が壊れているか未対応の形式です"));
        }
        match CGImageSourceGetCount(src.mut_ptr()) {
            0 => return Err(unsupported("画像が含まれていません")),
            1 => {}
            _ => return Err(unsupported("複数フレームの画像 (アニメーション等) は対象外です")),
        }
        let props = Cf::from_create(CGImageSourceCopyPropertiesAtIndex(src.mut_ptr(), 0, std::ptr::null()));
        let orientation = props
            .as_ref()
            .and_then(|p| cf_dict_i64(p.ptr(), kCGImagePropertyOrientation))
            .filter(|o| (1..=8).contains(o))
            .unwrap_or(1) as u16;
        let opts = cf_dict(&[(kCGImageSourceShouldCacheImmediately, kCFBooleanTrue)]);
        let img = Cf::from_create(CGImageSourceCreateImageAtIndex(src.mut_ptr(), 0, opts.ptr()))
            .ok_or_else(|| unsupported("画像をデコードできません"))?;
        let cg = img.mut_ptr();

        let info = CGImageGetBitmapInfo(cg);
        if info & FLOAT_COMPONENTS != 0 {
            return Err(unsupported("浮動小数点 (HDR) 画像は劣化なしに変換できないため対象外です"));
        }
        let space = CGImageGetColorSpace(cg);
        let model = if space.is_null() { -1 } else { CGColorSpaceGetModel(space) };
        let (gray, color) = match model {
            MODEL_RGB => (false, color_info(space)),
            MODEL_MONOCHROME => (true, color_info(space)),
            _ => return Err(unsupported("CMYK・パレット等の色空間は劣化なしに変換できないため対象外です")),
        };
        let layout = Layout {
            width: CGImageGetWidth(cg),
            height: CGImageGetHeight(cg),
            bpc: CGImageGetBitsPerComponent(cg),
            bpp: CGImageGetBitsPerPixel(cg),
            row_bytes: CGImageGetBytesPerRow(cg),
            alpha: info & ALPHA_MASK,
            byte_order: info & BYTE_ORDER_MASK,
            gray,
        };
        let raw = Cf::from_create(CGDataProviderCopyData(CGImageGetDataProvider(cg)))
            .ok_or_else(|| EncodeError::Failed("画素データを取得できません".into()))?;
        let pixels = layout.to_rgba(cf_data_bytes(raw.ptr())).ok_or_else(|| {
            unsupported(format!(
                "未対応の画素形式 ({} bpc / {} bpp / alpha {} / order {:#x})",
                layout.bpc, layout.bpp, layout.alpha, layout.byte_order
            ))
        })?;
        Ok(SourceImage {
            width: layout.width as u32,
            height: layout.height as u32,
            sample_bits: if layout.bpc > 8 { 16 } else { 8 },
            pixels,
            has_alpha: layout.alpha_real(),
            premultiplied: matches!(layout.alpha, ALPHA_PREMUL_FIRST | ALPHA_PREMUL_LAST),
            gray,
            color,
            orientation,
        })
    }
}

/// A CGImage's memory layout, converted to tightly packed RGBA without changing any value.
struct Layout {
    width: usize,
    height: usize,
    bpc: usize,
    bpp: usize,
    row_bytes: usize,
    alpha: u32,
    byte_order: u32,
    gray: bool,
}

impl Layout {
    fn alpha_real(&self) -> bool {
        matches!(self.alpha, ALPHA_PREMUL_FIRST | ALPHA_PREMUL_LAST | ALPHA_FIRST | ALPHA_LAST)
    }

    fn to_rgba(&self, src: &[u8]) -> Option<Vec<u8>> {
        let bytes = match self.bpc {
            8 => 1,
            16 => 2,
            _ => return None,
        };
        let has_slot = self.alpha != ALPHA_NONE;
        let color_channels = if self.gray { 1 } else { 3 };
        let channels = color_channels + usize::from(has_slot);
        if self.bpp != channels * self.bpc {
            return None;
        }
        // Byte order: 32-bit little-endian 8-bpc pixels are stored reversed (ARGB → BGRA in memory).
        let reversed = match (bytes, self.byte_order) {
            (1, 0 | BYTE_ORDER_32_BIG) => false,
            (1, BYTE_ORDER_32_LITTLE) if channels == 4 => true,
            (2, 0 | BYTE_ORDER_16_BIG | BYTE_ORDER_16_LITTLE) => false,
            _ => return None,
        };
        let little16 = self.byte_order == BYTE_ORDER_16_LITTLE;
        let pixel_bytes = self.bpp / 8;
        let used = self.width * pixel_bytes;
        if self.height == 0 || self.row_bytes < used || src.len() < self.row_bytes * (self.height - 1) + used {
            return None;
        }
        let alpha_first = matches!(self.alpha, ALPHA_PREMUL_FIRST | ALPHA_FIRST | ALPHA_SKIP_FIRST);
        let (color_at, alpha_at) = match (has_slot, alpha_first) {
            (true, true) => (1, Some(0)),
            (true, false) => (0, Some(color_channels)),
            (false, _) => (0, None),
        };
        let alpha_at = alpha_at.filter(|_| self.alpha_real());
        let read = |px: &[u8], ch: usize| -> u16 {
            let slot = if reversed { channels - 1 - ch } else { ch };
            if bytes == 1 {
                u16::from(px[slot])
            } else {
                let b = [px[slot * 2], px[slot * 2 + 1]];
                if little16 { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }
            }
        };
        let opaque = if bytes == 1 { 0xFF } else { 0xFFFF };
        let mut out = Vec::with_capacity(self.width * self.height * 4 * bytes);
        for y in 0..self.height {
            let row = &src[y * self.row_bytes..y * self.row_bytes + used];
            for px in row.chunks_exact(pixel_bytes) {
                let a = alpha_at.map_or(opaque, |s| read(px, s));
                let rgb = if self.gray {
                    let v = read(px, color_at);
                    [v, v, v]
                } else {
                    [read(px, color_at), read(px, color_at + 1), read(px, color_at + 2)]
                };
                for v in [rgb[0], rgb[1], rgb[2], a] {
                    if bytes == 1 {
                        out.push(v as u8);
                    } else {
                        out.extend_from_slice(&v.to_ne_bytes());
                    }
                }
            }
        }
        Some(out)
    }
}

/// Whether macOS can display JPEG XL (ImageIO decodes it since macOS 14).
pub fn viewer_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| unsafe {
        Cf::from_create(CGImageSourceCopyTypeIdentifiers())
            .is_some_and(|a| cf_array_contains_str(a.ptr(), "public.jpeg-xl"))
    })
}

/// A note for the user when this Mac cannot show JPEG XL files.
pub fn viewer_hint() -> Option<String> {
    (!viewer_available()).then(|| {
        "この macOS では JPEG XL を表示できません (macOS 14 Sonoma 以降で標準対応)。変換した写真を見るには macOS を更新してください。".to_string()
    })
}

/// Writes decoded RGBA samples as PNG with ImageIO, keeping the exact values and colour profile.
pub fn encode_png(img: &crate::jxl::Decoded) -> Result<Vec<u8>, String> {
    if img.channels != 4 {
        return Err("RGBA 以外は PNG に書き出せません".into());
    }
    let bpc = if img.bits > 8 { 16 } else { 8 };
    unsafe {
        let space = match &img.icc {
            Some(icc) => {
                let data = cf_data(icc);
                Cf::from_create(CGColorSpaceCreateWithICCData(data.ptr()))
            }
            None => None,
        }
        .or_else(|| Cf::from_create(CGColorSpaceCreateWithName(kCGColorSpaceSRGB)))
        .ok_or("色空間を作成できません")?;
        let data = cf_data_borrowed(&img.pixels);
        let provider =
            Cf::from_create(CGDataProviderCreateWithCFData(data.ptr())).ok_or("CGDataProvider を作成できません")?;
        let byte_order = if bpc == 16 { BYTE_ORDER_16_LITTLE } else { 0 };
        let image = Cf::from_create(CGImageCreate(
            img.width as usize,
            img.height as usize,
            bpc,
            bpc * 4,
            img.width as usize * 4 * bpc / 8,
            space.mut_ptr(),
            ALPHA_LAST | byte_order,
            provider.mut_ptr(),
            std::ptr::null(),
            false,
            0,
        ))
        .ok_or("CGImage を作成できません")?;
        let out = Cf::from_create(CFDataCreateMutable(kCFAllocatorDefault, 0)).ok_or("メモリ不足")?;
        let uti = cf_string("public.png");
        let dest = Cf::from_create(CGImageDestinationCreateWithData(out.mut_ptr(), uti.ptr(), 1, std::ptr::null()))
            .ok_or("PNG エンコーダを開けません")?;
        CGImageDestinationAddImage(dest.mut_ptr(), image.mut_ptr(), std::ptr::null());
        if !CGImageDestinationFinalize(dest.mut_ptr()) {
            return Err("PNG を書き出せませんでした".into());
        }
        drop(dest);
        Ok(cf_data_bytes(out.ptr()).to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(bpc: usize, bpp: usize, alpha: u32, order: u32, gray: bool) -> Layout {
        Layout { width: 2, height: 1, bpc, bpp, row_bytes: 2 * bpp / 8, alpha, byte_order: order, gray }
    }

    #[test]
    fn converts_common_layouts_exactly() {
        // RGBA straight, 8 bpc
        let l = layout(8, 32, ALPHA_LAST, 0, false);
        assert_eq!(l.to_rgba(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        // BGRA in memory (premultiplied first + 32 little)
        let l = layout(8, 32, ALPHA_PREMUL_FIRST, BYTE_ORDER_32_LITTLE, false);
        assert_eq!(l.to_rgba(&[3, 2, 1, 9, 30, 20, 10, 90]).unwrap(), vec![1, 2, 3, 9, 10, 20, 30, 90]);
        // RGB without alpha → opaque alpha
        let l = layout(8, 24, ALPHA_NONE, 0, false);
        assert_eq!(l.to_rgba(&[1, 2, 3, 4, 5, 6]).unwrap(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
        // RGBX: X ignored
        let l = layout(8, 32, ALPHA_SKIP_LAST, 0, false);
        assert_eq!(l.to_rgba(&[1, 2, 3, 77, 4, 5, 6, 77]).unwrap(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
        // Grey 8
        let l = layout(8, 8, ALPHA_NONE, 0, true);
        assert_eq!(l.to_rgba(&[9, 200]).unwrap(), vec![9, 9, 9, 255, 200, 200, 200, 255]);
        // RGB 16, big-endian
        let l = layout(16, 48, ALPHA_NONE, 0, false);
        let out = l.to_rgba(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0xFF, 0xFE, 0, 1, 0, 2]).unwrap();
        let s = |i: usize| u16::from_ne_bytes([out[i * 2], out[i * 2 + 1]]);
        assert_eq!((s(0), s(1), s(2), s(3)), (0x0102, 0x0304, 0x0506, 0xFFFF));
        assert_eq!((s(4), s(5), s(6)), (0xFFFE, 1, 2));
    }

    #[test]
    fn rejects_unknown_layouts() {
        assert!(layout(5, 16, ALPHA_NONE, 0, false).to_rgba(&[0; 4]).is_none());
        assert!(layout(8, 16, ALPHA_NONE, 0, false).to_rgba(&[0; 4]).is_none());
    }
}
