//! HTJ2K codestream assembly and parsing (ISO/IEC 15444-15).
//!
//! The tile geometry, wavelet transform and DICOM frame adapter are shared
//! with the classic JPEG 2000 codestream; only the packet headers and
//! code-block coder differ.

use crate::geometry::{CodeBlock, Component, PrecinctBand};
use crate::marker::{
    cap_ccap, write_cap, write_eoc, write_soc, write_sod, write_tlm, Cod, CodingStyle, MainHeader,
    QStep, Quantization, Siz, SizComp, Sot, EOC, SOD, SOT,
};
use crate::quantization::{bit_plane_depth, reversible_steps, subband_index};
use crate::wavelet;

use super::cleanup::{decode_cleanup, encode_cleanup};
use super::packet::{decode_ht_packet, encode_ht_packet};
use super::quant::{
    create_openjph_steps, create_quality_steps, decode_delta, from_sign_magnitude, kmax,
    to_sign_magnitude,
};

const CODE_BLOCK: i32 = 64;
const GUARD_BITS: u8 = 1;

/// Encoder settings shared by the codestream and DICOM frame layers.
pub(crate) struct EncodeOptions {
    pub(crate) progression: u8,
    pub(crate) reversible: bool,
    pub(crate) levels: i32,
    pub(crate) mct: bool,
    /// 1..=100 selects an explicit irreversible quality; 0 keeps the default.
    pub(crate) quality: u32,
    pub(crate) insert_tlm: bool,
}

impl EncodeOptions {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(0..=32).contains(&self.levels) {
            return Err("invalid decomposition level count".into());
        }
        if self.quality > 100 {
            return Err("HTJ2K quality must be between 1 and 100".into());
        }
        if self.progression >= 3 {
            return Err(
                "PCRL and CPRL progression orders are not implemented by the HTJ2K codec".into(),
            );
        }
        Ok(())
    }
}

