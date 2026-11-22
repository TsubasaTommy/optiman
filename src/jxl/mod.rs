//! JPEG XL: encoding with libjxl (reference implementation, statically linked) and verification
//! with jxl-rs (the Rust decoder used by Chrome and Firefox).
//!
//! - JPEG sources are always recompressed losslessly; the original file can be rebuilt
//!   byte-for-byte.
//! - Other sources are encoded as lossless modular images in their original colour space, bit
//!   depth and alpha mode (the codestream orientation replaces pixel rotation). Only when the user
//!   opts in (`EncodeOptions::quality`) are they encoded lossily (VarDCT; alpha stays lossless).

pub mod ffi;

use std::ffi::c_void;
use std::fmt;

use jxl::api::{
    Endianness, Event, JxlColorType, JxlDataFormat, JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, JxlPixelFormat,
    ProfileLevel,
};

use crate::image::{ColorInfo, SourceImage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The file cannot be converted without loss (or at all). Not retried until it changes.
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

#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// TIFF-structured EXIF.
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// libjxl effort 1 (fast) - 9 (smallest).
    pub effort: u8,
    pub threads: usize,
    /// `None`: lossless. `Some(q)`: lossy at JPEG-style quality `q` (90 ≈ visually lossless).
    /// JPEG sources ignore this and are always recompressed losslessly.
    pub quality: Option<u8>,
}

/// Limits for a lossy result against its source. They catch broken output (garbage, a wrong
/// colour space or channel order), not quality differences: real 4K photos measured 36.6-44.7 dB
/// at quality 60-95, and synthetic hard-edged test images, the worst case, 22 dB at quality 60.
pub const LOSSY_MIN_PSNR: f64 = 20.0;
/// Largest allowed shift of a channel's mean (fraction of full scale). Lossy coding keeps means
/// within 0.8% even on the worst-case test images; colour-space mix-ups move them much more.
pub const LOSSY_MAX_MEAN_SHIFT: f64 = 0.02;

/// libjxl version, e.g. "0.13.0".
pub fn libjxl_version() -> String {
    let v = unsafe { ffi::JxlEncoderVersion() };
    format!("{}.{}.{}", v / 1_000_000, (v / 1_000) % 1_000, v % 1_000)
}

// ---------- RAII ----------

struct Encoder(*mut ffi::JxlEncoder);
impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ffi::JxlEncoderDestroy(self.0) }
    }
}

struct Decoder(*mut ffi::JxlDecoder);
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::JxlDecoderDestroy(self.0) }
    }
}

/// libjxl's worker pool; threads exist only while this value lives (one encode).
struct Runner(*mut c_void);
impl Drop for Runner {
    fn drop(&mut self) {
        unsafe { ffi::JxlThreadParallelRunnerDestroy(self.0) }
    }
}

fn error_for(enc: &Encoder, what: &str) -> EncodeError {
    let code = unsafe { ffi::JxlEncoderGetError(enc.0) };
    match code {
        ffi::JXL_ENC_ERR_JBRD | ffi::JXL_ENC_ERR_NOT_SUPPORTED | ffi::JXL_ENC_ERR_BAD_INPUT => {
            EncodeError::Unsupported(format!(
                "{what}: この画像は JPEG XL へ劣化なしに変換できない構造です (code {code:#x})"
            ))
        }
        ffi::JXL_ENC_ERR_OOM => EncodeError::Failed(format!("{what}: メモリ不足")),
        _ => EncodeError::Failed(format!("{what}: libjxl エラー (code {code:#x})")),
    }
}

fn check(enc: &Encoder, status: i32, what: &str) -> Result<(), EncodeError> {
    if status == ffi::JXL_ENC_SUCCESS { Ok(()) } else { Err(error_for(enc, what)) }
}

/// Creates an encoder with a thread pool attached. The pool must outlive the encoder.
fn new_encoder(threads: usize) -> Result<(Runner, Encoder), EncodeError> {
    let runner = Runner(unsafe { ffi::JxlThreadParallelRunnerCreate(std::ptr::null(), threads.max(1)) });
    if runner.0.is_null() {
        return Err(EncodeError::Failed("libjxl のスレッドを作成できません".into()));
    }
    let enc = Encoder(unsafe { ffi::JxlEncoderCreate(std::ptr::null()) });
    if enc.0.is_null() {
        return Err(EncodeError::Failed("メモリ不足 (JxlEncoderCreate)".into()));
    }
    check(
        &enc,
        unsafe { ffi::JxlEncoderSetParallelRunner(enc.0, ffi::JxlThreadParallelRunner, runner.0) },
        "スレッド設定",
    )?;
    check(&enc, unsafe { ffi::JxlEncoderUseContainer(enc.0, ffi::JXL_TRUE) }, "コンテナ設定")?;
    Ok((runner, enc))
}

