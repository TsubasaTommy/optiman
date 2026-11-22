//! Decoded source image, exactly as stored in the file: no colour conversion, no rotation.

/// How the source's colours are defined. The values in [`SourceImage::pixels`] are always in
/// this space; they are never converted, so no precision is lost before encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorInfo {
    /// sRGB (also used for untagged images) → signalled with CICP, no ICC needed.
    Srgb,
    /// Display P3 → CICP primaries 12 + sRGB transfer.
    DisplayP3,
    /// Anything else: the original ICC profile is embedded unchanged.
    Icc(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct SourceImage {
    pub width: u32,
    pub height: u32,
    /// 8 or 16. With 16, `pixels` holds native-endian `u16` samples.
    pub sample_bits: u8,
    /// RGBA, row-major, tightly packed.
    pub pixels: Vec<u8>,
    pub has_alpha: bool,
    /// Colour samples are premultiplied by alpha (kept as-is and signalled in the JPEG XL).
    pub premultiplied: bool,
    /// Grey image (R = G = B); encoded as monochrome.
    pub gray: bool,
    pub color: ColorInfo,
    /// EXIF orientation 1-8. Stored in the JPEG XL codestream; the pixels are not rotated.
    pub orientation: u16,
}

impl SourceImage {
    pub fn bytes_per_sample(&self) -> usize {
        usize::from(self.sample_bits / 8)
    }

    pub fn row_bytes(&self) -> usize {
        self.width as usize * 4 * self.bytes_per_sample()
    }

    /// Display size after applying the orientation.
    pub fn display_size(&self) -> (u32, u32) {
        if (5..=8).contains(&self.orientation) { (self.height, self.width) } else { (self.width, self.height) }
    }

    /// True if any pixel is not fully opaque (for sources that declare alpha but do not use it).
    pub fn alpha_used(&self) -> bool {
        if !self.has_alpha {
            return false;
        }
        match self.sample_bits {
            16 => self.pixels.as_chunks::<8>().0.iter().any(|p| u16::from_ne_bytes([p[6], p[7]]) != u16::MAX),
            _ => self.pixels.as_chunks::<4>().0.iter().any(|p| p[3] != u8::MAX),
        }
    }
}
