use radicom::error::Error;
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
}

impl RleLayout {
    pub(crate) fn from_pixel_data(data: &PixelDataRef<'_>) -> Result<Self, String> {
        Self::from_values(
            data.height(),
            data.width(),
            data.bits_allocated().map_err(display_error)?,
            data.samples_per_pixel(),
            data.planar_configuration(),
        )
    }

    pub(crate) fn from_pixel_data_mut(data: &PixelData<'_>) -> Result<Self, String> {
        Self::from_values(
            data.height(),
            data.width(),
            data.bits_allocated().map_err(display_error)?,
            data.samples_per_pixel(),
            data.planar_configuration(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        rows: u32,
        columns: u32,
        bits_allocated: u16,
        samples: u16,
        planar: u16,
    ) -> Result<Self, String> {
        Self::from_values(rows, columns, bits_allocated, samples, planar)
    }

    fn from_values(
        rows: u32,
        columns: u32,
        bits_allocated: u16,
        samples: u16,
        planar: u16,
    ) -> Result<Self, String> {
        if rows == 0 || columns == 0 {
            return Err(format!(
                "image dimensions must be non-zero, got {columns}x{rows}"
            ));
        }
        let bytes_allocated: usize = match bits_allocated {
            8 => 1,
            16 => 2,
            other => {
                return Err(format!(
                    "unsupported BitsAllocated {other}; expected 8 or 16"
                ))
            }
        };
        let samples = usize::from(samples);
        if samples == 0 {
            return Err("SamplesPerPixel must be at least 1".to_owned());
        }
        if samples > 1 && planar > 1 {
            return Err(format!("PlanarConfiguration {planar} must be 0 or 1"));
        }
        let rows = usize::try_from(rows).map_err(|_| "rows do not fit usize".to_owned())?;
        let columns =
            usize::try_from(columns).map_err(|_| "columns do not fit usize".to_owned())?;
        let pixel_count = rows
            .checked_mul(columns)
            .ok_or_else(|| "pixel count overflows usize".to_owned())?;
        let segment_count = bytes_allocated
            .checked_mul(samples)
            .ok_or_else(|| "segment count overflows usize".to_owned())?;
        if !(1..=15).contains(&segment_count) {
            return Err(format!("segment count {segment_count} is outside 1..=15"));
        }
        let frame_length = pixel_count
            .checked_mul(segment_count)
            .ok_or_else(|| "frame length overflows usize".to_owned())?;
        Ok(Self {
            rows,
            columns,
            samples,
            bytes_allocated,
            planar,
            pixel_count,
            frame_length,
            segment_count,
        })
    }

    pub(crate) fn raw_offset(
        &self,
        pixel: usize,
        sample: usize,
        plane: usize,
    ) -> Result<usize, String> {
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
    fn rejects_invalid_layouts() {
        assert!(RleLayout::new_for_test(0, 1, 8, 1, 0).is_err());
        assert!(RleLayout::new_for_test(1, 1, 12, 1, 0).is_err());
        assert!(RleLayout::new_for_test(1, 1, 16, 8, 0).is_err());
        assert!(RleLayout::new_for_test(1, 1, 8, 3, 2).is_err());
    }
}