fn new_frame_settings(enc: &Encoder, effort: u8) -> Result<*mut ffi::JxlEncoderFrameSettings, EncodeError> {
    let settings = unsafe { ffi::JxlEncoderFrameSettingsCreate(enc.0, std::ptr::null()) };
    if settings.is_null() {
        return Err(EncodeError::Failed("メモリ不足 (JxlEncoderFrameSettingsCreate)".into()));
    }
    let effort = i64::from(effort.clamp(1, 9));
    check(
        enc,
        unsafe { ffi::JxlEncoderFrameSettingsSetOption(settings, ffi::JXL_ENC_FRAME_SETTING_EFFORT, effort) },
        "effort",
    )?;
    Ok(settings)
}

fn drain_output(enc: &Encoder) -> Result<Vec<u8>, EncodeError> {
    let mut out = vec![0u8; 64 * 1024];
    let mut used = 0usize;
    loop {
        let mut next = unsafe { out.as_mut_ptr().add(used) };
        let mut avail = out.len() - used;
        let status = unsafe { ffi::JxlEncoderProcessOutput(enc.0, &mut next, &mut avail) };
        used = out.len() - avail;
        match status {
            ffi::JXL_ENC_SUCCESS => {
                out.truncate(used);
                out.shrink_to_fit();
                return Ok(out);
            }
            ffi::JXL_ENC_NEED_MORE_OUTPUT => out.resize(out.len() * 2, 0),
            _ => return Err(error_for(enc, "エンコード")),
        }
    }
}

// ---------- encoding ----------

/// Lossless JPEG recompression. The JPEG is not decoded to pixels: its DCT coefficients and
/// every marker (EXIF, XMP, ICC, …) are stored so the original file can be rebuilt exactly.
pub fn transcode_jpeg(jpeg: &[u8], o: &EncodeOptions) -> Result<Vec<u8>, EncodeError> {
    let (_runner, enc) = new_encoder(o.threads)?;
    check(&enc, unsafe { ffi::JxlEncoderStoreJPEGMetadata(enc.0, ffi::JXL_TRUE) }, "JPEG 復元情報")?;
    // Above effort 7 JPEG recompression only costs time (measured: identical size, 20x slower).
    let settings = new_frame_settings(&enc, o.effort.min(7))?;
    check(&enc, unsafe { ffi::JxlEncoderAddJPEGFrame(settings, jpeg.as_ptr(), jpeg.len()) }, "JPEG の再圧縮")?;
    unsafe { ffi::JxlEncoderCloseInput(enc.0) };
    drain_output(&enc)
}

/// Repacks RGBA samples into the channel layout that is actually needed (G, GA, RGB or RGBA).
fn pack_channels(img: &SourceImage, alpha: bool) -> (Vec<u8>, u32) {
    let bps = img.bytes_per_sample();
    let keep: &[usize] = match (img.gray, alpha) {
        (true, false) => &[0],
        (true, true) => &[0, 3],
        (false, false) => &[0, 1, 2],
        (false, true) => &[0, 1, 2, 3],
    };
    if keep.len() == 4 {
        return (img.pixels.clone(), 4);
    }
    let mut out = Vec::with_capacity(img.width as usize * img.height as usize * keep.len() * bps);
    for px in img.pixels.chunks_exact(4 * bps) {
        for &c in keep {
            out.extend_from_slice(&px[c * bps..(c + 1) * bps]);
        }
    }
    (out, keep.len() as u32)
}

