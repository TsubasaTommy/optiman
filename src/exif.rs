//! Tiny TIFF/EXIF helpers: read and normalize the Orientation tag (0x0112) in IFD0.
//!
//! When we bake the orientation into the pixels, the EXIF Orientation must become 1,
//! otherwise viewers that honour EXIF would rotate the image a second time.

const ORIENTATION_TAG: u16 = 0x0112;
const TYPE_SHORT: u16 = 3;

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, b: &[u8], off: usize) -> Option<u16> {
        let s: [u8; 2] = b.get(off..off + 2)?.try_into().ok()?;
        Some(match self {
            Endian::Little => u16::from_le_bytes(s),
            Endian::Big => u16::from_be_bytes(s),
        })
    }
    fn u32(self, b: &[u8], off: usize) -> Option<u32> {
        let s: [u8; 4] = b.get(off..off + 4)?.try_into().ok()?;
        Some(match self {
            Endian::Little => u32::from_le_bytes(s),
            Endian::Big => u32::from_be_bytes(s),
        })
    }
    fn put_u16(self, v: u16) -> [u8; 2] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }
}

fn header(tiff: &[u8]) -> Option<(Endian, usize)> {
    let endian = match tiff.get(0..4)? {
        [b'I', b'I', 42, 0] => Endian::Little,
        [b'M', b'M', 0, 42] => Endian::Big,
        _ => return None,
    };
    Some((endian, endian.u32(tiff, 4)? as usize))
}

/// Offset of the Orientation entry's value field inside IFD0.
fn orientation_value_offset(tiff: &[u8]) -> Option<(Endian, usize)> {
    let (endian, ifd) = header(tiff)?;
    let count = endian.u16(tiff, ifd)? as usize;
    for i in 0..count {
        let entry = ifd + 2 + i * 12;
        if endian.u16(tiff, entry)? == ORIENTATION_TAG {
            if endian.u16(tiff, entry + 2)? != TYPE_SHORT || endian.u32(tiff, entry + 4)? != 1 {
                return None;
            }
            let value = entry + 8;
            tiff.get(value..value + 2)?;
            return Some((endian, value));
        }
    }
    None
}

/// Returns the EXIF orientation (1-8) if present and valid.
pub fn read_orientation(tiff: &[u8]) -> Option<u16> {
    let (endian, off) = orientation_value_offset(tiff)?;
    let v = endian.u16(tiff, off)?;
    (1..=8).contains(&v).then_some(v)
}

/// Sets Orientation to 1 (top-left) in place. Returns true if a tag was rewritten.
pub fn normalize_orientation(tiff: &mut [u8]) -> bool {
    let Some((endian, off)) = orientation_value_offset(tiff) else { return false };
    tiff[off..off + 2].copy_from_slice(&endian.put_u16(1));
    true
}

/// True if the blob starts with a TIFF header.
pub fn is_tiff(data: &[u8]) -> bool {
    header(data).is_some()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a minimal TIFF with IFD0 holding Make (ASCII) and Orientation.
    pub fn sample_tiff(big_endian: bool, orientation: u16) -> Vec<u8> {
        let e = if big_endian { Endian::Big } else { Endian::Little };
        let u16b = |v: u16| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let u32b = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut t = Vec::new();
        t.extend_from_slice(if big_endian { b"MM\0*" } else { b"II*\0" });
        t.extend_from_slice(&u32b(8));
        t.extend_from_slice(&u16b(2)); // two entries
        // Make: ASCII, count 4, inline "Cam\0"
        t.extend_from_slice(&u16b(0x010f));
        t.extend_from_slice(&u16b(2));
        t.extend_from_slice(&u32b(4));
        t.extend_from_slice(b"Cam\0");
        // Orientation: SHORT, count 1
        t.extend_from_slice(&u16b(ORIENTATION_TAG));
        t.extend_from_slice(&u16b(TYPE_SHORT));
        t.extend_from_slice(&u32b(1));
        t.extend_from_slice(&e.put_u16(orientation));
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&u32b(0)); // next IFD
        t
    }

    #[test]
    fn reads_and_normalizes_both_endians() {
        for be in [false, true] {
            let mut t = sample_tiff(be, 6);
            assert!(is_tiff(&t));
            assert_eq!(read_orientation(&t), Some(6));
            assert!(normalize_orientation(&mut t));
            assert_eq!(read_orientation(&t), Some(1));
            // Make tag untouched
            assert_eq!(&t[18..22], b"Cam\0");
        }
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(read_orientation(b"nope"), None);
        assert_eq!(read_orientation(&[]), None);
        let mut t = sample_tiff(false, 6);
        t.truncate(20);
        assert_eq!(read_orientation(&t), None);
        assert!(!normalize_orientation(&mut t));
    }
}
