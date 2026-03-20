//! Image loading with format fallbacks not covered by the `image` crate.
//!
//! The public entry point is [`load_image`], which wraps `image::load_from_memory`
//! and transparently handles additional cases — currently palette-indexed TIFFs
//! (`PhotometricInterpretation = RGBPalette`), which the `image` crate's TIFF
//! decoder rejects.
//!
//! The [`palette_tiff`] submodule abstracts all direct use of the `tiff` crate.

/// Load an image from raw bytes into a [`image::DynamicImage`].
///
/// Tries `image::load_from_memory` first.  If that fails and the data looks
/// like a palette-indexed TIFF, falls back to [`palette_tiff::decode`].
pub fn load_image(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    match image::load_from_memory(bytes) {
        Ok(img) => Ok(img),
        Err(_) if palette_tiff::is_palette_tiff(bytes) => palette_tiff::decode(bytes),
        Err(e) => Err(e.to_string()),
    }
}

/// Palette-indexed TIFF support.
///
/// The `image` crate delegates TIFF decoding to the `tiff` crate, which does
/// not currently implement `PhotometricInterpretation = RGBPalette`.  We work
/// around this by:
///
/// 1. Reading the `ColorMap` tag directly via the `tiff` crate decoder.
/// 2. Patching the `PhotometricInterpretation` tag in-memory from `3` (Palette)
///    to `1` (BlackIsZero), so the `image` crate treats each pixel as a raw
///    palette index rather than a colour sample.
/// 3. Decoding the patched bytes via `image::load_from_memory` (yields luma8).
/// 4. Expanding each index through the colormap to produce a full RGB image.
mod palette_tiff {
    use std::io::Cursor;
    use tiff::decoder::{Decoder, ifd::Value};
    use tiff::tags::Tag;

    const PMI_PALETTE: u16 = 3;
    const PMI_BLACKISZERO: u16 = 1;

    /// Returns `true` if `bytes` is a TIFF whose first IFD has
    /// `PhotometricInterpretation = 3` (Palette / colour-mapped).
    pub fn is_palette_tiff(bytes: &[u8]) -> bool {
        read_pmi(bytes) == Some(PMI_PALETTE)
    }

    /// Decode a palette-indexed TIFF into an RGB [`image::DynamicImage`].
    pub fn decode(bytes: &[u8]) -> Result<image::DynamicImage, String> {
        let (reds, greens, blues) = read_colormap(bytes)?;

        let mut patched = bytes.to_vec();
        patch_pmi(&mut patched, PMI_PALETTE, PMI_BLACKISZERO)
            .ok_or("palette TIFF: PhotometricInterpretation tag not found")?;

        // After the patch the TIFF looks like a greyscale image; each
        // pixel value is actually a palette index.
        let luma = image::load_from_memory(&patched)
            .map_err(|e| format!("palette TIFF: decode after patch failed: {e}"))?
            .into_luma8();

        let (w, h) = (luma.width(), luma.height());
        let mut rgb = image::RgbImage::new(w, h);
        for (x, y, px) in luma.enumerate_pixels() {
            let i = px[0] as usize;
            rgb.put_pixel(x, y, image::Rgb([reds[i], greens[i], blues[i]]));
        }
        Ok(image::DynamicImage::ImageRgb8(rgb))
    }

    // ── TIFF ColorMap reader ─────────────────────────────────────────────────

    /// Read the TIFF `ColorMap` tag and return three 256-entry 8-bit tables
    /// `(reds, greens, blues)`.
    ///
    /// TIFF stores colormap entries as 16-bit values (0–65535); we right-shift
    /// by 8 to convert to the conventional 8-bit range.
    type Palette = ([u8; 256], [u8; 256], [u8; 256]);

    fn read_colormap(bytes: &[u8]) -> Result<Palette, String> {
        let mut dec = Decoder::new(Cursor::new(bytes))
            .map_err(|e| format!("palette TIFF: init decoder: {e}"))?;

        let val = dec
            .get_tag(Tag::ColorMap)
            .map_err(|e| format!("palette TIFF: ColorMap tag missing: {e}"))?;

        let flat: Vec<u16> = match val {
            Value::List(items) => items
                .iter()
                .map(|v| match v {
                    Value::Short(n) => *n,
                    Value::UnsignedBig(n) => *n as u16,
                    Value::Byte(n) => *n as u16,
                    _ => 0,
                })
                .collect(),
            _ => return Err("palette TIFF: unexpected ColorMap value type".into()),
        };

        if flat.len() != 768 {
            return Err(format!(
                "palette TIFF: expected 768 ColorMap entries, got {}",
                flat.len()
            ));
        }

        let mut reds   = [0u8; 256];
        let mut greens = [0u8; 256];
        let mut blues  = [0u8; 256];
        for i in 0..256 {
            reds[i]   = (flat[i]       >> 8) as u8;
            greens[i] = (flat[256 + i] >> 8) as u8;
            blues[i]  = (flat[512 + i] >> 8) as u8;
        }
        Ok((reds, greens, blues))
    }