/// Encodes decoded pixels: lossless modular in their original colour space, or lossy VarDCT when
/// `o.quality` is set. Lossy sRGB / Display P3 images use XYB (the best compression); images with
/// an ICC profile stay in that profile, so they decode back to the same colour space.
pub fn encode_pixels(img: &SourceImage, meta: &Metadata, o: &EncodeOptions) -> Result<Vec<u8>, EncodeError> {
    if img.width == 0 || img.height == 0 || img.pixels.len() != img.row_bytes() * img.height as usize {
        return Err(EncodeError::Unsupported("画像サイズが不正".into()));
    }
    let alpha = img.alpha_used();
    let (_runner, enc) = new_encoder(o.threads)?;
    unsafe {
        let mut info: ffi::JxlBasicInfo = std::mem::zeroed();
        ffi::JxlEncoderInitBasicInfo(&mut info);
        info.xsize = img.width;
        info.ysize = img.height;
        info.bits_per_sample = u32::from(img.sample_bits);
        info.exponent_bits_per_sample = 0;
        let xyb = o.quality.is_some() && !matches!(img.color, ColorInfo::Icc(_));
        info.uses_original_profile = if xyb { ffi::JXL_FALSE } else { ffi::JXL_TRUE };
        info.num_color_channels = if img.gray { 1 } else { 3 };
        info.orientation = i32::from(img.orientation.clamp(1, 8));
        if alpha {
            info.num_extra_channels = 1;
            info.alpha_bits = u32::from(img.sample_bits);
            info.alpha_exponent_bits = 0;
            info.alpha_premultiplied = i32::from(img.premultiplied);
        }
        check(&enc, ffi::JxlEncoderSetBasicInfo(enc.0, &info), "画像情報")?;

        match &img.color {
            ColorInfo::Icc(icc) => check(&enc, ffi::JxlEncoderSetICCProfile(enc.0, icc.as_ptr(), icc.len()), "ICC")?,
            cicp => {
                let mut ce: ffi::JxlColorEncoding = std::mem::zeroed();
                ffi::JxlColorEncodingSetToSRGB(&mut ce, i32::from(img.gray));
                if *cicp == ColorInfo::DisplayP3 && !img.gray {
                    ce.primaries = ffi::JXL_PRIMARIES_P3;
                }
                check(&enc, ffi::JxlEncoderSetColorEncoding(enc.0, &ce), "色空間")?;
            }
        }

        if meta.exif.is_some() || meta.xmp.is_some() {
            check(&enc, ffi::JxlEncoderUseBoxes(enc.0), "メタデータ")?;
            if let Some(exif) = &meta.exif {
                // JPEG XL `Exif` box: 4-byte big-endian offset to the TIFF header, then the TIFF data.
                let mut payload = 0u32.to_be_bytes().to_vec();
                payload.extend_from_slice(exif);
                check(
                    &enc,
                    ffi::JxlEncoderAddBox(enc.0, b"Exif".as_ptr(), payload.as_ptr(), payload.len(), ffi::JXL_FALSE),
                    "EXIF",
                )?;
            }
            if let Some(xmp) = &meta.xmp {
                check(
                    &enc,
                    ffi::JxlEncoderAddBox(enc.0, b"xml ".as_ptr(), xmp.as_ptr(), xmp.len(), ffi::JXL_FALSE),
                    "XMP",
                )?;
            }
            ffi::JxlEncoderCloseBoxes(enc.0);
        }

        let settings = new_frame_settings(&enc, o.effort)?;
        match o.quality {
            None => check(&enc, ffi::JxlEncoderSetFrameLossless(settings, ffi::JXL_TRUE), "ロスレス設定")?,
            Some(q) => {
                let distance = ffi::JxlEncoderDistanceFromQuality(f32::from(q));
                check(&enc, ffi::JxlEncoderSetFrameDistance(settings, distance), "画質設定")?;
                if alpha {
                    // Transparency edges show every error; keep alpha exact.
                    check(&enc, ffi::JxlEncoderSetExtraChannelDistance(settings, 0, 0.0), "透過の画質設定")?;
                }
            }
        }
        let (buf, channels) = pack_channels(img, alpha);
        let format = ffi::JxlPixelFormat {
            num_channels: channels,
            data_type: if img.sample_bits == 16 { ffi::JXL_TYPE_UINT16 } else { ffi::JXL_TYPE_UINT8 },
            endianness: ffi::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        check(&enc, ffi::JxlEncoderAddImageFrame(settings, &format, buf.as_ptr().cast(), buf.len()), "画素データ")?;
        drop(buf);
        ffi::JxlEncoderCloseInput(enc.0);
    }
    drain_output(&enc)
}

// ---------- verification ----------

/// Rebuilds the original JPEG from a JPEG XL file made by [`transcode_jpeg`] (libjxl decoder).
/// Returns `Ok(None)` if the file holds no JPEG reconstruction data.
pub fn reconstruct_jpeg(jxl: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let dec = Decoder(unsafe { ffi::JxlDecoderCreate(std::ptr::null()) });
    if dec.0.is_null() {
        return Err("メモリ不足 (JxlDecoderCreate)".into());
    }
    let mut out = vec![0u8; jxl.len().saturating_mul(2).max(64 * 1024)];
    let mut is_jpeg = false;
    unsafe {
        if ffi::JxlDecoderSubscribeEvents(dec.0, ffi::JXL_DEC_JPEG_RECONSTRUCTION | ffi::JXL_DEC_FULL_IMAGE)
            != ffi::JXL_DEC_SUCCESS
        {
            return Err("JxlDecoderSubscribeEvents に失敗".into());
        }
        ffi::JxlDecoderSetInput(dec.0, jxl.as_ptr(), jxl.len());
        ffi::JxlDecoderCloseInput(dec.0);
        let mut used = 0usize;
        loop {
            match ffi::JxlDecoderProcessInput(dec.0) {
                ffi::JXL_DEC_JPEG_RECONSTRUCTION => {
                    is_jpeg = true;
                    ffi::JxlDecoderSetJPEGBuffer(dec.0, out.as_mut_ptr(), out.len());
                }
                ffi::JXL_DEC_JPEG_NEED_MORE_OUTPUT => {
                    used = out.len() - ffi::JxlDecoderReleaseJPEGBuffer(dec.0);
                    out.resize(out.len() * 2, 0);
                    ffi::JxlDecoderSetJPEGBuffer(dec.0, out.as_mut_ptr().add(used), out.len() - used);
                }
                ffi::JXL_DEC_NEED_IMAGE_OUT_BUFFER => return Ok(None),
                ffi::JXL_DEC_FULL_IMAGE => {}
                ffi::JXL_DEC_SUCCESS => {
                    if !is_jpeg {
                        return Ok(None);
                    }
                    used = out.len() - ffi::JxlDecoderReleaseJPEGBuffer(dec.0);
                    out.truncate(used);
                    return Ok(Some(out));
                }
                status => return Err(format!("JPEG の復元に失敗 (status {status})")),
            }
            let _ = used;
        }
    }
}

/// A fully decoded image (jxl-rs), stored layout, no orientation applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    pub orientation: u16,
    pub channels: u32,
    /// Interleaved samples (u16 native-endian when `bits > 8`).
    pub pixels: Vec<u8>,
    /// The image's colour profile as ICC (generated from CICP when not stored as ICC).
    pub icc: Option<Vec<u8>>,
}

