//! Windows Imaging Component: decode with orientation and colour management, and AVIF
//! verification when an AVIF codec (AV1 Video Extension) is installed.

use windows::Win32::Foundation::WINCODEC_ERR_COMPONENTNOTFOUND;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICBitmapDecoder, IWICBitmapFrameDecode, IWICBitmapSource,
    IWICColorContext, IWICImagingFactory, IWICPalette, IWICStream, WICBitmapCacheOnLoad, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeCustom, WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical,
    WICBitmapTransformOptions, WICBitmapTransformRotate90, WICBitmapTransformRotate180, WICBitmapTransformRotate270,
    WICColorContextProfile, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{Interface, w};

use super::ensure_com;
use crate::encode::{DecodedImage, EncodeError, Encoded};
use crate::platform::Primaries;
use crate::{exif, metadata};

pub fn native_encoder_available() -> bool {
    false
}

pub fn native_encode(_data: &[u8], _quality: u8) -> Result<Encoded, EncodeError> {
    Err(EncodeError::Failed("この OS には AVIF エンコーダがありません".into()))
}

fn failed(e: windows::core::Error) -> EncodeError {
    EncodeError::Failed(format!("WIC: {}", e.message()))
}

fn factory() -> Result<IWICImagingFactory, EncodeError> {
    ensure_com();
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.map_err(failed)
}

struct Opened {
    factory: IWICImagingFactory,
    _stream: IWICStream,
    decoder: IWICBitmapDecoder,
}

fn open(data: &[u8]) -> Result<Opened, windows::core::Error> {
    let factory = factory().map_err(|_| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
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

fn transform_for(orientation: u16) -> WICBitmapTransformOptions {
    let combine = |a: WICBitmapTransformOptions, b: WICBitmapTransformOptions| WICBitmapTransformOptions(a.0 | b.0);
    match orientation {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => combine(WICBitmapTransformRotate90, WICBitmapTransformFlipHorizontal),
        6 => WICBitmapTransformRotate90,
        7 => combine(WICBitmapTransformRotate270, WICBitmapTransformFlipHorizontal),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformOptions(0),
    }
}

/// Display P3 sources keep their values (signalled as P3 in the AVIF); other ICC profiles are
/// converted to sRGB; untagged images are assumed to be sRGB.
fn color_managed(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
) -> Result<(IWICBitmapSource, Primaries), windows::core::Error> {
    let plain = || frame.cast::<IWICBitmapSource>();
    unsafe {
        let mut count = 0u32;
        if frame.GetColorContexts(&mut [], &mut count).is_err() || count == 0 {
            return Ok((plain()?, Primaries::Srgb));
        }
        let mut contexts: Vec<Option<IWICColorContext>> =
            (0..count).map(|_| factory.CreateColorContext().ok()).collect();
        if contexts.iter().any(Option::is_none) || frame.GetColorContexts(&mut contexts, &mut count).is_err() {
            return Ok((plain()?, Primaries::Srgb));
        }
        let Some(Some(ctx)) = contexts.into_iter().next() else { return Ok((plain()?, Primaries::Srgb)) };
        if ctx.GetType().ok() != Some(WICColorContextProfile) {
            return Ok((plain()?, Primaries::Srgb));
        }
        let mut len = 0u32;
        let _ = ctx.GetProfileBytes(&mut [], &mut len);
        let mut icc = vec![0u8; len as usize];
        if len == 0 || ctx.GetProfileBytes(&mut icc, &mut len).is_err() {
            return Ok((plain()?, Primaries::Srgb));
        }
        if metadata::icc_mentions(&icc, "Display P3") {
            return Ok((plain()?, Primaries::DisplayP3));
        }
        if metadata::icc_mentions(&icc, "sRGB") {
            return Ok((plain()?, Primaries::Srgb));
        }
        let srgb = factory.CreateColorContext()?;
        srgb.InitializeFromExifColorSpace(1)?;
        let transform = factory.CreateColorTransformer()?;
        match transform.Initialize(frame, &ctx, &srgb, &GUID_WICPixelFormat32bppRGBA) {
            Ok(()) => Ok((transform.cast()?, Primaries::Srgb)),
            Err(e) => {
                crate::warn!("色変換できないため色補正なしで変換します ({})", e.message());
                Ok((plain()?, Primaries::Srgb))
            }
        }
    }
}

pub fn decode(data: &[u8]) -> Result<DecodedImage, EncodeError> {
    let opened = open(data).map_err(|e| {
        if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
            EncodeError::Unsupported(
                "この形式のデコーダがありません (HEIC/WebP は Microsoft Store の拡張機能が必要な場合があります)".into(),
            )
        } else {
            EncodeError::Unsupported(format!("画像として読み込めません ({})", e.message()))
        }
    })?;
    unsafe {
        let frames = opened.decoder.GetFrameCount().map_err(failed)?;
        if frames != 1 {
            return Err(EncodeError::Unsupported("複数フレームの画像 (アニメーション等) は対象外です".into()));
        }
        let frame = opened.decoder.GetFrame(0).map_err(failed)?;
        let orientation = orientation_from_wic(&frame)
            .or_else(|| metadata::extract_exif(data).and_then(|e| exif::read_orientation(&e)))
            .unwrap_or(1);
        let f = &opened.factory;
        let (source, primaries) = color_managed(f, &frame).map_err(failed)?;
        let conv = f.CreateFormatConverter().map_err(failed)?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(|e| EncodeError::Unsupported(format!("ピクセル形式を変換できません ({})", e.message())))?;
        let bitmap = f.CreateBitmapFromSource(&conv, WICBitmapCacheOnLoad).map_err(failed)?;
        let oriented: IWICBitmapSource = if orientation == 1 {
            bitmap.cast().map_err(failed)?
        } else {
            let rot = f.CreateBitmapFlipRotator().map_err(failed)?;
            rot.Initialize(&bitmap, transform_for(orientation)).map_err(failed)?;
            rot.cast().map_err(failed)?
        };
        let (mut w, mut h) = (0u32, 0u32);
        oriented.GetSize(&mut w, &mut h).map_err(failed)?;
        if w == 0 || h == 0 {
            return Err(EncodeError::Unsupported("画像サイズが 0 です".into()));
        }
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        oriented.CopyPixels(std::ptr::null(), w * 4, &mut rgba).map_err(failed)?;
        Ok(DecodedImage { width: w, height: h, rgba, primaries })
    }
}

/// Fully decodes the AVIF with WIC. `None` when no AVIF codec is installed.
pub fn verify_decode(avif: &[u8]) -> Option<Result<(u32, u32), String>> {
    let opened = match open(avif) {
        Ok(o) => o,
        Err(e) if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND => return None,
        Err(e) => return Some(Err(e.message())),
    };
    Some(unsafe {
        (|| -> windows::core::Result<(u32, u32)> {
            let frame = opened.decoder.GetFrame(0)?;
            let conv = opened.factory.CreateFormatConverter()?;
            conv.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let (mut w, mut h) = (0u32, 0u32);
            conv.GetSize(&mut w, &mut h)?;
            let mut buf = vec![0u8; w as usize * h as usize * 4];
            conv.CopyPixels(std::ptr::null(), w * 4, &mut buf)?;
            Ok((w, h))
        })()
        .map_err(|e| e.message())
    })
}
