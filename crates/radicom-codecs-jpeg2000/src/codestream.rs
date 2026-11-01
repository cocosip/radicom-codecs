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
    let mut components = Vec::with_capacity(samples.len());
    for (index, plane) in samples.iter().enumerate() {
        if plane.len() != (width as usize) * (height as usize) {
            return Err("component sample count does not match the image size".into());
        }
        let mut component = Component::new(index, 0, 0, width, height, levels, precision, signed);
        component.coefficients = plane.clone();
        components.push(component);
    }
    Ok(components)
}

/// Encodes a complete codestream for one tile.
pub(crate) fn encode(components: &mut [Component], reversible: bool) -> Result<Vec<u8>, String> {
    encode_with_precincts(components, reversible, None)
}

/// Encodes one tile, optionally with explicit precinct exponents per resolution.
pub(crate) fn encode_with_precincts(
    components: &mut [Component],
    reversible: bool,
    precincts: Option<&[(u8, u8)]>,
) -> Result<Vec<u8>, String> {
    if components.is_empty() {
        return Err("no components to encode".into());
    }
    let levels = components[0].levels;
    if !(0..=32).contains(&levels) {
        return Err("invalid decomposition level count".into());
    }
    let precision = components[0].precision;
    for component in components.iter_mut() {
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
            let mut plane: Vec<f64> = component
                .coefficients
                .iter()
                .map(|&v| f64::from(v))
                .collect();
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
        let steps = irreversible_steps(levels as u8);
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
        mct: 0,
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
    let (header, mut offset) = MainHeader::parse(data)?;
    if header.siz.xrsiz() != 1 || header.siz.yrsiz() != 1 {
        return Err("subsampled JPEG 2000 components are not supported".into());
    }
    if header.siz.xosiz != 0 || header.siz.yosiz != 0 {
        return Err("JPEG 2000 image offsets are not supported".into());
    }
    if header.siz.xtsiz < header.siz.xsiz || header.siz.ytsiz < header.siz.ysiz {
        return Err("multi-tile JPEG 2000 codestreams are not supported".into());
    }
    let levels = i32::from(header.cod.style.decompositions);
    let (code_block_width, code_block_height) = header.cod.style.code_block_size()?;
    let mut components = Vec::with_capacity(header.siz.comps.len());
    for (index, comp) in header.siz.comps.iter().enumerate() {
        if comp.xrsiz != 1 || comp.yrsiz != 1 {
            return Err("subsampled JPEG 2000 components are not supported".into());
        }
        let width = header.siz.xsiz as i32 - header.siz.xosiz as i32;
        let height = header.siz.ysiz as i32 - header.siz.yosiz as i32;
        let mut component = Component::new(
            index,
            0,
            0,
            width,
            height,
            levels,
            comp.precision(),
            comp.signed(),
        );
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
    loop {
        if offset + 2 > data.len() {
            return Err("truncated codestream before SOT".into());
        }
        let marker = u16::from_be_bytes([data[offset], data[offset + 1]]);
        if marker == EOC {
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
        offset += 12;
        if offset + 2 > data.len() {
            return Err("truncated tile-part before SOD".into());
        }
        let sod = u16::from_be_bytes([data[offset], data[offset + 1]]);
        if sod != SOD {
            return Err("missing SOD marker".into());
        }
        offset += 2;
        let end = if sot.length != 0 {
            sot_start
                .checked_add(sot.length as usize)
                .ok_or_else(|| "tile-part length overflow".to_string())?
        } else {
            find_next_tile_marker(data, offset)?
        };
        if end > data.len() || end < offset {
            return Err("tile-part extends past the codestream".into());
        }
        tile_data.extend_from_slice(&data[offset..end]);
        offset = end;
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
                            let step = decode_step_size(encoded, component.precision);
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
        let codestream = encode(&mut components, true).unwrap();
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
        let codestream = encode_with_precincts(&mut components, true, Some(&precincts)).unwrap();
        let decoded = decode(&codestream).unwrap();
        assert_eq!(decoded[0].samples, samples[0]);
        let (header, _) = MainHeader::parse(&codestream).unwrap();
        assert_eq!(header.cod.scod, 1);
        assert_eq!(header.cod.style.precincts, precincts);
    }

    #[test]
    fn irreversible_codestream_round_trips_within_tolerance() {
        for levels in [0, 1, 2, 5] {
            let width = 48usize;
            let height = 40usize;
            let samples = vec![pattern(width, height, 3)];
            let mut components =
                build_components(&samples, width as i32, height as i32, 8, false, levels).unwrap();
            let codestream = encode(&mut components, false).unwrap();
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
}