/// Decodes the whole file with jxl-rs into samples as stored (original colour space and bit
/// depth, alpha as-is, orientation not applied). `gray`/`alpha` choose the channel layout.
pub fn decode_exact(data: &[u8], gray: bool, alpha: bool) -> Result<Decoded, String> {
    let mut options = JxlDecoderOptions::default();
    options.adjust_orientation = false;
    options.max_profile_level = ProfileLevel::Main10;
    let mut decoder = JxlDecoder::new(options);
    let mut input: &[u8] = data;
    let mut pixels: Vec<u8> = Vec::new();
    let mut geometry = (0usize, 0usize, 0usize); // width, height, bytes per row
    let (mut bits, mut orientation, mut channels) = (8u32, 1u16, 0u32);
    let mut icc = None;
    let mut closed = false;
    loop {
        let event = {
            let mut outputs = (!pixels.is_empty()).then(|| [JxlOutputBuffer::new(&mut pixels, geometry.1, geometry.2)]);
            decoder.process(&mut input, outputs.as_mut().map(|o| &mut o[..]), None).map_err(|e| format!("{e:?}"))?
        };
        match event {
            Event::BasicInfo => {
                icc = decoder.embedded_color_profile().and_then(|p| p.try_as_icc()).map(|c| c.into_owned());
                let info = decoder.basic_info().ok_or("画像情報がありません")?;
                let (w, h) = info.size;
                bits = info.bit_depth.bits_per_sample();
                orientation = info.orientation as u16;
                let extra = info.extra_channels.len();
                let color_type = match (gray, alpha) {
                    (true, false) => JxlColorType::Grayscale,
                    (true, true) => JxlColorType::GrayscaleAlpha,
                    (false, false) => JxlColorType::Rgb,
                    (false, true) => JxlColorType::Rgba,
                };
                let data_format = if bits > 8 {
                    JxlDataFormat::U16 { endianness: Endianness::native(), bit_depth: 16 }
                } else {
                    JxlDataFormat::U8 { bit_depth: 8 }
                };
                let bytes = data_format.bytes_per_sample();
                channels = color_type.samples_per_pixel() as u32;
                decoder
                    .set_pixel_format(JxlPixelFormat {
                        color_type,
                        color_data_format: Some(data_format),
                        extra_channel_format: vec![None; extra],
                    })
                    .map_err(|e| format!("{e:?}"))?;
                geometry = (w, h, w * channels as usize * bytes);
                pixels = vec![0u8; geometry.2 * h];
            }
            Event::FrameHeader => {}
            Event::FrameComplete { .. } | Event::Complete => break,
            Event::NeedMoreInput { .. } if !closed => {
                decoder.close_input();
                closed = true;
            }
            Event::NeedMoreInput { .. } => return Err("JPEG XL が途中で終わっています".into()),
        }
    }
    if pixels.is_empty() {
        return Err("画像が含まれていません".into());
    }
    Ok(Decoded { width: geometry.0 as u32, height: geometry.1 as u32, bits, orientation, channels, pixels, icc })
}

