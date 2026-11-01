use radicom::error::Error;
use radicom::pixel::photometric::PhotometricInterpretation;
use radicom::pixel::{PixelData, PixelDataRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RleLayout {
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    pub(crate) samples: usize,
    pub(crate) bytes_allocated: usize,
    pub(crate) planar: u16,
    pub(crate) pixel_count: usize,
    pub(crate) frame_length: usize,
    pub(crate) segment_count: usize,
    /// `BitsAllocated == 1`: the native frame packs pixels LSB-first without
    /// row alignment, while each RLE segment still holds one byte per pixel.
    pub(crate) bit_packed: bool,
}

/// Raw pixel metadata used to build a layout; private to keep the signature flat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayoutInput {
    rows: u32,
    columns: u32,
    bits_allocated: u16,
    bits_stored: u16,
    high_bit: u16,
    samples: u16,
    planar: u16,
    photometric: Option<PhotometricInterpretation>,
}

impl RleLayout {
    pub(crate) fn from_pixel_data(data: &PixelDataRef<'_>) -> Result<Self, String> {
        Self::from_values(LayoutInput {
            rows: data.height(),
            columns: data.width(),
            bits_allocated: data.bits_allocated().map_err(display_error)?,
            bits_stored: data.bits_stored(),
            high_bit: data.high_bit(),
            samples: data.samples_per_pixel(),
            planar: data.planar_configuration(),
            photometric: data.photometric_interpretation(),
        })
    }

    pub(crate) fn from_pixel_data_mut(data: &PixelData<'_>) -> Result<Self, String> {
        Self::from_values(LayoutInput {
            rows: data.height(),
            columns: data.width(),
            bits_allocated: data.bits_allocated().map_err(display_error)?,
            bits_stored: data.bits_stored(),
            high_bit: data.high_bit(),
            samples: data.samples_per_pixel(),
            planar: data.planar_configuration(),
            photometric: data.photometric_interpretation(),
        })
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        rows: u32,
        columns: u32,
        bits_allocated: u16,
        samples: u16,
        planar: u16,
    ) -> Result<Self, String> {
        Self::from_values(LayoutInput {
            rows,
            columns,
            bits_allocated,
            bits_stored: bits_allocated,
            high_bit: bits_allocated.saturating_sub(1),
            samples,
            planar,
            photometric: Some(if samples == 1 {
                PhotometricInterpretation::Monochrome2
            } else {
                PhotometricInterpretation::Rgb
            }),
        })
    }

    fn from_values(input: LayoutInput) -> Result<Self, String> {
        if input.rows == 0 || input.columns == 0 {
            return Err(format!(
                "image dimensions must be non-zero, got {}x{}",
                input.columns, input.rows
            ));
        }
        let bit_packed = input.bits_allocated == 1;
        let bytes_allocated: usize = match input.bits_allocated {
            1 | 8 => 1,
            16 => 2,
            other => {
                return Err(format!(
                    "unsupported BitsAllocated {other}; expected 1, 8, or 16"
                ))
            }
        };
        let samples = usize::from(input.samples);
        if samples == 0 {
            return Err("SamplesPerPixel must be at least 1".to_owned());
        }
        if bit_packed {
            if input.bits_stored != 1 {
                return Err(format!(
                    "BitsAllocated 1 requires BitsStored 1, got {}",
                    input.bits_stored
                ));
            }
            if input.high_bit != 0 {
                return Err(format!(
                    "BitsAllocated 1 requires HighBit 0, got {}",
                    input.high_bit
                ));
            }
            if samples != 1 {
                return Err(format!(
                    "BitsAllocated 1 requires SamplesPerPixel 1, got {samples}"
                ));
            }
            if !matches!(
                input.photometric,
                Some(
                    PhotometricInterpretation::Monochrome1 | PhotometricInterpretation::Monochrome2
                )
            ) {
                return Err(
                    "BitsAllocated 1 requires PhotometricInterpretation MONOCHROME1 or MONOCHROME2"
                        .to_owned(),
                );
            }
        }
        if samples > 1 && input.planar > 1 {
            return Err(format!(
                "PlanarConfiguration {} must be 0 or 1",
                input.planar
            ));
        }
        let rows = usize::try_from(input.rows).map_err(|_| "rows do not fit usize".to_owned())?;
        let columns =
            usize::try_from(input.columns).map_err(|_| "columns do not fit usize".to_owned())?;
        let pixel_count = rows
            .checked_mul(columns)
            .ok_or_else(|| "pixel count overflows usize".to_owned())?;
        let segment_count = bytes_allocated
            .checked_mul(samples)
            .ok_or_else(|| "segment count overflows usize".to_owned())?;
        if !(1..=15).contains(&segment_count) {
            return Err(format!("segment count {segment_count} is outside 1..=15"));
        }
        let frame_length = if bit_packed {
            // The whole frame rounds up to a byte; rows are not byte-aligned.
            pixel_count
                .checked_mul(samples)
                .ok_or_else(|| "frame length overflows usize".to_owned())?
                .div_ceil(8)
        } else {
            pixel_count
                .checked_mul(segment_count)
                .ok_or_else(|| "frame length overflows usize".to_owned())?
        };
        Ok(Self {
            rows,
            columns,
            samples,
            bytes_allocated,
            planar: input.planar,
            pixel_count,
            frame_length,
            segment_count,
            bit_packed,
        })
    }

