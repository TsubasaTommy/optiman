//! End-to-end conversion tests with real files: every conversion must be provably lossless.

use std::assert_matches;
use std::fs;
use std::path::{Path, PathBuf};

use oif::config::{Config, OriginalPolicy};
use oif::convert::{ConvertCtx, Outcome, convert_file};
use oif::{jxl, platform, restore};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn ctx(original: OriginalPolicy) -> ConvertCtx {
    let cfg = Config { original, effort: 3, skip_if_larger: false, ..Config::default() };
    ConvertCtx::new(&cfg)
}

fn copy_to(dir: &Path, name: &str) -> PathBuf {
    let dst = dir.join(name);
    fs::copy(fixture(name), &dst).unwrap();
    dst
}

fn converted(outcome: Outcome, name: &str) -> PathBuf {
    match outcome {
        Outcome::Converted { output, .. } => output,
        other => panic!("{name}: {other:?}"),
    }
}

const JPEGS: &[&str] = &[
    "orient1.jpg",
    "orient2.jpg",
    "orient3.jpg",
    "orient4.jpg",
    "orient5.jpg",
    "orient6.jpg",
    "orient7.jpg",
    "orient8.jpg",
    "photo_orient6.jpg",
    "p3.jpg",
    "progressive.jpg",
];

#[test]
fn jpeg_recompression_rebuilds_the_identical_file() {
    for name in JPEGS {
        let dir = tempfile::tempdir().unwrap();
        let src = copy_to(dir.path(), name);
        let original = fs::read(&src).unwrap();
        let out = converted(convert_file(&ctx(OriginalPolicy::Keep), &src), name);
        assert_eq!(out.extension().unwrap(), "jxl");
        let jxl_bytes = fs::read(&out).unwrap();
        let rebuilt = jxl::reconstruct_jpeg(&jxl_bytes).unwrap().expect("JPEG reconstruction data");
        assert_eq!(rebuilt, original, "{name}: rebuilt JPEG differs");
        assert!(jxl_bytes.len() < original.len(), "{name}: JPEG XL should be smaller");
    }
}

#[test]
fn jpeg_orientation_is_kept() {
    for o in 1..=8u16 {
        let jxl_bytes = jxl::transcode_jpeg(
            &fs::read(fixture(&format!("orient{o}.jpg"))).unwrap(),
            &jxl::EncodeOptions { effort: 3, threads: 2 },
        )
        .unwrap();
        let decoded = jxl::decode_exact(&jxl_bytes, false, false).unwrap();
        assert_eq!(decoded.orientation, o, "orientation {o}");
        assert_eq!((decoded.width, decoded.height), (64, 48), "stored size is unrotated");
    }
}

#[test]
fn lossless_formats_decode_to_identical_pixels() {
    for name in ["alpha.png", "rgb16.png", "gray.png", "photo.tiff", "photo.bmp", "photo_lossless.webp"] {
        let dir = tempfile::tempdir().unwrap();
        let src = copy_to(dir.path(), name);
        let source = platform::image::decode(&fs::read(&src).unwrap()).unwrap();
        let out = converted(convert_file(&ctx(OriginalPolicy::Keep), &src), name);
        // convert_file already verified; check again independently here.
        jxl::verify_lossless(&fs::read(&out).unwrap(), &source).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn lossy_sources_are_not_converted() {
    for name in ["photo.heic", "photo.webp"] {
        let dir = tempfile::tempdir().unwrap();
        let src = copy_to(dir.path(), name);
        let outcome = convert_file(&ctx(OriginalPolicy::Delete), &src);
        assert_matches!(outcome, Outcome::Skipped { remember: Some(_), .. }, "{name}");
        assert!(src.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "{name}: nothing written");
    }
}

#[test]
fn jpeg_with_gain_map_is_not_converted() {
    // A JPEG carrying an MPF (multi-picture) segment, as HDR/gain-map JPEGs do.
    let mut data = fs::read(fixture("orient1.jpg")).unwrap();
    let mpf = [0xFF, 0xE2, 0x00, 0x0A, b'M', b'P', b'F', 0, b'I', b'I', b'*', 0];
    data.splice(2..2, mpf);
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hdr.jpg");
    fs::write(&src, &data).unwrap();
    assert_matches!(convert_file(&ctx(OriginalPolicy::Delete), &src), Outcome::Skipped { .. });
    assert!(src.exists());
}

#[test]
fn delete_policy_removes_original_only_after_success() {
    for name in ["progressive.jpg", "photo.bmp"] {
        let dir = tempfile::tempdir().unwrap();
        let src = copy_to(dir.path(), name);
        let outcome = convert_file(&ctx(OriginalPolicy::Delete), &src);
        assert_matches!(outcome, Outcome::Converted { note: None, .. }, "{name}");
        assert!(!src.exists(), "{name}: original should be deleted");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "{name}: unexpected files {names:?}");
    }
}

#[test]
fn restore_gives_back_the_original() {
    let dir = tempfile::tempdir().unwrap();
    // JPEG: byte-identical file.
    let src = copy_to(dir.path(), "p3.jpg");
    let original = fs::read(&src).unwrap();
    let out = converted(convert_file(&ctx(OriginalPolicy::Delete), &src), "p3.jpg");
    let back = restore::restore_file(&out, true).unwrap();
    assert_eq!(fs::read(&back).unwrap(), original);
    assert!(!out.exists());

    // PNG with alpha: identical pixels.
    let src = copy_to(dir.path(), "alpha.png");
    let source = platform::image::decode(&fs::read(&src).unwrap()).unwrap();
    let out = converted(convert_file(&ctx(OriginalPolicy::Delete), &src), "alpha.png");
    let back = restore::restore_file(&out, false).unwrap();
    let restored = platform::image::decode(&fs::read(&back).unwrap()).unwrap();
    assert_eq!((restored.width, restored.height), (source.width, source.height));
    assert_eq!(restored.pixels, source.pixels);
}

#[test]
fn corrupt_file_is_skipped_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("broken.png");
    fs::write(&src, b"\x89PNG\r\n\x1a\n this is not really a png").unwrap();
    let outcome = convert_file(&ctx(OriginalPolicy::Delete), &src);
    assert_matches!(outcome, Outcome::Skipped { .. } | Outcome::Failed { .. });
    assert!(src.exists());
    let (fp, _) = outcome.remember().expect("corrupt files must be remembered");
    assert_eq!(fp.size, fs::metadata(&src).unwrap().len());
}

/// Writes an incompressible (white noise) RGB PNG using stored deflate blocks.
fn noise_png(path: &Path, w: u32, h: u32) {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &x in data {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(body);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    }
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut raw = Vec::new();
    for _ in 0..h {
        let row = (0..w * 3).map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        });
        raw.extend(std::iter::once(0).chain(row)); // filter byte 0: none
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(u8::from(last));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

#[test]
fn never_writes_a_larger_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("noise.png");
    noise_png(&src, 96, 64);
    let before = fs::metadata(&src).unwrap().len();
    let mut c = ctx(OriginalPolicy::Delete);
    c.skip_if_larger = true;
    match convert_file(&c, &src) {
        Outcome::Skipped { remember: Some(_), .. } => {
            assert!(src.exists());
            assert!(!dir.path().join("noise.jxl").exists());
        }
        Outcome::Converted { after, .. } => assert!(after < before, "{after} >= {before}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn dry_run_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let src = copy_to(dir.path(), "photo.bmp");
    let mut c = ctx(OriginalPolicy::Delete);
    c.dry_run = true;
    assert_matches!(convert_file(&c, &src), Outcome::WouldConvert(_));
    assert!(src.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}