/// Encodes one HTJ2K tile.
pub(crate) fn encode(
    components: &mut [Component],
    options: &EncodeOptions,
) -> Result<Vec<u8>, String> {
    if components.is_empty() {
        return Err("no components to encode".into());
    }
    options.validate()?;
    let levels = options.levels;
    let reversible = options.reversible;
    let progression = options.progression;
    let precision = components[0].precision;
    // T.800 Table A.15: MCT is a flag; the wavelet transform selects RCT (5/3)
    // or ICT (9/7).
    let mct = u8::from(options.mct && components.len() >= 3);
    if reversible && mct != 0 {
        forward_rct(components);
    }
    let scale = 2f64.powi(i32::from(precision));
    for component in components.iter_mut() {
        if component.levels != levels {
            return Err("components disagree on the decomposition level count".into());
        }
        component.build_code_blocks(CODE_BLOCK, CODE_BLOCK, None)?;
        if reversible {
            wavelet::forward_53(
                &mut component.coefficients,
                component.width as usize,
                component.height as usize,
                levels as u8,
                0,
                0,
            )?;
        }
    }
    if !reversible {
        let mut planes: Vec<Vec<f64>> = components
            .iter()
            .map(|component| {
                component
                    .coefficients
                    .iter()
                    .map(|&value| f64::from(value) / scale)
                    .collect()
            })
            .collect();
        if mct != 0 {
            forward_ict_planes(&mut planes);
        }
        for (component, mut plane) in components.iter_mut().zip(planes) {
            wavelet::forward_97(
                &mut plane,
                component.width as usize,
                component.height as usize,
                levels as u8,
            )?;
            component.irreversible = Some(plane);
        }
    }

    let (steps, encoded_steps, quant_style) = if reversible {
        let step_bytes = reversible_steps(precision, levels as u8);
        let steps: Vec<QStep> = step_bytes
            .iter()
            .map(|&byte| QStep {
                exponent: byte >> 3,
                mantissa: u16::from(byte & 0x07),
            })
            .collect();
        let encoded = steps
            .iter()
            .map(|step| (u16::from(step.exponent) << 11) | (step.mantissa & 0x07FF))
            .collect();
        (steps, encoded, 0u8)
    } else {
        let encoded = if options.quality == 0 {
            create_openjph_steps(levels as u8, precision, -1.0)
        } else {
            create_quality_steps(levels as u8, precision, options.quality)
        };
        let steps: Vec<QStep> = encoded
            .iter()
            .map(|&value| QStep {
                exponent: (value >> 11) as u8,
                mantissa: value & 0x07FF,
            })
            .collect();
        (steps, encoded, 2u8)
    };

    let mut tile_data = Vec::new();
    for resolution in 0..=levels {
        for component in components.iter_mut() {
            let precincts = component.precincts_including_empty(resolution);
            for precinct in precincts {
                let plane_width = component.width as usize;
                if reversible {
                    let plane = std::mem::take(&mut component.coefficients);
                    let bands = component
                        .bands_mut(resolution, precinct)
                        .ok_or_else(|| "missing precinct geometry".to_string())?;
                    for band in bands.iter_mut() {
                        let index = subband_index(resolution, band.orientation).max(0) as usize;
                        let encoded = encoded_steps.get(index).copied().unwrap_or(0);
                        let subband_bits = bit_plane_depth(encoded, GUARD_BITS);
                        let k_max = subband_bits.max(1);
                        let missing = (k_max - 1).max(0) as u32;
                        let shift = (31 - k_max).clamp(0, 30) as u32;
                        for block in band.code_blocks.iter_mut() {
                            encode_block(block, &plane, plane_width, missing, shift)?;
                        }
                    }
                    let refs: Vec<&PrecinctBand> = bands.iter().collect();
                    let packet = encode_ht_packet(&refs);
                    tile_data.extend_from_slice(&packet);
                    component.coefficients = plane;
                } else {
                    let plane = component
                        .irreversible
                        .take()
                        .ok_or_else(|| "missing irreversible plane".to_string())?;
                    let bands = component
                        .bands_mut(resolution, precinct)
                        .ok_or_else(|| "missing precinct geometry".to_string())?;
                    for band in bands.iter_mut() {
                        let index = subband_index(resolution, band.orientation).max(0) as usize;
                        let encoded = encoded_steps.get(index).copied().unwrap_or(0);
                        let k_max = kmax(encoded, GUARD_BITS).max(1);
                        let delta = decode_delta(encoded, band.orientation, k_max);
                        let missing = (k_max - 1).max(0) as u32;
                        for block in band.code_blocks.iter_mut() {
                            encode_block_irreversible(block, &plane, plane_width, missing, delta)?;
                        }
                    }
                    let refs: Vec<&PrecinctBand> = bands.iter().collect();
                    let packet = encode_ht_packet(&refs);
                    tile_data.extend_from_slice(&packet);
                    component.irreversible = Some(plane);
                }
            }
        }
    }
    let width = components[0].width;
    let height = components[0].height;
    let siz = Siz {
        rsiz: 0x4000,
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
        scod: 0,
        progression,
        layers: 1,
        mct,
        style: CodingStyle {
            decompositions: levels as u8,
            code_block_width: 4,
            code_block_height: 4,
            style: 0x40,
            transform: u8::from(reversible),
            precincts: Vec::new(),
        },
    };
    let quant = Quantization {
        guard_bits: GUARD_BITS,
        style: quant_style,
        steps,
    };
    let cap = cap_ccap(precision, reversible);

    let mut output = Vec::new();
    write_soc(&mut output);
    siz.write(&mut output);
    write_cap(&mut output, cap);
    cod.write(&mut output);
    quant.write(&mut output);
    let tile_part_length = (14 + tile_data.len()) as u32;
    if options.insert_tlm {
        write_tlm(&mut output, tile_part_length);
    }
    let sot = Sot {
        tile: 0,
        length: tile_part_length,
        part: 0,
        parts: 1,
    };
    sot.write(&mut output);
    write_sod(&mut output);
    output.extend_from_slice(&tile_data);
    write_eoc(&mut output);
    Ok(output)
}

