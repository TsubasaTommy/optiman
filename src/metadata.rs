//! Extracts the raw EXIF (TIFF-structured) block from common photo containers so it can be
//! carried into the AVIF by the rav1e path. (The macOS ImageIO path copies metadata natively.)

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

/// Returns the TIFF-structured EXIF block (starting with `II*\0` / `MM\0*`), if any.
pub fn extract_exif(data: &[u8]) -> Option<Vec<u8>> {
    let raw = match sniff(data) {
        Container::Jpeg => jpeg_exif(data),
        Container::Png => png_exif(data),
        Container::WebP => webp_exif(data),
        Container::Heif => heif_exif(data),
        _ => None,
    }?;
    let raw = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    exif::is_tiff(raw).then(|| raw.to_vec())
}

/// True if an ICC profile's text (desc/mluc tags, ASCII or UTF-16BE) contains `needle`.
pub fn icc_mentions(icc: &[u8], needle: &str) -> bool {
    let ascii = needle.as_bytes();
    let utf16: Vec<u8> = needle.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    icc.windows(ascii.len()).any(|w| w == ascii) || icc.windows(utf16.len()).any(|w| w == &utf16[..])
}

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

fn jpeg_exif(d: &[u8]) -> Option<&[u8]> {
    let mut i = 2;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            return None;
        }
        let marker = d[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            return None; // start of scan / end of image
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = be16(d, i + 2)? as usize;
        if len < 2 {
            return None;
        }
        let body = d.get(i + 4..i + 2 + len)?;
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
            return Some(body);
        }
        i += 2 + len;
    }
    None
}

fn png_exif(d: &[u8]) -> Option<&[u8]> {
    let mut i = 8;
    while i + 8 <= d.len() {
        let len = be32(d, i)? as usize;
        let kind = d.get(i + 4..i + 8)?;
        let body = d.get(i + 8..i + 8 + len)?;
        if kind == b"eXIf" {
            return Some(body);
        }
        // eXIf should precede IDAT, but some writers put it later, so scan up to IEND.
        if kind == b"IEND" {
            return None;
        }
        i += 12 + len;
    }
    None
}

fn webp_exif(d: &[u8]) -> Option<&[u8]> {
    let mut i = 12;
    while i + 8 <= d.len() {
        let kind = d.get(i..i + 4)?;
        let len = le32(d, i + 4)? as usize;
        let body = d.get(i + 8..i + 8 + len)?;
        if kind == b"EXIF" {
            return Some(body);
        }
        i += 8 + len + (len & 1);
    }
    None
}

/// Iterates ISO-BMFF boxes in `d[start..end]`, yielding (type, payload_start, payload_end).
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

fn heif_exif(d: &[u8]) -> Option<&[u8]> {
    let (_, ms, me) = boxes(d, 0, d.len()).find(|(k, _, _)| k == b"meta")?;
    let ms = ms + 4; // FullBox version/flags
    let mut exif_id = None;
    let mut iloc = None;
    for (kind, s, e) in boxes(d, ms, me) {
        match &kind {
            b"iinf" => exif_id = find_exif_item(d, s, e),
            b"iloc" => iloc = Some((s, e)),
            _ => {}
        }
    }
    let (s, e) = iloc?;
    let (offset, length) = locate_item(d, s, e, exif_id?)?;
    let item = d.get(offset..offset.checked_add(length)?)?;
    // HEIF Exif item: 4-byte big-endian offset to the TIFF header, then the payload.
    let skip = be32(item, 0)? as usize;
    item.get(4 + skip..)
}

fn find_exif_item(d: &[u8], s: usize, e: usize) -> Option<u32> {
    let version = *d.get(s)?;
    let entries_start = if version == 0 { s + 6 } else { s + 8 };
    for (kind, is, ie) in boxes(d, entries_start, e) {
        if &kind != b"infe" {
            continue;
        }
        let v = *d.get(is)?;
        let (id, type_off) = match v {
            2 => (u32::from(be16(d, is + 4)?), is + 8),
            3 => (be32(d, is + 4)?, is + 10),
            _ => continue,
        };
        if type_off + 4 <= ie && &d[type_off..type_off + 4] == b"Exif" {
            return Some(id);
        }
    }
    None
}

