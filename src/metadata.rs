//! Container-level inspection of the source file (pure Rust, identical on every OS): raw
//! EXIF/XMP extraction, lossless-WebP detection, and detection of content a JPEG XL conversion
//! would lose (HDR gain maps, multi-picture JPEGs, HDR PNGs). HEIF is only recognised, to be
//! skipped.

use crate::exif;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Jpeg,
    Png,
    WebP,
    Heif,
    Tiff,
    Bmp,
    Unknown,
}

pub fn sniff(data: &[u8]) -> Container {
    match data {
        [0xFF, 0xD8, 0xFF, ..] => Container::Jpeg,
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Container::Png,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Container::WebP,
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => Container::Heif,
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Container::Tiff,
        [b'B', b'M', ..] => Container::Bmp,
        _ => Container::Unknown,
    }
}

const XMP_JPEG_PREFIX: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const GAIN_MAP: &str = "HDR ゲインマップを含むため (変換すると HDR 表示が失われます)";

// ---------- byte helpers ----------

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn le32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

// ---------- container walkers ----------

/// JPEG marker segments before the first scan: (marker, payload).
fn jpeg_segments(d: &[u8]) -> impl Iterator<Item = (u8, &[u8])> + '_ {
    let mut i = 2;
    std::iter::from_fn(move || {
        loop {
            if i + 4 > d.len() || d[i] != 0xFF {
                return None;
            }
            let marker = d[i + 1];
            match marker {
                0xFF => i += 1, // fill byte
                0xD8 | 0x01 | 0xD0..=0xD7 => i += 2,
                0xDA | 0xD9 => return None,
                _ => {
                    let len = be16(d, i + 2)? as usize;
                    let body = d.get(i + 4..i + 2 + len.max(2))?;
                    i += 2 + len.max(2);
                    return Some((marker, body));
                }
            }
        }
    })
}

/// PNG chunks: (type, data).
fn png_chunks(d: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> + '_ {
    let mut i = 8;
    std::iter::from_fn(move || {
        let len = be32(d, i)? as usize;
        let kind: [u8; 4] = d.get(i + 4..i + 8)?.try_into().ok()?;
        let body = d.get(i + 8..i + 8 + len)?;
        i += 12 + len;
        Some((kind, body))
    })
}

/// WebP RIFF chunks: (fourcc, data).
fn webp_chunks(d: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> + '_ {
    let mut i = 12;
    std::iter::from_fn(move || {
        let kind: [u8; 4] = d.get(i..i + 4)?.try_into().ok()?;
        let len = le32(d, i + 4)? as usize;
        let body = d.get(i + 8..i + 8 + len)?;
        i += 8 + len + (len & 1);
        Some((kind, body))
    })
}

// ---------- EXIF ----------

/// Returns the TIFF-structured EXIF block (starting with `II*\0` / `MM\0*`), if any.
pub fn extract_exif(data: &[u8]) -> Option<Vec<u8>> {
    let raw: &[u8] = match sniff(data) {
        Container::Jpeg => jpeg_segments(data).find(|(m, b)| *m == 0xE1 && b.starts_with(b"Exif\0\0"))?.1,
        Container::Png => png_chunks(data).take_while(|(k, _)| k != b"IEND").find(|(k, _)| k == b"eXIf")?.1,
        Container::WebP => webp_chunks(data).find(|(k, _)| k == b"EXIF")?.1,
        _ => return None,
    };
    let raw = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    exif::is_tiff(raw).then(|| raw.to_vec())
}

// ---------- XMP ----------

