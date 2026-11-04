//! DICOM pixel-data adapter for the JPEG 2000 codecs.
#![allow(clippy::needless_range_loop)]

use radicom::{
    model::{tag::Tag, vr::Vr},
    pixel::{photometric::PhotometricInterpretation, PixelData, PixelDataRef},
};

use crate::codestream;
use crate::geometry::Component;
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
    ///
    /// 4:2:2 destinations size frames at two bytes per pixel, so the frame
    /// length is recomputed for the three full-resolution components.
    pub(crate) fn rgb_output(&self) -> Result<Self, String> {
        let mut layout = *self;
        if layout.components == 3 {
            layout.pi = PhotometricInterpretation::Rgb;
            layout.ybr422 = false;
            layout.frame_len = layout
                .width
                .checked_mul(layout.height)
                .and_then(|n| n.checked_mul(3))
                .and_then(|n| n.checked_mul((layout.bits_allocated / 8) as usize))
                .ok_or("frame length overflow")?;
        }
        Ok(layout)
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
            PhotometricInterpretation::Monochrome1
                | PhotometricInterpretation::Monochrome2
                | PhotometricInterpretation::PaletteColor
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

pub(crate) fn encode(
    layout: &Layout,
    raw: &[u8],
    reversible: bool,
    mct: bool,
    rates: &[f64],
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
        codestream::DEFAULT_LEVELS,
    )?;
    codestream::encode_with_precincts(&mut components, reversible, None, mct, rates)
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
    // fo-dicom passes `BitsAllocated` to OpenJPH, so a narrow sample range is
    // encoded inside a wider codestream: the SIZ precision and the level shift
    // both follow the allocated width.
    let precision = layout.bits_allocated as u8;
    let mut planes = layout.read(raw)?;
    if !layout.signed && layout.bits_allocated > u16::from(layout.bits_stored) {
        let extra = (1i32 << (layout.bits_allocated - 1)) - (1i32 << (layout.bits_stored - 1));
        for plane in &mut planes {
            for value in plane.iter_mut() {
                *value -= extra;
            }
        }
    }
    let mut components = codestream::build_components(
        &planes,
        layout.width as i32,
        layout.height as i32,
        precision,
        layout.signed,
        options.levels,
    )?;
    ht_codestream::encode(&mut components, options)
}

/// Re-centres samples when a codestream carries a wider precision than the
/// destination's `BitsStored`. fo-dicom's OpenJPH encoder writes `BitsAllocated`
/// into SIZ, so a 12-bit dataset arrives in a 16-bit codestream. Lossless
/// codestreams refuse values that do not fit the stored precision; lossy
/// reconstruction overshoot is clamped instead, because ringing is expected.
fn align_precision(components: &mut [Component], layout: &Layout) -> Result<(), String> {
    let lossy = components
        .iter()
        .any(|component| component.float_samples.is_some());
    let limit = 1i32 << (i32::from(layout.bits_stored) - 1);
    let clamp = |value: i32| value.clamp(-limit, limit - 1);
    // Each component may carry its own precision (SIZ stores per-component
    // precision), so realign every plane against its own precision instead of
    // assuming the first component's value applies to all.
    for component in components.iter_mut() {
        let precision = component.precision;
        if precision == layout.bits_stored {
            continue;
        }
        if layout.signed {
            // Signed codestream samples are not level shifted, so a wider
            // codestream precision is only a container width: no re-centring
            // needed, just a range check.
            for sample in &mut component.samples {
                if *sample < -limit || *sample >= limit {
                    if lossy {
                        *sample = clamp(*sample);
                    } else {
                        return Err(
                            "codestream samples do not fit the destination Bits Stored".into()
                        );
                    }
                }
            }
        } else {
            let shift = (1i32 << (i32::from(precision) - 1)) - limit;
            for sample in &mut component.samples {
                let value = *sample + shift;
                if value < -limit || value >= limit {
                    if lossy {
                        *sample = clamp(value);
                        continue;
                    }
                    return Err("codestream samples do not fit the destination Bits Stored".into());
                }
                *sample = value;
            }
        }
    }
    Ok(())
}