fn locate_item(d: &[u8], s: usize, _e: usize, want: u32) -> Option<(usize, usize)> {
    let version = *d.get(s)?;
    let sizes = *d.get(s + 4)?;
    let sizes2 = *d.get(s + 5)?;
    let (offset_size, length_size) = ((sizes >> 4) as usize, (sizes & 0xF) as usize);
    let base_offset_size = (sizes2 >> 4) as usize;
    let index_size = if version == 1 || version == 2 { (sizes2 & 0xF) as usize } else { 0 };
    let mut p = s + 6;
    let count = if version < 2 {
        let c = be16(d, p)? as u32;
        p += 2;
        c
    } else {
        let c = be32(d, p)?;
        p += 4;
        c
    };
    for _ in 0..count {
        let id = if version < 2 {
            let v = be16(d, p)? as u32;
            p += 2;
            v
        } else {
            let v = be32(d, p)?;
            p += 4;
            v
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
            if first.is_none() {
                first = Some((off, len));
            }
        }
        if id == want {
            // Only file-offset construction with a single extent is supported (what cameras write).
            if method != 0 || extents != 1 {
                return None;
            }
            let (off, len) = first?;
            return Some(((base + off) as usize, len as usize));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exif::tests::sample_tiff;

    #[test]
    fn jpeg_app1() {
        let tiff = sample_tiff(false, 6);
        let mut j = vec![0xFF, 0xD8];
        // APP0 JFIF
        j.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        let mut body = b"Exif\0\0".to_vec();
        body.extend_from_slice(&tiff);
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(&body);
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        assert_eq!(extract_exif(&j).as_deref(), Some(&tiff[..]));
    }

    #[test]
    fn png_exif_chunk() {
        let tiff = sample_tiff(true, 3);
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        let chunk = |p: &mut Vec<u8>, kind: &[u8], body: &[u8]| {
            p.extend_from_slice(&(body.len() as u32).to_be_bytes());
            p.extend_from_slice(kind);
            p.extend_from_slice(body);
            p.extend_from_slice(&[0, 0, 0, 0]);
        };
        chunk(&mut p, b"IHDR", &[0; 13]);
        chunk(&mut p, b"eXIf", &tiff);
        chunk(&mut p, b"IEND", &[]);
        assert_eq!(extract_exif(&p).as_deref(), Some(&tiff[..]));
    }

    #[test]
    fn webp_exif_chunk() {
        let tiff = sample_tiff(false, 8);
        let mut w = b"RIFF\0\0\0\0WEBP".to_vec();
        w.extend_from_slice(b"VP8X");
        w.extend_from_slice(&10u32.to_le_bytes());
        w.extend_from_slice(&[0; 10]);
        w.extend_from_slice(b"EXIF");
        w.extend_from_slice(&(tiff.len() as u32).to_le_bytes());
        w.extend_from_slice(&tiff);
        assert_eq!(extract_exif(&w).as_deref(), Some(&tiff[..]));
    }

    fn bmff(kind: &[u8], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    #[test]
    fn heif_exif_item() {
        let tiff = sample_tiff(true, 6);
        let mut item = 0u32.to_be_bytes().to_vec();
        item.extend_from_slice(&tiff);

        // infe v2: version/flags, item_ID(2), protection(2), type(4), name "\0"
        let mut infe1 = vec![2, 0, 0, 0, 0, 1, 0, 0];
        infe1.extend_from_slice(b"av01\0");
        let mut infe2 = vec![2, 0, 0, 0, 0, 2, 0, 0];
        infe2.extend_from_slice(b"Exif\0");
        let mut iinf = vec![0, 0, 0, 0, 0, 2];
        iinf.extend(bmff(b"infe", &infe1));
        iinf.extend(bmff(b"infe", &infe2));

        let build = |exif_offset: u32| {
            // iloc v0, offset_size=4, length_size=4, base_offset_size=0
            let mut iloc = vec![0, 0, 0, 0, 0x44, 0x00, 0, 1];
            iloc.extend_from_slice(&2u16.to_be_bytes()); // item 2
            iloc.extend_from_slice(&0u16.to_be_bytes()); // data ref
            iloc.extend_from_slice(&1u16.to_be_bytes()); // extents
            iloc.extend_from_slice(&exif_offset.to_be_bytes());
            iloc.extend_from_slice(&(item.len() as u32).to_be_bytes());
            let mut meta = vec![0, 0, 0, 0];
            meta.extend(bmff(b"iinf", &iinf));
            meta.extend(bmff(b"iloc", &iloc));
            let mut file = bmff(b"ftyp", b"heic\0\0\0\0mif1heic");
            file.extend(bmff(b"meta", &meta));
            file
        };
        let head_len = build(0).len() + 8; // + mdat header
        let mut file = build(head_len as u32);
        file.extend(bmff(b"mdat", &item));
        assert_eq!(sniff(&file), Container::Heif);
        assert_eq!(extract_exif(&file).as_deref(), Some(&tiff[..]));
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
        assert_eq!(extract_exif(&[]), None);
    }
}