/// Returns the raw XMP packet, if any.
pub fn extract_xmp(data: &[u8]) -> Option<Vec<u8>> {
    let raw: &[u8] = match sniff(data) {
        Container::Jpeg => {
            jpeg_segments(data).find_map(|(m, b)| (m == 0xE1).then(|| b.strip_prefix(XMP_JPEG_PREFIX)).flatten())?
        }
        Container::Png => png_chunks(data).take_while(|(k, _)| k != b"IEND").find_map(|(k, b)| {
            // iTXt: keyword \0 compression_flag compression_method language \0 translated \0 text
            let rest = (&k == b"iTXt").then(|| b.strip_prefix(b"XML:com.adobe.xmp\0")).flatten()?;
            let (&[0, _], rest) = rest.split_at_checked(2)? else { return None }; // uncompressed only
            let after_lang = &rest[rest.iter().position(|&c| c == 0)? + 1..];
            Some(&after_lang[after_lang.iter().position(|&c| c == 0)? + 1..])
        })?,
        Container::WebP => webp_chunks(data).find(|(k, _)| k == b"XMP ")?.1,
        _ => return None,
    };
    (!raw.is_empty()).then(|| raw.to_vec())
}

/// Sets `tiff:Orientation` in an XMP packet to 1 (attribute and element forms), since the
/// orientation is carried by the JPEG XL codestream header.
pub fn normalize_xmp_orientation(xmp: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(xmp) else { return xmp.to_vec() };
    let mut out = text.to_string();
    for o in '2'..='8' {
        out = out
            .replace(&format!("tiff:Orientation=\"{o}\""), "tiff:Orientation=\"1\"")
            .replace(&format!("tiff:Orientation='{o}'"), "tiff:Orientation='1'")
            .replace(&format!("<tiff:Orientation>{o}</tiff:Orientation>"), "<tiff:Orientation>1</tiff:Orientation>");
    }
    out.into_bytes()
}

// ---------- WebP ----------

/// True for lossless WebP (VP8L bitstream); lossy WebP (VP8) is a lossy source and is skipped.
pub fn webp_is_lossless(data: &[u8]) -> bool {
    webp_chunks(data).find_map(|(k, _)| match &k {
        b"VP8L" => Some(true),
        b"VP8 " => Some(false),
        _ => None,
    }) == Some(true)
}

// ---------- content a JPEG XL conversion would drop ----------

/// Reason not to convert, if the file carries data the JPEG XL file would lose (so the original
/// is kept).
pub fn lossy_extras(data: &[u8]) -> Option<&'static str> {
    match sniff(data) {
        Container::Jpeg => {
            for (marker, body) in jpeg_segments(data) {
                if marker == 0xE2 && body.starts_with(b"MPF\0") {
                    return Some("複数画像 (MPF: HDR ゲインマップ/深度等) を含むため");
                }
                if marker == 0xE1
                    && body.starts_with(XMP_JPEG_PREFIX)
                    && (contains(body, b"hdrgm") || contains(body, b"HDRGainMap"))
                {
                    return Some(GAIN_MAP);
                }
            }
            None
        }
        // HDR PNGs: PQ (16) / HLG (18) transfer in cICP, or mastering metadata. SDR cICP is fine.
        Container::Png => png_chunks(data)
            .take_while(|(k, _)| k != b"IDAT")
            .any(|(k, body)| match &k {
                b"cICP" => matches!(body.get(1), Some(16 | 18)),
                b"mDCV" | b"cLLI" => true,
                _ => false,
            })
            .then_some("HDR (PQ/HLG) 画像のため"),
        _ => None,
    }
}

