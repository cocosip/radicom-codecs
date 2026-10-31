#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
use crate::{
    color,
    marker::{parse_header, split_scan, write_eoi, write_sof55, write_soi, write_sos, FrameHeader},
    preset::Preset,
    scan::{decode_scan, encode_scan, ScanConfig},
};
use radicom::pixel::codec::JpegLsParams;
use radicom::{
    model::{tag::Tag, vr::Vr},
    pixel::{photometric::PhotometricInterpretation, PixelData, PixelDataRef},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub width: usize,
    pub height: usize,
    pub bits_allocated: u16,
    pub bits_stored: u8,
    pub components: u8,
    pub planar: u16,
    pub signed: bool,
    pub pi: PhotometricInterpretation,
    pub frame_len: usize,
}
impl Layout {
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
        if !(2..=16).contains(&bits) || u16::from(bits) > ba {
            return Err("invalid BitsStored".into());
        }
        let components = u8::try_from(comp).map_err(|_| "SamplesPerPixel overflow")?;
        if !(1..=3).contains(&components) || planar > 1 || rep > 1 {
            return Err("unsupported pixel layout".into());
        }
        let pi = pi.ok_or("missing PhotometricInterpretation")?;
        if !color::is_supported(pi, components) {
            return Err("unsupported PhotometricInterpretation".into());
        }
        if rep == 1 && pi == PhotometricInterpretation::YbrFull {
            return Err("signed YBR_FULL samples are unsupported".into());
        }
        if components == 1 && planar != 0 {
            return Err("monochrome cannot be planar".into());
        }
        let frame_len = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(components as usize))
            .and_then(|n| n.checked_mul((ba / 8) as usize))
            .ok_or("frame length overflow")?;
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
        })
    }
    pub(crate) fn maxval(&self) -> u16 {
        if self.bits_stored == 16 {
            u16::MAX
        } else {
            (1u16 << self.bits_stored) - 1
        }
    }
    pub(crate) fn read(&self, b: &[u8]) -> Result<Vec<u16>, String> {
        if b.len() < self.frame_len {
            return Err("native frame is truncated".into());
        }
        let count = self.width * self.height * self.components as usize;
        let mask = self.maxval();
        let pixels = self.width * self.height;
        let mut out = Vec::with_capacity(count);
        for n in 0..count {
            let pix = n / self.components as usize;
            let c = n % self.components as usize;
            let logical = if self.planar == 1 {
                c * pixels + pix
            } else {
                n
            };
            let off = logical * (self.bits_allocated / 8) as usize;
            let raw = if self.bits_allocated == 8 {
                u16::from(b[off])
            } else {
                u16::from_le_bytes([b[off], b[off + 1]])
            };
            let v = raw & mask;
            out.push(if self.signed {
                let sign = 1u16 << (self.bits_stored - 1);
                let s = ((v ^ sign).wrapping_sub(sign)) as i16;
                (i32::from(s) + i32::from(sign)) as u16
            } else {
                v
            });
        }
        if self.pi == PhotometricInterpretation::YbrFull {
            color::ybr_to_rgb(&mut out, self.bits_stored)
        }
        Ok(out)
    }
    pub(crate) fn write(&self, s: &[u16]) -> Result<Vec<u8>, String> {
        let count = self.width * self.height * self.components as usize;
        if s.len() != count {
            return Err("decoded sample count mismatch".into());
        }
        let mut b = vec![0u8; self.frame_len];
        let mask = self.maxval();
        let pixels = self.width * self.height;
        for n in 0..count {
            let pix = n / self.components as usize;
            let c = n % self.components as usize;
            let logical = if self.planar == 1 {
                c * pixels + pix
            } else {
                n
            };
            let off = logical * (self.bits_allocated / 8) as usize;
            let v = if self.signed {
                let sign = 1i32 << (self.bits_stored - 1);
                (i32::from(s[n]) - sign).rem_euclid(1 << self.bits_stored) as u16 & mask
            } else {
                s[n] & mask
            };
            if self.bits_allocated == 8 {
                b[off] = v as u8
            } else {
                b[off..off + 2].copy_from_slice(&v.to_le_bytes())
            }
        }
        Ok(b)
    }
}
pub(crate) fn encode(layout: &Layout, raw: &[u8], p: &JpegLsParams) -> Result<Vec<u8>, String> {
    let samples = layout.read(raw)?;
    let near = u16::try_from(p.near_lossless).map_err(|_| "NEAR overflow")?;
    if near > u16::from(u8::MAX) {
        return Err("NEAR must fit the JPEG-LS SOS field".into());
    }
    if near > layout.maxval() / 2 {
        return Err("NEAR exceeds precision".into());
    }
    let ilv = resolve_interleave(layout, p.interleave_mode)?;
    if p.color_transformation != 0 || p.preset_coding_params != 0 || p.reset_contexts {
        return Err("requested JPEG-LS option is unsupported".into());
    }
    let preset = Preset::defaults(layout.maxval(), near)?;
    let cfg = ScanConfig {
        width: layout.width,
        height: layout.height,
        components: layout.components as usize,
        maxval: layout.maxval(),
        near,
        ilv,
        preset,
    };
    let entropy = encode_scan(cfg, &samples)?;
    let f = FrameHeader {
        precision: layout.bits_stored,
        height: u16::try_from(layout.height).map_err(|_| "height exceeds JPEG-LS limit")?,
        width: u16::try_from(layout.width).map_err(|_| "width exceeds JPEG-LS limit")?,
        components: layout.components,
    };
    let mut out = Vec::new();
    write_soi(&mut out);
    write_sof55(&mut out, &f)?;
    write_sos(&mut out, layout.components, near, ilv)?;
    out.extend(entropy);
    write_eoi(&mut out);
    Ok(out)
}