    pub(crate) fn raw_offset(
        &self,
        pixel: usize,
        sample: usize,
        plane: usize,
    ) -> Result<usize, String> {
        if self.bit_packed {
            return Err("raw offset is undefined for bit-packed pixels".to_owned());
        }
        if pixel >= self.pixel_count || sample >= self.samples || plane >= self.bytes_allocated {
            return Err("pixel-plane index is outside the layout".to_owned());
        }
        let offset = if self.planar == 1 && self.samples > 1 {
            sample
                .checked_mul(self.pixel_count)
                .and_then(|value| value.checked_mul(self.bytes_allocated))
                .and_then(|value| value.checked_add(pixel.checked_mul(self.bytes_allocated)?))
                .and_then(|value| value.checked_add(plane))
        } else {
            pixel
                .checked_mul(self.samples)
                .and_then(|value| value.checked_mul(self.bytes_allocated))
                .and_then(|value| value.checked_add(sample.checked_mul(self.bytes_allocated)?))
                .and_then(|value| value.checked_add(plane))
        };
        offset.ok_or_else(|| "raw pixel offset overflows usize".to_owned())
    }

    pub(crate) fn segment_indices(&self, segment: usize) -> Result<(usize, usize), String> {
        if segment >= self.segment_count {
            return Err("segment index is outside the layout".to_owned());
        }
        Ok((
            segment / self.bytes_allocated,
            self.bytes_allocated - 1 - segment % self.bytes_allocated,
        ))
    }
}

fn display_error(error: Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bit_layout(
        bits_stored: u16,
        high_bit: u16,
        samples: u16,
        photometric: PhotometricInterpretation,
    ) -> LayoutInput {
        LayoutInput {
            rows: 3,
            columns: 3,
            bits_allocated: 1,
            bits_stored,
            high_bit,
            samples,
            planar: 0,
            photometric: Some(photometric),
        }
    }

    #[test]
    fn maps_interleaved_and_planar_offsets() {
        let interleaved = RleLayout::new_for_test(1, 2, 16, 1, 0).unwrap();
        assert_eq!(interleaved.raw_offset(0, 0, 1).unwrap(), 1);
        assert_eq!(interleaved.raw_offset(1, 0, 0).unwrap(), 2);
        let planar = RleLayout::new_for_test(1, 2, 8, 3, 1).unwrap();
        assert_eq!(planar.raw_offset(0, 1, 0).unwrap(), 2);
        assert_eq!(planar.raw_offset(1, 2, 0).unwrap(), 5);
    }

    #[test]
    fn accepts_one_bit_monochrome() {
        let layout =
            RleLayout::from_values(bit_layout(1, 0, 1, PhotometricInterpretation::Monochrome2))
                .unwrap();
        assert!(layout.bit_packed);
        assert_eq!(layout.bytes_allocated, 1);
        assert_eq!(layout.segment_count, 1);
        assert_eq!(layout.pixel_count, 9);
        assert_eq!(layout.frame_length, 2);
        assert!(layout.raw_offset(0, 0, 0).is_err());
    }

    #[test]
    fn rejects_invalid_one_bit_layouts() {
        let spp = RleLayout::from_values(bit_layout(1, 0, 3, PhotometricInterpretation::Rgb))
            .unwrap_err();
        assert!(spp.contains("SamplesPerPixel 1"), "{spp}");
        let stored =
            RleLayout::from_values(bit_layout(2, 0, 1, PhotometricInterpretation::Monochrome2))
                .unwrap_err();
        assert!(stored.contains("BitsStored 1"), "{stored}");
        let high_bit =
            RleLayout::from_values(bit_layout(1, 1, 1, PhotometricInterpretation::Monochrome2))
                .unwrap_err();
        assert!(high_bit.contains("HighBit 0"), "{high_bit}");
        let palette =
            RleLayout::from_values(bit_layout(1, 0, 1, PhotometricInterpretation::PaletteColor))
                .unwrap_err();
        assert!(palette.contains("MONOCHROME1 or MONOCHROME2"), "{palette}");
    }

    #[test]
    fn rejects_invalid_layouts() {
        assert!(RleLayout::new_for_test(0, 1, 8, 1, 0).is_err());
        assert!(RleLayout::new_for_test(1, 1, 12, 1, 0).is_err());
        assert!(RleLayout::new_for_test(1, 1, 16, 8, 0).is_err());
        assert!(RleLayout::new_for_test(1, 1, 8, 3, 2).is_err());
    }
}
