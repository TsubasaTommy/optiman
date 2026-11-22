//! Windows Imaging Component: exact decoding (no colour transform, no rotation), PNG writing
//! for `restore`, and detection of a JPEG XL viewer (the "JPEG XL Image Extension" WIC codec).

use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HGLOBAL, WINCODEC_ERR_COMPONENTINITIALIZEFAILURE, WINCODEC_ERR_COMPONENTNOTFOUND};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppCMYK,
    GUID_WICPixelFormat32bppPBGRA, GUID_WICPixelFormat32bppPRGBA, GUID_WICPixelFormat32bppRGBA,
    GUID_WICPixelFormat40bppCMYKAlpha, GUID_WICPixelFormat64bppCMYK, GUID_WICPixelFormat64bppPBGRA,
    GUID_WICPixelFormat64bppPRGBA, GUID_WICPixelFormat64bppRGBA, GUID_WICPixelFormat80bppCMYKAlpha, IWICBitmapDecoder,
    IWICBitmapFrameDecode, IWICColorContext, IWICImagingFactory, IWICPalette, IWICPixelFormatInfo2, IWICStream,
    WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom, WICColorContextProfile,
    WICDecodeMetadataCacheOnDemand, WICPixelFormatNumericRepresentationFixed, WICPixelFormatNumericRepresentationFloat,
    WICPixelFormatNumericRepresentationIndexed,
};
use windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, GetHGlobalFromStream, PROPVARIANT, PropVariantClear,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Variant::VT_UI2;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, Interface, PCWSTR, w};

use super::ensure_com;
use crate::image::{ColorInfo, SourceImage};
use crate::jxl::EncodeError;
use crate::{exif, metadata};

/// Microsoft Store product id of the free "JPEG XL Image Extension".
pub const JXL_EXTENSION_STORE_ID: &str = "9MZPRTH5C0TB";

fn failed(e: windows::core::Error) -> EncodeError {
    EncodeError::Failed(format!("WIC: {}", e.message()))
}