/// Checks a lossless encode: the decoded samples must equal the source samples exactly.
pub fn verify_lossless(jxl: &[u8], img: &SourceImage) -> Result<(), String> {
    let alpha = img.alpha_used();
    let decoded = decode_exact(jxl, img.gray, alpha)?;
    if (decoded.width, decoded.height) != (img.width, img.height) {
        return Err(format!("サイズが一致しません: {}x{}", decoded.width, decoded.height));
    }
    if decoded.orientation != img.orientation {
        return Err(format!("向きが一致しません: {}", decoded.orientation));
    }
    let (expected, _) = pack_channels(img, alpha);
    if decoded.pixels != expected {
        let first = decoded.pixels.iter().zip(&expected).position(|(a, b)| a != b).unwrap_or(0);
        return Err(format!("画素が一致しません (最初の差: バイト {first})"));
    }
    Ok(())
}

/// Checks a lossy encode: same size and orientation, decoded samples close to the source (PSNR at
/// least [`LOSSY_MIN_PSNR`]) and channel means kept (within [`LOSSY_MAX_MEAN_SHIFT`]). Colours
/// under fully transparent pixels are not stored by a lossy encode and are not compared.
/// Returns the PSNR in dB.
pub fn verify_lossy(jxl: &[u8], img: &SourceImage) -> Result<f64, String> {
    let alpha = img.alpha_used();
    let decoded = decode_exact(jxl, img.gray, alpha)?;
    if (decoded.width, decoded.height) != (img.width, img.height) {
        return Err(format!("サイズが一致しません: {}x{}", decoded.width, decoded.height));
    }
    if decoded.orientation != img.orientation {
        return Err(format!("向きが一致しません: {}", decoded.orientation));
    }
    let (expected, _) = pack_channels(img, alpha);
    if decoded.pixels.len() != expected.len() {
        return Err(format!("画素数が一致しません ({} / {} バイト)", decoded.pixels.len(), expected.len()));
    }
    let d = compare(&decoded.pixels, &expected, img.sample_bits == 16, decoded.channels as usize, alpha);
    if d.psnr < LOSSY_MIN_PSNR {
        return Err(format!("元の画像と大きく異なります (PSNR {:.1} dB)", d.psnr));
    }
    if d.mean_shift > LOSSY_MAX_MEAN_SHIFT {
        return Err(format!("色が元の画像からずれています (平均値の差 {:.1}%)", d.mean_shift * 100.0));
    }
    Ok(d.psnr)
}

/// How far a decoded image is from its source.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Difference {
    /// Peak signal-to-noise ratio in dB (infinite when identical).
    psnr: f64,
    /// Largest difference of a channel mean, as a fraction of full scale.
    mean_shift: f64,
}

