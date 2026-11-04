//! JPEG 2000 codestream assembly and parsing (ISO/IEC 15444-1 Annex A).

use crate::geometry::{CodeBlockSegment, Component};
use crate::marker::{
    write_eoc, write_soc, write_sod, Cod, CodingStyle, MainHeader, QStep, Quantization, Siz,
    SizComp, Sot, EOC, SOD, SOT,
};
use crate::quantization::{
    bit_plane_depth, decode_step_size, encode_step_size, gain_bits, irreversible_steps,
    reversible_steps, subband_index,
};
use crate::{tier1, tier2, wavelet};

pub(crate) const DEFAULT_LEVELS: i32 = 5;
pub(crate) const DEFAULT_CODE_BLOCK: i32 = 64;
const GUARD_BITS: u8 = 2;

/// Builds one component per image component, with `samples` already level
/// shifted for unsigned data.
pub(crate) fn build_components(
    samples: &[Vec<i32>],
    width: i32,
    height: i32,
    precision: u8,
    signed: bool,
    levels: i32,
) -> Result<Vec<Component>, String> {
    if width <= 0 || height <= 0 {
        return Err("image dimensions must be positive".into());
    }
    let area = crate::geometry::checked_total(width as u64, height as u64, samples.len())? as usize;
    let mut components = Vec::with_capacity(samples.len());
    for (index, plane) in samples.iter().enumerate() {
        if plane.len() != area {
            return Err("component sample count does not match the image size".into());
        }
        let mut component = Component::new(index, 0, 0, width, height, levels, precision, signed)?;
        component.coefficients = plane.clone();
        components.push(component);
    }
    Ok(components)
}

/// Encodes one tile with an optional multiple component transform.
#[cfg(test)]
pub(crate) fn encode_with_mct(
    components: &mut [Component],
    reversible: bool,
    mct: bool,
) -> Result<Vec<u8>, String> {
    encode_with_precincts(components, reversible, None, mct, 1.0)
}

/// Encodes one tile with the irreversible quantization steps scaled by
/// `quant_scale`, which is how `rate` reaches a smaller codestream.
pub(crate) fn encode_with_mct_scaled(
    components: &mut [Component],
    reversible: bool,
    mct: bool,
    quant_scale: f64,
) -> Result<Vec<u8>, String> {
    encode_with_precincts(components, reversible, None, mct, quant_scale)
}