fn factory() -> windows::core::Result<IWICImagingFactory> {
    ensure_com();
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

struct Opened {
    factory: IWICImagingFactory,
    _stream: IWICStream,
    decoder: IWICBitmapDecoder,
}

fn open(data: &[u8]) -> windows::core::Result<Opened> {
    let factory = factory()?;
    unsafe {
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(data)?;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        Ok(Opened { factory, _stream: stream, decoder })
    }
}

fn orientation_from_wic(frame: &IWICBitmapFrameDecode) -> Option<u16> {
    unsafe {
        let reader = frame.GetMetadataQueryReader().ok()?;
        let mut pv = PROPVARIANT::default();
        reader.GetMetadataByName(w!("System.Photo.Orientation"), &mut pv).ok()?;
        let value = (pv.Anonymous.Anonymous.vt == VT_UI2).then(|| pv.Anonymous.Anonymous.Anonymous.uiVal);
        let _ = PropVariantClear(&mut pv);
        value.filter(|v| (1..=8).contains(v))
    }
}

/// The frame's colour profile: CICP for sRGB/Display P3, otherwise the ICC bytes as stored.
fn color_info(factory: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> ColorInfo {
    unsafe {
        let mut count = 0u32;
        if frame.GetColorContexts(&mut [], &mut count).is_err() || count == 0 {
            return ColorInfo::Srgb;
        }
        let mut contexts: Vec<Option<IWICColorContext>> =
            (0..count).map(|_| factory.CreateColorContext().ok()).collect();
        if contexts.iter().any(Option::is_none) || frame.GetColorContexts(&mut contexts, &mut count).is_err() {
            return ColorInfo::Srgb;
        }
        let Some(Some(ctx)) = contexts.into_iter().next() else { return ColorInfo::Srgb };
        if ctx.GetType().ok() != Some(WICColorContextProfile) {
            return ColorInfo::Srgb; // EXIF colour space tag: sRGB or uncalibrated
        }
        let mut len = 0u32;
        let _ = ctx.GetProfileBytes(&mut [], &mut len);
        let mut icc = vec![0u8; len as usize];
        if len == 0 || ctx.GetProfileBytes(&mut icc, &mut len).is_err() {
            return ColorInfo::Srgb;
        }
        if metadata::icc_mentions(&icc, "Display P3") {
            ColorInfo::DisplayP3
        } else if metadata::icc_mentions(&icc, "sRGB IEC61966-2.1") {
            ColorInfo::Srgb
        } else {
            ColorInfo::Icc(icc)
        }
    }
}

const CMYK: [GUID; 4] = [
    GUID_WICPixelFormat32bppCMYK,
    GUID_WICPixelFormat64bppCMYK,
    GUID_WICPixelFormat40bppCMYKAlpha,
    GUID_WICPixelFormat80bppCMYKAlpha,
];
const PREMULTIPLIED: [GUID; 4] = [
    GUID_WICPixelFormat32bppPBGRA,
    GUID_WICPixelFormat32bppPRGBA,
    GUID_WICPixelFormat64bppPBGRA,
    GUID_WICPixelFormat64bppPRGBA,
];

/// Decodes the first frame exactly as stored: original orientation, colour space and bit depth.
pub fn decode(data: &[u8]) -> Result<SourceImage, EncodeError> {
    let opened = open(data).map_err(|e| {
        let no_codec =
            e.code() == WINCODEC_ERR_COMPONENTNOTFOUND || e.code() == WINCODEC_ERR_COMPONENTINITIALIZEFAILURE;
        match () {
            () if no_codec && data.get(8..12) == Some(b"WEBP") => EncodeError::Unsupported(
                "WebP のデコーダがありません (Microsoft Store の「WebP 画像拡張機能」が必要です)".into(),
            ),
            () if no_codec => EncodeError::Unsupported("この形式のデコーダがありません".into()),
            () => EncodeError::Unsupported(format!("画像として読み込めません ({})", e.message())),
        }
    })?;
    unsafe {
        if opened.decoder.GetFrameCount().map_err(failed)? != 1 {
            return Err(EncodeError::Unsupported("複数フレームの画像 (アニメーション等) は対象外です".into()));
        }
        let frame = opened.decoder.GetFrame(0).map_err(failed)?;
        let orientation = orientation_from_wic(&frame)
            .or_else(|| metadata::extract_exif(data).and_then(|e| exif::read_orientation(&e)))
            .unwrap_or(1);
        let format = frame.GetPixelFormat().map_err(failed)?;
        if CMYK.contains(&format) {
            return Err(EncodeError::Unsupported("CMYK 画像は劣化なしに変換できないため対象外です".into()));
        }
        let info: IWICPixelFormatInfo2 =
            opened.factory.CreateComponentInfo(&format).and_then(|c| c.cast()).map_err(failed)?;
        let numeric = info.GetNumericRepresentation().map_err(failed)?;
        if numeric == WICPixelFormatNumericRepresentationFloat || numeric == WICPixelFormatNumericRepresentationFixed {
            return Err(EncodeError::Unsupported("浮動小数点 (HDR) 画像は劣化なしに変換できないため対象外です".into()));
        }
        let bpp = info.GetBitsPerPixel().map_err(failed)?;
        let channels = info.GetChannelCount().map_err(failed)?.max(1);
        let indexed = numeric == WICPixelFormatNumericRepresentationIndexed;
        let has_alpha = info.SupportsTransparency().map_err(failed)?.as_bool();
        let gray = !indexed && channels <= 2;
        let bits_per_channel = if indexed { 8 } else { bpp / channels };
        let sample_bits: u8 = if bits_per_channel > 8 { 16 } else { 8 };
        let premultiplied = PREMULTIPLIED.contains(&format);
        let target = match (sample_bits, premultiplied) {
            (8, false) => GUID_WICPixelFormat32bppRGBA,
            (8, true) => GUID_WICPixelFormat32bppPRGBA,
            (_, false) => GUID_WICPixelFormat64bppRGBA,
            (_, true) => GUID_WICPixelFormat64bppPRGBA,
        };
        let conv = opened.factory.CreateFormatConverter().map_err(failed)?;
        conv.Initialize(
            &frame,
            &target,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(|e| EncodeError::Unsupported(format!("ピクセル形式を変換できません ({})", e.message())))?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).map_err(failed)?;
        if w == 0 || h == 0 {
            return Err(EncodeError::Unsupported("画像サイズが 0 です".into()));
        }
        let stride = w * 4 * u32::from(sample_bits / 8);
        let mut pixels = vec![0u8; stride as usize * h as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut pixels).map_err(failed)?;
        Ok(SourceImage {
            width: w,
            height: h,
            sample_bits,
            pixels,
            has_alpha,
            premultiplied,
            gray,
            color: color_info(&opened.factory, &frame),
            orientation,
        })
    }
}

/// Whether Windows can display JPEG XL: probe the WIC codec with a tiny JPEG XL file.
pub fn viewer_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let img = SourceImage {
            width: 8,
            height: 8,
            sample_bits: 8,
            pixels: vec![128; 8 * 8 * 4],
            has_alpha: false,
            premultiplied: false,
            gray: false,
            color: ColorInfo::Srgb,
            orientation: 1,
        };
        let opts = crate::jxl::EncodeOptions { effort: 1, threads: 1 };
        crate::jxl::encode_lossless(&img, &crate::jxl::Metadata::default(), &opts)
            .is_ok_and(|jxl| open(&jxl).and_then(|o| unsafe { o.decoder.GetFrame(0) }).is_ok())
    })
}