    // ── Low-level TIFF IFD helpers ───────────────────────────────────────────

    /// Read the `PhotometricInterpretation` value from the first IFD.
    fn read_pmi(bytes: &[u8]) -> Option<u16> {
        let (le, ifd_off) = parse_header(bytes)?;
        let ifd_off = ifd_off as usize;
        if ifd_off + 2 > bytes.len() {
            return None;
        }
        let count = ru16(bytes, ifd_off, le) as usize;
        for i in 0..count {
            let off = ifd_off + 2 + i * 12;
            if off + 12 > bytes.len() {
                break;
            }
            if ru16(bytes, off, le) == 262 {
                // PMI tag — value is stored inline (SHORT, count=1).
                return Some(ru16(bytes, off + 8, le));
            }
        }
        None
    }

    /// Overwrite the `PhotometricInterpretation` value in-place.
    /// Returns `Some(())` if the tag was found with the expected `from` value.
    fn patch_pmi(bytes: &mut [u8], from: u16, to: u16) -> Option<()> {
        let (le, ifd_off) = parse_header(bytes)?;
        let ifd_off = ifd_off as usize;
        if ifd_off + 2 > bytes.len() {
            return None;
        }
        let count = ru16(bytes, ifd_off, le) as usize;
        for i in 0..count {
            let off = ifd_off + 2 + i * 12;
            if off + 12 > bytes.len() {
                break;
            }
            if ru16(bytes, off, le) == 262 && ru16(bytes, off + 8, le) == from {
                wu16(bytes, off + 8, to, le);
                return Some(());
            }
        }
        None
    }