/// Encodes one tile, optionally with explicit precinct exponents per resolution.
pub(crate) fn encode_with_precincts(
    components: &mut [Component],
    reversible: bool,
    precincts: Option<&[(u8, u8)]>,
    mct: bool,
    quant_scale: f64,
) -> Result<Vec<u8>, String> {
    if components.is_empty() {
        return Err("no components to encode".into());
    }
    let levels = components[0].levels;
    if !(0..=32).contains(&levels) {
        return Err("invalid decomposition level count".into());
    }
    let precision = components[0].precision;
    let mct = mct && components.len() >= 3;
    if mct && reversible {
        forward_rct(components);
    }
    let mut ict_planes: Option<Vec<Vec<f64>>> = None;
    if mct && !reversible {
        let mut planes: Vec<Vec<f64>> = components
            .iter()
            .map(|component| {
                component
                    .coefficients
                    .iter()
                    .map(|&v| f64::from(v))
                    .collect()
            })
            .collect();
        forward_ict_planes(&mut planes);
        ict_planes = Some(planes);
    }
    for (index, component) in components.iter_mut().enumerate() {
        if component.levels != levels {
            return Err("components disagree on the decomposition level count".into());
        }
        component.build_code_blocks(DEFAULT_CODE_BLOCK, DEFAULT_CODE_BLOCK, precincts)?;
        if reversible {
            wavelet::forward_53(
                &mut component.coefficients,
                component.width as usize,
                component.height as usize,
                levels as u8,
                0,
                0,
            )?;
        } else {
            let mut plane: Vec<f64> = match &ict_planes {
                Some(planes) => planes[index].clone(),
                None => component
                    .coefficients
                    .iter()
                    .map(|&v| f64::from(v))
                    .collect(),
            };
            wavelet::forward_97(
                &mut plane,
                component.width as usize,
                component.height as usize,
                levels as u8,
            )?;
            component.irreversible = Some(plane);
        }
    }

    let quant = if reversible {
        let bytes = reversible_steps(precision, levels as u8);
        Quantization {
            guard_bits: GUARD_BITS,
            style: 0,
            steps: bytes
                .iter()
                .map(|&byte| QStep {
                    exponent: byte >> 3,
                    mantissa: u16::from(byte & 0x07),
                })
                .collect(),
        }
    } else {
        let steps = irreversible_steps(levels as u8, quant_scale);
        Quantization {
            guard_bits: GUARD_BITS,
            style: 2,
            steps: steps
                .iter()
                .map(|&step| {
                    let encoded = encode_step_size(step, precision);
                    QStep {
                        exponent: (encoded >> 11) as u8,
                        mantissa: encoded & 0x07FF,
                    }
                })
                .collect(),
        }
    };
    let encoded_steps: Vec<u16> = quant
        .steps
        .iter()
        .map(|step| (u16::from(step.exponent) << 11) | (step.mantissa & 0x07FF))
        .collect();

    for component in components.iter_mut() {
        let plane_width = component.width as usize;
        let reversible_plane = std::mem::take(&mut component.coefficients);
        let irreversible_plane = component.irreversible.take();
        for resolution in 0..=levels {
            let precincts = component.precincts_including_empty(resolution);
            for precinct in precincts {
                let bands = component
                    .bands_mut(resolution, precinct)
                    .ok_or_else(|| "missing precinct geometry".to_string())?;
                for band in bands.iter_mut() {
                    let index = subband_index(resolution, band.orientation);
                    let encoded = encoded_steps
                        .get(index.max(0) as usize)
                        .copied()
                        .unwrap_or(0);
                    let subband_bits = bit_plane_depth(encoded, GUARD_BITS);
                    for block in band.code_blocks.iter_mut() {
                        let width = block.width.max(0) as usize;
                        let height = block.height.max(0) as usize;
                        if width == 0 || height == 0 {
                            continue;
                        }
                        let mut coefficients = vec![0i32; width * height];
                        if reversible {
                            for y in 0..height {
                                for x in 0..width {
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    coefficients[y * width + x] =
                                        reversible_plane[sy * plane_width + sx] << 6;
                                }
                            }
                        } else {
                            let plane = irreversible_plane
                                .as_ref()
                                .ok_or_else(|| "missing irreversible plane".to_string())?;
                            // The QCD carries `1/norm`; the subband's 2-D gain
                            // multiplies it in the inverse transform of OpenJPEG
                            // and OpenJPH (E-4), so the coefficient domain here
                            // uses `gain * 1/norm` as well.
                            let step = decode_step_size(encoded, precision)
                                * f64::from(1u32 << gain_bits(band.orientation));
                            for y in 0..height {
                                for x in 0..width {
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    coefficients[y * width + x] =
                                        (plane[sy * plane_width + sx] / step * 64.0).round() as i32;
                                }
                            }
                        }
                        let encoded_block = tier1::encode_code_block(
                            &coefficients,
                            width,
                            height,
                            band.orientation,
                            0,
                        )?;
                        let bit_count = (encoded_block.max_bit_plane - 5).max(0);
                        block.zero_bitplanes = (subband_bits - bit_count).clamp(0, 31) as u8;
                        block.total_passes = encoded_block.pass_count;
                        block.data = encoded_block.data.clone();
                        block.segments = if encoded_block.pass_count == 0 {
                            Vec::new()
                        } else {
                            vec![CodeBlockSegment {
                                data: encoded_block.data,
                                pass_count: encoded_block.pass_count,
                            }]
                        };
                    }
                }
            }
        }
        component.coefficients = reversible_plane;
        component.irreversible = irreversible_plane;
    }

    let tile_data = tier2::encode_packets(components, 1, 0)?;
    let width = components[0].width;
    let height = components[0].height;
    let siz = Siz {
        rsiz: 0,
        xsiz: width as u32,
        ysiz: height as u32,
        xosiz: 0,
        yosiz: 0,
        xtsiz: width as u32,
        ytsiz: height as u32,
        xtosiz: 0,
        ytosiz: 0,
        comps: components
            .iter()
            .map(|component| SizComp {
                ssiz: (component.precision - 1) | if component.signed { 0x80 } else { 0 },
                xrsiz: 1,
                yrsiz: 1,
            })
            .collect(),
    };
    let cod = Cod {
        scod: if precincts.is_some() { 1 } else { 0 },
        progression: 0,
        layers: 1,
        mct: u8::from(mct),
        style: CodingStyle {
            decompositions: levels as u8,
            code_block_width: 4,
            code_block_height: 4,
            style: 0,
            transform: u8::from(reversible),
            precincts: precincts.map(|p| p.to_vec()).unwrap_or_default(),
        },
    };
    let mut output = Vec::new();
    write_soc(&mut output);
    siz.write(&mut output);
    cod.write(&mut output);
    quant.write(&mut output);
    let sot = Sot {
        tile: 0,
        length: (14 + tile_data.len()) as u32,
        part: 0,
        parts: 1,
    };
    sot.write(&mut output);
    write_sod(&mut output);
    output.extend_from_slice(&tile_data);
    write_eoc(&mut output);
    Ok(output)
}