/// Validates that the decoded component set matches the destination layout.
/// Every component (not just the first) must agree, because SIZ carries
/// per-component precision/signedness and a mismatch would otherwise decode
/// into corrupted or clamped samples.
fn validate_components(components: &[Component], layout: &Layout) -> Result<(), String> {
    if components.len() != layout.components as usize {
        return Err("codestream component count does not match destination".into());
    }
    for component in components {
        if component.width as usize != layout.width
            || component.height as usize != layout.height
            || component.precision < layout.bits_stored
            || u16::from(component.precision) > layout.bits_allocated
            || component.signed != layout.signed
        {
            return Err("codestream layout does not match destination".into());
        }
    }
    Ok(())
}

pub(crate) fn decode_ht(
    layout: &Layout,
    data: &[u8],
    expected_progression: Option<u8>,
) -> Result<Vec<u8>, String> {
    let mut components = ht_codestream::decode(data, expected_progression)?;
    validate_components(&components, layout)?;
    align_precision(&mut components, layout)?;
    let planes: Vec<Vec<i32>> = components
        .iter()
        .map(|component| component.samples.clone())
        .collect();
    layout.write(&planes)
}
pub(crate) fn decode(layout: &Layout, data: &[u8]) -> Result<Vec<u8>, String> {
    let mut components = codestream::decode(data)?;
    validate_components(&components, layout)?;
    align_precision(&mut components, layout)?;
    let planes: Vec<Vec<i32>> = components
        .iter()
        .map(|component| component.samples.clone())
        .collect();
    layout.write(&planes)
}

/// Sets the encoded dataset's colour metadata. `mct_label` is the photometric
/// interpretation an applied multiple component transform requires
/// (`YBR_RCT`/`YBR_ICT`); without one the codec writes RGB when the caller
/// asked for it or when the source was converted from YBR to RGB.
pub(crate) fn update_color_metadata(
    dst: &mut PixelData<'_>,
    layout: &Layout,
    mct_label: Option<&str>,
    update_photometric: bool,
) {
    if layout.components != 3 {
        return;
    }
    let converted_from_ybr = matches!(
        layout.pi,
        PhotometricInterpretation::YbrFull | PhotometricInterpretation::YbrFull422
    );
    if let Some(label) = mct_label {
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, label);
    } else if update_photometric || converted_from_ybr {
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
    }
    dst.dataset_mut()
        .insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
}