    /// Parse the TIFF file header; returns `(little_endian, ifd_offset)`.
    fn parse_header(bytes: &[u8]) -> Option<(bool, u32)> {
        if bytes.len() < 8 {
            return None;
        }
        let le = match &bytes[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        if ru16(bytes, 2, le) != 42 {
            return None;
        }
        Some((le, ru32(bytes, 4, le)))
    }

    fn ru16(b: &[u8], off: usize, le: bool) -> u16 {
        let arr = [b[off], b[off + 1]];
        if le { u16::from_le_bytes(arr) } else { u16::from_be_bytes(arr) }
    }

    fn ru32(b: &[u8], off: usize, le: bool) -> u32 {
        let arr = [b[off], b[off + 1], b[off + 2], b[off + 3]];
        if le { u32::from_le_bytes(arr) } else { u32::from_be_bytes(arr) }
    }

    fn wu16(b: &mut [u8], off: usize, val: u16, le: bool) {
        let arr = if le { val.to_le_bytes() } else { val.to_be_bytes() };
        b[off]     = arr[0];
        b[off + 1] = arr[1];
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Build a minimal valid 2×2 uncompressed palette TIFF in memory.
        ///
        /// Palette: index 0 = white (255, 255, 255), index 1 = black (0, 0, 0).
        /// Pixels (row-major): `[1, 0, 0, 1]`
        ///   → top-left=black, top-right=white, bottom-left=white, bottom-right=black.
        fn make_test_tiff() -> Vec<u8> {
            // Layout:
            //   0..8    — header
            //   8..134  — IFD (2-byte count + 10 × 12-byte entries + 4-byte next-IFD)
            //   134..1670 — ColorMap (768 × 2 bytes)
            //   1670..1674 — pixel data
            const COLORMAP_OFF: u32 = 134;
            const PIXEL_OFF: u32 = 134 + 768 * 2; // 1670

            // Write a 12-byte IFD entry: tag, type, count, value-or-offset (all LE).
            fn ifd(buf: &mut Vec<u8>, tag: u16, type_id: u16, count: u32, val: u32) {
                buf.extend_from_slice(&tag.to_le_bytes());
                buf.extend_from_slice(&type_id.to_le_bytes());
                buf.extend_from_slice(&count.to_le_bytes());
                buf.extend_from_slice(&val.to_le_bytes());
            }
            const SHORT: u16 = 3;
            const LONG: u16 = 4;

            let mut buf: Vec<u8> = Vec::with_capacity(1674);

            // Header
            buf.extend_from_slice(b"II");
            buf.extend_from_slice(&42u16.to_le_bytes());
            buf.extend_from_slice(&8u32.to_le_bytes()); // IFD at offset 8

            // IFD: 10 entries (tags must be in ascending numeric order)
            buf.extend_from_slice(&10u16.to_le_bytes());
            ifd(&mut buf, 256, SHORT, 1, 2);              // ImageWidth = 2
            ifd(&mut buf, 257, SHORT, 1, 2);              // ImageLength = 2
            ifd(&mut buf, 258, SHORT, 1, 8);              // BitsPerSample = 8
            ifd(&mut buf, 259, SHORT, 1, 1);              // Compression = 1 (none)
            ifd(&mut buf, 262, SHORT, 1, 3);              // PhotometricInterpretation = Palette
            ifd(&mut buf, 273, LONG,  1, PIXEL_OFF);      // StripOffsets
            ifd(&mut buf, 277, SHORT, 1, 1);              // SamplesPerPixel = 1
            ifd(&mut buf, 278, SHORT, 1, 2);              // RowsPerStrip = 2
            ifd(&mut buf, 279, LONG,  1, 4);              // StripByteCounts = 4
            ifd(&mut buf, 320, SHORT, 768, COLORMAP_OFF); // ColorMap
            buf.extend_from_slice(&0u32.to_le_bytes());   // next IFD = none

            assert_eq!(buf.len(), 134);

            // ColorMap: three 256-entry tables of u16 LE (R, G, B).
            // Index 0 = white (65535), all others = black (0).
            for _channel in 0..3 {
                buf.extend_from_slice(&65535u16.to_le_bytes()); // index 0
                for _ in 1..256 {
                    buf.extend_from_slice(&0u16.to_le_bytes());
                }
            }

            assert_eq!(buf.len(), 1670);

            // Pixel data: row 0 = [1, 0] (black, white), row 1 = [0, 1] (white, black)
            buf.extend_from_slice(&[1u8, 0, 0, 1]);

            buf
        }

        #[test]
        fn test_tiff_detected_as_palette() {
            assert!(is_palette_tiff(&make_test_tiff()));
        }

        #[test]
        fn non_tiff_not_detected_as_palette() {
            assert!(!is_palette_tiff(b"not a tiff at all"));
            assert!(!is_palette_tiff(&[]));
            // A valid PNG header should not be detected as a palette TIFF.
            assert!(!is_palette_tiff(b"\x89PNG\r\n\x1a\n"));
        }

        #[test]
        fn patch_pmi_changes_value() {
            let mut tiff = make_test_tiff();
            assert_eq!(read_pmi(&tiff), Some(PMI_PALETTE));
            patch_pmi(&mut tiff, PMI_PALETTE, PMI_BLACKISZERO).expect("patch failed");
            assert_eq!(read_pmi(&tiff), Some(PMI_BLACKISZERO));
        }

        #[test]
        fn patch_pmi_wrong_from_value_returns_none() {
            let mut tiff = make_test_tiff();
            // Patching from a value that isn't actually stored → None.
            assert!(patch_pmi(&mut tiff, 99, PMI_BLACKISZERO).is_none());
            // Original value unchanged.
            assert_eq!(read_pmi(&tiff), Some(PMI_PALETTE));
        }

        #[test]
        fn decode_palette_tiff_correct_pixels() {
            let tiff = make_test_tiff();
            let img = decode(&tiff).expect("decode failed");
            let rgb = img.into_rgb8();
            assert_eq!(rgb.dimensions(), (2, 2));
            // Index 1 → black
            assert_eq!(rgb.get_pixel(0, 0), &image::Rgb([0, 0, 0]));
            // Index 0 → white
            assert_eq!(rgb.get_pixel(1, 0), &image::Rgb([255, 255, 255]));
            assert_eq!(rgb.get_pixel(0, 1), &image::Rgb([255, 255, 255]));
            assert_eq!(rgb.get_pixel(1, 1), &image::Rgb([0, 0, 0]));
        }

        #[test]
        fn load_image_handles_palette_tiff() {
            let tiff = make_test_tiff();
            let result = super::super::load_image(&tiff);
            assert!(result.is_ok(), "load_image failed: {:?}", result.err());
            let img = result.unwrap();
            assert_eq!((img.width(), img.height()), (2, 2));
        }

        #[test]
        fn load_image_falls_through_for_unsupported_format() {
            let result = super::super::load_image(b"garbage bytes that are not an image");
            assert!(result.is_err());
        }
    }
}
