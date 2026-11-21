//! Built-in AV1 still-image encoder: rav1e (single-threaded, no rayon pool) + avif-serialize.
//!
//! Speed tweaks and the quality→quantizer curve follow ravif (BSD-3-Clause, Kornel Lesiński),
//! but colour primaries are signalled per image so Display P3 photos keep their gamut.

use avif_serialize::Aviffy;
use avif_serialize::constants::{
    ColorPrimaries as AvifPrimaries, MatrixCoefficients as AvifMatrix, TransferCharacteristics as AvifTransfer,
};
use rav1e::data::FrameType;
use rav1e::prelude::*;

use super::{DecodedImage, EncodeError, Encoded};
use crate::platform::Primaries;

const BIT_DEPTH: usize = 10;
/// BT.601 luma coefficients (Kr, Kg, Kb), as used by ravif/libavif defaults.
const BT601: [f32; 3] = [0.2990, 0.5870, 0.1140];

pub fn quality_to_quantizer(quality: u8) -> usize {
    let q = f32::from(quality.clamp(1, 100)) / 100.0;
    let x = if q >= 0.82 {
        (1.0 - q) * 2.6
    } else if q > 0.25 {
        q.mul_add(-0.5, 1.0 - 0.125)
    } else {
        1.0 - q
    };
    (x * 255.0).round() as usize
}

pub fn encode(img: &DecodedImage, exif: Option<Vec<u8>>, quality: u8, speed: u8) -> Result<Encoded, EncodeError> {
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 || img.rgba.len() != w * h * 4 {
        return Err(EncodeError::Unsupported("画像サイズが不正".into()));
    }
    let primaries = match img.primaries {
        Primaries::Srgb => (ColorPrimaries::BT709, AvifPrimaries::Bt709),
        Primaries::DisplayP3 => (ColorPrimaries::SMPTE432, AvifPrimaries::DisplayP3),
    };
    let quantizer = quality_to_quantizer(quality);
    let speed = speed.clamp(1, 10);

    let color = encode_plane_set(
        w,
        h,
        quantizer,
        speed,
        ChromaSampling::Cs444,
        Some(ColorDescription {
            color_primaries: primaries.0,
            transfer_characteristics: TransferCharacteristics::SRGB,
            matrix_coefficients: MatrixCoefficients::BT601,
        }),
        |frame| fill_ycbcr(frame, img),
    )?;

    let alpha = if img.has_alpha() {
        // Alpha edges are very visible; encode it a bit higher than the colour quality.
        let aq = quality_to_quantizer(quality.saturating_add(5).min(100));
        Some(encode_plane_set(w, h, aq, speed, ChromaSampling::Cs400, None, |frame| fill_alpha(frame, img))?)
    } else {
        None
    };

    let mut avif = Aviffy::new();
    avif.set_color_primaries(primaries.1)
        .set_transfer_characteristics(AvifTransfer::Srgb)
        .set_matrix_coefficients(AvifMatrix::Bt601)
        .set_full_color_range(true)
        .set_chroma_subsampling((false, false));
    if let Some(exif) = exif {
        avif.set_exif(exif);
    }
    let file = avif.to_vec(&color, alpha.as_deref(), img.width, img.height, BIT_DEPTH as u8);
    Ok(Encoded { avif: file, width: img.width, height: img.height })
}

#[inline]
fn to_10bit(v: f32) -> u16 {
    v.round().clamp(0.0, 1023.0) as u16
}

fn fill_ycbcr(frame: &mut Frame<u16>, img: &DecodedImage) {
    let w = img.width as usize;
    let [kr, kg, kb] = BT601;
    let scale = 1023.0 / 255.0;
    let mut planes = frame.planes.iter_mut();
    let (Some(py), Some(pu), Some(pv)) = (planes.next(), planes.next(), planes.next()) else { return };
    let mut ys = py.mut_slice(Default::default());
    let mut us = pu.mut_slice(Default::default());
    let mut vs = pv.mut_slice(Default::default());
    let rows = img.rgba.chunks_exact(w * 4);
    for (((src, yr), ur), vr) in rows.zip(ys.rows_iter_mut()).zip(us.rows_iter_mut()).zip(vs.rows_iter_mut()) {
        for (x, px) in src.as_chunks::<4>().0.iter().enumerate() {
            let (r, g, b) = (f32::from(px[0]) * scale, f32::from(px[1]) * scale, f32::from(px[2]) * scale);
            let y = kr * r + kg * g + kb * b;
            yr[x] = to_10bit(y);
            ur[x] = to_10bit((b - y) * (0.5 / (1.0 - kb)) + 512.0);
            vr[x] = to_10bit((r - y) * (0.5 / (1.0 - kr)) + 512.0);
        }
    }
}

