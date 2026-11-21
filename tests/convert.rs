//! End-to-end conversion tests against real image files decoded by the OS.

use std::fs;
use std::path::{Path, PathBuf};

use oif::config::{Config, OriginalPolicy};
use oif::convert::{ConvertCtx, Outcome, convert_file};
use oif::encode::{self, EncoderKind};
use oif::platform::{self, Primaries};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn ctx(encoder: EncoderKind, original: OriginalPolicy) -> ConvertCtx {
    let cfg = Config { original, quality: 80, speed: 10, skip_if_larger: false, ..Config::default() };
    ConvertCtx::new(&cfg, encoder)
}

fn copy_to(dir: &Path, name: &str) -> PathBuf {
    let dst = dir.join(name);
    fs::copy(fixture(name), &dst).unwrap();
    dst
}

/// HEIC needs optional Store extensions on Windows; WebP may be missing on older Windows.
fn optional_codec(name: &str) -> bool {
    cfg!(windows) && (name.ends_with(".heic") || name.ends_with(".webp"))
}

fn encoders() -> Vec<EncoderKind> {
    let mut v = vec![EncoderKind::Rav1e];
    if platform::image::native_encoder_available() {
        v.push(EncoderKind::Native);
    }
    v
}

fn pixel(img: &encode::DecodedImage, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * img.width + x) * 4) as usize;
    [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
}

fn color_name(p: [u8; 3]) -> &'static str {
    match p {
        [r, g, b] if r > 200 && g < 80 && b < 80 => "red",
        [r, g, b] if r < 80 && g > 200 && b < 80 => "green",
        [r, g, b] if r > 200 && g > 200 && b > 200 => "white",
        _ => "other",
    }
}

#[test]
fn decode_applies_all_exif_orientations() {
    // Stored image: 64x48, red block at stored top-left, green at stored top-right.
    // Expected display corner of (red, green) per EXIF orientation.
    let expected = [
        (1, "TL", "TR"),
        (2, "TR", "TL"),
        (3, "BR", "BL"),
        (4, "BL", "BR"),
        (5, "TL", "BL"),
        (6, "TR", "BR"),
        (7, "BR", "TR"),
        (8, "BL", "TL"),
    ];
    for (o, red, green) in expected {
        let data = fs::read(fixture(&format!("orient{o}.jpg"))).unwrap();
        let img = platform::image::decode(&data).unwrap_or_else(|e| panic!("orientation {o}: {e}"));
        let dims = if o >= 5 { (48, 64) } else { (64, 48) };
        assert_eq!((img.width, img.height), dims, "orientation {o}");
        let corner = |c: &str| {
            let (x, y) = match c {
                "TL" => (3, 3),
                "TR" => (img.width - 4, 3),
                "BL" => (3, img.height - 4),
                _ => (img.width - 4, img.height - 4),
            };
            color_name(pixel(&img, x, y))
        };
        assert_eq!(corner(red), "red", "orientation {o}: red marker not at {red}");
        assert_eq!(corner(green), "green", "orientation {o}: green marker not at {green}");
    }
}

#[test]
fn decode_detects_display_p3_and_alpha() {
    let p3 = platform::image::decode(&fs::read(fixture("p3.jpg")).unwrap()).unwrap();
    assert_eq!(p3.primaries, Primaries::DisplayP3);
    assert!(!p3.has_alpha());
    let alpha = platform::image::decode(&fs::read(fixture("alpha.png")).unwrap()).unwrap();
    assert_eq!(alpha.primaries, Primaries::Srgb);
    assert!(alpha.has_alpha());
}

#[test]
fn converts_every_format_with_every_encoder() {
    for encoder in encoders() {
        for name in ["photo_orient6.jpg", "p3.jpg", "alpha.png", "photo.heic", "photo.tiff", "photo.bmp", "photo.webp"]
        {
            let dir = tempfile::tempdir().unwrap();
            let src = copy_to(dir.path(), name);
            let outcome = convert_file(&ctx(encoder, OriginalPolicy::Keep), &src);
            let output = match outcome {
                Outcome::Converted { output, .. } => output,
                Outcome::Skipped { reason, .. } if optional_codec(name) => {
                    eprintln!("{name}: skipped on this system ({reason})");
                    continue;
                }
                other => panic!("{encoder} {name}: {other:?}"),
            };
            assert!(src.exists(), "keep policy must keep the original");
            assert_eq!(output.extension().unwrap(), "avif");
            let avif = fs::read(&output).unwrap();
            let parsed = avif_parse::read_avif(&mut &avif[..]).unwrap();
            let meta = parsed.primary_item_metadata().unwrap();
            let (w, h) = (meta.max_frame_width.get(), meta.max_frame_height.get());
            if name == "photo_orient6.jpg" {
                assert_eq!((w, h), (400, 600), "{encoder}: orientation must be baked in");
            }
            if name == "alpha.png" {
                // avif-parse does not recognise ImageIO's alpha item linkage; check with the OS decoder.
                let has_alpha = match encoder {
                    EncoderKind::Rav1e => parsed.alpha_item.is_some(),
                    EncoderKind::Native => platform::image::decode(&avif).unwrap().has_alpha(),
                };
                assert!(has_alpha, "{encoder}: alpha lost");
            }
            assert_eq!(
                fs::metadata(&output).unwrap().modified().unwrap(),
                fs::metadata(&src).unwrap().modified().unwrap(),
                "{encoder} {name}: mtime must be copied"
            );
        }
    }
}