/// Compares two equally laid out interleaved buffers (u8, or native u16) with `channels` samples
/// per pixel. With `alpha` (the last channel), only alpha is compared where the source pixel is
/// fully transparent.
fn compare(a: &[u8], b: &[u8], wide: bool, channels: usize, alpha: bool) -> Difference {
    let bytes = if wide { 2 } else { 1 };
    let peak: f64 = if wide { 65535.0 } else { 255.0 };
    let sample = |px: &[u8], c: usize| -> f64 {
        if wide { f64::from(u16::from_ne_bytes([px[2 * c], px[2 * c + 1]])) } else { f64::from(px[c]) }
    };
    let (mut squared, mut count) = (0f64, 0usize);
    let mut sums = vec![(0f64, 0f64, 0usize); channels]; // per channel: Σa, Σb, n
    for (pa, pb) in a.chunks_exact(channels * bytes).zip(b.chunks_exact(channels * bytes)) {
        let visible = !alpha || sample(pb, channels - 1) > 0.0;
        for (c, sum) in sums.iter_mut().enumerate() {
            if visible || c == channels - 1 {
                let (x, y) = (sample(pa, c), sample(pb, c));
                squared += (x - y) * (x - y);
                count += 1;
                *sum = (sum.0 + x, sum.1 + y, sum.2 + 1);
            }
        }
    }
    let psnr = if squared == 0.0 || count == 0 {
        f64::INFINITY
    } else {
        10.0 * (peak * peak / (squared / count as f64)).log10()
    };
    let mean_shift =
        sums.iter().filter(|s| s.2 > 0).map(|&(x, y, n)| ((x - y) / n as f64).abs() / peak).fold(0.0, f64::max);
    Difference { psnr, mean_shift }
}

