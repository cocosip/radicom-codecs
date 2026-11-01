//! HTJ2K codestream assembly and parsing (ISO/IEC 15444-15).
//!
//! The tile geometry, wavelet transform and DICOM frame adapter are shared
//! with the classic JPEG 2000 codestream; only the packet headers and
//! code-block coder differ.

use crate::geometry::{CodeBlock, Component, PrecinctBand};
use crate::marker::{
    write_cap, write_eoc, write_soc, write_sod, Cod, CodingStyle, MainHeader, QStep, Quantization,
    Siz, SizComp, Sot, EOC, SOD, SOT,
};
use crate::quantization::{bit_plane_depth, reversible_steps, subband_index};
use crate::wavelet;

use super::cleanup::{decode_cleanup, encode_cleanup};
use super::packet::{decode_ht_packet, encode_ht_packet};
use super::quant::{
    create_openjph_steps, decode_delta, from_sign_magnitude, kmax, to_sign_magnitude,
};

const CODE_BLOCK: i32 = 64;
const GUARD_BITS: u8 = 1;

/// Encodes one HTJ2K tile.
pub(crate) fn encode(
    components: &mut [Component],
    progression: u8,
    reversible: bool,
    levels: i32,
    use_mct: bool,
) -> Result<Vec<u8>, String> {
    if components.is_empty() {
        return Err("no components to encode".into());
    }
    if !(0..=32).contains(&levels) {
        return Err("invalid decomposition level count".into());
    }
    let precision = components[0].precision;
    let mct = u8::from(use_mct && components.len() >= 3);
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
        let encoded = create_openjph_steps(levels as u8, precision, -1.0);
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

    let mut output = Vec::new();
    write_soc(&mut output);
    siz.write(&mut output);
    write_cap(&mut output);
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
    if let Some(expected) = expected_progression {
        if header.cod.progression != expected {
            return Err(format!(
                "HTJ2K codestream progression is {} but {} is required",
                header.cod.progression, expected
            ));
        }
    }
    let reversible = header.cod.style.transform == 1;
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
    let levels = i32::from(header.cod.style.decompositions);
    let (code_block_width, code_block_height) = header.cod.style.code_block_size()?;
    let mut components = Vec::with_capacity(header.siz.comps.len());
    for (index, comp) in header.siz.comps.iter().enumerate() {
        if comp.xrsiz != 1 || comp.yrsiz != 1 {
            return Err("subsampled HTJ2K components are not supported".into());
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
            wavelet::inverse_97_openjpeg(
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

    #[test]
    fn reversible_codestream_round_trips() {
        let mut components =
            build_components(&[vec![1, 2, 3, 4, 5, 6, 7, 8, 9]], 3, 3, 8, false, 0).unwrap();
        let levels = components[0].levels;
        let mct = components.len() >= 3;
        let encoded = encode(&mut components, 0, true, levels, mct).unwrap();
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
        let levels = components[0].levels;
        let mct = components.len() >= 3;
        let encoded = encode(&mut components, 0, true, levels, mct).unwrap();
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
        let levels = components[0].levels;
        let mct = components.len() >= 3;
        let encoded = encode(&mut components, 0, false, levels, mct).unwrap();
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
}