fn encode_block_irreversible(
    block: &mut CodeBlock,
    plane: &[f64],
    plane_width: usize,
    missing: u32,
    delta: f64,
) -> Result<(), String> {
    let width = block.width.max(0) as usize;
    let height = block.height.max(0) as usize;
    block.total_passes = 0;
    block.zero_bitplanes = 0;
    block.data.clear();
    block.segments.clear();
    if width == 0 || height == 0 {
        return Ok(());
    }
    let mut values = Vec::with_capacity(width * height);
    let mut any = false;
    for y in 0..height {
        for x in 0..width {
            let value = plane[(block.y0 as usize + y) * plane_width + block.x0 as usize + x];
            if value != 0.0 {
                any = true;
            }
            values.push(value);
        }
    }
    if !any {
        return Ok(());
    }
    let coefficients = to_sign_magnitude(&values, delta);
    let data = encode_cleanup(&coefficients, width, height, missing)?;
    block.total_passes = 1;
    block.zero_bitplanes = missing.min(255) as u8;
    block.data = data.clone();
    block.segments = vec![crate::geometry::CodeBlockSegment {
        data,
        pass_count: 1,
    }];
    Ok(())
}
fn encode_block(
    block: &mut CodeBlock,
    plane: &[i32],
    plane_width: usize,
    missing: u32,
    shift: u32,
) -> Result<(), String> {
    let width = block.width.max(0) as usize;
    let height = block.height.max(0) as usize;
    block.total_passes = 0;
    block.zero_bitplanes = 0;
    block.data.clear();
    block.segments.clear();
    if width == 0 || height == 0 {
        return Ok(());
    }
    let mut coefficients = vec![0i32; width * height];
    let mut any = false;
    for y in 0..height {
        for x in 0..width {
            let sx = block.x0 as usize + x;
            let sy = block.y0 as usize + y;
            let value = plane[sy * plane_width + sx];
            if value != 0 {
                any = true;
            }
            let magnitude = (i64::from(value).unsigned_abs() << shift) as u32;
            coefficients[y * width + x] = if value < 0 {
                (0x8000_0000u32 | magnitude) as i32
            } else {
                magnitude as i32
            };
        }
    }
    if !any {
        return Ok(());
    }
    let data = encode_cleanup(&coefficients, width, height, missing)?;
    block.total_passes = 1;
    block.zero_bitplanes = missing.min(255) as u8;
    block.data = data.clone();
    block.segments = vec![crate::geometry::CodeBlockSegment {
        data,
        pass_count: 1,
    }];
    Ok(())
}

