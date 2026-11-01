//! DICOM pixel-data adapter for the JPEG 2000 codecs.
#![allow(clippy::needless_range_loop)]

use radicom::{
    model::{tag::Tag, vr::Vr},
    pixel::{photometric::PhotometricInterpretation, PixelData, PixelDataRef},
};

use crate::codestream;
use crate::htj2k::codestream as ht_codestream;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) bits_allocated: u16,
    pub(crate) bits_stored: u8,
    pub(crate) components: u8,
    pub(crate) planar: u16,
    pub(crate) signed: bool,
    pub(crate) pi: PhotometricInterpretation,
    pub(crate) frame_len: usize,
    pub(crate) ybr422: bool,
}

impl Layout {
    /// Decoded three-component output is always full-range RGB; the codestream
    /// may have used RCT/ICT internally, but the adapter exposes RGB samples.
    pub(crate) fn rgb_output(&self) -> Self {
        let mut layout = *self;
        if layout.components == 3 {
            layout.pi = PhotometricInterpretation::Rgb;
            layout.ybr422 = false;
        }
        layout
    }

    pub(crate) fn source(d: &PixelDataRef<'_>) -> Result<Self, String> {
        Self::values(
            d.width(),
            d.height(),
            d.bits_allocated().map_err(|e| e.to_string())?,
            d.bits_stored(),
            d.samples_per_pixel(),
            d.planar_configuration(),
            d.pixel_representation(),
            d.photometric_interpretation(),
        )
    }