/// Decodes a complete codestream into components with integer samples.
pub(crate) fn decode(data: &[u8]) -> Result<Vec<Component>, String> {
    let (mut header, mut offset) = MainHeader::parse(data)?;
    if header.siz.xrsiz() != 1 || header.siz.yrsiz() != 1 {
        return Err("subsampled JPEG 2000 components are not supported".into());
    }
    if header.siz.xosiz != 0 || header.siz.yosiz != 0 {
        return Err("JPEG 2000 image offsets are not supported".into());
    }
    if header.siz.xtsiz < header.siz.xsiz || header.siz.ytsiz < header.siz.ysiz {
        return Err("multi-tile JPEG 2000 codestreams are not supported".into());
    }
    if header.cap || header.siz.rsiz & 0x4000 != 0 || header.cod.style.style & 0x40 != 0 {
        return Err("HTJ2K codestream is not supported by the classic JPEG 2000 codec".into());
    }
    if (0..header.siz.comps.len()).any(|index| header.coding_style(index).style & 0x40 != 0) {
        return Err("HTJ2K codestream is not supported by the classic JPEG 2000 codec".into());
    }
    let width = u64::from(header.siz.xsiz) - u64::from(header.siz.xosiz);
    let height = u64::from(header.siz.ysiz) - u64::from(header.siz.yosiz);
    if width > i32::MAX as u64 || height > i32::MAX as u64 {
        return Err("image dimensions exceed the supported range".into());
    }
    crate::geometry::checked_total(width, height, header.siz.comps.len())?;
    if header.cod.mct > 1 {
        return Err("unsupported multiple component transform value".into());
    }
    if header.cod.mct != 0 && header.siz.comps.len() < 3 {
        return Err("multiple component transform requires at least three components".into());
    }
    let levels = i32::from(header.cod.style.decompositions);
    let (code_block_width, code_block_height) = header.cod.style.code_block_size()?;
    let mut components = Vec::with_capacity(header.siz.comps.len());
    for (index, comp) in header.siz.comps.iter().enumerate() {
        if comp.xrsiz != 1 || comp.yrsiz != 1 {
            return Err("subsampled JPEG 2000 components are not supported".into());
        }
        let mut component = Component::new(
            index,
            0,
            0,
            width as i32,
            height as i32,
            levels,
            comp.precision(),
            comp.signed(),
        )?;
        let style = header.coding_style(index);
        let precincts = if style.precincts.is_empty() {
            None
        } else {
            Some(style.precincts.as_slice())
        };
        component.build_code_blocks(
            code_block_width as i32,
            code_block_height as i32,
            precincts,
        )?;
        components.push(component);
    }

    let mut tile_data = Vec::new();
    let mut tile_parts = 0usize;
    let eoc_offset;
    loop {
        if offset + 2 > data.len() {
            return Err("truncated codestream before SOT".into());
        }
        let marker = u16::from_be_bytes([data[offset], data[offset + 1]]);
        if marker == EOC {
            eoc_offset = offset;
            break;
        }
        if marker != SOT {
            return Err(format!("unexpected marker 0x{marker:04X} before tile data"));
        }
        let sot_start = offset;
        if offset + 12 > data.len() {
            return Err("truncated SOT segment".into());
        }
        let sot = Sot::parse(&data[offset + 4..offset + 12])?;
        if sot.tile != 0 {
            return Err("only tile 0 is supported".into());
        }
        if sot.length != 0 && sot.length < 14 {
            return Err("tile-part length is shorter than its SOT and SOD segments".into());
        }
        offset += 12;
        loop {
            while offset + 1 < data.len() && data[offset] == 0xFF && data[offset + 1] == 0xFF {
                offset += 1;
            }
            if offset + 2 > data.len() {
                return Err("truncated tile-part before SOD".into());
            }
            if data[offset] != 0xFF {
                return Err("expected marker in tile-part header".into());
            }
            let marker = u16::from_be_bytes([data[offset], data[offset + 1]]);
            if marker == SOD {
                offset += 2;
                break;
            }
            if offset + 4 > data.len() {
                return Err("truncated tile-part header segment".into());
            }
            let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            if length < 2 || offset + 2 + length > data.len() {
                return Err("invalid tile-part header segment length".into());
            }
            header.apply_tile_part_header(marker, &data[offset + 4..offset + 2 + length])?;
            offset += 2 + length;
        }
        if (0..header.siz.comps.len()).any(|index| header.coding_style(index).style & 0x40 != 0) {
            return Err("HTJ2K codestream is not supported by the classic JPEG 2000 codec".into());
        }
        let end = if sot.length != 0 {
            sot_start
                .checked_add(sot.length as usize)
                .ok_or_else(|| "tile-part length overflow".to_string())?
        } else {
            // Psot=0 means "the tile-part runs to EOC", so it can only be the
            // last tile-part of the codestream.
            let position = find_next_tile_marker(data, offset)?;
            if u16::from_be_bytes([data[position], data[position + 1]]) != EOC {
                return Err("a zero-length tile-part must be the last one".into());
            }
            position
        };
        if end > data.len() || end < offset {
            return Err("tile-part extends past the codestream".into());
        }
        tile_parts += 1;
        tile_data.extend_from_slice(&data[offset..end]);
        offset = end;
    }
    if eoc_offset + 2 < data.len() {
        let trailing = &data[eoc_offset + 2..];
        if trailing.len() > 1 || trailing.iter().any(|&byte| byte != 0) {
            return Err("trailing bytes after the EOC marker".into());
        }
    }
    if tile_parts == 0 {
        return Err("codestream has no tile-part".into());
    }

    let styles: Vec<u8> = (0..components.len())
        .map(|index| header.coding_style(index).style)
        .collect();
    tier2::decode_packets(
        &tile_data,
        &mut components,
        header.cod.layers,
        header.cod.progression,
        &styles,
    )?;

    for (index, component) in components.iter_mut().enumerate() {
        let style = header.coding_style(index);
        let quant = header.quantization(index);
        let reversible = style.transform == 1;
        if reversible {
            component.coefficients = vec![0; (component.width * component.height) as usize];
        } else {
            component.irreversible = Some(vec![0.0; (component.width * component.height) as usize]);
        }
        for resolution in 0..=levels {
            let precincts = component.precincts_including_empty(resolution);
            for precinct in precincts {
                let bands = component.bands(resolution, precinct).to_vec();
                for band in &bands {
                    let index_in_band = subband_index(resolution, band.orientation).max(0) as usize;
                    let step = quant.step(index_in_band).unwrap_or(QStep {
                        exponent: 0,
                        mantissa: 0,
                    });
                    let encoded = (u16::from(step.exponent) << 11) | (step.mantissa & 0x07FF);
                    let subband_bits = bit_plane_depth(encoded, quant.guard_bits);
                    for block in &band.code_blocks {
                        let max_bit_plane = subband_bits - i32::from(block.zero_bitplanes) + 5;
                        let decoded = tier1::decode_code_block(
                            &block.segments,
                            block.width as usize,
                            block.height as usize,
                            band.orientation,
                            max_bit_plane,
                            style.style,
                        )?;
                        let width = block.width.max(0) as usize;
                        let height = block.height.max(0) as usize;
                        let plane_width = component.width as usize;
                        if reversible {
                            for y in 0..height {
                                for x in 0..width {
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    component.coefficients[sy * plane_width + sx] =
                                        decoded[y * width + x];
                                }
                            }
                        } else {
                            let step = decode_step_size(encoded, component.precision)
                                * f64::from(1u32 << gain_bits(band.orientation));
                            let plane = component
                                .irreversible
                                .as_mut()
                                .ok_or_else(|| "missing irreversible plane".to_string())?;
                            for y in 0..height {
                                for x in 0..width {
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    plane[sy * plane_width + sx] =
                                        f64::from(decoded[y * width + x]) * step;
                                }
                            }
                        }
                    }
                }
            }
        }
        if reversible {
            wavelet::inverse_53(
                &mut component.coefficients,
                component.width as usize,
                component.height as usize,
                levels as u8,
                0,
                0,
            )?;
            component.samples = component.coefficients.clone();
        } else {
            let mut plane = component
                .irreversible
                .take()
                .ok_or_else(|| "missing irreversible plane".to_string())?;
            wavelet::inverse_97(
                &mut plane,
                component.width as usize,
                component.height as usize,
                levels as u8,
            )?;
            component.samples = plane.iter().map(|&v| v.round() as i32).collect();
            component.float_samples = Some(plane);
        }
    }
    if header.cod.mct != 0 && components.len() >= 3 {
        if header.cod.style.transform == 1 {
            inverse_rct(&mut components);
        } else {
            inverse_ict(&mut components);
        }
    }
    Ok(components)
}