/// Checks a JPEG recompression: the rebuilt JPEG must equal the original byte for byte, and
/// jxl-rs must be able to decode the image.
pub fn verify_jpeg(jxl: &[u8], original: &[u8]) -> Result<(), String> {
    match reconstruct_jpeg(jxl)? {
        None => return Err("JPEG 復元情報がありません".into()),
        Some(rebuilt) if rebuilt != original => {
            return Err(format!("復元した JPEG が元と一致しません ({} / {} バイト)", rebuilt.len(), original.len()));
        }
        Some(_) => {}
    }
    decode_exact(jxl, false, false).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, sample_bits: u8, alpha: bool, gray: bool) -> SourceImage {
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = (x * 255 / w.max(1)) as u16;
                let (r, g, b) = if gray { (v, v, v) } else { (v, (y * 255 / h.max(1)) as u16, ((x * y) % 256) as u16) };
                let a = if alpha { ((x + y) % 256) as u16 } else { 255 };
                for s in [r, g, b, a] {
                    if sample_bits == 16 {
                        pixels.extend_from_slice(&((s << 8) | (x as u16 & 0xFF)).to_ne_bytes());
                    } else {
                        pixels.push(s as u8);
                    }
                }
            }
        }
        SourceImage {
            width: w,
            height: h,
            sample_bits,
            pixels,
            has_alpha: alpha,
            premultiplied: false,
            gray,
            color: ColorInfo::Srgb,
            orientation: 1,
        }
    }

    const O: EncodeOptions = EncodeOptions { effort: 3, threads: 2, quality: None };

    #[test]
    fn version_is_0_13() {
        assert!(libjxl_version().starts_with("0.13."), "{}", libjxl_version());
    }

    #[test]
    fn lossless_roundtrips_exactly() {
        for (bits, alpha, gray, orientation) in [
            (8, false, false, 1),
            (8, true, false, 6),
            (16, false, false, 3),
            (16, true, false, 1),
            (8, false, true, 8),
        ] {
            let mut img = gradient(67, 45, bits, alpha, gray);
            img.orientation = orientation;
            let jxl = encode_pixels(&img, &Metadata::default(), &O).unwrap();
            verify_lossless(&jxl, &img).unwrap_or_else(|e| panic!("{bits} {alpha} {gray}: {e}"));
        }
    }

    #[test]
    fn verification_detects_a_changed_pixel() {
        let img = gradient(16, 16, 8, false, false);
        let jxl = encode_pixels(&img, &Metadata::default(), &O).unwrap();
        let mut other = img.clone();
        other.pixels[100] ^= 1;
        assert!(verify_lossless(&jxl, &other).is_err());
    }

    #[test]
    fn keeps_icc_exif_and_xmp() {
        let mut img = gradient(24, 24, 8, false, false);
        img.color = ColorInfo::DisplayP3;
        let meta = Metadata {
            exif: Some(crate::exif::tests::sample_tiff(false, 1)),
            xmp: Some(b"<x:xmpmeta xmlns:x='adobe:ns:meta/'>hello-xmp</x:xmpmeta>".to_vec()),
        };
        let jxl = encode_pixels(&img, &meta, &O).unwrap();
        let has = |needle: &[u8]| jxl.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"hello-xmp"));
        assert!(has(b"Cam\0"));
        verify_lossless(&jxl, &img).unwrap();
        assert_eq!(reconstruct_jpeg(&jxl).unwrap(), None);
    }

    /// Smooth content (like a photo) for the lossy tests; `gradient`'s patterns are noise-like.
    fn smooth(w: u32, h: u32, sample_bits: u8, alpha: bool, gray: bool) -> SourceImage {
        let mut img = gradient(w, h, sample_bits, alpha, gray);
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let fx = f64::from(x) / f64::from(w);
                let fy = f64::from(y) / f64::from(h);
                let r = 0.5 + 0.4 * (fx * 6.0).sin() * (fy * 3.0).cos();
                let (g, b) = if gray { (r, r) } else { (0.3 + 0.6 * fy, 0.5 + 0.3 * (fx + fy).cos()) };
                let a = if alpha { 0.4 + 0.6 * fx } else { 1.0 };
                for v in [r, g, b, a] {
                    if sample_bits == 16 {
                        pixels.extend_from_slice(&((v * 65535.0).round() as u16).to_ne_bytes());
                    } else {
                        pixels.push((v * 255.0).round() as u8);
                    }
                }
            }
        }
        img.pixels = pixels;
        img
    }

    #[test]
    fn lossy_is_close_and_keeps_layout() {
        for (bits, alpha, gray, orientation, color) in [
            (8, false, false, 1, ColorInfo::Srgb),
            (8, true, false, 6, ColorInfo::Srgb),
            (16, false, false, 3, ColorInfo::DisplayP3),
            (8, false, true, 8, ColorInfo::Srgb),
        ] {
            let mut img = smooth(96, 64, bits, alpha, gray);
            img.orientation = orientation;
            img.color = color;
            for quality in [70, 80, 90] {
                let o = EncodeOptions { quality: Some(quality), ..O };
                let jxl = encode_pixels(&img, &Metadata::default(), &o).unwrap();
                let psnr = verify_lossy(&jxl, &img).unwrap_or_else(|e| panic!("{bits} {alpha} {gray} q{quality}: {e}"));
                assert!(psnr.is_finite(), "q{quality} should not be lossless");
            }
        }
    }

    #[test]
    fn lossy_verification_rejects_a_different_image() {
        let img = smooth(64, 64, 8, false, false);
        let jxl = encode_pixels(&img, &Metadata::default(), &EncodeOptions { quality: Some(90), ..O }).unwrap();
        let mut inverted = img.clone();
        inverted.pixels.iter_mut().step_by(4).for_each(|r| *r = 255 - *r);
        assert!(verify_lossy(&jxl, &inverted).is_err());
        // Swapping red and blue keeps every value plausible but moves the channel means.
        let mut swapped = img.clone();
        swapped.pixels.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.swap(0, 2));
        assert!(verify_lossy(&jxl, &swapped).is_err());
    }

    #[test]
    fn psnr_of_known_buffers() {
        let psnr = |a: &[u8], b: &[u8], ch, alpha| compare(a, b, false, ch, alpha).psnr;
        assert_eq!(psnr(&[10, 20], &[10, 20], 1, false), f64::INFINITY);
        // One sample off by 255 out of two: MSE = 255² / 2 → 10·log10(2) dB.
        assert!((psnr(&[0, 7], &[255, 7], 1, false) - 10.0 * 2f64.log10()).abs() < 1e-9);
        // Gray+alpha: the colour of a fully transparent pixel is ignored, its alpha is not.
        assert_eq!(psnr(&[200, 0, 50, 255], &[0, 0, 50, 255], 2, true), f64::INFINITY);
        assert!(psnr(&[0, 9, 50, 255], &[0, 0, 50, 255], 2, true).is_finite());
        // Mean shift: 51 out of 255 on the only channel = 20%.
        assert!((compare(&[51, 51], &[0, 0], false, 1, false).mean_shift - 0.2).abs() < 1e-9);
    }
}