    pub(crate) fn destination(d: &PixelData<'_>) -> Result<Self, String> {
        Self::values(
            d.width(),
            d.height(),
            d.bits_allocated().map_err(|e| e.to_string())?,
            d.bits_stored(),
            d.samples_per_pixel(),
            d.planar_configuration(),
            d.pixel_representation(),
            d.photometric_interpretation(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn values(
        w: u32,
        h: u32,
        ba: u16,
        bs: u16,
        comp: u16,
        planar: u16,
        rep: u16,
        pi: Option<PhotometricInterpretation>,
    ) -> Result<Self, String> {
        let width = usize::try_from(w).map_err(|_| "width overflow")?;
        let height = usize::try_from(h).map_err(|_| "height overflow")?;
        if width == 0 || height == 0 {
            return Err("dimensions must be non-zero".into());
        }
        if !matches!(ba, 8 | 16) {
            return Err("BitsAllocated must be 8 or 16".into());
        }
        let bits = u8::try_from(bs).map_err(|_| "BitsStored overflow")?;
        if !(1..=16).contains(&bits) || u16::from(bits) > ba {
            return Err("invalid BitsStored".into());
        }
        let components = u8::try_from(comp).map_err(|_| "SamplesPerPixel overflow")?;
        if !(1..=3).contains(&components) || planar > 1 || rep > 1 {
            return Err("unsupported pixel layout".into());
        }
        let pi = pi.ok_or("missing PhotometricInterpretation")?;
        if !is_supported(pi, components) {
            return Err("unsupported PhotometricInterpretation".into());
        }
        let ybr422 = pi == PhotometricInterpretation::YbrFull422;
        if ybr422 && (ba != 8 || width % 2 != 0) {
            return Err("YBR_FULL_422 requires 8-bit samples and an even width".into());
        }
        if ybr422 && planar != 0 {
            return Err("YBR_FULL_422 must be interleaved".into());
        }
        if rep == 1
            && matches!(
                pi,
                PhotometricInterpretation::YbrFull | PhotometricInterpretation::YbrFull422
            )
        {
            return Err("signed YBR samples are unsupported".into());
        }
        if components == 1 && planar != 0 {
            return Err("monochrome cannot be planar".into());
        }
        let frame_len = if ybr422 {
            width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(2))
                .ok_or("frame length overflow")?
        } else {
            width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(components as usize))
                .and_then(|n| n.checked_mul((ba / 8) as usize))
                .ok_or("frame length overflow")?
        };
        Ok(Self {
            width,
            height,
            bits_allocated: ba,
            bits_stored: bits,
            components,
            planar,
            signed: rep == 1,
            pi,
            frame_len,
            ybr422,
        })
    }

    fn sample(&self, bytes: &[u8], index: usize) -> i32 {
        let offset = index * (self.bits_allocated / 8) as usize;
        let raw = if self.bits_allocated == 8 {
            u32::from(bytes[offset])
        } else {
            u32::from(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
        };
        let mask = if self.bits_stored == 16 {
            0xFFFF
        } else {
            (1u32 << self.bits_stored) - 1
        };
        let value = raw & mask;
        if self.signed {
            let sign = 1u32 << (self.bits_stored - 1);
            ((value ^ sign) as i32) - sign as i32
        } else {
            value as i32
        }
    }

    /// Reads a native frame into component planes, level shifted for unsigned
    /// samples and converted to RGB when the source is YBR.
    pub(crate) fn read(&self, bytes: &[u8]) -> Result<Vec<Vec<i32>>, String> {
        if bytes.len() < self.frame_len {
            return Err("native frame is truncated".into());
        }
        let pixels = self.width * self.height;
        let mut planes = vec![vec![0i32; pixels]; self.components as usize];
        if self.ybr422 {
            for pair in 0..pixels / 2 {
                let base = pair * 4;
                let y0 = i32::from(bytes[base]);
                let cb = i32::from(bytes[base + 1]);
                let y1 = i32::from(bytes[base + 2]);
                let cr = i32::from(bytes[base + 3]);
                let (r0, g0, b0) = ybr_to_rgb(y0, cb, cr, self.bits_stored);
                let (r1, g1, b1) = ybr_to_rgb(y1, cb, cr, self.bits_stored);
                planes[0][pair * 2] = r0;
                planes[1][pair * 2] = g0;
                planes[2][pair * 2] = b0;
                planes[0][pair * 2 + 1] = r1;
                planes[1][pair * 2 + 1] = g1;
                planes[2][pair * 2 + 1] = b1;
            }
        } else {
            for (component, plane) in planes.iter_mut().enumerate() {
                for pixel in 0..pixels {
                    let index = if self.planar == 1 {
                        component * pixels + pixel
                    } else {
                        pixel * self.components as usize + component
                    };
                    plane[pixel] = self.sample(bytes, index);
                }
            }
            if self.pi == PhotometricInterpretation::YbrFull {
                for pixel in 0..pixels {
                    let (r, g, b) = ybr_to_rgb(
                        planes[0][pixel],
                        planes[1][pixel],
                        planes[2][pixel],
                        self.bits_stored,
                    );
                    planes[0][pixel] = r;
                    planes[1][pixel] = g;
                    planes[2][pixel] = b;
                }
            }
        }
        if !self.signed {
            let shift = 1i32 << (self.bits_stored - 1);
            for plane in &mut planes {
                for value in plane.iter_mut() {
                    *value -= shift;
                }
            }
        }
        Ok(planes)
    }

    /// Packs decoded component planes (level shifted) into a native frame.
    pub(crate) fn write(&self, planes: &[Vec<i32>]) -> Result<Vec<u8>, String> {
        if planes.len() != self.components as usize {
            return Err("decoded component count mismatch".into());
        }
        let pixels = self.width * self.height;
        if planes.iter().any(|plane| plane.len() != pixels) {
            return Err("decoded sample count mismatch".into());
        }
        let maxval = if self.bits_stored == 16 {
            i32::from(u16::MAX)
        } else {
            (1i32 << self.bits_stored) - 1
        };
        let shift = if self.signed {
            0
        } else {
            1i32 << (self.bits_stored - 1)
        };
        let min = if self.signed {
            -(1i32 << (self.bits_stored - 1))
        } else {
            0
        };
        let max = if self.signed {
            (1i32 << (self.bits_stored - 1)) - 1
        } else {
            maxval
        };
        let mask = if self.bits_stored == 16 {
            u16::MAX
        } else {
            (1u16 << self.bits_stored) - 1
        };
        let mut bytes = vec![0u8; self.frame_len];
        if self.ybr422 {
            if planes.len() != 3 {
                return Err("YBR_FULL_422 requires three components".into());
            }
            for pair in 0..pixels / 2 {
                let base = pair * 4;
                let r0 = (planes[0][pair * 2] + shift).clamp(0, maxval);
                let g0 = (planes[1][pair * 2] + shift).clamp(0, maxval);
                let b0 = (planes[2][pair * 2] + shift).clamp(0, maxval);
                let r1 = (planes[0][pair * 2 + 1] + shift).clamp(0, maxval);
                let g1 = (planes[1][pair * 2 + 1] + shift).clamp(0, maxval);
                let b1 = (planes[2][pair * 2 + 1] + shift).clamp(0, maxval);
                let (y0, cb0, cr0) = rgb_to_ybr(r0, g0, b0, self.bits_stored);
                let (y1, cb1, cr1) = rgb_to_ybr(r1, g1, b1, self.bits_stored);
                bytes[base] = y0 as u8;
                bytes[base + 1] = ((cb0 + cb1) / 2) as u8;
                bytes[base + 2] = y1 as u8;
                bytes[base + 3] = ((cr0 + cr1) / 2) as u8;
            }
            return Ok(bytes);
        }
        for component in 0..self.components as usize {
            for pixel in 0..pixels {
                let value = (planes[component][pixel] + shift).clamp(min, max);
                let raw = if self.signed {
                    value as i16 as u16
                } else {
                    (value as u16) & mask
                };
                let index = if self.planar == 1 {
                    component * pixels + pixel
                } else {
                    pixel * self.components as usize + component
                };
                let offset = index * (self.bits_allocated / 8) as usize;
                if self.bits_allocated == 8 {
                    bytes[offset] = raw as u8;
                } else {
                    bytes[offset..offset + 2].copy_from_slice(&raw.to_le_bytes());
                }
            }
        }
        Ok(bytes)
    }
}

fn is_supported(pi: PhotometricInterpretation, components: u8) -> bool {
    match components {
        1 => matches!(
            pi,
            PhotometricInterpretation::Monochrome1 | PhotometricInterpretation::Monochrome2
        ),
        3 => matches!(
            pi,
            PhotometricInterpretation::Rgb
                | PhotometricInterpretation::YbrFull
                | PhotometricInterpretation::YbrFull422
                | PhotometricInterpretation::YbrRct
                | PhotometricInterpretation::YbrIct
        ),
        _ => false,
    }
}

fn ybr_to_rgb(y: i32, cb: i32, cr: i32, bits: u8) -> (i32, i32, i32) {
    let max = (1i32 << bits) - 1;
    let center = 1i32 << (bits - 1);
    let y = f64::from(y);
    let cb = f64::from(cb - center);
    let cr = f64::from(cr - center);
    let clamp = |value: f64| value.round().clamp(0.0, f64::from(max)) as i32;
    (
        clamp(y + 1.4020 * cr),
        clamp(y - 0.3441 * cb - 0.7141 * cr),
        clamp(y + 1.7720 * cb),
    )
}

fn rgb_to_ybr(r: i32, g: i32, b: i32, bits: u8) -> (i32, i32, i32) {
    let max = f64::from((1i32 << bits) - 1);
    let center = f64::from(1i32 << (bits - 1));
    let r = f64::from(r);
    let g = f64::from(g);
    let b = f64::from(b);
    let clamp = |value: f64| value.round().clamp(0.0, max) as i32;
    (
        clamp(0.2990 * r + 0.5870 * g + 0.1140 * b),
        clamp(-0.1687 * r - 0.3313 * g + 0.5000 * b + center),
        clamp(0.5000 * r - 0.4187 * g - 0.0813 * b + center),
    )
}

pub(crate) fn encode(layout: &Layout, raw: &[u8], reversible: bool) -> Result<Vec<u8>, String> {
    if matches!(
        layout.pi,
        PhotometricInterpretation::YbrRct | PhotometricInterpretation::YbrIct
    ) {
        return Err("native source cannot use YBR_RCT or YBR_ICT".into());
    }
    let planes = layout.read(raw)?;
    let mut components = codestream::build_components(
        &planes,
        layout.width as i32,
        layout.height as i32,
        layout.bits_stored,
        layout.signed,
        codestream::DEFAULT_LEVELS,
    )?;
    codestream::encode(&mut components, reversible)
}

pub(crate) fn encode_ht(
    layout: &Layout,
    raw: &[u8],
    options: &ht_codestream::EncodeOptions,
) -> Result<Vec<u8>, String> {
    if matches!(
        layout.pi,
        PhotometricInterpretation::YbrRct | PhotometricInterpretation::YbrIct
    ) {
        return Err("native source cannot use YBR_RCT or YBR_ICT".into());
    }
    let planes = layout.read(raw)?;
    let mut components = codestream::build_components(
        &planes,
        layout.width as i32,
        layout.height as i32,
        layout.bits_stored,
        layout.signed,
        options.levels,
    )?;
    ht_codestream::encode(&mut components, options)
}

pub(crate) fn decode_ht(
    layout: &Layout,
    data: &[u8],
    expected_progression: Option<u8>,
) -> Result<Vec<u8>, String> {
    let components = ht_codestream::decode(data, expected_progression)?;
    if components.len() != layout.components as usize {
        return Err("codestream component count does not match destination".into());
    }
    if components[0].width as usize != layout.width
        || components[0].height as usize != layout.height
        || components[0].precision != layout.bits_stored
        || components[0].signed != layout.signed
    {
        return Err("codestream layout does not match destination".into());
    }
    let planes: Vec<Vec<i32>> = components
        .iter()
        .map(|component| component.samples.clone())
        .collect();
    layout.write(&planes)
}
pub(crate) fn decode(layout: &Layout, data: &[u8]) -> Result<Vec<u8>, String> {
    let components = codestream::decode(data)?;
    if components.len() != layout.components as usize {
        return Err("codestream component count does not match destination".into());
    }
    if components[0].width as usize != layout.width
        || components[0].height as usize != layout.height
        || components[0].precision != layout.bits_stored
        || components[0].signed != layout.signed
    {
        return Err("codestream layout does not match destination".into());
    }
    let planes: Vec<Vec<i32>> = components
        .iter()
        .map(|component| component.samples.clone())
        .collect();
    layout.write(&planes)
}

pub(crate) fn update_color_metadata(dst: &mut PixelData<'_>, layout: &Layout) {
    if layout.components == 3 {
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
        dst.dataset_mut()
            .insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
    }
}

pub(crate) fn update_decoded_color_metadata(dst: &mut PixelData<'_>, layout: &Layout) {
    if layout.components == 3 {
        let photometric = if layout.ybr422 { "YBR_FULL_422" } else { "RGB" };
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, photometric);
        dst.dataset_mut()
            .insert_value(Tag::PLANAR_CONFIGURATION, layout.planar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono8() -> Layout {
        Layout {
            width: 4,
            height: 2,
            bits_allocated: 8,
            bits_stored: 8,
            components: 1,
            planar: 0,
            signed: false,
            pi: PhotometricInterpretation::Monochrome2,
            frame_len: 8,
            ybr422: false,
        }
    }

    #[test]
    fn reads_and_writes_level_shifted_samples() {
        let layout = mono8();
        let raw: Vec<u8> = (0..8).map(|i| (i * 31) as u8).collect();
        let planes = layout.read(&raw).unwrap();
        assert_eq!(planes[0][0], -128);
        let bytes = layout.write(&planes).unwrap();
        assert_eq!(bytes, raw);
    }

    #[test]
    fn ybr_full_converts_to_rgb() {
        let layout = Layout {
            width: 1,
            height: 1,
            components: 3,
            frame_len: 3,
            pi: PhotometricInterpretation::YbrFull,
            ..mono8()
        };
        let raw = [128u8, 128, 128];
        let planes = layout.read(&raw).unwrap();
        for plane in &planes {
            assert_eq!(plane[0], 0);
        }
    }
}