#[test]
fn rav1e_keeps_exif_and_signals_p3() {
    let dir = tempfile::tempdir().unwrap();
    let src = copy_to(dir.path(), "p3.jpg");
    let Outcome::Converted { output, .. } = convert_file(&ctx(EncoderKind::Rav1e, OriginalPolicy::Keep), &src) else {
        panic!("conversion failed")
    };
    let avif = fs::read(output).unwrap();
    assert!(avif.windows(19).any(|w| w == b"2024:05:01 12:34:56"), "EXIF DateTimeOriginal missing");
    // colr box, nclx: primaries(2) transfer(2) matrix(2) full_range(1)
    let i = avif.windows(8).position(|w| w == b"colrnclx").expect("no nclx colr box");
    let primaries = u16::from_be_bytes([avif[i + 8], avif[i + 9]]);
    assert_eq!(primaries, 12, "Display P3 primaries expected");
}

#[test]
fn rav1e_normalizes_exif_orientation() {
    let dir = tempfile::tempdir().unwrap();
    let src = copy_to(dir.path(), "photo_orient6.jpg");
    let Outcome::Converted { output, .. } = convert_file(&ctx(EncoderKind::Rav1e, OriginalPolicy::Keep), &src) else {
        panic!("conversion failed")
    };
    let avif = fs::read(output).unwrap();
    let start = avif.windows(4).position(|w| w == b"II*\0" || w == b"MM\0*").expect("no EXIF TIFF header");
    assert_eq!(oif::exif::read_orientation(&avif[start..]), Some(1));
}

#[test]
fn delete_policy_removes_original_only_after_success() {
    for encoder in encoders() {
        let dir = tempfile::tempdir().unwrap();
        let src = copy_to(dir.path(), "photo.bmp");
        let outcome = convert_file(&ctx(encoder, OriginalPolicy::Delete), &src);
        assert!(matches!(outcome, Outcome::Converted { note: None, .. }), "{encoder}: {outcome:?}");
        assert!(!src.exists(), "{encoder}: original should be deleted");
        assert!(dir.path().join("photo.avif").exists());
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{encoder}: unexpected files {leftovers:?}");
    }
}

#[test]
fn corrupt_file_is_skipped_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("broken.jpg");
    fs::write(&src, b"\xFF\xD8\xFF\xE0 this is not really a jpeg").unwrap();
    let c = ctx(EncoderKind::Rav1e, OriginalPolicy::Delete);
    let outcome = convert_file(&c, &src);
    assert!(matches!(outcome, Outcome::Skipped { .. } | Outcome::Failed { .. }), "{outcome:?}");
    assert!(src.exists());
    // Remembered so the agent does not retry until the file changes.
    let (fp, _) = outcome.remember().expect("corrupt files must be remembered");
    assert_eq!(fp.size, fs::metadata(&src).unwrap().len());
}

#[test]
fn larger_output_keeps_original() {
    // 1x1 PNG: any AVIF is bigger than this.
    let png: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF,
        0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tiny.png");
    fs::write(&src, png).unwrap();
    let mut c = ctx(EncoderKind::Rav1e, OriginalPolicy::Delete);
    c.skip_if_larger = true;
    let outcome = convert_file(&c, &src);
    assert!(matches!(outcome, Outcome::Skipped { remember: Some(_), .. }), "{outcome:?}");
    assert!(src.exists());
    assert!(!dir.path().join("tiny.avif").exists());
}

#[test]
fn dry_run_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let src = copy_to(dir.path(), "photo.bmp");
    let mut c = ctx(EncoderKind::Rav1e, OriginalPolicy::Delete);
    c.dry_run = true;
    assert!(matches!(convert_file(&c, &src), Outcome::WouldConvert(_)));
    assert!(src.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}