/// Decodes one reversible HTJ2K codestream into integer component samples.
pub(crate) fn decode(
    data: &[u8],
    expected_progression: Option<u8>,
) -> Result<Vec<Component>, String> {
    let (header, mut offset) = MainHeader::parse(data)?;
    if header.cod.style.style & 0x40 == 0 {
        return Err("codestream is not HTJ2K (COD style 0x40 is missing)".into());
    }
    if header.cod.style.style & 0xB7 != 0 {
        return Err("unsupported HTJ2K code-block style bits".into());
    }
    if header.cod.scod & 1 != 0
        || header
            .cocs
            .iter()
            .any(|coc| !coc.style.precincts.is_empty())
    {
        return Err("HTJ2K codestreams with explicit precincts are not supported".into());
    }
    if header.cod.progression >= 3 {
        return Err(
            "PCRL and CPRL progression orders are not implemented by the HTJ2K codec".into(),
        );
    }
    if header.cod.layers != 1 {
        // Multi-layer HTJ2K needs SigProp/MagRef refinement passes, which
        // OpenJPH also rejects (`0x00030053`); no reference encoder emits them.
        return Err(format!(
            "HTJ2K quality layers are limited to 1, this codestream has {}",
            header.cod.layers
        ));
    }
    if let Some(expected) = expected_progression {
        if header.cod.progression != expected {
            return Err(format!(
                "HTJ2K codestream progression is {} but {} is required",
                header.cod.progression, expected
            ));
        }
    }
    let reversible = header.cod.style.transform == 1;
    if header.cod.mct > 1 {
        return Err("unsupported multiple component transform value".into());
    }
    if header.cod.mct != 0 && header.siz.comps.len() < 3 {
        return Err("multiple component transform requires at least three components".into());
    }
    if header
        .siz
        .comps
        .iter()
        .any(|comp| comp.xrsiz != 1 || comp.yrsiz != 1)
    {
        return Err("subsampled HTJ2K components are not supported".into());
    }
    if header.siz.xosiz != 0 || header.siz.yosiz != 0 {
        return Err("HTJ2K image offsets are not supported".into());
    }
    let width = u64::from(header.siz.xsiz) - u64::from(header.siz.xosiz);
    let height = u64::from(header.siz.ysiz) - u64::from(header.siz.yosiz);
    if width > i32::MAX as u64 || height > i32::MAX as u64 {
        return Err("HTJ2K image dimensions exceed the supported range".into());
    }
    crate::geometry::checked_total(width, height, header.siz.comps.len())?;
    let levels = i32::from(header.cod.style.decompositions);
    let (code_block_width, code_block_height) = header.cod.style.code_block_size()?;
    let mut components = Vec::with_capacity(header.siz.comps.len());
    for (index, comp) in header.siz.comps.iter().enumerate() {
        if comp.xrsiz != 1 || comp.yrsiz != 1 {
            return Err("subsampled HTJ2K components are not supported".into());
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
        component.build_code_blocks(code_block_width as i32, code_block_height as i32, None)?;
        components.push(component);
    }

    let mut tile_data = Vec::new();
    loop {
        if offset + 2 > data.len() {
            return Err("truncated HTJ2K codestream before SOT".into());
        }
        let marker = u16::from_be_bytes([data[offset], data[offset + 1]]);
        if marker == EOC {
            break;
        }
        if marker != SOT {
            // DICOM frame padding after the last tile-part is all zeroes.
            break;
        }
        let sot_start = offset;
        if offset + 12 > data.len() {
            return Err("truncated HTJ2K SOT segment".into());
        }
        let sot = Sot::parse(&data[offset + 4..offset + 12])?;
        if sot.tile != 0 {
            return Err("only tile 0 is supported".into());
        }
        if sot.length == 0
            && (offset + 14 > data.len()
                || u16::from_be_bytes([data[offset + 12], data[offset + 13]]) != SOD)
        {
            // A zero-length SOT with no SOD is trailing fragment padding.
            break;
        }
        offset += 12;
        if offset + 2 > data.len() {
            return Err("truncated HTJ2K tile-part before SOD".into());
        }
        if u16::from_be_bytes([data[offset], data[offset + 1]]) != SOD {
            return Err("missing SOD marker".into());
        }
        offset += 2;
        let end = if sot.length == 0 {
            data.len()
        } else {
            sot_start
                .checked_add(sot.length as usize)
                .ok_or_else(|| "tile-part length overflow".to_string())?
        };
        if end > data.len() || end < offset {
            return Err("tile-part extends past the codestream".into());
        }
        tile_data.extend_from_slice(&data[offset..end]);
        offset = end;
    }

    let mut packet_offset = 0usize;
    for resolution in 0..=levels {
        for component in components.iter_mut() {
            let precincts = component.precincts_including_empty(resolution);
            for precinct in precincts {
                let snapshot = component.bands(resolution, precinct).to_vec();
                let refs: Vec<&PrecinctBand> = snapshot.iter().collect();
                let (consumed, decoded) = decode_ht_packet(&tile_data[packet_offset..], &refs)?;
                packet_offset += consumed;
                let mut by_index = std::collections::HashMap::new();
                for item in decoded {
                    by_index.insert(item.index, item);
                }

                let plane_width = component.width as usize;
                let quant = header.quantization(component.index);
                if reversible {
                    let mut plane = std::mem::take(&mut component.coefficients);
                    let bands = component
                        .bands_mut(resolution, precinct)
                        .ok_or_else(|| "missing precinct geometry".to_string())?;
                    for band in bands.iter_mut() {
                        let index = subband_index(resolution, band.orientation).max(0) as usize;
                        let step = quant.step(index).unwrap_or(QStep {
                            exponent: 0,
                            mantissa: 0,
                        });
                        let encoded = (u16::from(step.exponent) << 11) | (step.mantissa & 0x07FF);
                        let subband_bits = bit_plane_depth(encoded, quant.guard_bits).max(1);
                        let shift = (31 - subband_bits).clamp(0, 30) as u32;
                        for block in band.code_blocks.iter_mut() {
                            let item = match by_index.remove(&block.index) {
                                Some(item) => item,
                                None => continue,
                            };
                            let width = block.width.max(0) as usize;
                            let height = block.height.max(0) as usize;
                            if width == 0 || height == 0 {
                                continue;
                            }
                            let sign_magnitude = decode_cleanup(
                                &item.data,
                                width,
                                height,
                                u32::from(item.zero_bitplanes),
                            )?;
                            for y in 0..height {
                                for x in 0..width {
                                    let value = sign_magnitude[y * width + x];
                                    let magnitude = ((value as u32) & 0x7FFF_FFFF) >> shift;
                                    let signed = if value < 0 {
                                        -(magnitude as i32)
                                    } else {
                                        magnitude as i32
                                    };
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    plane[sy * plane_width + sx] = signed;
                                }
                            }
                        }
                    }
                    component.coefficients = plane;
                } else {
                    let mut plane = component.irreversible.take().unwrap_or_else(|| {
                        vec![0.0; (component.width * component.height) as usize]
                    });
                    let precision = component.precision;
                    let bands = component
                        .bands_mut(resolution, precinct)
                        .ok_or_else(|| "missing precinct geometry".to_string())?;
                    for band in bands.iter_mut() {
                        let index = subband_index(resolution, band.orientation).max(0) as usize;
                        let step = quant.step(index).unwrap_or(QStep {
                            exponent: 0,
                            mantissa: 0,
                        });
                        let encoded = (u16::from(step.exponent) << 11) | (step.mantissa & 0x07FF);
                        let k_max = kmax(encoded, quant.guard_bits).max(1);
                        let delta = decode_delta(encoded, band.orientation, k_max)
                            * 2f64.powi(i32::from(precision));
                        for block in band.code_blocks.iter_mut() {
                            let item = match by_index.remove(&block.index) {
                                Some(item) => item,
                                None => continue,
                            };
                            let width = block.width.max(0) as usize;
                            let height = block.height.max(0) as usize;
                            if width == 0 || height == 0 {
                                continue;
                            }
                            let sign_magnitude = decode_cleanup(
                                &item.data,
                                width,
                                height,
                                u32::from(item.zero_bitplanes),
                            )?;
                            let values = from_sign_magnitude(&sign_magnitude, delta);
                            for y in 0..height {
                                for x in 0..width {
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    plane[sy * plane_width + sx] = values[y * width + x];
                                }
                            }
                        }
                    }
                    component.irreversible = Some(plane);
                }
            }
        }
    }

    for component in components.iter_mut() {
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
            component.samples = plane
                .iter()
                .map(|&value| {
                    if value >= 0.0 {
                        (value + 0.5) as i32
                    } else {
                        (value - 0.5) as i32
                    }
                })
                .collect();
            component.float_samples = Some(plane);
        }
    }
    if header.cod.mct != 0 && components.len() >= 3 {
        if reversible {
            inverse_rct(&mut components);
        } else {
            inverse_ict(&mut components);
        }
    }
    Ok(components)
}