fn forward_rct(components: &mut [Component]) {
    let count = components
        .iter()
        .take(3)
        .map(|component| component.coefficients.len())
        .min()
        .unwrap_or(0);
    for index in 0..count {
        let r = components[0].coefficients[index];
        let g = components[1].coefficients[index];
        let b = components[2].coefficients[index];
        components[0].coefficients[index] = (r + 2 * g + b) >> 2;
        components[1].coefficients[index] = b - g;
        components[2].coefficients[index] = r - g;
    }
}

fn forward_ict_planes(planes: &mut [Vec<f64>]) {
    if planes.len() < 3 {
        return;
    }
    let (red, rest) = planes.split_at_mut(1);
    let (green, blue) = rest.split_at_mut(1);
    for ((r, g), b) in red[0]
        .iter_mut()
        .zip(green[0].iter_mut())
        .zip(blue[0].iter_mut())
    {
        let y = 0.299 * *r + 0.587 * *g + 0.114 * *b;
        *g = (0.5 / (1.0 - 0.114)) * (*b - y);
        *b = (0.5 / (1.0 - 0.299)) * (*r - y);
        *r = y;
    }
}

fn inverse_rct(components: &mut [Component]) {
    let count = components
        .iter()
        .take(3)
        .map(|component| component.samples.len())
        .min()
        .unwrap_or(0);
    for index in 0..count {
        let y = components[0].samples[index];
        let u = components[1].samples[index];
        let v = components[2].samples[index];
        let g = y - ((u + v) >> 2);
        components[0].samples[index] = v + g;
        components[1].samples[index] = g;
        components[2].samples[index] = u + g;
    }
}

fn inverse_ict(components: &mut [Component]) {
    let count = components
        .iter()
        .take(3)
        .map(|component| component.samples.len())
        .min()
        .unwrap_or(0);
    for index in 0..count {
        let y = components[0]
            .float_samples
            .as_ref()
            .map(|p| p[index])
            .unwrap_or(f64::from(components[0].samples[index]));
        let cb = components[1]
            .float_samples
            .as_ref()
            .map(|p| p[index])
            .unwrap_or(f64::from(components[1].samples[index]));
        let cr = components[2]
            .float_samples
            .as_ref()
            .map(|p| p[index])
            .unwrap_or(f64::from(components[2].samples[index]));
        let r = y + 1.402 * cr;
        let g = y - 0.344_136 * cb - 0.714_136 * cr;
        let b = y + 1.772 * cb;
        if let Some(plane) = components[0].float_samples.as_mut() {
            plane[index] = r;
        }
        if let Some(plane) = components[1].float_samples.as_mut() {
            plane[index] = g;
        }
        if let Some(plane) = components[2].float_samples.as_mut() {
            plane[index] = b;
        }
        components[0].samples[index] = r.round() as i32;
        components[1].samples[index] = g.round() as i32;
        components[2].samples[index] = b.round() as i32;
    }
}