/// The Windows build number (e.g. 26100 for Windows 11 24H2), from the registry because
/// `GetVersionEx` reports a compatibility version.
pub fn os_build() -> u32 {
    let mut buf = [0u16; 16];
    let mut size = size_of_val(&buf) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .is_ok();
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if ok { String::from_utf16_lossy(&buf[..end]).parse().unwrap_or(0) } else { 0 }
}

/// The JPEG XL Image Extension needs Windows 11 24H2 (build 26100) or later.
pub fn viewer_extension_supported() -> bool {
    os_build() >= 26100
}

pub fn viewer_hint() -> Option<String> {
    if viewer_available() {
        None
    } else if viewer_extension_supported() {
        Some("JPEG XL をエクスプローラーやフォトで表示するには、Microsoft 公式の無料拡張「JPEG XL Image Extension」が必要です。設定画面の「拡張機能をインストール」から入れられます。".into())
    } else {
        Some("この Windows では JPEG XL を標準のアプリで表示できません (Windows 11 24H2 以降が必要)。変換は通常どおり行われ、`restore` でいつでも元に戻せます。".into())
    }
}

/// Installs the JPEG XL Image Extension from the Microsoft Store through winget (the user asked
/// for it by pressing the button). Returns whether winget reported success.
/// Gives up after 10 minutes so an installer never hangs on the Store.
pub fn install_viewer_extension() -> bool {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const TIMEOUT: Duration = Duration::from_secs(600);
    let spawn = |exe: &OsStr| {
        Command::new(exe)
            .args(["install", "--id", JXL_EXTENSION_STORE_ID, "--source", "msstore", "--exact"])
            .args(["--accept-package-agreements", "--accept-source-agreements", "--silent"])
            .arg("--disable-interactivity")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    };
    // winget is an app execution alias in %LOCALAPPDATA%\Microsoft\WindowsApps (normally on PATH).
    let alias = std::env::var_os("LOCALAPPDATA")
        .map(|d| std::path::PathBuf::from(d).join("Microsoft\\WindowsApps\\winget.exe"));
    let child = spawn(OsStr::new("winget")).or_else(|e| match &alias {
        Some(a) => spawn(a.as_os_str()),
        None => Err(e),
    });
    let Ok(mut child) = child else { return false };
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(500)),
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// Opens the extension's page in the Microsoft Store app.
pub fn open_store_page() {
    let url: Vec<u16> = format!("ms-windows-store://pdp/?ProductId={JXL_EXTENSION_STORE_ID}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Writes decoded RGBA samples as PNG with WIC, keeping the exact values and colour profile.
pub fn encode_png(img: &crate::jxl::Decoded) -> Result<Vec<u8>, String> {
    if img.channels != 4 {
        return Err("RGBA 以外は PNG に書き出せません".into());
    }
    let msg = |e: windows::core::Error| e.message();
    unsafe {
        let factory = factory().map_err(msg)?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).map_err(msg)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).map_err(msg)?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).map_err(msg)?;
        let mut frame = None;
        let mut props = None;
        encoder.CreateNewFrame(&mut frame, &mut props).map_err(msg)?;
        let frame = frame.ok_or("PNG フレームを作成できません")?;
        frame.Initialize(props.as_ref()).map_err(msg)?;
        frame.SetSize(img.width, img.height).map_err(msg)?;
        // 8-bit PNG frames take BGRA; swizzle so the stored values stay identical.
        let (mut format, pixels) = if img.bits > 8 {
            (GUID_WICPixelFormat64bppRGBA, img.pixels.clone())
        } else {
            let mut bgra = img.pixels.clone();
            for px in bgra.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            (GUID_WICPixelFormat32bppBGRA, bgra)
        };
        let wanted = format;
        frame.SetPixelFormat(&mut format).map_err(msg)?;
        if format != wanted {
            return Err("PNG エンコーダが画素形式を受け付けません".into());
        }
        if let Some(icc) = &img.icc {
            let ctx = factory.CreateColorContext().map_err(msg)?;
            ctx.InitializeFromMemory(icc).map_err(msg)?;
            let _ = frame.SetColorContexts(&[Some(ctx)]);
        }
        let stride = img.width * 4 * if img.bits > 8 { 2 } else { 1 };
        frame.WritePixels(img.height, stride, &pixels).map_err(msg)?;
        frame.Commit().map_err(msg)?;
        encoder.Commit().map_err(msg)?;
        let hglobal = GetHGlobalFromStream(&stream).map_err(msg)?;
        let size = GlobalSize(hglobal);
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            return Err("PNG データを取得できません".into());
        }
        let out = std::slice::from_raw_parts(ptr.cast::<u8>(), size).to_vec();
        let _ = GlobalUnlock(hglobal);
        Ok(out)
    }
}