fn fill_alpha(frame: &mut Frame<u16>, img: &DecodedImage) {
    let w = img.width as usize;
    let mut ys = frame.planes[0].mut_slice(Default::default());
    for (src, yr) in img.rgba.chunks_exact(w * 4).zip(ys.rows_iter_mut()) {
        for (x, px) in src.as_chunks::<4>().0.iter().enumerate() {
            let a = u16::from(px[3]);
            yr[x] = (a << 2) | (a >> 6);
        }
    }
}

fn speed_settings(speed: u8, quantizer: usize) -> SpeedSettings {
    let high_quality = quantizer < quality_to_quantizer(80);
    let mut s = SpeedSettings::from_preset(speed);
    s.multiref = false;
    s.rdo_lookahead_frames = 1;
    s.scene_detection_mode = SceneDetectionSpeed::None;
    s.motion.include_near_mvs = false;
    let (min, max) = match speed {
        0..=4 => (BlockSize::BLOCK_4X4, BlockSize::BLOCK_16X16),
        5..=8 => (BlockSize::BLOCK_8X8, BlockSize::BLOCK_16X16),
        _ => (BlockSize::BLOCK_16X16, BlockSize::BLOCK_16X16),
    };
    s.partition.partition_range = PartitionRange::new(min, max);
    s.prediction.prediction_modes =
        if speed <= 1 { PredictionModesSetting::ComplexAll } else { PredictionModesSetting::Simple };
    s.transform.rdo_tx_decision = speed <= 4 && !high_quality;
    s.prediction.fine_directional_intra = speed <= 6;
    s.fast_deblock = speed >= 7 && !high_quality;
    s
}

fn encode_plane_set(
    width: usize,
    height: usize,
    quantizer: usize,
    speed: u8,
    chroma_sampling: ChromaSampling,
    color_description: Option<ColorDescription>,
    fill: impl FnOnce(&mut Frame<u16>),
) -> Result<Vec<u8>, EncodeError> {
    let enc = EncoderConfig {
        width,
        height,
        time_base: Rational::new(1, 1),
        bit_depth: BIT_DEPTH,
        chroma_sampling,
        chroma_sample_position: ChromaSamplePosition::Unknown,
        pixel_range: PixelRange::Full,
        color_description,
        still_picture: true,
        quantizer,
        min_quantizer: quantizer as u8,
        tune: Tune::Psychovisual,
        tiles: 1,
        speed_settings: speed_settings(speed, quantizer),
        ..EncoderConfig::default()
    };
    let cfg = Config::new().with_encoder_config(enc).with_threads(1);
    let mut ctx: Context<u16> = cfg.new_context().map_err(|e| EncodeError::Failed(format!("rav1e 設定エラー: {e}")))?;
    let mut frame = ctx.new_frame();
    fill(&mut frame);
    ctx.send_frame(frame).map_err(|e| EncodeError::Failed(format!("rav1e: {e}")))?;
    ctx.flush();
    let mut out = Vec::new();
    loop {
        match ctx.receive_packet() {
            Ok(mut packet) => {
                if packet.frame_type == FrameType::KEY {
                    out.append(&mut packet.data);
                }
            }
            Err(EncoderStatus::Encoded) => {}
            Err(EncoderStatus::LimitReached) => break,
            Err(e) => return Err(EncodeError::Failed(format!("rav1e: {e}"))),
        }
    }
    if out.is_empty() {
        return Err(EncodeError::Failed("rav1e が何も出力しませんでした".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, alpha: bool) -> DecodedImage {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    128,
                    if alpha { (x % 256) as u8 } else { 255 },
                ]);
            }
        }
        DecodedImage { width: w, height: h, rgba, primaries: Primaries::Srgb }
    }

    #[test]
    fn quantizer_curve_is_monotonic() {
        let mut last = usize::MAX;
        for q in 1..=100 {
            let v = quality_to_quantizer(q);
            assert!(v <= last, "q={q}");
            last = v;
        }
        assert_eq!(quality_to_quantizer(100), 0);
    }

    #[test]
    fn encodes_opaque_and_alpha() {
        for alpha in [false, true] {
            let img = gradient(67, 45, alpha);
            let out = encode(&img, Some(crate::exif::tests::sample_tiff(false, 1)), 80, 10).unwrap();
            let parsed = avif_parse::read_avif(&mut &out.avif[..]).unwrap();
            let meta = parsed.primary_item_metadata().unwrap();
            assert_eq!((meta.max_frame_width.get(), meta.max_frame_height.get()), (67, 45));
            assert_eq!(meta.bit_depth, 10);
            assert!(meta.still_picture);
            assert_eq!(parsed.alpha_item.is_some(), alpha);
        }
    }
}