fn forward_ict_planes(planes: &mut [Vec<f64>]) {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codestream::build_components;

    fn options(components: &[Component], reversible: bool) -> EncodeOptions {
        EncodeOptions {
            progression: 0,
            reversible,
            levels: components[0].levels,
            mct: components.len() >= 3,
            quality: 0,
            insert_tlm: false,
        }
    }

    #[test]
    fn reversible_codestream_round_trips() {
        let mut components =
            build_components(&[vec![1, 2, 3, 4, 5, 6, 7, 8, 9]], 3, 3, 8, false, 0).unwrap();
        let opts = options(&components, true);
        let encoded = encode(&mut components, &opts).unwrap();
        let decoded = decode(&encoded, None).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].samples, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn multi_code_block_and_color_round_trip() {
        let width = 128usize;
        let height = 128usize;
        let mut planes = Vec::new();
        for c in 0..3usize {
            let mut plane = vec![0i32; width * height];
            for y in 0..height {
                for x in 0..width {
                    let value = ((x * (c + 1) + y * 3 + c * 17) % 256) as i32;
                    plane[y * width + x] = value - 128;
                }
            }
            planes.push(plane);
        }
        let mut components =
            build_components(&planes, width as i32, height as i32, 8, false, 5).unwrap();
        let opts = options(&components, true);
        let encoded = encode(&mut components, &opts).unwrap();
        let decoded = decode(&encoded, None).unwrap();
        assert_eq!(decoded.len(), 3);
        for (component, plane) in decoded.iter().zip(planes.iter()) {
            assert_eq!(&component.samples, plane);
        }
    }

    #[test]
    fn irreversible_codestream_round_trips_within_tolerance() {
        let width = 64usize;
        let height = 64usize;
        let mut plane = vec![0i32; width * height];
        for y in 0..height {
            for x in 0..width {
                plane[y * width + x] = (((x * 3 + y * 5) % 256) as i32) - 128;
            }
        }
        let mut components =
            build_components(&[plane.clone()], width as i32, height as i32, 8, false, 5).unwrap();
        let opts = options(&components, false);
        let encoded = encode(&mut components, &opts).unwrap();
        let decoded = decode(&encoded, None).unwrap();
        let actual = &decoded[0].samples;
        assert_eq!(actual.len(), plane.len());
        let total: i64 = plane
            .iter()
            .zip(actual)
            .map(|(a, b)| i64::from((a - b).abs()))
            .sum();
        let mean = total as f64 / plane.len() as f64;
        assert!(mean < 2.0, "mean absolute error {mean}");
    }

    #[test]
    fn tlm_marker_lists_the_tile_part_length() {
        let mut components = build_components(&[vec![7i32; 64]], 8, 8, 8, false, 1).unwrap();
        let opts = EncodeOptions {
            insert_tlm: true,
            ..options(&components, true)
        };
        let encoded = encode(&mut components, &opts).unwrap();
        let position = encoded
            .windows(2)
            .position(|pair| pair == [0xFF, 0x55])
            .expect("TLM marker");
        assert_eq!(
            &encoded[position + 2..position + 6],
            &[0x00, 0x08, 0x01, 0x40]
        );
        let sot = encoded
            .windows(2)
            .position(|pair| pair == [0xFF, 0x90])
            .expect("SOT marker");
        let target = u32::from_be_bytes([
            encoded[position + 6],
            encoded[position + 7],
            encoded[position + 8],
            encoded[position + 9],
        ]);
        let length = u32::from_be_bytes([
            encoded[sot + 6],
            encoded[sot + 7],
            encoded[sot + 8],
            encoded[sot + 9],
        ]);
        assert_eq!(target, length);
        assert_eq!(decode(&encoded, None).unwrap()[0].samples, vec![7i32; 64]);
    }

    #[test]
    fn multi_layer_codestreams_are_rejected() {
        let mut components = build_components(&[vec![3i32; 16]], 4, 4, 8, false, 0).unwrap();
        let opts = options(&components, true);
        let mut encoded = encode(&mut components, &opts).unwrap();
        let cod = encoded
            .windows(2)
            .position(|pair| pair == [0xFF, 0x52])
            .expect("COD marker");
        encoded[cod + 7] = 2;
        let message = decode(&encoded, None).unwrap_err();
        assert!(message.contains("quality layers"), "{message}");
    }

    #[test]
    fn quality_level_changes_the_lossy_error() {
        let width = 32usize;
        let height = 32usize;
        let mut plane = vec![0i32; width * height];
        for y in 0..height {
            for x in 0..width {
                plane[y * width + x] = (((x * 7 + y * 13) % 256) as i32) - 128;
            }
        }
        let error_for = |quality: u32| {
            let mut components =
                build_components(&[plane.clone()], width as i32, height as i32, 8, false, 3)
                    .unwrap();
            let opts = EncodeOptions {
                quality,
                ..options(&components, false)
            };
            let encoded = encode(&mut components, &opts).unwrap();
            let decoded = decode(&encoded, None).unwrap();
            let total: i64 = plane
                .iter()
                .zip(&decoded[0].samples)
                .map(|(a, b)| i64::from((a - b).abs()))
                .sum();
            total as f64 / plane.len() as f64
        };
        let coarse = error_for(20);
        let fine = error_for(95);
        assert!(coarse > fine, "coarse {coarse} fine {fine}");
        assert!(fine < 2.0, "fine quality error {fine}");
    }

    fn cod_position(stream: &[u8]) -> usize {
        stream
            .windows(2)
            .position(|pair| pair == [0xFF, 0x52])
            .expect("COD marker")
    }

    fn cap_ccap_of(stream: &[u8]) -> u16 {
        let cap = stream
            .windows(2)
            .position(|pair| pair == [0xFF, 0x50])
            .expect("CAP marker");
        u16::from_be_bytes([stream[cap + 8], stream[cap + 9]])
    }

    #[test]
    fn cap_ccap_follows_precision_and_transform() {
        let mut eight = build_components(&[vec![0i32; 16]], 4, 4, 8, false, 0).unwrap();
        let opts = options(&eight, true);
        let encoded = encode(&mut eight, &opts).unwrap();
        assert_eq!(cap_ccap_of(&encoded), 0x0002);

        let sixteen_bit: Vec<i32> = (0..16).map(|i| i * 1000 - 8000).collect();
        let mut sixteen =
            build_components(std::slice::from_ref(&sixteen_bit), 4, 4, 16, false, 0).unwrap();
        let opts = options(&sixteen, true);
        let encoded = encode(&mut sixteen, &opts).unwrap();
        assert_eq!(cap_ccap_of(&encoded), 0x000A);

        let mut lossy = build_components(&[sixteen_bit], 4, 4, 16, false, 0).unwrap();
        let opts = options(&lossy, false);
        let encoded = encode(&mut lossy, &opts).unwrap();
        assert_eq!(cap_ccap_of(&encoded), 0x002A);
    }

    #[test]
    fn pcrl_and_cprl_are_rejected() {
        let mut components = build_components(&[vec![0i32; 16]], 4, 4, 8, false, 0).unwrap();
        for progression in [3u8, 4] {
            let opts = EncodeOptions {
                progression,
                ..options(&components, true)
            };
            let error = encode(&mut components, &opts).unwrap_err();
            assert!(error.contains("PCRL"), "{error}");
        }

        let mut components = build_components(&[vec![0i32; 16]], 4, 4, 8, false, 0).unwrap();
        let opts = options(&components, true);
        let mut encoded = encode(&mut components, &opts).unwrap();
        let cod = cod_position(&encoded);
        encoded[cod + 5] = 3;
        let error = decode(&encoded, None).unwrap_err();
        assert!(error.contains("PCRL"), "{error}");
    }

    #[test]
    fn multi_precinct_codestreams_are_rejected() {
        let mut components = build_components(&[vec![0i32; 16]], 4, 4, 8, false, 0).unwrap();
        let opts = options(&components, true);
        let encoded = encode(&mut components, &opts).unwrap();
        let (header, offset) = MainHeader::parse(&encoded).unwrap();
        let mut cod = header.cod;
        cod.scod |= 1;
        cod.style.precincts = vec![(0, 0)];
        let mut rebuilt = Vec::new();
        write_soc(&mut rebuilt);
        header.siz.write(&mut rebuilt);
        cod.write(&mut rebuilt);
        header.qcd.write(&mut rebuilt);
        rebuilt.extend_from_slice(&encoded[offset..]);
        let error = decode(&rebuilt, None).unwrap_err();
        assert!(error.contains("precinct"), "{error}");
    }

    #[test]
    fn unsupported_code_block_style_bits_are_rejected() {
        let mut components = build_components(&[vec![0i32; 16]], 4, 4, 8, false, 0).unwrap();
        let opts = options(&components, true);
        let encoded = encode(&mut components, &opts).unwrap();
        let (header, offset) = MainHeader::parse(&encoded).unwrap();
        let mut cod = header.cod;
        cod.style.style |= 0x80;
        let mut rebuilt = Vec::new();
        write_soc(&mut rebuilt);
        header.siz.write(&mut rebuilt);
        cod.write(&mut rebuilt);
        header.qcd.write(&mut rebuilt);
        rebuilt.extend_from_slice(&encoded[offset..]);
        let error = decode(&rebuilt, None).unwrap_err();
        assert!(error.contains("unsupported HTJ2K"), "{error}");
    }
}