fn resolve_interleave(layout: &Layout, requested: u32) -> Result<u8, String> {
    match layout.components {
        1 if requested == 0 => Ok(0),
        1 => Err("monochrome JPEG-LS requires interleave mode None".into()),
        3 => match requested {
            // Mode zero follows DICOM storage: planar data uses line
            // interleave, while interleaved data uses sample interleave.
            0 => Ok(if layout.planar == 1 { 1 } else { 2 }),
            // Mode one describes planar DICOM input; JPEG-LS represents it
            // with line interleave after the adapter normalizes the samples.
            1 if layout.planar == 1 => Ok(1),
            1 => Err("interleave mode Planar requires PlanarConfiguration=1".into()),
            2 => Ok(1),
            3 => Ok(2),
            _ => Err("unsupported interleave mode".into()),
        },
        _ => Err("unsupported component count".into()),
    }
}
pub(crate) fn decode(
    layout: &Layout,
    data: &[u8],
    _p: &JpegLsParams,
    expected_near: Option<u16>,
) -> Result<Vec<u8>, String> {
    let h = parse_header(data)?;
    if h.frame.width as usize != layout.width
        || h.frame.height as usize != layout.height
        || h.frame.components != layout.components
        || h.frame.precision != layout.bits_stored
    {
        return Err("codestream layout does not match destination".into());
    }
    if h.scan.point_transform != 0 || h.scan.near > layout.maxval() / 2 {
        return Err("unsupported JPEG-LS scan parameters".into());
    }
    if expected_near.is_some_and(|near| h.scan.near != near) {
        return Err("decode NEAR parameter does not match codestream".into());
    }
    if h.scan.ilv == 0 && layout.components != 1
        || h.scan.ilv > 2
        || h.scan.ilv != 0 && layout.components == 1
    {
        return Err("invalid JPEG-LS interleave mode".into());
    }
    let preset = h
        .preset
        .unwrap_or(Preset::defaults(layout.maxval(), h.scan.near)?);
    if preset.maxval != layout.maxval() {
        return Err("preset MAXVAL mismatch".into());
    }
    let (entropy, tail) = split_scan(data, h.scan_start)?;
    let cfg = ScanConfig {
        width: layout.width,
        height: layout.height,
        components: layout.components as usize,
        maxval: layout.maxval(),
        near: h.scan.near,
        ilv: h.scan.ilv,
        preset,
    };
    let samples = decode_scan(cfg, entropy)?;
    let trailing = data.get(tail + 2..).ok_or("EOI outside codestream")?;
    if !trailing.is_empty() && trailing != [0] {
        return Err("trailing data after EOI".into());
    }
    layout.write(&samples)
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
        dst.dataset_mut()
            .insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono8() -> Layout {
        Layout {
            width: 1,
            height: 1,
            bits_allocated: 8,
            bits_stored: 8,
            components: 1,
            planar: 0,
            signed: false,
            pi: PhotometricInterpretation::Monochrome2,
            frame_len: 1,
        }
    }

    #[test]
    fn lossless_decode_rejects_a_near_lossless_codestream() {
        let encoded = encode(
            &mono8(),
            &[7],
            &JpegLsParams {
                near_lossless: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(decode(&mono8(), &encoded, &JpegLsParams::default(), Some(0)).is_err());
    }

    #[test]
    fn signed_ybr_full_is_rejected_at_layout_validation() {
        assert!(Layout::values(
            1,
            1,
            8,
            8,
            3,
            0,
            1,
            Some(PhotometricInterpretation::YbrFull),
        )
        .is_err());
    }

    #[test]
    fn resolves_interleave_from_storage_and_explicit_modes() {
        let mut layout = mono8();
        assert_eq!(resolve_interleave(&layout, 0).unwrap(), 0);
        assert!(resolve_interleave(&layout, 3).is_err());

        layout.components = 3;
        layout.planar = 0;
        assert_eq!(resolve_interleave(&layout, 0).unwrap(), 2);
        assert_eq!(resolve_interleave(&layout, 2).unwrap(), 1);
        assert_eq!(resolve_interleave(&layout, 3).unwrap(), 2);

        layout.planar = 1;
        assert_eq!(resolve_interleave(&layout, 0).unwrap(), 1);
        assert_eq!(resolve_interleave(&layout, 1).unwrap(), 1);
    }
}