fn find_next_tile_marker(data: &[u8], start: usize) -> Result<usize, String> {
    let mut position = start;
    while position + 1 < data.len() {
        if data[position] == 0xFF {
            let marker = u16::from_be_bytes([data[position], data[position + 1]]);
            if marker == SOT || marker == EOC {
                return Ok(position);
            }
        }
        position += 1;
    }
    Err("unterminated tile-part".into())
}

impl Siz {
    fn xrsiz(&self) -> u8 {
        self.comps.first().map(|c| c.xrsiz).unwrap_or(1)
    }

    fn yrsiz(&self) -> u8 {
        self.comps.first().map(|c| c.yrsiz).unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marker::{write_cap, COC, COD, SIZ};

    fn pattern(width: usize, height: usize, seed: i32) -> Vec<i32> {
        (0..width * height)
            .map(|i| ((i as i32 * 37 + seed) % 256) - 128)
            .collect()
    }

    #[test]
    fn reversible_codestream_round_trips_exactly() {
        let width = 48usize;
        let height = 40usize;
        let samples = vec![pattern(width, height, 11)];
        let mut components =
            build_components(&samples, width as i32, height as i32, 8, false, 5).unwrap();
        let codestream = encode_with_mct(&mut components, true, false).unwrap();
        let decoded = decode(&codestream).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].samples, samples[0]);
    }

    #[test]
    fn reversible_codestream_with_custom_precincts_round_trips_exactly() {
        let width = 48usize;
        let height = 40usize;
        let samples = vec![pattern(width, height, 17)];
        let precincts = vec![(2u8, 2u8); 6];
        let mut components =
            build_components(&samples, width as i32, height as i32, 8, false, 5).unwrap();
        let codestream =
            encode_with_precincts(&mut components, true, Some(&precincts), false, 1.0).unwrap();
        let decoded = decode(&codestream).unwrap();
        assert_eq!(decoded[0].samples, samples[0]);
        let (header, _) = MainHeader::parse(&codestream).unwrap();
        assert_eq!(header.cod.scod, 1);
        assert_eq!(header.cod.style.precincts, precincts);
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
        digits
            .chunks(2)
            .map(|pair| {
                let high = (pair[0] as char).to_digit(16).unwrap() as u8;
                let low = (pair[1] as char).to_digit(16).unwrap() as u8;
                (high << 4) | low
            })
            .collect()
    }

    /// Codestream written by OpenJPEG 2.5.4 (`opj_compress -I -r 1 -n 3`) from a
    /// 16x16 8-bit ramp, with the samples that same release decodes back. It
    /// pins the irreversible convention: the QCD carries `1/norm` and the
    /// inverse transform scales each subband by its `2^gain`, so our decoded
    /// samples must agree with OpenJPEG's to within rounding.
    #[test]
    fn decodes_an_openjpeg_irreversible_codestream() {
        const CODESTREAM: &str = "
            ff4fff5100290000000000100000001000000000000000000000001000000010
            00000000000000000001070101ff52000c00000001000204040000ff5c001142
            5f5250055005504757d357d35762ff640025000143726561746564206279204f
            70656e4a5045472076657273696f6e20322e352e34ff90000a00000000013700
            01ff93c3f013103ca7026cd3b4d93fb45ab28a47bdfd33b627c7e0251f80a47e
            024021c44f88efd5323a21c3e0ddd507d3bde74f1a36ae378a7c109bdec7d5dd
            561b72fce54d98bf2403f5065c632d4aa13e863684a2fed246adc7e07d1f8204
            fcd2c05fd53e9853e72f0a08379b4dc595a64a01cd7bd1dbd42d21347cc538c9
            507c9e6dd24b7931297eb7ea0bff1f44847a4ad91d263d738a7277d7c8821d29
            3f4dd011dbf733ad80313679762ceef86474e6db4b3913a75713fdab8e210c8b
            bf72a4c1098ec5614bee854dfd82f6886aa9b6b0d52a3e181c2a519b9dc96b04
            3f42ea0073794b193bb26daa4719d8e7d27f7c5aab4c86a6779dcfc0a4b0aad2
            06042ad2556c88f7bba18a38817df7872ac3d8677f8db2179c4d9f0881beae28
            6fb66382a74900a213ab3737ffd9";
        const EXPECTED: &str = "
            01356a265c914e84b874aade9cd006c4457b376ca15e93c885baeface116d308
            8a477cb16ea3d895caffbcf126e3184d578cc17eb2e8a5da0fcc0136f3285d1a
            9dd18ec3f8b5ea1fdc114603386d2a5fe19ed308c5fb2fec215513487d3a6fa4
            ade318d50a3ffc316623588d4a7fb471f328e61a4f0c407633689d5a8fc381b7
            38f52a5f1c51864378ad6a9fd491c6fb053a6f2c61965388bd7aafe4a1d60bc7
            4a7f3c71a66398cd8abef4b1e61bd80d8f4c81b673a8dd9acf04c1f72be81d52
            5c91c682b8edaadf14d1063bf82d621fa1d693c8fdbaf024e2164c083d722f64
            e6a3d80ccaff34f1265b184d823f74a9b3e81ddb0f4401376b285c924f84b976";
        let stream = hex_bytes(CODESTREAM);
        let expected = hex_bytes(EXPECTED);
        assert_eq!(expected.len(), 256, "fixture is one 16x16 8-bit frame");
        let components = decode(&stream).unwrap();
        assert_eq!(components.len(), 1);
        let samples = &components[0].samples;
        assert_eq!(samples.len(), expected.len());
        let shift = 1i32 << 7;
        let mut worst = 0;
        for (index, &reference) in expected.iter().enumerate() {
            let difference = (samples[index] + shift - i32::from(reference)).abs();
            worst = worst.max(difference);
        }
        assert!(
            worst <= 1,
            "decoded samples differ from OpenJPEG by {worst}"
        );
    }

    #[test]
    fn irreversible_codestream_keeps_high_frequency_detail() {
        // The mean-tolerance test above cannot see a mis-scaled detail band.
        // A checkerboard concentrates energy in HL/LH/HH, so any relative
        // error in those bands shows up directly in the reconstruction.
        let width = 48usize;
        let height = 40usize;
        let pattern: Vec<i32> = (0..width * height)
            .map(|i| {
                if (i + i / width).is_multiple_of(2) {
                    127
                } else {
                    -128
                }
            })
            .collect();
        let samples = vec![pattern];
        let mut components =
            build_components(&samples, width as i32, height as i32, 8, false, 2).unwrap();
        let codestream = encode_with_mct(&mut components, false, false).unwrap();
        let decoded = decode(&codestream).unwrap();
        let max = samples[0]
            .iter()
            .zip(&decoded[0].samples)
            .map(|(a, b)| (a - b).abs())
            .max()
            .unwrap();
        assert!(max <= 8, "high-frequency reconstruction error {max}");
    }

    #[test]
    fn irreversible_codestream_round_trips_within_tolerance() {
        for levels in [0, 1, 2, 5] {
            let width = 48usize;
            let height = 40usize;
            let samples = vec![pattern(width, height, 3)];
            let mut components =
                build_components(&samples, width as i32, height as i32, 8, false, levels).unwrap();
            let codestream = encode_with_mct(&mut components, false, false).unwrap();
            let decoded = decode(&codestream).unwrap();
            let mut total = 0i64;
            let mut max = 0i64;
            for (a, b) in samples[0].iter().zip(&decoded[0].samples) {
                let error = i64::from((a - b).abs());
                total += error;
                max = max.max(error);
            }
            let mean = total as f64 / samples[0].len() as f64;
            println!("levels={levels} mean={mean} max={max}");
            assert!(mean < 4.0, "levels={levels} mean absolute error {mean}");
        }
    }

    fn main_segment(stream: &[u8], marker: u16) -> Option<(usize, usize)> {
        let mut offset = 2;
        while offset + 4 <= stream.len() {
            let current = u16::from_be_bytes([stream[offset], stream[offset + 1]]);
            let length = u16::from_be_bytes([stream[offset + 2], stream[offset + 3]]) as usize;
            if current == marker {
                return Some((offset + 4, offset + 2 + length));
            }
            if current == SOT || current == EOC {
                break;
            }
            offset += 2 + length;
        }
        None
    }

    fn three_component_stream(reversible: bool, mct: bool) -> Vec<u8> {
        let width = 48usize;
        let height = 40usize;
        let samples: Vec<Vec<i32>> = (0..3)
            .map(|component| {
                (0..width * height)
                    .map(|i| ((i * 13 + component * 29) % 200) as i32 - 100)
                    .collect()
            })
            .collect();
        let mut components =
            build_components(&samples, width as i32, height as i32, 8, false, 5).unwrap();
        encode_with_mct(&mut components, reversible, mct).unwrap()
    }

    #[test]
    fn reversible_multiple_component_transform_round_trips_exactly() {
        let width = 48usize;
        let height = 40usize;
        let samples: Vec<Vec<i32>> = (0..3)
            .map(|component| {
                (0..width * height)
                    .map(|i| ((i * 13 + component * 29) % 200) as i32 - 100)
                    .collect()
            })
            .collect();
        let mut components =
            build_components(&samples, width as i32, height as i32, 8, false, 5).unwrap();
        let codestream = encode_with_mct(&mut components, true, true).unwrap();
        let (header, _) = MainHeader::parse(&codestream).unwrap();
        assert_eq!(header.cod.mct, 1);
        let decoded = decode(&codestream).unwrap();
        for (expected, actual) in samples.iter().zip(&decoded) {
            assert_eq!(&actual.samples, expected);
        }
    }

    #[test]
    fn irreversible_multiple_component_transform_decodes_within_tolerance() {
        let width = 48usize;
        let height = 40usize;
        let samples: Vec<Vec<i32>> = (0..3)
            .map(|component| {
                (0..width * height)
                    .map(|i| ((i * 7 + component * 41) % 220) as i32 - 110)
                    .collect()
            })
            .collect();
        let mut components =
            build_components(&samples, width as i32, height as i32, 8, false, 3).unwrap();
        let codestream = encode_with_mct(&mut components, false, true).unwrap();
        let (header, _) = MainHeader::parse(&codestream).unwrap();
        assert_eq!(header.cod.mct, 1);
        assert_eq!(header.cod.style.transform, 0);
        let decoded = decode(&codestream).unwrap();
        for (expected, actual) in samples.iter().zip(&decoded) {
            let max = expected
                .iter()
                .zip(&actual.samples)
                .map(|(a, b)| (a - b).abs())
                .max()
                .unwrap();
            assert!(max <= 8, "ICT reconstruction error {max}");
        }
    }

    #[test]
    fn multiple_component_transform_requires_three_components() {
        let stream = three_component_stream(true, false);
        let (cod_payload, _) = main_segment(&stream, COD).unwrap();
        let mut patched = stream.clone();
        patched[cod_payload + 4] = 1; // mct = 1 with csiz = 3
        assert_eq!(decode(&patched).unwrap().len(), 3);
        let mut mono = {
            let width = 16usize;
            let samples = vec![pattern(width, 16, 1)];
            let mut components = build_components(&samples, width as i32, 16, 8, false, 2).unwrap();
            encode_with_mct(&mut components, true, false).unwrap()
        };
        let (mono_cod, _) = main_segment(&mono, COD).unwrap();
        mono[mono_cod + 4] = 1;
        let error = decode(&mono).unwrap_err();
        assert!(error.contains("three components"), "{error}");
    }

    #[test]
    fn oversized_siz_is_rejected_before_any_allocation() {
        let mut stream = three_component_stream(true, false);
        let (siz_payload, _) = main_segment(&stream, SIZ).unwrap();
        stream[siz_payload + 2..siz_payload + 6].copy_from_slice(&65535u32.to_be_bytes());
        stream[siz_payload + 6..siz_payload + 10].copy_from_slice(&65535u32.to_be_bytes());
        stream[siz_payload + 18..siz_payload + 22].copy_from_slice(&65535u32.to_be_bytes());
        stream[siz_payload + 22..siz_payload + 26].copy_from_slice(&65535u32.to_be_bytes());
        let error = decode(&stream).unwrap_err();
        assert!(error.contains("sample count"), "{error}");
    }

    #[test]
    fn a_tile_larger_than_the_image_is_a_valid_single_tile() {
        let stream = three_component_stream(true, false);
        let (siz_payload, _) = main_segment(&stream, SIZ).unwrap();
        let mut larger = stream.clone();
        larger[siz_payload + 18..siz_payload + 22].copy_from_slice(&65535u32.to_be_bytes());
        larger[siz_payload + 22..siz_payload + 26].copy_from_slice(&65535u32.to_be_bytes());
        assert_eq!(decode(&larger).unwrap().len(), 3);

        let mut smaller = stream.clone();
        smaller[siz_payload + 18..siz_payload + 22].copy_from_slice(&32u32.to_be_bytes());
        let error = decode(&smaller).unwrap_err();
        assert!(error.contains("multi-tile"), "{error}");
    }

    #[test]
    fn the_classic_codec_rejects_htj2k_codestreams() {
        let stream = three_component_stream(true, false);
        let (siz_payload, siz_end) = main_segment(&stream, SIZ).unwrap();
        let (cod_payload, _) = main_segment(&stream, COD).unwrap();

        let mut capped = stream.clone();
        let mut cap = Vec::new();
        write_cap(&mut cap, 2);
        capped.splice(siz_end..siz_end, cap);
        let error = decode(&capped).unwrap_err();
        assert!(error.contains("HTJ2K"), "{error}");

        let mut rsiz = stream.clone();
        rsiz[siz_payload..siz_payload + 2].copy_from_slice(&0x4000u16.to_be_bytes());
        let error = decode(&rsiz).unwrap_err();
        assert!(error.contains("HTJ2K"), "{error}");

        let mut style = stream.clone();
        style[cod_payload + 8] |= 0x40;
        let error = decode(&style).unwrap_err();
        assert!(error.contains("HTJ2K"), "{error}");
    }

    #[test]
    fn trailing_non_padding_bytes_after_eoc_are_rejected() {
        let mut stream = three_component_stream(true, false);
        assert_eq!(&stream[stream.len() - 2..], &EOC.to_be_bytes());
        stream.push(0x00);
        assert!(decode(&stream).is_ok(), "a single padding byte is allowed");
        stream.push(0x7F);
        let error = decode(&stream).unwrap_err();
        assert!(error.contains("trailing"), "{error}");
    }

    #[test]
    fn zero_length_tile_parts_are_valid_only_for_the_last_part() {
        let stream = three_component_stream(true, false);
        let sot = {
            let mut offset = 2;
            loop {
                let marker = u16::from_be_bytes([stream[offset], stream[offset + 1]]);
                if marker == SOT {
                    break offset;
                }
                let length = u16::from_be_bytes([stream[offset + 2], stream[offset + 3]]) as usize;
                offset += 2 + length;
            }
        };
        let mut patched = stream.clone();
        patched[sot + 6..sot + 10].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(decode(&patched).unwrap().len(), 3);

        let mut short = stream.clone();
        short[sot + 6..sot + 10].copy_from_slice(&10u32.to_be_bytes());
        let error = decode(&short).unwrap_err();
        assert!(error.contains("shorter"), "{error}");

        let mut long = stream.clone();
        long[sot + 6..sot + 10].copy_from_slice(&(stream.len() as u32).to_be_bytes());
        let error = decode(&long).unwrap_err();
        assert!(error.contains("past the codestream"), "{error}");
    }

    #[test]
    fn trailing_tile_data_is_rejected() {
        let stream = three_component_stream(true, false);
        let sot = {
            let mut offset = 2;
            loop {
                let marker = u16::from_be_bytes([stream[offset], stream[offset + 1]]);
                if marker == SOT {
                    break offset;
                }
                let length = u16::from_be_bytes([stream[offset + 2], stream[offset + 3]]) as usize;
                offset += 2 + length;
            }
        };
        let mut patched = stream;
        let psot = u32::from_be_bytes([
            patched[sot + 6],
            patched[sot + 7],
            patched[sot + 8],
            patched[sot + 9],
        ]);
        patched[sot + 6..sot + 10].copy_from_slice(&(psot + 1).to_be_bytes());
        patched.insert(patched.len() - 2, 0xAA);
        let error = decode(&patched).unwrap_err();
        assert!(error.contains("trailing bytes"), "{error}");
    }

    #[test]
    fn tile_part_coc_and_qcc_are_applied() {
        let samples = vec![pattern(24, 20, 3)];
        let mut components = build_components(&samples, 24, 20, 8, false, 2).unwrap();
        let stream = encode_with_mct(&mut components, true, false).unwrap();
        let marker = |code| {
            stream
                .windows(2)
                .position(|pair| pair == [0xFF, code])
                .unwrap_or_else(|| panic!("marker 0x{code:02X}"))
        };
        let (cod, qcd, sot, sod) = (marker(0x52), marker(0x5C), marker(0x90), marker(0x93));
        let cod_length = u16::from_be_bytes([stream[cod + 2], stream[cod + 3]]) as usize;
        let qcd_length = u16::from_be_bytes([stream[qcd + 2], stream[qcd + 3]]) as usize;
        let mut coc = vec![0u8, stream[cod + 4]];
        coc.extend_from_slice(&stream[cod + 9..cod + 2 + cod_length]);
        let mut qcc = vec![0u8];
        qcc.extend_from_slice(&stream[qcd + 4..qcd + 2 + qcd_length]);
        let mut insertion = vec![0xFF, 0x53];
        insertion.extend_from_slice(&(coc.len() as u16 + 2).to_be_bytes());
        insertion.extend_from_slice(&coc);
        insertion.extend_from_slice(&[0xFF, 0x5D]);
        insertion.extend_from_slice(&(qcc.len() as u16 + 2).to_be_bytes());
        insertion.extend_from_slice(&qcc);

        let mut patched = stream[..sod].to_vec();
        patched.extend_from_slice(&insertion);
        patched.extend_from_slice(&stream[sod..]);
        let psot = u32::from_be_bytes([
            patched[sot + 6],
            patched[sot + 7],
            patched[sot + 8],
            patched[sot + 9],
        ]);
        patched[sot + 6..sot + 10].copy_from_slice(&(psot + insertion.len() as u32).to_be_bytes());

        let decoded = decode(&patched).unwrap();
        assert_eq!(decoded[0].samples, samples[0]);
    }

    #[test]
    fn every_truncation_of_a_valid_codestream_is_rejected() {
        let stream = three_component_stream(true, false);
        for length in [0, 1, 2, 3, stream.len() - 1] {
            assert!(decode(&stream[..length]).is_err(), "length {length}");
        }
    }

    #[test]
    fn scalar_derived_quantization_decodes_end_to_end() {
        let samples = vec![pattern(16, 16, 5)];
        let mut components = build_components(&samples, 16, 16, 8, false, 0).unwrap();
        let stream = encode_with_mct(&mut components, true, false).unwrap();
        let (header, _) = MainHeader::parse(&stream).unwrap();
        let marker = stream
            .windows(2)
            .position(|pair| pair == [0xFF, 0x5C])
            .expect("QCD marker");
        let length = u16::from_be_bytes([stream[marker + 2], stream[marker + 3]]) as usize;
        let end = marker + 2 + length;
        let style1 = Quantization {
            guard_bits: header.qcd.guard_bits,
            style: 1,
            steps: vec![header.qcd.steps[0]],
        };
        let mut patched = stream[..marker].to_vec();
        style1.write(&mut patched);
        patched.extend_from_slice(&stream[end..]);
        let decoded = decode(&patched).unwrap();
        assert_eq!(decoded[0].samples, samples[0]);
    }

    #[test]
    fn coc_transform_mismatch_under_mct_is_rejected() {
        let stream = three_component_stream(true, true); // reversible + MCT
        let (header, _) = MainHeader::parse(&stream).unwrap();
        let style = &header.cod.style;
        let mut payload = vec![0u8, 0u8]; // one-byte component index 0, Scoc 0
        payload.extend_from_slice(&[
            style.decompositions,
            style.code_block_width,
            style.code_block_height,
            style.style,
            0, // transform 0 while the COD/MCT stream is reversible (1)
        ]);
        let mut coc = Vec::new();
        coc.extend_from_slice(&COC.to_be_bytes());
        coc.extend_from_slice(&((2 + payload.len()) as u16).to_be_bytes());
        coc.extend_from_slice(&payload);

        let (_, cod_end) = main_segment(&stream, COD).unwrap();
        let mut patched = stream[..cod_end].to_vec();
        patched.extend_from_slice(&coc);
        patched.extend_from_slice(&stream[cod_end..]);
        let error = decode(&patched).unwrap_err();
        assert!(error.contains("transform"), "{error}");
    }
}
