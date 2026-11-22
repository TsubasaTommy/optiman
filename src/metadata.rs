//! Container-level inspection of the source file (pure Rust, identical on every OS):
//! raw EXIF/XMP extraction, lossless-WebP detection, and detection of content that a JPEG XL
//! conversion would lose (HDR gain maps, depth maps, multi-picture JPEGs, CICP-tagged PNGs).

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

// ---------- byte helpers ----------

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn be64(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(o..o + 8)?.try_into().ok()?))
}
fn le32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn be_n(d: &[u8], o: usize, n: usize) -> Option<u64> {
    match n {
        0 => Some(0),
        2 => be16(d, o).map(u64::from),
        4 => be32(d, o).map(u64::from),
        8 => be64(d, o),
        _ => None,
    }
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

/// ISO-BMFF boxes in `d[start..end]`: (type, payload_start, payload_end).
fn boxes(d: &[u8], start: usize, end: usize) -> impl Iterator<Item = ([u8; 4], usize, usize)> + '_ {
    let mut i = start;
    std::iter::from_fn(move || {
        if i + 8 > end {
            return None;
        }
        let size32 = be32(d, i)? as usize;
        let kind: [u8; 4] = d.get(i + 4..i + 8)?.try_into().ok()?;
        let (header, size) = match size32 {
            1 => (16, be64(d, i + 8)? as usize),
            0 => (8, end - i),
            s => (8, s),
        };
        if size < header || i + size > end {
            return None;
        }
        let item = (kind, i + header, i + size);
        i += size;
        Some(item)
    })
}

fn find_box(d: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    boxes(d, start, end).find(|(k, _, _)| k == kind).map(|(_, s, e)| (s, e))
}

/// Payload range of the top-level `meta` box (after its FullBox header).
fn heif_meta(d: &[u8]) -> Option<(usize, usize)> {
    let (s, e) = find_box(d, 0, d.len(), b"meta")?;
    Some((s + 4, e))
}

/// `infe` entries: (item id, item type, bytes after the type field).
fn heif_items(d: &[u8], ms: usize, me: usize) -> Vec<(u32, [u8; 4], (usize, usize))> {
    let mut out = Vec::new();
    let Some((s, e)) = find_box(d, ms, me, b"iinf") else { return out };
    let Some(&version) = d.get(s) else { return out };
    let entries = if version == 0 { s + 6 } else { s + 8 };
    for (kind, is, ie) in boxes(d, entries, e) {
        if &kind != b"infe" {
            continue;
        }
        let parsed = match d.get(is) {
            Some(2) => be16(d, is + 4).map(|id| (u32::from(id), is + 8)),
            Some(3) => be32(d, is + 4).map(|id| (id, is + 10)),
            _ => None,
        };
        if let Some((id, t)) = parsed
            && let Some(ty) = d.get(t..t + 4).and_then(|b| <[u8; 4]>::try_from(b).ok())
        {
            out.push((id, ty, (t + 4, ie)));
        }
    }
    out
}

fn heif_item_data(d: &[u8], ms: usize, me: usize, want: u32) -> Option<&[u8]> {
    let (s, _) = find_box(d, ms, me, b"iloc")?;
    let version = *d.get(s)?;
    let sizes = *d.get(s + 4)?;
    let sizes2 = *d.get(s + 5)?;
    let (offset_size, length_size) = ((sizes >> 4) as usize, (sizes & 0xF) as usize);
    let base_offset_size = (sizes2 >> 4) as usize;
    let index_size = if version == 1 || version == 2 { (sizes2 & 0xF) as usize } else { 0 };
    let mut p = s + 6;
    let count = if version < 2 {
        p += 2;
        u32::from(be16(d, p - 2)?)
    } else {
        p += 4;
        be32(d, p - 4)?
    };
    for _ in 0..count {
        let id = if version < 2 {
            p += 2;
            u32::from(be16(d, p - 2)?)
        } else {
            p += 4;
            be32(d, p - 4)?
        };
        let mut method = 0;
        if version == 1 || version == 2 {
            method = be16(d, p)? & 0xF;
            p += 2;
        }
        p += 2; // data_reference_index
        let base = be_n(d, p, base_offset_size)?;
        p += base_offset_size;
        let extents = be16(d, p)?;
        p += 2;
        let mut first = None;
        for _ in 0..extents {
            p += index_size;
            let off = be_n(d, p, offset_size)?;
            p += offset_size;
            let len = be_n(d, p, length_size)?;
            p += length_size;
            first.get_or_insert((off, len));
        }
        if id == want {
            // Only file-offset construction with a single extent is supported (what cameras write).
            if method != 0 || extents != 1 {
                return None;
            }
            let (off, len) = first?;
            let start = usize::try_from(base + off).ok()?;
            return d.get(start..start.checked_add(usize::try_from(len).ok()?)?);
        }
    }
    None
}

// ---------- EXIF ----------