/// True if an ICC profile's text (desc/mluc tags, ASCII or UTF-16BE) contains `needle`.
pub fn icc_mentions(icc: &[u8], needle: &str) -> bool {
    let utf16: Vec<u8> = needle.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    contains(icc, needle.as_bytes()) || contains(icc, &utf16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif::tests::sample_tiff;

    fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, marker];
        s.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(body);
        s
    }

    fn jpeg(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        for p in parts {
            j.extend_from_slice(p);
        }
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        j
    }

    fn png(chunks: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in chunks {
            p.extend_from_slice(&(body.len() as u32).to_be_bytes());
            p.extend_from_slice(kind);
            p.extend_from_slice(body);
            p.extend_from_slice(&[0, 0, 0, 0]);
        }
        p
    }

    #[test]
    fn jpeg_exif_and_xmp() {
        let tiff = sample_tiff(false, 6);
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&tiff);
        let mut xmp = XMP_JPEG_PREFIX.to_vec();
        xmp.extend_from_slice(b"<x:xmpmeta tiff:Orientation=\"6\"/>");
        let j = jpeg(&[segment(0xE0, &[0, 0]), segment(0xE1, &exif), segment(0xE1, &xmp)]);
        assert_eq!(extract_exif(&j).as_deref(), Some(&tiff[..]));
        let x = extract_xmp(&j).unwrap();
        assert_eq!(x, b"<x:xmpmeta tiff:Orientation=\"6\"/>");
        assert_eq!(normalize_xmp_orientation(&x), b"<x:xmpmeta tiff:Orientation=\"1\"/>");
    }

    #[test]
    fn jpeg_extras_detected() {
        let mpf = jpeg(&[segment(0xE2, b"MPF\0II*\0")]);
        assert!(lossy_extras(&mpf).is_some());
        let mut xmp = XMP_JPEG_PREFIX.to_vec();
        xmp.extend_from_slice(b"xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\"");
        assert!(lossy_extras(&jpeg(&[segment(0xE1, &xmp)])).is_some());
        assert!(lossy_extras(&jpeg(&[segment(0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0])])).is_none());
    }

    #[test]
    fn png_exif_xmp_and_hdr() {
        let tiff = sample_tiff(true, 3);
        let itxt = b"XML:com.adobe.xmp\0\0\0\0\0<xmp/>".to_vec();
        let p = png(&[(b"IHDR", &[0; 13]), (b"eXIf", &tiff), (b"iTXt", &itxt), (b"IEND", &[])]);
        assert_eq!(extract_exif(&p).as_deref(), Some(&tiff[..]));
        assert_eq!(extract_xmp(&p).as_deref(), Some(&b"<xmp/>"[..]));
        assert!(lossy_extras(&p).is_none());
        let hdr = png(&[(b"IHDR", &[0; 13]), (b"cICP", &[9, 16, 0, 1]), (b"IDAT", &[]), (b"IEND", &[])]);
        assert!(lossy_extras(&hdr).is_some());
        let sdr = png(&[(b"IHDR", &[0; 13]), (b"cICP", &[1, 1, 0, 1]), (b"IDAT", &[]), (b"IEND", &[])]);
        assert!(lossy_extras(&sdr).is_none());
    }

    #[test]
    fn webp_exif_and_lossless_flag() {
        let tiff = sample_tiff(false, 8);
        let mut w = b"RIFF\0\0\0\0WEBP".to_vec();
        for (k, b) in [(b"VP8X", vec![0u8; 10]), (b"EXIF", tiff.clone()), (b"VP8L", vec![0u8; 4])] {
            w.extend_from_slice(k);
            w.extend_from_slice(&(b.len() as u32).to_le_bytes());
            w.extend_from_slice(&b);
        }
        assert_eq!(extract_exif(&w).as_deref(), Some(&tiff[..]));
        assert!(webp_is_lossless(&w));
        let lossy = b"RIFF\0\0\0\0WEBPVP8 \x04\0\0\0\0\0\0\0".to_vec();
        assert!(!webp_is_lossless(&lossy));
    }

    #[test]
    fn heif_is_recognised() {
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0"), Container::Heif);
    }

    #[test]
    fn icc_text_search() {
        let mut icc = vec![0u8; 16];
        icc.extend("Display P3".encode_utf16().flat_map(|u| u.to_be_bytes()));
        assert!(icc_mentions(&icc, "Display P3"));
        assert!(icc_mentions(b"..desc....sRGB IEC61966-2.1", "sRGB"));
        assert!(!icc_mentions(b"Adobe RGB (1998)", "sRGB"));
    }

    #[test]
    fn none_for_missing_or_garbage() {
        assert_eq!(extract_exif(b"\xFF\xD8\xFF\xDA\x00\x02"), None);
        assert_eq!(extract_exif(b"garbage"), None);
        assert_eq!(extract_xmp(&[]), None);
        assert!(!webp_is_lossless(b"garbage"));
    }
}