/// Encapsulated 16-bit Pixel Data must carry OB, not OW. The dataset stores
/// encapsulated pixel data as a fragment sequence, so the sequence is rebuilt
/// with the same fragments and the other VR.
pub(crate) fn use_ob_vr_for_16_bit(dst: &mut PixelData<'_>) {
    use radicom::model::item::DicomItem;

    if !matches!(dst.bits_allocated(), Ok(16)) {
        return;
    }
    let fragments = match dst.dataset().item(&Tag::PIXEL_DATA) {
        Some(DicomItem::Fragments(sequence)) if sequence.vr() == Vr::Ow => {
            sequence.fragments().to_vec()
        }
        _ => return,
    };
    let mut replacement = radicom::model::item::DicomFragmentSequence::new(Tag::PIXEL_DATA, Vr::Ob);
    for fragment in fragments {
        replacement.push_fragment(fragment);
    }
    dst.dataset_mut()
        .insert_or_update(DicomItem::Fragments(replacement));
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

    fn full_width_codestream(values: &[i32]) -> Vec<u8> {
        let width = 32;
        let height = 8;
        let centered: Vec<i32> = values.iter().map(|value| value - 32768).collect();
        let mut components =
            crate::codestream::build_components(&[centered], width, height, 16, false, 2).unwrap();
        crate::codestream::encode_with_mct(&mut components, true, false).unwrap()
    }

    fn narrow_layout() -> Layout {
        Layout {
            width: 32,
            height: 8,
            bits_allocated: 16,
            bits_stored: 12,
            components: 1,
            planar: 0,
            signed: false,
            pi: PhotometricInterpretation::Monochrome2,
            frame_len: 32 * 8 * 2,
            ybr422: false,
        }
    }

    #[test]
    fn decodes_a_full_width_codestream_into_a_narrower_destination() {
        // fo-dicom's OpenJPH encoder writes BitsAllocated (16) as the SIZ
        // precision, so a 12-bit dataset arrives in a 16-bit codestream; the
        // samples must be re-centred on the destination's level shift.
        let values: Vec<i32> = (0..(32 * 8)).map(|i| (i * 37) % 4096).collect();
        let bytes = decode(&narrow_layout(), &full_width_codestream(&values)).unwrap();
        for (index, value) in values.iter().enumerate() {
            let stored = i32::from(u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]));
            assert_eq!(stored, *value, "sample {index}");
        }
    }

    #[test]
    fn rejects_a_full_width_codestream_whose_samples_do_not_fit() {
        let values: Vec<i32> = (0..(32 * 8)).map(|i| 4095 + (i % 900)).collect();
        let error = decode(&narrow_layout(), &full_width_codestream(&values)).unwrap_err();
        assert!(error.contains("do not fit"), "{error}");
    }

    #[test]
    fn signed_full_width_codestream_decodes_into_a_narrower_destination() {
        let values: Vec<i32> = (0..(32 * 8)).map(|i| (i * 53) % 4096 - 2048).collect();
        let mut components =
            crate::codestream::build_components(std::slice::from_ref(&values), 32, 8, 16, true, 2)
                .unwrap();
        let stream = crate::codestream::encode_with_mct(&mut components, true, false).unwrap();
        let layout = Layout {
            signed: true,
            ..narrow_layout()
        };
        let bytes = decode(&layout, &stream).unwrap();
        for (index, value) in values.iter().enumerate() {
            let stored = i16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]);
            assert_eq!(i32::from(stored), *value, "sample {index}");
        }
    }

    #[test]
    fn signed_ht_full_width_codestream_decodes_into_a_narrower_destination() {
        let values: Vec<i32> = (0..(32 * 8)).map(|i| (i * 53) % 4096 - 2048).collect();
        let mut components =
            crate::codestream::build_components(std::slice::from_ref(&values), 32, 8, 16, true, 2)
                .unwrap();
        let options = ht_codestream::EncodeOptions {
            progression: 0,
            reversible: true,
            levels: 2,
            mct: false,
            quality: 0,
            insert_tlm: false,
        };
        let stream = ht_codestream::encode(&mut components, &options).unwrap();
        let layout = Layout {
            signed: true,
            ..narrow_layout()
        };
        let bytes = decode_ht(&layout, &stream, None).unwrap();
        for (index, value) in values.iter().enumerate() {
            let stored = i16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]);
            assert_eq!(i32::from(stored), *value, "sample {index}");
        }
    }

    #[test]
    fn lossy_overshoot_is_clamped_but_lossless_refuses_it() {
        let mut components = vec![Component::new(0, 0, 0, 2, 1, 0, 16, false).unwrap()];
        components[0].samples = vec![40000, -40000];
        components[0].float_samples = Some(vec![0.0; 2]);
        let mut layout = narrow_layout();
        layout.width = 2;
        layout.height = 1;
        layout.frame_len = 4;
        align_precision(&mut components, &layout).unwrap();
        assert_eq!(components[0].samples, vec![2047, -2048]);

        components[0].float_samples = None;
        components[0].samples = vec![40000, -40000];
        assert!(align_precision(&mut components, &layout).is_err());
    }

    #[test]
    fn every_component_layout_is_validated() {
        let mut layout = narrow_layout();
        layout.components = 2;
        let good = vec![
            Component::new(0, 0, 0, 32, 8, 0, 12, false).unwrap(),
            Component::new(1, 0, 0, 32, 8, 0, 12, false).unwrap(),
        ];
        assert!(validate_components(&good, &layout).is_ok());

        let mut signed_mismatch = good.clone();
        signed_mismatch[1].signed = true;
        assert!(validate_components(&signed_mismatch, &layout).is_err());

        let mut low_precision = good.clone();
        low_precision[1].precision = 8; // below BitsStored (12)
        assert!(validate_components(&low_precision, &layout).is_err());
    }

    #[test]
    fn align_precision_uses_each_component_precision() {
        let mut layout = narrow_layout();
        layout.components = 2;
        layout.width = 2;
        layout.height = 1;
        layout.frame_len = 4;
        let mut components = vec![
            Component::new(0, 0, 0, 2, 1, 0, 12, false).unwrap(), // == BitsStored
            Component::new(1, 0, 0, 2, 1, 0, 16, false).unwrap(), // wider container
        ];
        components[0].samples = vec![0, 100];
        components[1].samples = vec![-30720, -30719];
        align_precision(&mut components, &layout).unwrap();
        assert_eq!(components[0].samples, vec![0, 100]);
        assert_eq!(components[1].samples, vec![0, 1]);
    }
}