/// Returns the TIFF-structured EXIF block (starting with `II*\0` / `MM\0*`), if any.
pub fn extract_exif(data: &[u8]) -> Option<Vec<u8>> {
    let raw: &[u8] = match sniff(data) {
        Container::Jpeg => jpeg_segments(data).find(|(m, b)| *m == 0xE1 && b.starts_with(b"Exif\0\0"))?.1,
        Container::Png => png_chunks(data).take_while(|(k, _)| k != b"IEND").find(|(k, _)| k == b"eXIf")?.1,
        Container::WebP => webp_chunks(data).find(|(k, _)| k == b"EXIF")?.1,
        Container::Heif => {
            let (ms, me) = heif_meta(data)?;
            let id = heif_items(data, ms, me).into_iter().find(|(_, t, _)| t == b"Exif")?.0;
            let item = heif_item_data(data, ms, me, id)?;
            // HEIF Exif item: 4-byte big-endian offset to the TIFF header, then the payload.
            item.get(4 + be32(item, 0)? as usize..)?
        }
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
        Container::Heif => {
            let (ms, me) = heif_meta(data)?;
            let id = heif_items(data, ms, me).into_iter().find_map(|(id, ty, (s, e))| {
                (&ty == b"mime" && contains(data.get(s..e)?, b"application/rdf+xml")).then_some(id)
            })?;
            heif_item_data(data, ms, me, id)?
        }
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
        Container::Heif => {
            let (ms, me) = heif_meta(data)?;
            let meta = data.get(ms..me)?;
            if contains(meta, b"hdrgainmap") || contains(meta, b"urn:iso:std:iso:ts:21496") || contains(meta, b"tmap") {
                return Some("HDR ゲインマップを含むため (変換すると HDR 表示が失われます)");
            }
            // Alpha (auxid:1) converts fine; depth/disparity and Apple's mattes do not.
            (contains(meta, b"urn:com:apple:photo") || contains(meta, b"urn:mpeg:hevc:2015:auxid:2"))
                .then_some("深度マップ等の補助画像を含むため (ポートレート編集情報が失われます)")
        }
        Container::Jpeg => {
            for (marker, body) in jpeg_segments(data) {
                if marker == 0xE2 && body.starts_with(b"MPF\0") {
                    return Some("複数画像 (MPF: HDR ゲインマップ/深度等) を含むため");
                }
                if marker == 0xE1
                    && body.starts_with(XMP_JPEG_PREFIX)
                    && (contains(body, b"hdrgm") || contains(body, b"HDRGainMap"))
                {
                    return Some("HDR ゲインマップを含むため (変換すると HDR 表示が失われます)");
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
pub(crate) mod tests {
    use super::*;
    use crate::exif::tests::sample_tiff;

    fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, marker];
        s.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(body);
        s
    }

    fn sof(factors: [(u8, u8); 3]) -> Vec<u8> {
        let mut b = vec![8, 0, 16, 0, 16, 3];
        for (i, (h, v)) in factors.iter().enumerate() {
            b.extend_from_slice(&[i as u8 + 1, (h << 4) | v, 0]);
        }
        segment(0xC0, &b)
    }

    fn jpeg(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        for p in parts {
            j.extend_from_slice(p);
        }
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        j
    }

    pub(crate) fn png(chunks: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in chunks {
            p.extend_from_slice(&(body.len() as u32).to_be_bytes());
            p.extend_from_slice(kind);
            p.extend_from_slice(body);
            p.extend_from_slice(&[0, 0, 0, 0]);
        }
        p
    }

    fn bmff(kind: &[u8], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
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
        assert!(lossy_extras(&jpeg(&[sof([(2, 2), (1, 1), (1, 1)])])).is_none());
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
    fn webp_chunks_and_chroma() {
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

    /// Minimal HEIF with an hvcC property, an Exif item and an optional extra marker in `meta`.
    fn heif(chroma_idc: u8, extra: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let tiff = sample_tiff(true, 6);
        let mut item = 0u32.to_be_bytes().to_vec();
        item.extend_from_slice(&tiff);
        let mut infe1 = vec![2, 0, 0, 0, 0, 1, 0, 0];
        infe1.extend_from_slice(b"hvc1\0");
        let mut infe2 = vec![2, 0, 0, 0, 0, 2, 0, 0];
        infe2.extend_from_slice(b"Exif\0");
        let mut iinf = vec![0, 0, 0, 0, 0, 2];
        iinf.extend(bmff(b"infe", &infe1));
        iinf.extend(bmff(b"infe", &infe2));
        let mut hvcc = vec![0u8; 23];
        hvcc[16] = 0xFC | chroma_idc;
        let ipco = bmff(b"ipco", &bmff(b"hvcC", &hvcc));
        let iprp = bmff(b"iprp", &ipco);
        let build = |exif_offset: u32| {
            let mut iloc = vec![0, 0, 0, 0, 0x44, 0x00, 0, 1];
            iloc.extend_from_slice(&2u16.to_be_bytes());
            iloc.extend_from_slice(&0u16.to_be_bytes());
            iloc.extend_from_slice(&1u16.to_be_bytes());
            iloc.extend_from_slice(&exif_offset.to_be_bytes());
            iloc.extend_from_slice(&(item.len() as u32).to_be_bytes());
            let mut meta = vec![0, 0, 0, 0];
            meta.extend(bmff(b"iinf", &iinf));
            meta.extend(bmff(b"iloc", &iloc));
            meta.extend(iprp.clone());
            if !extra.is_empty() {
                meta.extend(bmff(b"free", extra));
            }
            let mut file = bmff(b"ftyp", b"heic\0\0\0\0mif1heic");
            file.extend(bmff(b"meta", &meta));
            file
        };
        let head_len = build(0).len() + 8;
        let mut file = build(head_len as u32);
        file.extend(bmff(b"mdat", &item));
        (file, tiff)
    }

    #[test]
    fn heif_exif_and_extras() {
        let (file, tiff) = heif(1, b"");
        assert_eq!(sniff(&file), Container::Heif);
        assert_eq!(extract_exif(&file).as_deref(), Some(&tiff[..]));
        assert!(lossy_extras(&file).is_none());
        assert!(lossy_extras(&heif(1, b"urn:com:apple:photo:2020:aux:hdrgainmap").0).is_some());
        assert!(lossy_extras(&heif(1, b"urn:com:apple:photo:2018:aux:portraiteffectsmatte").0).is_some());
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
