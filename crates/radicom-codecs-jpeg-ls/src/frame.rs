#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
use crate::{
    color,
    marker::{
        parse_header, write_color_transform, write_dri, write_eoi, write_sof55, write_soi,
        write_sos, write_sos_ids, FrameHeader,
    },
    preset::Preset,
    scan::{decode_line_scan_planes, decode_scan, encode_restart_scan, ScanConfig},
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
        if w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
            return Err("dimensions exceed JPEG-LS limit".into());
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
        if rep == 1
            && matches!(
                pi,
                PhotometricInterpretation::YbrFull
                    | PhotometricInterpretation::Rgb
                    | PhotometricInterpretation::PaletteColor
            )
        {
            // PS3.5 Table 8.2.3-1 requires Pixel Representation 0 for the
            // colour PIs, so their samples are never sign extended.
            return Err("signed RGB, YBR_FULL and PALETTE COLOR samples are unsupported".into());
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
            out.push(raw & mask);
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
            let value = s[n] & mask;
            let v = if self.signed {
                let sign = 1i32 << (self.bits_stored - 1);
                ((i32::from(value) ^ sign) - sign) as i16 as u16
            } else {
                value
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
    let mut samples = layout.read(raw)?;
    let near = u16::try_from(p.near_lossless).map_err(|_| "NEAR overflow")?;
    if near > u16::from(u8::MAX) {
        return Err("NEAR must fit the JPEG-LS SOS field".into());
    }
    if near > layout.maxval() / 2 {
        return Err("NEAR exceeds precision".into());
    }
    if layout.signed && near > 0 {
        let sign = 1i32 << (layout.bits_stored - 1);
        let lower = -sign + i32::from(near);
        let upper = sign - 1 - i32::from(near);
        if samples.iter().any(|&value| {
            let signed = (i32::from(value) ^ sign) - sign;
            signed < lower || signed > upper
        }) {
            return Err("signed samples must remain NEAR away from the signed limits".into());
        }
    }
    if near > 0 && layout.pi == PhotometricInterpretation::PaletteColor {
        // PS3.5 Table 8.2.3-1 permits PALETTE COLOR only for JPEG-LS Lossless;
        // NEAR>0 would quantize the LUT indices.
        return Err("PALETTE COLOR requires lossless JPEG-LS (NEAR=0)".into());
    }
    let ilv = resolve_interleave(layout, p.interleave_mode)?;
    let color_transformation =
        u8::try_from(p.color_transformation).map_err(|_| "unknown JPEG-LS color transformation")?;
    if color_transformation > 3 {
        return Err("unknown JPEG-LS color transformation".into());
    }
    if color_transformation != 0 && near > 0 {
        // The inverse is a linear combination of two components, so the
        // reconstructed error can reach 2*NEAR. CharLS only enables the HP
        // transforms for lossless coding.
        return Err("JPEG-LS color transformation requires lossless coding (NEAR=0)".into());
    }
    if color_transformation != 0 && (layout.components != 3 || layout.signed) {
        return Err("color transformation requires three unsigned components".into());
    }
    // CharLS restricts the HP transforms to the 8- and 16-bit containers
    // (`UnsupportedBitDepthForTransform` otherwise), because a transformed
    // sample must still fit MAXVAL.
    if color_transformation != 0 && layout.bits_stored != 8 && layout.bits_stored != 16 {
        return Err("color transformation requires 8 or 16 bits stored".into());
    }
    if p.preset_coding_params != 0 || p.reset_contexts {
        return Err("requested JPEG-LS option is unsupported".into());
    }
    // The HP transforms are a CharLS extension signalled by an APP8 segment, not
    // part of T.87. CharLS itself skips the transform for non-interleaved (ILV=0)
    // scans, so a plane-interleaved stream would carry a transform that no
    // implementation inverts. Refuse to write one instead of emitting bytes only
    // this decoder reads.
    if color_transformation != 0 && ilv == 0 {
        return Err(
            "plane interleaving (interleave_mode 1) cannot carry a JPEG-LS color transformation"
                .into(),
        );
    }
    if color_transformation != 0 {
        // CharLS applies the transform to the sample container before coding and
        // signals it with an APP8 `mrfx` segment.
        color::forward_transform(&mut samples, color_transformation, layout.bits_stored);
    }
    let preset = Preset::defaults(layout.maxval(), near)?;
    let f = FrameHeader {
        precision: layout.bits_stored,
        height: u16::try_from(layout.height).map_err(|_| "height exceeds JPEG-LS limit")?,
        width: u16::try_from(layout.width).map_err(|_| "width exceeds JPEG-LS limit")?,
        components: layout.components,
    };
    let mut out = Vec::new();
    write_soi(&mut out);
    if color_transformation != 0 {
        // CharLS writes the colour transform before the frame header.
        write_color_transform(&mut out, color_transformation)?;
    }
    write_sof55(&mut out, &f)?;
    let restart = u32::from(p.restart_interval);
    if restart != 0 {
        write_dri(&mut out, p.restart_interval);
    }
    if ilv == 0 && layout.components > 1 {
        // T.87 B.2: a plane-interleaved scan holds one component, so the image
        // is written as one scan per plane (DICOM PS3.5 8.2.3 component
        // interleaving). Every scan starts with fresh model and neighbours.
        let planes = layout.components as usize;
        for c in 0..planes {
            let plane: Vec<u16> = samples.iter().skip(c).step_by(planes).copied().collect();
            let cfg = ScanConfig {
                width: layout.width,
                height: layout.height,
                components: 1,
                maxval: layout.maxval(),
                near,
                ilv: 0,
                preset,
            };
            write_sos_ids(&mut out, &[(c + 1) as u8], near, 0)?;
            out.extend(encode_restart_scan(cfg, &plane, restart)?);
        }
    } else {
        let cfg = ScanConfig {
            width: layout.width,
            height: layout.height,
            components: layout.components as usize,
            maxval: layout.maxval(),
            near,
            ilv,
            preset,
        };
        write_sos(&mut out, layout.components, near, ilv)?;
        out.extend(encode_restart_scan(cfg, &samples, restart)?);
    }
    write_eoi(&mut out);
    Ok(out)
}

fn resolve_interleave(layout: &Layout, requested: u32) -> Result<u8, String> {
    match layout.components {
        1 if requested == 0 => Ok(0),
        1 => Err("monochrome JPEG-LS requires interleave mode None".into()),
        3 => match requested {
            // Mode zero is a storage preference, not a codestream mode: planar
            // data becomes line interleaved and interleaved data stays sample
            // interleaved, which is what every deployed decoder reads. Plane
            // interleaving (ILV=0) is never produced implicitly: CharLS and
            // fo-dicom mis-decode it, so it must be asked for explicitly.
            0 => Ok(if layout.planar == 1 { 1 } else { 2 }),
            // Mode one is plane interleaving: one scan per component, valid for
            // any source storage because the adapter de-interleaves the samples.
            1 => Ok(0),
            2 => Ok(1),
            3 => Ok(2),
            _ => Err("unsupported interleave mode".into()),
        },
        _ => Err("unsupported component count".into()),
    }
}

/// One component plane on its own grid. T.87 A.2.1 derives the plane from the
/// SOF55 sampling factors: `columns = ceil(X*Hi/Hmax)`,
/// `lines = ceil(Y*Vi/Vmax)`.
#[derive(Clone, Copy, Debug)]
struct PlaneGeometry {
    columns: usize,
    lines: usize,
    horizontal: usize,
    vertical: usize,
}

fn plane_geometry(
    frame: &FrameHeader,
    sampling: &[(u8, u8)],
) -> Result<Vec<PlaneGeometry>, String> {
    if sampling.len() != frame.components as usize {
        return Err("SOF55 sampling factors do not match the component count".into());
    }
    let horizontal_max = sampling.iter().map(|factor| factor.0).max().unwrap_or(1) as usize;
    let vertical_max = sampling.iter().map(|factor| factor.1).max().unwrap_or(1) as usize;
    let width = frame.width as usize;
    let height = frame.height as usize;
    Ok(sampling
        .iter()
        .map(|&(hs, vs)| PlaneGeometry {
            columns: (width * hs as usize).div_ceil(horizontal_max),
            lines: (height * vs as usize).div_ceil(vertical_max),
            horizontal: hs as usize,
            vertical: vs as usize,
        })
        .collect())
}

/// Interleaves the component planes into one frame, replicating every plane
/// sample over its block on the full grid (nearest-neighbour upsampling). T.87
/// E.1.1 sub-samples a component by keeping every m-th sample, so the inverse
/// maps frame sample `p` to plane sample `p * factor / factor_max`.
fn assemble_planes(
    layout: &Layout,
    planes: &[Vec<u16>],
    geometry: &[PlaneGeometry],
) -> Result<Vec<u16>, String> {
    let components = planes.len();
    if components != geometry.len() || components != layout.components as usize {
        return Err("decoded planes do not match the layout".into());
    }
    let horizontal_max = geometry.iter().map(|g| g.horizontal).max().unwrap_or(1);
    let vertical_max = geometry.iter().map(|g| g.vertical).max().unwrap_or(1);
    let mut samples = vec![0u16; layout.width * layout.height * components];
    for (index, plane) in planes.iter().enumerate() {
        let plane_geometry = geometry[index];
        if plane.len() != plane_geometry.columns * plane_geometry.lines {
            return Err("decoded plane size does not match the sampling factors".into());
        }
        for y in 0..layout.height {
            let source_y = y * plane_geometry.vertical / vertical_max;
            for x in 0..layout.width {
                let source_x = x * plane_geometry.horizontal / horizontal_max;
                samples[(y * layout.width + x) * components + index] =
                    plane[source_y * plane_geometry.columns + source_x];
            }
        }
    }
    Ok(samples)
}

pub(crate) fn decode(
    layout: &Layout,
    data: &[u8],
    _p: &JpegLsParams,
    expected_near: Option<u16>,
    source_ybr: bool,
) -> Result<Vec<u8>, String> {
    if source_ybr && layout.signed {
        return Err("signed YBR_FULL samples are unsupported".into());
    }
    let DecodedPlanes {
        planes,
        geometry,
        color_transformation,
    } = decode_planes(layout, data, expected_near)?;
    let mut samples = assemble_planes(layout, &planes, &geometry)?;
    if color_transformation != 0 {
        // Undo the CharLS colour transform before any colour-space conversion.
        color::inverse_transform(&mut samples, color_transformation, layout.bits_stored);
    }
    if source_ybr {
        color::ybr_to_rgb(&mut samples, layout.bits_stored);
    }
    layout.write(&samples)
}

/// One plane per component, each on its own sampling grid.
struct DecodedPlanes {
    planes: Vec<Vec<u16>>,
    geometry: Vec<PlaneGeometry>,
    /// CharLS colour transform carried by the APP8 `mrfx` segment.
    color_transformation: u8,
}

/// Decodes every scan of the image into one plane per component, without
/// resampling: a sub-sampled frame keeps each plane on its own grid and
/// `assemble_planes` maps it back onto the frame.
fn decode_planes(
    layout: &Layout,
    data: &[u8],
    expected_near: Option<u16>,
) -> Result<DecodedPlanes, String> {
    let h = parse_header(data)?;
    if h.frame.width as usize != layout.width
        || h.frame.height as usize != layout.height
        || h.frame.components != layout.components
        || h.frame.precision != layout.bits_stored
    {
        return Err("codestream layout does not match destination".into());
    }
    let first = &h.scans[0];
    if first.scan.point_transform != 0 || first.scan.near > layout.maxval() / 2 {
        return Err("unsupported JPEG-LS scan parameters".into());
    }
    if expected_near.is_some_and(|near| first.scan.near != near) {
        return Err("decode NEAR parameter does not match codestream".into());
    }
    if first.scan.ilv > 2 || first.scan.ilv != 0 && layout.components == 1 {
        return Err("invalid JPEG-LS interleave mode".into());
    }
    let preset = h
        .preset
        .unwrap_or(Preset::defaults(layout.maxval(), first.scan.near)?);
    let geometry = plane_geometry(&h.frame, &h.sampling)?;
    if first.scan.ilv == 2
        && geometry
            .iter()
            .any(|plane| plane.columns != geometry[0].columns || plane.lines != geometry[0].lines)
    {
        return Err("sample interleaving requires equal component dimensions".into());
    }
    let entry = &h.scans[0];
    let planes: Vec<Vec<u16>> = if first.scan.ilv == 0 && layout.components > 1 {
        // Plane interleaving: one scan per component, each with its own model
        // and neighbourhood state initialized at the scan start, decoded on the
        // component plane grid derived from the sampling factors. The SOS id
        // decides which plane a scan fills, not the scan order.
        let mut planes = vec![Vec::new(); layout.components as usize];
        for entry in &h.scans {
            let plane_grid = geometry[entry.component];
            let cfg = ScanConfig {
                width: plane_grid.columns,
                height: plane_grid.lines,
                components: 1,
                maxval: preset.maxval,
                near: first.scan.near,
                ilv: 0,
                preset,
            };
            planes[entry.component] =
                decode_scan(cfg, &data[entry.start..entry.end], h.restart_interval)?;
        }
        planes
    } else if first.scan.ilv == 1 && layout.components > 1 {
        // Line interleaving: one MCU holds Vi lines of each component, so a
        // sub-sampled frame decodes straight into the component planes.
        let cfg = ScanConfig {
            width: layout.width,
            height: layout.height,
            components: layout.components as usize,
            maxval: preset.maxval,
            near: first.scan.near,
            ilv: 1,
            preset,
        };
        let planes: Vec<(usize, usize, usize)> = geometry
            .iter()
            .map(|plane| (plane.columns, plane.lines, plane.vertical))
            .collect();
        decode_line_scan_planes(
            cfg,
            &planes,
            &data[entry.start..entry.end],
            h.restart_interval,
        )?
    } else {
        let cfg = ScanConfig {
            width: layout.width,
            height: layout.height,
            components: layout.components as usize,
            maxval: preset.maxval,
            near: first.scan.near,
            ilv: first.scan.ilv,
            preset,
        };
        let samples = decode_scan(cfg, &data[entry.start..entry.end], h.restart_interval)?;
        let components = layout.components as usize;
        (0..components)
            .map(|index| {
                samples
                    .iter()
                    .skip(index)
                    .step_by(components)
                    .copied()
                    .collect()
            })
            .collect()
    };
    Ok(DecodedPlanes {
        planes,
        geometry,
        color_transformation: h.color_transformation,
    })
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
    use crate::scan::encode_scan;

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
        assert!(decode(&mono8(), &encoded, &JpegLsParams::default(), Some(0), false).is_err());
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
        // An RGB-labelled destination must not smuggle signed samples into the
        // YBR_FULL decode conversion, whose chroma math is unsigned.
        let mut layout = mono8();
        layout.components = 3;
        layout.pi = PhotometricInterpretation::Rgb;
        layout.signed = true;
        layout.frame_len = 3;
        let encoded = encode(&layout, &[1, 2, 3], &JpegLsParams::default()).unwrap();
        assert!(decode(&layout, &encoded, &JpegLsParams::default(), Some(0), true).is_err());
        assert!(decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).is_ok());
    }

    #[test]
    fn resolves_interleave_from_storage_and_explicit_modes() {
        let mut layout = mono8();
        assert_eq!(resolve_interleave(&layout, 0).unwrap(), 0);
        assert!(resolve_interleave(&layout, 3).is_err());

        layout.components = 3;
        layout.planar = 0;
        // Mode zero follows DICOM storage: interleaved data stays sample
        // interleaved, which every deployed decoder reads.
        assert_eq!(resolve_interleave(&layout, 0).unwrap(), 2);
        assert_eq!(resolve_interleave(&layout, 2).unwrap(), 1);
        assert_eq!(resolve_interleave(&layout, 3).unwrap(), 2);
        // Mode one is plane interleaving and needs no planar storage.
        assert_eq!(resolve_interleave(&layout, 1).unwrap(), 0);

        layout.planar = 1;
        assert_eq!(resolve_interleave(&layout, 0).unwrap(), 1);
        assert_eq!(resolve_interleave(&layout, 1).unwrap(), 0);
        assert_eq!(resolve_interleave(&layout, 2).unwrap(), 1);
    }

    #[test]
    fn signed_subword_output_is_sign_extended() {
        let mut layout = mono8();
        layout.width = 4;
        layout.bits_allocated = 16;
        layout.bits_stored = 12;
        layout.signed = true;
        layout.frame_len = 8;
        let native = [0x00, 0xf8, 0xff, 0xff, 0x00, 0x00, 0xff, 0x07];
        let encoded = encode(&layout, &native, &JpegLsParams::default()).unwrap();
        assert_eq!(
            decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
            native
        );
    }

    #[test]
    fn signed_encoding_preserves_the_unsigned_wire_bit_pattern() {
        let mut layout = mono8();
        layout.width = 4;
        layout.frame_len = 4;
        let raw = [128, 255, 0, 127];
        let unsigned = encode(&layout, &raw, &JpegLsParams::default()).unwrap();
        layout.signed = true;
        assert_eq!(
            encode(&layout, &raw, &JpegLsParams::default()).unwrap(),
            unsigned
        );
    }

    #[test]
    fn signed_near_lossless_checks_signed_extrema_before_encoding() {
        let mut layout = mono8();
        layout.signed = true;
        let params = JpegLsParams {
            near_lossless: 3,
            ..Default::default()
        };
        for raw in [128, 129, 130, 125, 126, 127] {
            assert!(encode(&layout, &[raw], &params).is_err(), "sample={raw}");
        }
        for raw in [131, 255, 0, 124] {
            assert!(encode(&layout, &[raw], &params).is_ok(), "sample={raw}");
        }
    }

    #[test]
    fn near_lossless_rejects_palette_color() {
        // PS3.5 Table 8.2.3-1 allows PALETTE COLOR only for JPEG-LS Lossless
        // (.80); a NEAR>0 codestream would quantize LUT indices.
        let mut layout = mono8();
        layout.pi = PhotometricInterpretation::PaletteColor;
        let error = encode(
            &layout,
            &[7],
            &JpegLsParams {
                near_lossless: 3,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("PALETTE COLOR"), "{error}");
        // Lossless palette coding is the only conforming combination.
        assert_eq!(
            decode(
                &layout,
                &encode(&layout, &[7], &JpegLsParams::default()).unwrap(),
                &JpegLsParams::default(),
                Some(0),
                false
            )
            .unwrap(),
            [7]
        );
    }

    #[test]
    fn near_lossless_rejects_the_hp_color_transformation() {
        // The inverse transform is a linear combination of two components, so
        // NEAR could double; CharLS only enables HP1..3 when NEAR == 0.
        let layout = rgb_layout(2, 2, 8, 8);
        let raw = layout.write(&[7u16; 12]).unwrap();
        for transform in 1..=3u32 {
            let error = encode(
                &layout,
                &raw,
                &JpegLsParams {
                    color_transformation: transform,
                    near_lossless: 2,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(error.contains("NEAR"), "HP{transform}: {error}");
            assert!(
                encode(
                    &layout,
                    &raw,
                    &JpegLsParams {
                        color_transformation: transform,
                        ..Default::default()
                    }
                )
                .is_ok(),
                "HP{transform} must stay available for lossless coding"
            );
        }
    }

    #[test]
    fn signed_rgb_and_palette_color_are_rejected_at_layout_validation() {
        // PS3.5 Table 8.2.3-1 requires Pixel Representation 0 for RGB and
        // PALETTE COLOR, like YBR_FULL.
        for (pi, components) in [
            (PhotometricInterpretation::Rgb, 3u16),
            (PhotometricInterpretation::PaletteColor, 1),
        ] {
            let error = Layout::values(1, 1, 8, 8, components, 0, 1, Some(pi)).unwrap_err();
            assert!(error.contains("signed"), "{pi:?}: {error}");
            assert!(Layout::values(1, 1, 8, 8, components, 0, 0, Some(pi)).is_ok());
        }
    }

    #[test]
    fn rejects_dimensions_that_cannot_be_signaled_before_encoding() {
        assert!(Layout::values(
            65536,
            1,
            8,
            8,
            1,
            0,
            0,
            Some(PhotometricInterpretation::Monochrome2)
        )
        .is_err());
    }

    #[test]
    fn malformed_codestream_mutations_never_panic() {
        let layout = mono8();
        let original = encode(&layout, &[7], &JpegLsParams::default()).unwrap();
        for end in 0..original.len() {
            assert!(decode(
                &layout,
                &original[..end],
                &JpegLsParams::default(),
                None,
                false
            )
            .is_err());
        }
        for index in 0..original.len() {
            for value in 0..=255 {
                let mut bytes = original.clone();
                bytes[index] = value;
                let result = std::panic::catch_unwind(|| {
                    decode(&layout, &bytes, &JpegLsParams::default(), None, false)
                });
                assert!(result.is_ok(), "index={index} value={value}");
            }
        }
    }

    #[test]
    fn decode_stops_at_eoi_and_ignores_fragment_trailing_bytes() {
        // fo-dicom sizes fragments by ArrayPool bucket, so stale non-zero bytes
        // follow EOI; a decoder must stop at EOI and still require it.
        let mut layout = mono8();
        layout.width = 4;
        layout.frame_len = 4;
        let raw = [3u8, 91, 200, 17];
        let encoded = encode(&layout, &raw, &JpegLsParams::default()).unwrap();
        let mut padded = encoded.clone();
        padded.extend((0..64u8).map(|i| i | 1));
        assert_eq!(
            decode(&layout, &padded, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );
        let truncated = &encoded[..encoded.len() - 2];
        assert!(
            decode(&layout, truncated, &JpegLsParams::default(), Some(0), false)
                .unwrap_err()
                .contains("missing EOI")
        );
    }

    #[test]
    fn decodes_a_scan_split_by_restart_markers() {
        use crate::{marker::write_dri, scan::encode_restart_scan};
        let mut layout = mono8();
        layout.width = 5;
        layout.height = 2;
        layout.frame_len = 10;
        let raw = [7u8, 9, 200, 3, 255, 0, 128, 64, 33, 250];
        let cfg = ScanConfig {
            width: 5,
            height: 2,
            components: 1,
            maxval: 255,
            near: 0,
            ilv: 0,
            preset: Preset::defaults(255, 0).unwrap(),
        };
        let samples: Vec<u16> = raw.iter().map(|v| u16::from(*v)).collect();
        let entropy = encode_restart_scan(cfg, &samples, 1).unwrap();
        assert!(
            entropy.windows(2).any(|w| w == [0xff, 0xd0]),
            "one image line is one MCU, so interval 1 writes RST0"
        );
        let frame = FrameHeader {
            precision: 8,
            width: 5,
            height: 2,
            components: 1,
        };
        let mut encoded = Vec::new();
        write_soi(&mut encoded);
        write_sof55(&mut encoded, &frame).unwrap();
        write_dri(&mut encoded, 1);
        write_sos(&mut encoded, 1, 0, 0).unwrap();
        encoded.extend(&entropy);
        write_eoi(&mut encoded);
        assert_eq!(
            decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );

        // Without DRI the same restart markers are illegal.
        let mut no_dri = Vec::new();
        write_soi(&mut no_dri);
        write_sof55(&mut no_dri, &frame).unwrap();
        write_sos(&mut no_dri, 1, 0, 0).unwrap();
        no_dri.extend(&entropy);
        write_eoi(&mut no_dri);
        assert!(decode(&layout, &no_dri, &JpegLsParams::default(), Some(0), false).is_err());
    }

    #[test]
    fn plane_interleaved_rgb_round_trips_and_differs_from_interleaved_streams() {
        let mut layout = mono8();
        layout.components = 3;
        layout.width = 4;
        layout.height = 3;
        layout.frame_len = 36;
        layout.pi = PhotometricInterpretation::Rgb;
        let raw: Vec<u8> = (0..36).map(|i| (i * 31 + 5) as u8).collect();
        let params = |mode| JpegLsParams {
            interleave_mode: mode,
            ..Default::default()
        };
        let plane = encode(&layout, &raw, &params(1)).unwrap();
        let line = encode(&layout, &raw, &params(2)).unwrap();
        let sample = encode(&layout, &raw, &params(3)).unwrap();
        // T.87 B.2: ILV=0 carries one component per scan; ILV=1/2 use one scan.
        assert_eq!(sos_count(&plane), 3);
        assert_eq!(sos_count(&line), 1);
        assert_eq!(sos_count(&sample), 1);
        assert_ne!(plane, line);
        assert_ne!(plane, sample);
        for encoded in [&plane, &line, &sample] {
            assert_eq!(
                decode(&layout, encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
                raw
            );
        }
    }

    #[test]
    fn default_mode_derives_interleaving_from_the_source_storage() {
        let mut layout = mono8();
        layout.components = 3;
        layout.width = 4;
        layout.height = 3;
        layout.frame_len = 36;
        layout.pi = PhotometricInterpretation::Rgb;
        let params = |mode| JpegLsParams {
            interleave_mode: mode,
            ..Default::default()
        };
        let raw: Vec<u8> = (0..36).map(|i| (i * 13 + 3) as u8).collect();
        // Interleaved storage derives to sample interleaving, which is what
        // deployed decoders read; the default never emits plane interleaving.
        assert_eq!(
            encode(&layout, &raw, &params(0)).unwrap(),
            encode(&layout, &raw, &params(3)).unwrap()
        );
        // Planar storage derives to line interleaving.
        layout.planar = 1;
        assert_eq!(
            encode(&layout, &raw, &params(0)).unwrap(),
            encode(&layout, &raw, &params(2)).unwrap()
        );
        // Mode one unlocks plane interleaving for planar storage as well.
        let plane = encode(&layout, &raw, &params(1)).unwrap();
        assert_eq!(sos_count(&plane), 3);
        assert_eq!(
            decode(&layout, &plane, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );
    }

    #[test]
    fn plane_interleaved_scans_support_restart_intervals() {
        use crate::{
            marker::{write_dri, write_sos_ids},
            scan::{encode_restart_scan, ScanConfig},
        };
        let mut layout = mono8();
        layout.components = 3;
        layout.width = 4;
        layout.height = 2;
        layout.frame_len = 24;
        layout.pi = PhotometricInterpretation::Rgb;
        let raw: Vec<u8> = (0..24).map(|i| (i * 23 + 7) as u8).collect();
        let samples: Vec<u16> = raw.iter().map(|v| u16::from(*v)).collect();
        let frame = FrameHeader {
            precision: 8,
            width: 4,
            height: 2,
            components: 3,
        };
        let mut encoded = Vec::new();
        write_soi(&mut encoded);
        write_sof55(&mut encoded, &frame).unwrap();
        write_dri(&mut encoded, 1);
        for c in 0..3usize {
            let plane: Vec<u16> = samples.iter().skip(c).step_by(3).copied().collect();
            let cfg = ScanConfig {
                width: 4,
                height: 2,
                components: 1,
                maxval: 255,
                near: 0,
                ilv: 0,
                preset: Preset::defaults(255, 0).unwrap(),
            };
            write_sos_ids(&mut encoded, &[(c + 1) as u8], 0, 0).unwrap();
            // Interval 1 over 2 rows: every plane is split into two segments.
            encoded.extend(encode_restart_scan(cfg, &plane, 1).unwrap());
        }
        write_eoi(&mut encoded);
        assert_eq!(
            decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );
    }

    #[test]
    fn plane_interleaved_scans_may_come_in_any_component_order() {
        use crate::{
            marker::write_sos_ids,
            scan::{encode_scan, ScanConfig},
        };
        let mut layout = mono8();
        layout.components = 3;
        layout.width = 3;
        layout.height = 2;
        layout.frame_len = 18;
        layout.pi = PhotometricInterpretation::Rgb;
        let raw: Vec<u8> = (0..18).map(|i| (i * 29 + 11) as u8).collect();
        let samples: Vec<u16> = raw.iter().map(|v| u16::from(*v)).collect();
        let frame = FrameHeader {
            precision: 8,
            width: 3,
            height: 2,
            components: 3,
        };
        let mut encoded = Vec::new();
        write_soi(&mut encoded);
        write_sof55(&mut encoded, &frame).unwrap();
        // Scans arrive as components 3, 1, 2; the SOS id decides where each
        // plane lands, not the scan position.
        for c in [2usize, 0, 1] {
            let plane: Vec<u16> = samples.iter().skip(c).step_by(3).copied().collect();
            let cfg = ScanConfig {
                width: 3,
                height: 2,
                components: 1,
                maxval: 255,
                near: 0,
                ilv: 0,
                preset: Preset::defaults(255, 0).unwrap(),
            };
            write_sos_ids(&mut encoded, &[(c + 1) as u8], 0, 0).unwrap();
            encoded.extend(encode_scan(cfg, &plane).unwrap());
        }
        write_eoi(&mut encoded);
        assert_eq!(
            decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );
    }

    #[test]
    fn decodes_the_official_plane_interleaved_conformance_stream() {
        // T.87 encoder conformance stream T8C0E0.JLS: 256x256 RGB, ILV=0, three
        // plane-interleaved scans of TEST8.PPM. The fixture lives in the ignored
        // interop tree, so the test reports and skips when it is absent.
        let dir = std::path::Path::new("../../.interop/tmp-t87/img");
        let Ok(jls) = std::fs::read(dir.join("T8C0E0.JLS")) else {
            eprintln!("skipping: {} not present", dir.display());
            return;
        };
        let Ok(ppm) = std::fs::read(dir.join("TEST8.PPM")) else {
            eprintln!("skipping: {} not present", dir.display());
            return;
        };
        let mut layout = mono8();
        layout.width = 256;
        layout.height = 256;
        layout.components = 3;
        layout.planar = 0;
        layout.pi = PhotometricInterpretation::Rgb;
        layout.frame_len = 256 * 256 * 3;
        let expected = &ppm[ppm.len() - layout.frame_len..];
        assert_eq!(sos_count(&jls), 3, "the fixture is a three-scan image");
        let decoded = decode(&layout, &jls, &JpegLsParams::default(), Some(0), false).unwrap();
        assert_eq!(decoded.len(), layout.frame_len);
        assert_eq!(decoded, expected, "T8C0E0 must decode to TEST8.PPM");
    }

    fn sos_count(codestream: &[u8]) -> usize {
        codestream
            .windows(2)
            .filter(|w| w[0] == 0xff && w[1] == 0xda)
            .count()
    }

    /// Header token reader for the PGM fixtures of T.87 Annex E.
    fn pgm_token<'a>(data: &'a [u8], index: &mut usize) -> &'a [u8] {
        while *index < data.len() && data[*index] <= b' ' {
            *index += 1;
        }
        let start = *index;
        while *index < data.len() && data[*index] > b' ' {
            *index += 1;
        }
        &data[start..*index]
    }

    fn read_pgm(path: &std::path::Path) -> Option<(usize, usize, Vec<u16>)> {
        let data = std::fs::read(path).ok()?;
        let mut index = 0usize;
        if pgm_token(&data, &mut index) != b"P5" {
            return None;
        }
        let width: usize = std::str::from_utf8(pgm_token(&data, &mut index))
            .ok()?
            .parse()
            .ok()?;
        let height: usize = std::str::from_utf8(pgm_token(&data, &mut index))
            .ok()?
            .parse()
            .ok()?;
        let maxval: u16 = std::str::from_utf8(pgm_token(&data, &mut index))
            .ok()?
            .parse()
            .ok()?;
        if maxval > 255 {
            return None;
        }
        // Exactly one whitespace byte separates the header from the payload.
        let payload = data.get(index + 1..)?;
        let samples = payload.iter().map(|value| u16::from(*value)).collect();
        Some((width, height, samples))
    }

    fn subsampled_layout() -> Layout {
        let mut layout = mono8();
        layout.width = 256;
        layout.height = 256;
        layout.components = 3;
        layout.pi = PhotometricInterpretation::Rgb;
        layout.frame_len = 256 * 256 * 3;
        layout
    }

    #[test]
    fn decodes_the_subsampled_conformance_stream() {
        // T.87 E.1.2 test 7: T8SSE0.JLS is a lossless line-interleaved scan of
        // TEST8 whose components are sub-sampled: red 256x256 (H2V4), green
        // 256x64 (H2V1) and blue 128x128 (H1V2).
        let dir = std::path::Path::new("../../.interop/tmp-t87/img");
        let (Ok(jls), Some(red), Some(green), Some(blue)) = (
            std::fs::read(dir.join("T8SSE0.JLS")),
            read_pgm(&dir.join("TEST8R.PGM")),
            read_pgm(&dir.join("TEST8GR4.PGM")),
            read_pgm(&dir.join("TEST8BS2.PGM")),
        ) else {
            eprintln!(
                "skipping: sub-sampled fixtures are not present in {}",
                dir.display()
            );
            return;
        };
        let layout = subsampled_layout();
        let decoded = decode_planes(&layout, &jls, Some(0)).unwrap();
        assert_eq!(decoded.color_transformation, 0);
        let (planes, geometry) = (decoded.planes, decoded.geometry);
        assert_eq!(planes.len(), 3);
        assert_eq!((geometry[0].columns, geometry[0].lines), (256, 256));
        assert_eq!((geometry[1].columns, geometry[1].lines), (256, 64));
        assert_eq!((geometry[2].columns, geometry[2].lines), (128, 128));
        for (plane, (width, height, samples)) in planes.iter().zip([&red, &green, &blue]) {
            assert_eq!(*width * *height, plane.len());
            assert_eq!(plane.as_slice(), samples.as_slice(), "plane mismatch");
        }

        // The frame is normalized to three samples per pixel on the full grid
        // by nearest-neighbour replication of each plane.
        let decoded = decode(&layout, &jls, &JpegLsParams::default(), Some(0), false).unwrap();
        assert_eq!(decoded.len(), layout.frame_len);
        let horizontal_max = geometry.iter().map(|grid| grid.horizontal).max().unwrap();
        let vertical_max = geometry.iter().map(|grid| grid.vertical).max().unwrap();
        let mut expected = vec![0u8; layout.frame_len];
        for (index, (plane, grid)) in planes.iter().zip(&geometry).enumerate() {
            for y in 0..layout.height {
                let source_y = y * grid.vertical / vertical_max;
                for x in 0..layout.width {
                    let source_x = x * grid.horizontal / horizontal_max;
                    expected[(y * layout.width + x) * 3 + index] =
                        plane[source_y * grid.columns + source_x] as u8;
                }
            }
        }
        assert_eq!(decoded, expected);
    }

    #[test]
    fn near_lossless_subsampled_stream_stays_within_the_near_bound() {
        // T.87 E.1.2 test 8: the same frame with NEAR=3.
        let dir = std::path::Path::new("../../.interop/tmp-t87/img");
        let (Ok(jls), Some(red), Some(green), Some(blue)) = (
            std::fs::read(dir.join("T8SSE3.JLS")),
            read_pgm(&dir.join("TEST8R.PGM")),
            read_pgm(&dir.join("TEST8GR4.PGM")),
            read_pgm(&dir.join("TEST8BS2.PGM")),
        ) else {
            eprintln!(
                "skipping: sub-sampled fixtures are not present in {}",
                dir.display()
            );
            return;
        };
        let layout = subsampled_layout();
        let planes = decode_planes(&layout, &jls, Some(3)).unwrap().planes;
        let mut worst = 0u16;
        for (plane, (_, _, samples)) in planes.iter().zip([&red, &green, &blue]) {
            for (reconstructed, source) in plane.iter().zip(samples) {
                worst = worst.max(reconstructed.abs_diff(*source));
            }
        }
        assert!(worst <= 3, "worst near-lossless error {worst} exceeds NEAR");
    }

    #[test]
    fn subsampled_restart_intervals_count_line_interleaved_mcus() {
        // T.87 B.4: a line-interleaved MCU holds V1 lines of C1, then V2 of C2
        // and V3 of C3, so this image has 256/4 = 64 MCUs. With interval 64 the
        // scan holds one segment (no markers needed); interval 63 would need
        // two and must be rejected, which also proves the count is not the
        // 256 line count of the unsampled layout.
        let dir = std::path::Path::new("../../.interop/tmp-t87/img");
        let Ok(jls) = std::fs::read(dir.join("T8SSE0.JLS")) else {
            eprintln!(
                "skipping: sub-sampled fixtures are not present in {}",
                dir.display()
            );
            return;
        };
        let layout = subsampled_layout();
        let sos = jls
            .windows(2)
            .position(|window| window == [0xff, 0xda])
            .unwrap();
        let with_dri = |interval: u16| {
            let mut codestream = jls[..sos].to_vec();
            write_dri(&mut codestream, interval);
            codestream.extend(&jls[sos..]);
            codestream
        };
        assert!(decode(
            &layout,
            &with_dri(64),
            &JpegLsParams::default(),
            Some(0),
            false
        )
        .is_ok());
        let error = decode(
            &layout,
            &with_dri(63),
            &JpegLsParams::default(),
            Some(0),
            false,
        )
        .unwrap_err();
        assert!(error.contains("restart marker count"), "{error}");
    }

    /// Three-component RGB layout with the given allocation.
    fn rgb_layout(width: usize, height: usize, bits_allocated: u16, bits_stored: u8) -> Layout {
        let mut layout = mono8();
        layout.width = width;
        layout.height = height;
        layout.components = 3;
        layout.bits_allocated = bits_allocated;
        layout.bits_stored = bits_stored;
        layout.pi = PhotometricInterpretation::Rgb;
        layout.frame_len = width * height * 3 * usize::from(bits_allocated / 8);
        layout
    }

    /// Appends a colour-transform APP8 segment to a codestream, right after SOI.
    fn with_color_transform(codestream: &[u8], transform: u8) -> Vec<u8> {
        let mut tagged = codestream[..2].to_vec();
        write_color_transform(&mut tagged, transform).unwrap();
        tagged.extend(&codestream[2..]);
        tagged
    }

    #[test]
    fn color_transformations_round_trip_in_every_layout() {
        for (bits_allocated, bits_stored) in [(8u16, 8u8), (16, 16)] {
            let layout = rgb_layout(5, 3, bits_allocated, bits_stored);
            let maxval = (1u32 << bits_stored) - 1;
            let samples: Vec<u16> = (0..layout.width * layout.height * 3)
                .map(|i| ((i as u32 * 977 + 13) % (maxval + 1)) as u16)
                .collect();
            let raw = layout.write(&samples).unwrap();
            for transform in 1..=3u32 {
                for interleave_mode in [0u32, 2, 3] {
                    let params = JpegLsParams {
                        color_transformation: transform,
                        interleave_mode,
                        ..Default::default()
                    };
                    let encoded = encode(&layout, &raw, &params).unwrap();
                    assert_eq!(
                        parse_header(&encoded).unwrap().color_transformation,
                        transform as u8
                    );
                    assert_eq!(
                        decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false)
                            .unwrap(),
                        raw,
                        "HP{transform} {bits_stored}bit ilv{interleave_mode}"
                    );
                }
            }
        }
    }

    #[test]
    fn rejects_color_transformations_that_charls_cannot_code() {
        // CharLS reports `UnsupportedBitDepthForTransform` for 12-bit samples,
        // because a transformed value no longer fits MAXVAL.
        let layout = rgb_layout(2, 2, 16, 12);
        let raw = layout.write(&[1u16; 12]).unwrap();
        assert!(encode(
            &layout,
            &raw,
            &JpegLsParams {
                color_transformation: 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("8 or 16 bits stored"));
    }

    #[test]
    fn writes_the_color_transform_segment_with_its_id() {
        let layout = rgb_layout(2, 2, 8, 8);
        let samples: Vec<u16> = (0..12).map(|i| (i * 19) as u16).collect();
        let raw = layout.write(&samples).unwrap();
        for transform in 1..=3u32 {
            let encoded = encode(
                &layout,
                &raw,
                &JpegLsParams {
                    color_transformation: transform,
                    ..Default::default()
                },
            )
            .unwrap();
            // SOI, then APP8 "mrfx" with the transform id, then SOF55.
            assert_eq!(
                &encoded[2..11],
                &[
                    0xff,
                    0xe8,
                    0x00,
                    0x07,
                    b'm',
                    b'r',
                    b'f',
                    b'x',
                    transform as u8
                ]
            );
            assert_eq!(&encoded[11..13], &[0xff, 0xf7]);
        }
        let plain = encode(&layout, &raw, &JpegLsParams::default()).unwrap();
        assert!(!plain.windows(4).any(|window| window == b"mrfx"));
    }

    #[test]
    fn applies_the_inverse_color_transform_of_a_foreign_segment() {
        // The stream holds plain RGB samples; adding an `mrfx` segment must make
        // the decoder undo a transform that the encoder never applied, so the
        // expected output follows the CharLS inverse formulas directly.
        let layout = rgb_layout(3, 2, 8, 8);
        let samples: Vec<u16> = (0..18).map(|i| (i * 11) % 256).collect();
        let raw = layout.write(&samples).unwrap();
        let plain = encode(&layout, &raw, &JpegLsParams::default()).unwrap();
        assert_eq!(
            decode(&layout, &plain, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );
        for transform in 1..=3u8 {
            let mut expected = samples.clone();
            for pixel in expected.chunks_mut(3) {
                let (v1, v2, v3) = (
                    i32::from(pixel[0]),
                    i32::from(pixel[1]),
                    i32::from(pixel[2]),
                );
                let (r, g, b) = match transform {
                    // R = v1+v2-128, G = v2, B = v3+v2-128
                    1 => (v1 + v2 - 128, v2, v3 + v2 - 128),
                    // R = v1+v2-128, G = v2, B = v3+((R+G)>>1)-128
                    2 => {
                        let r = (v1 + v2 - 128).rem_euclid(256);
                        (r, v2, v3 + ((r + v2) >> 1) - 128)
                    }
                    // G = v1-((v3+v2)>>2)+64, R = v3+G-128, B = v2+G-128
                    _ => {
                        let g = v1 - ((v3 + v2) >> 2) + 64;
                        (v3 + g - 128, g, v2 + g - 128)
                    }
                };
                pixel[0] = r.rem_euclid(256) as u16;
                pixel[1] = g.rem_euclid(256) as u16;
                pixel[2] = b.rem_euclid(256) as u16;
            }
            assert_eq!(
                decode(
                    &layout,
                    &with_color_transform(&plain, transform),
                    &JpegLsParams::default(),
                    Some(0),
                    false
                )
                .unwrap(),
                layout.write(&expected).unwrap(),
                "HP{transform} inverse"
            );
        }
    }

    #[test]
    fn rejects_unknown_and_misplaced_color_transformations() {
        let layout = rgb_layout(2, 2, 8, 8);
        let raw = layout
            .write(&(0..12).map(|i| (i * 7) as u16).collect::<Vec<_>>())
            .unwrap();
        let plain = encode(&layout, &raw, &JpegLsParams::default()).unwrap();
        let mut bad = plain[..2].to_vec();
        bad.extend([0xff, 0xe8, 0x00, 0x07, b'm', b'r', b'f', b'x', 4]);
        bad.extend(&plain[2..]);
        assert!(
            decode(&layout, &bad, &JpegLsParams::default(), Some(0), false)
                .unwrap_err()
                .contains("unknown JPEG-LS color transformation")
        );

        // A transform needs three components on both paths.
        assert!(encode(
            &mono8(),
            &[7],
            &JpegLsParams {
                color_transformation: 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("three unsigned components"));
        let mono = encode(
            &mono8(),
            &[7],
            &JpegLsParams {
                near_lossless: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(decode(
            &mono8(),
            &with_color_transform(&mono, 2),
            &JpegLsParams::default(),
            Some(0),
            false
        )
        .unwrap_err()
        .contains("three components"));

        // Out-of-range request values are rejected before coding.
        assert!(encode(
            &layout,
            &raw,
            &JpegLsParams {
                color_transformation: 9,
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn color_transformation_combines_with_restarts() {
        let layout = rgb_layout(6, 4, 8, 8);
        let samples: Vec<u16> = (0..layout.width * layout.height * 3)
            .map(|i| (i * 13 % 256) as u16)
            .collect();
        let raw = layout.write(&samples).unwrap();
        let params = JpegLsParams {
            color_transformation: 2,
            interleave_mode: 3,
            restart_interval: 2,
            ..Default::default()
        };
        let encoded = encode(&layout, &raw, &params).unwrap();
        assert!(encoded.windows(2).any(|window| window == [0xff, 0xd0]));
        assert_eq!(
            decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
            raw
        );
    }

    #[test]
    fn rejects_color_transformations_on_plane_interleaved_scans() {
        // CharLS skips HP transforms for non-interleaved scans, so a plane
        // interleaved stream with a transform has no implementation that
        // inverts it; the encoder refuses to write one.
        let layout = rgb_layout(6, 4, 8, 8);
        let raw = layout
            .write(&vec![7u16; layout.width * layout.height * 3])
            .unwrap();
        for (interleave_mode, transformation) in [(1u32, 1u32), (1, 2), (1, 3)] {
            let params = JpegLsParams {
                color_transformation: transformation,
                interleave_mode,
                ..Default::default()
            };
            let error = encode(&layout, &raw, &params).unwrap_err();
            assert!(
                error.contains("cannot carry a JPEG-LS color transformation"),
                "unexpected error: {error}"
            );
        }
        // Line and sample interleaving still accept the transform.
        for interleave_mode in [2u32, 3] {
            let params = JpegLsParams {
                color_transformation: 1,
                interleave_mode,
                ..Default::default()
            };
            let encoded = encode(&layout, &raw, &params).unwrap();
            assert_eq!(
                decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
                raw
            );
        }
    }

    #[test]
    fn decodes_lse_with_reduced_maxval_and_custom_reset() {
        let mut layout = mono8();
        layout.width = 32;
        layout.height = 3;
        layout.frame_len = 96;
        let preset = Preset::new(100, 3, 7, 21, 8).unwrap();
        let values = (0..96)
            .map(|i| if i % 5 < 3 { 0 } else { i % 101 })
            .collect::<Vec<u16>>();
        let entropy = encode_scan(
            ScanConfig {
                width: 32,
                height: 3,
                components: 1,
                maxval: 100,
                near: 0,
                ilv: 0,
                preset,
            },
            &values,
        )
        .unwrap();
        let mut encoded = Vec::new();
        write_soi(&mut encoded);
        write_sof55(
            &mut encoded,
            &FrameHeader {
                precision: 8,
                width: 32,
                height: 3,
                components: 1,
            },
        )
        .unwrap();
        encoded.extend([0xff, 0xf8, 0, 13, 1, 0, 100, 0, 3, 0, 7, 0, 21, 0, 8]);
        write_sos(&mut encoded, 1, 0, 0).unwrap();
        encoded.extend(entropy);
        write_eoi(&mut encoded);
        assert_eq!(
            decode(&layout, &encoded, &JpegLsParams::default(), Some(0), false).unwrap(),
            values.iter().map(|v| *v as u8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn deterministic_layout_matrix_preserves_samples_and_near_bound() {
        for bits in 2..=16 {
            let maxval = ((1u32 << bits) - 1) as u16;
            for near in [0, 1, 3.min(maxval / 2), 255.min(maxval / 2)] {
                for (width, height) in [(1, 1), (1, 17), (17, 1), (13, 9)] {
                    for (components, planar, ilv) in [(1, 0, 0), (3, 0, 2), (3, 0, 3), (3, 1, 0)] {
                        let layout = Layout {
                            width,
                            height,
                            bits_allocated: if bits <= 8 { 8 } else { 16 },
                            bits_stored: bits,
                            components,
                            planar,
                            signed: false,
                            pi: if components == 1 {
                                PhotometricInterpretation::Monochrome2
                            } else {
                                PhotometricInterpretation::Rgb
                            },
                            frame_len: width
                                * height
                                * components as usize
                                * if bits <= 8 { 1 } else { 2 },
                        };
                        let values = (0..width * height * components as usize)
                            .map(|i| match i % 7 {
                                0..=2 => 0,
                                3 => maxval,
                                _ => ((i * 7919 + i * i * 17) & usize::from(maxval)) as u16,
                            })
                            .collect::<Vec<_>>();
                        let raw = layout.write(&values).unwrap();
                        let params = JpegLsParams {
                            near_lossless: u32::from(near),
                            interleave_mode: ilv,
                            ..Default::default()
                        };
                        let encoded = encode(&layout, &raw, &params).unwrap();
                        let decoded = decode(&layout, &encoded, &params, None, false).unwrap();
                        let actual = layout.read(&decoded).unwrap();
                        assert!(values.iter().zip(&actual).all(|(a, b)| a.abs_diff(*b) <= near),
                            "bits={bits}, near={near}, {width}x{height}, components={components}, planar={planar}, ilv={ilv}");
                    }
                }
            }
        }
    }
}
