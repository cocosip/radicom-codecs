//! JPEG 2000 codestream assembly and parsing (ISO/IEC 15444-1 Annex A).

use crate::geometry::{CodeBlock, CodeBlockSegment, Component};
use crate::marker::{
    write_eoc, write_soc, write_sod, Cod, CodingStyle, MainHeader, QStep, Quantization, Siz,
    SizComp, Sot, COM, EOC, SOD, SOT,
};
use crate::quantization::{
    band_stepsize, bit_plane_depth, decode_step_size, dwt_norm_53, dwt_norm_97, encode_step_size,
    gain_bits, irreversible_steps, reversible_steps, subband_index, MCT_NORMS_ICT, MCT_NORMS_RCT,
};
use crate::tier1::{self, PassWeight};
use crate::{tier2, wavelet};

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
    encode_with_precincts(components, reversible, None, mct, &[0.0])
}

/// Encodes one tile, optionally with explicit precinct exponents per
/// resolution, using OpenJPEG-compatible quality-layer rate allocation.
///
/// `rates` are the per-layer compression ratios OpenJPEG receives in
/// `tcp_rates`; a final `0.0` means the last layer carries every remaining
/// coding pass (lossless for the reversible transform). An empty slice is
/// treated as a single lossless layer.
pub(crate) fn encode_with_precincts(
    components: &mut [Component],
    reversible: bool,
    precincts: Option<&[(u8, u8)]>,
    mct: bool,
    rates: &[f64],
) -> Result<Vec<u8>, String> {
    if components.is_empty() {
        return Err("no components to encode".into());
    }
    let levels = components[0].levels;
    if !(0..=32).contains(&levels) {
        return Err("invalid decomposition level count".into());
    }
    let precision = components[0].precision;
    if components
        .iter()
        .any(|component| component.levels != levels)
    {
        return Err("components disagree on the decomposition level count".into());
    }
    let mct = mct && components.len() >= 3;
    if mct && reversible {
        forward_rct(components);
    }
    let mut irreversible_planes: Option<Vec<Vec<f32>>> = None;
    if mct && !reversible {
        let mut planes: Vec<Vec<f32>> = components
            .iter()
            .map(|component| component.coefficients.iter().map(|&v| v as f32).collect())
            .collect();
        forward_ict_planes_f32(&mut planes);
        irreversible_planes = Some(planes);
    }

    // Reversible coding keeps integer coefficients (already level shifted and
    // transformed); irreversible coding carries the OpenJPEG f32 9/7 result.
    let mut float_planes: Vec<Vec<f32>> = Vec::with_capacity(components.len());
    for (index, component) in components.iter_mut().enumerate() {
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
            float_planes.push(Vec::new());
        } else {
            let mut plane: Vec<f32> = match &irreversible_planes {
                Some(planes) => planes[index].clone(),
                None => component.coefficients.iter().map(|&v| v as f32).collect(),
            };
            forward_97_f32(
                &mut plane,
                component.width as usize,
                component.height as usize,
                levels as u8,
            )?;
            float_planes.push(plane);
        }
    }

    let quantization = if reversible {
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
        // OpenJPEG's default irreversible step sizes: `2^gain / norm`.
        let steps = irreversible_steps(levels as u8, 1.0);
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
    let encoded_steps: Vec<u16> = quantization
        .steps
        .iter()
        .map(|step| (u16::from(step.exponent) << 11) | (step.mantissa & 0x07FF))
        .collect();

    let layer_rates: Vec<f64> = if rates.is_empty() {
        vec![0.0]
    } else {
        rates.to_vec()
    };
    let layer_count =
        u16::try_from(layer_rates.len()).map_err(|_| "too many quality layers".to_string())?;

    for (component_index, component) in components.iter_mut().enumerate() {
        let plane_width = component.width as usize;
        let reversible_plane = std::mem::take(&mut component.coefficients);
        let float_plane = std::mem::take(&mut float_planes[component_index]);
        let mct_norm = if mct {
            let norms = if reversible {
                MCT_NORMS_RCT
            } else {
                MCT_NORMS_ICT
            };
            norms[component_index.min(2)]
        } else {
            1.0
        };
        for resolution in 0..=levels {
            let precinct_list = component.precincts_including_empty(resolution);
            for precinct in precinct_list {
                let bands = component
                    .bands_mut(resolution, precinct)
                    .ok_or_else(|| "missing precinct geometry".to_string())?;
                for band in bands.iter_mut() {
                    let index = subband_index(resolution, band.orientation).max(0) as usize;
                    let encoded = encoded_steps.get(index).copied().unwrap_or(0);
                    let subband_bits = bit_plane_depth(encoded, GUARD_BITS);
                    let orientation = band.orientation;
                    let gain = gain_bits(orientation);
                    let level = levels - resolution;
                    let log2_gain = match orientation {
                        0 => 0,
                        3 => 2,
                        _ => 1,
                    };
                    let band_step = if reversible {
                        1.0f32
                    } else {
                        band_stepsize(encoded, precision, gain)
                    };
                    let weight = if reversible {
                        PassWeight {
                            w1: mct_norm,
                            w2: dwt_norm_53(level, orientation),
                            stepsize: 1.0,
                        }
                    } else {
                        PassWeight {
                            w1: mct_norm,
                            w2: dwt_norm_97(level, orientation),
                            stepsize: f64::from(band_step) / f64::from(1u32 << log2_gain),
                        }
                    };
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
                            for y in 0..height {
                                for x in 0..width {
                                    let sx = block.x0 as usize + x;
                                    let sy = block.y0 as usize + y;
                                    // OpenJPEG: lrintf((coef / band->stepsize) * 64).
                                    coefficients[y * width + x] =
                                        (float_plane[sy * plane_width + sx] / band_step * 64.0f32)
                                            .round_ties_even()
                                            as i32;
                                }
                            }
                        }
                        let encoded_block = tier1::encode_code_block(
                            &coefficients,
                            width,
                            height,
                            orientation,
                            0,
                            &weight,
                        )?;
                        let bit_count = (encoded_block.max_bit_plane - 5).max(0);
                        block.zero_bitplanes = (subband_bits - bit_count).clamp(0, 31) as u8;
                        block.total_passes = encoded_block.pass_count;
                        block.layer_passes = vec![0u16; layer_count as usize];
                        block.passes = encoded_block.passes;
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
        component.irreversible = None;
    }

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
        layers: layer_count,
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
    quantization.write(&mut output);
    write_comment(&mut output);
    let header_len = output.len();

    let targets = rate_byte_targets(
        &layer_rates,
        header_len,
        width,
        height,
        components.len(),
        precision,
    );
    allocate_layers(components, &targets)?;

    let (tile_data, _, _) =
        tier2::encode_packets(components, layer_count, 0, tier2::PacketMode::Final)?;
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

/// Writes the `Created by OpenJPEG version 2.5.4` comment marker that OpenJPEG
/// adds by default (Rcom = 1).
fn write_comment(out: &mut Vec<u8>) {
    const TEXT: &[u8; 33] = b"Created by OpenJPEG version 2.5.4";
    out.extend_from_slice(&COM.to_be_bytes());
    out.extend_from_slice(&(4 + TEXT.len() as u16).to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(TEXT);
}

/// Converts OpenJPEG's `tcp_rates` for one tile into byte budgets, replicating
/// `opj_j2k_update_rates()` (single tile, no tile-parts, no cinema profile).
///
/// The arithmetic is performed in `f32` exactly like the reference: the
/// bisection thresholds in the rate allocator are sensitive to the last bit.
fn rate_byte_targets(
    rates: &[f64],
    header_len: usize,
    width: i32,
    height: i32,
    components: usize,
    precision: u8,
) -> Vec<f64> {
    if rates.is_empty() {
        return Vec::new();
    }
    let size_pixel = components as u32 * u32::from(precision);
    let numerator = f64::from(size_pixel) * f64::from(width) * f64::from(height);
    let bits_empty = 8.0f32;
    let sot_remove = header_len as f32;
    let mut values: Vec<f32> = rates
        .iter()
        .map(|&rate| {
            if rate > 0.0 {
                let denominator = (rate as f32) * bits_empty;
                (numerator / f64::from(denominator)) as f32
            } else {
                0.0
            }
        })
        .collect();
    let last = values.len() - 1;
    if values[0] > 0.0 {
        values[0] -= sot_remove;
        if values[0] < 30.0 {
            values[0] = 30.0;
        }
    }
    for index in 1..last {
        if values[index] > 0.0 {
            values[index] -= sot_remove;
            if values[index] < values[index - 1] + 10.0 {
                values[index] = values[index - 1] + 20.0;
            }
        }
    }
    // A single layer is only adjusted by the first-layer rule above; OpenJPEG
    // never reaches the last-layer adjustment for it.
    if last > 0 && values[last] > 0.0 {
        values[last] -= sot_remove + 2.0;
        if values[last] < values[last - 1] + 10.0 {
            values[last] = values[last - 1] + 20.0;
        }
    }
    values.into_iter().map(f64::from).collect()
}

fn for_each_code_block_mut(components: &mut [Component], mut f: impl FnMut(&mut CodeBlock)) {
    for component in components.iter_mut() {
        for resolution in 0..=component.levels {
            let precincts = component.precincts_including_empty(resolution);
            for precinct in precincts {
                if let Some(bands) = component.bands_mut(resolution, precinct) {
                    for band in bands.iter_mut() {
                        for block in band.code_blocks.iter_mut() {
                            f(block);
                        }
                    }
                }
            }
        }
    }
}

fn passes_before(block: &CodeBlock, layer: usize) -> u32 {
    block
        .layer_passes
        .iter()
        .take(layer)
        .map(|&passes| u32::from(passes))
        .sum()
}

/// OpenJPEG's `opj_tcd_makelayer`: assigns pass truncation points for one layer
/// at the given rate/distortion threshold and reports whether the allocation is
/// unchanged from the previous call for this layer.
///
/// The comparison below is OpenJPEG's `thresh - dd/dr < DBL_EPSILON` verbatim;
/// adding `abs()` (as clippy suggests) would change which passes each layer
/// keeps.
#[allow(clippy::float_equality_without_abs)]
fn make_layer(components: &mut [Component], layer: usize, threshold: f64) -> bool {
    let mut allocation_is_same = true;
    for_each_code_block_mut(components, |block| {
        if layer >= block.layer_passes.len() {
            return;
        }
        let start = passes_before(block, layer);
        let total = block.passes.len() as u32;
        let end = if threshold < 0.0 {
            total
        } else {
            let mut end = start;
            for passno in start..total {
                let pass = &block.passes[passno as usize];
                let (dr, dd) = if end == 0 {
                    (pass.rate, pass.distortiondec)
                } else {
                    let previous = &block.passes[(end - 1) as usize];
                    (
                        pass.rate.wrapping_sub(previous.rate),
                        pass.distortiondec - previous.distortiondec,
                    )
                };
                let dr = dr as i32;
                if dr == 0 {
                    if dd != 0.0 {
                        end = passno + 1;
                    }
                    continue;
                }
                if threshold - dd / f64::from(dr) < f64::EPSILON {
                    end = passno + 1;
                }
            }
            end
        };
        let count = (end - start) as u16;
        if block.layer_passes[layer] != count {
            allocation_is_same = false;
            block.layer_passes[layer] = count;
        }
    });
    allocation_is_same
}

/// Global minimum/maximum pass slope, exactly as `opj_tcd_rateallocate` derives
/// its bisection brackets.
fn slope_bounds(components: &[Component]) -> (f64, f64) {
    let mut min = f64::MAX;
    let mut max = 0.0f64;
    for component in components {
        for resolution in 0..=component.levels {
            for precinct in component.precincts_including_empty(resolution) {
                for band in component.bands(resolution, precinct) {
                    for block in &band.code_blocks {
                        for (passno, pass) in block.passes.iter().enumerate() {
                            let (dr, dd) = if passno == 0 {
                                (pass.rate, pass.distortiondec)
                            } else {
                                let previous = &block.passes[passno - 1];
                                (
                                    pass.rate - previous.rate,
                                    pass.distortiondec - previous.distortiondec,
                                )
                            };
                            if dr == 0 {
                                continue;
                            }
                            let slope = dd / f64::from(dr as i32);
                            if slope < min {
                                min = slope;
                            }
                            if slope > max {
                                max = slope;
                            }
                        }
                    }
                }
            }
        }
    }
    (min, max)
}

/// Runs OpenJPEG's rate/distortion layer allocation over the encoded code
/// blocks, filling `block.layer_passes`.
fn allocate_layers(components: &mut [Component], targets: &[f64]) -> Result<(), String> {
    let layer_count = targets.len();
    for_each_code_block_mut(components, |block| {
        block.layer_passes = vec![0u16; layer_count];
    });
    let (min, max) = slope_bounds(components);
    for (layer, &target) in targets.iter().enumerate() {
        let good_threshold = if target > 0.0 {
            let max_len = target.ceil() as u32 as usize;
            let mut lo = min;
            let mut hi = max;
            let mut threshold = 0.0f64;
            let mut stable = 0.0f64;
            let mut last_ok = false;
            for iteration in 0..128 {
                let new_threshold = (lo + hi) / 2.0;
                if (new_threshold - threshold).abs() <= 0.5e-5 * threshold {
                    break;
                }
                threshold = new_threshold;
                let same = make_layer(components, layer, threshold) && iteration != 0;
                let fits = if same {
                    true
                } else {
                    tier2::encode_packets(
                        components,
                        (layer + 1) as u16,
                        0,
                        tier2::PacketMode::Measure { max_len },
                    )?
                    .2
                };
                if (same && !last_ok) || (!same && !fits) {
                    last_ok = false;
                    lo = threshold;
                    continue;
                }
                last_ok = true;
                hi = threshold;
                stable = threshold;
            }
            if stable == 0.0 {
                threshold
            } else {
                stable
            }
        } else {
            -1.0
        };
        make_layer(components, layer, good_threshold);
    }
    Ok(())
}

/// OpenJPEG's irreversible 9/7 lifting constants (f32, from Table F.4). They
/// are copied verbatim so the compiler rounds them to the same `f32` as the C
/// build.
#[allow(clippy::excessive_precision)]
const DWT_ALPHA_F32: f32 = -1.586134342;
#[allow(clippy::excessive_precision)]
const DWT_BETA_F32: f32 = -0.052980118;
#[allow(clippy::excessive_precision)]
const DWT_GAMMA_F32: f32 = 0.882911075;
#[allow(clippy::excessive_precision)]
const DWT_DELTA_F32: f32 = 0.443506852;
#[allow(clippy::excessive_precision)]
const DWT_K_F32: f32 = 1.230174105;
const DWT_INV_K_F32: f32 = (1.0f64 / 1.230174105) as f32;

/// OpenJPEG `opj_dwt_encode_step2` on an interleaved f32 line.
fn dwt_step2_f32(
    data: &mut [f32],
    fl: usize,
    mut fw: usize,
    end: usize,
    middle: usize,
    coefficient: f32,
) {
    let imax = end.min(middle);
    if imax > 0 {
        data[fw - 1] += (data[fl] + data[fw]) * coefficient;
        fw += 2;
        for _ in 1..imax {
            data[fw - 1] += (data[fw - 2] + data[fw]) * coefficient;
            fw += 2;
        }
    }
    if middle < end {
        data[fw - 1] += (2.0f32 * data[fw - 2]) * coefficient;
    }
}

fn dwt_step1_combined_f32(data: &mut [f32], iters_c1: usize, iters_c2: usize, c1: f32, c2: f32) {
    let common = iters_c1.min(iters_c2);
    let mut i = 0;
    while i < common {
        data[2 * i] *= c1;
        data[2 * i + 1] *= c2;
        i += 1;
    }
    if i < iters_c1 {
        data[2 * i] *= c1;
    } else if i < iters_c2 {
        data[2 * i + 1] *= c2;
    }
}

/// OpenJPEG `opj_dwt_encode_1_real` on one f32 line.
fn dwt_encode_1d_f32(data: &mut [f32], dn: usize, sn: usize, cas: usize) {
    let (a, b) = if cas == 0 {
        (0usize, 1usize)
    } else {
        (1usize, 0usize)
    };
    let alpha_m = dn.min(sn.saturating_sub(b));
    dwt_step2_f32(data, a, b + 1, dn, alpha_m, DWT_ALPHA_F32);
    let beta_m = sn.min(dn.saturating_sub(a));
    dwt_step2_f32(data, b, a + 1, sn, beta_m, DWT_BETA_F32);
    dwt_step2_f32(data, a, b + 1, dn, alpha_m, DWT_GAMMA_F32);
    dwt_step2_f32(data, b, a + 1, sn, beta_m, DWT_DELTA_F32);
    if a == 0 {
        dwt_step1_combined_f32(data, sn, dn, DWT_INV_K_F32, DWT_K_F32);
    } else {
        dwt_step1_combined_f32(data, dn, sn, DWT_K_F32, DWT_INV_K_F32);
    }
}

/// OpenJPEG's f32 forward 9/7 transform (`opj_dwt_encode_real`). The tile
/// always starts at (0, 0) in this encoder, so every level uses the even
/// (non-inverted) lifting order.
fn forward_97_f32(data: &mut [f32], width: usize, height: usize, levels: u8) -> Result<(), String> {
    if width == 0 || height == 0 || data.len() != width * height {
        return Err("invalid irreversible wavelet plane".into());
    }
    let mut current_width = width;
    let mut current_height = height;
    for _ in 0..levels {
        if current_width <= 1 && current_height <= 1 {
            break;
        }
        let low_width = current_width.div_ceil(2);
        let low_height = current_height.div_ceil(2);
        if current_height > 1 {
            let mut column = vec![0f32; current_height];
            for x in 0..current_width {
                for y in 0..current_height {
                    column[y] = data[y * width + x];
                }
                dwt_encode_1d_f32(&mut column, current_height - low_height, low_height, 0);
                for y in 0..low_height {
                    data[y * width + x] = column[2 * y];
                }
                for y in 0..(current_height - low_height) {
                    data[(low_height + y) * width + x] = column[2 * y + 1];
                }
            }
        }
        if current_width > 1 {
            let mut row = vec![0f32; current_width];
            for y in 0..current_height {
                row.copy_from_slice(&data[y * width..y * width + current_width]);
                dwt_encode_1d_f32(&mut row, current_width - low_width, low_width, 0);
                for x in 0..low_width {
                    data[y * width + x] = row[2 * x];
                }
                for x in 0..(current_width - low_width) {
                    data[y * width + low_width + x] = row[2 * x + 1];
                }
            }
        }
        current_width = low_width;
        current_height = low_height;
    }
    Ok(())
}

/// OpenJPEG's irreversible multiple-component transform, in f32 and with its
/// exact constants (`opj_mct_encode_real`).
fn forward_ict_planes_f32(planes: &mut [Vec<f32>]) {
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
        let (ro, go, bo) = (*r, *g, *b);
        *r = 0.299f32 * ro + 0.587f32 * go + 0.114f32 * bo;
        *g = -0.16875f32 * ro - 0.331260f32 * go + 0.5f32 * bo;
        *b = 0.5f32 * ro - 0.41869f32 * go - 0.08131f32 * bo;
    }
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
            encode_with_precincts(&mut components, true, Some(&precincts), false, &[0.0]).unwrap();
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

    /// Samples for the golden vectors: a 32x32 8-bit ramp, level-shifted the
    /// way the DICOM adapter shifts unsigned data before encoding.
    fn golden_samples() -> Vec<i32> {
        (0..32 * 32)
            .map(|index| (index * 13 + 7) % 256 - 128)
            .collect()
    }

    /// 32x32 8-bit `golden_samples()` encoded by OpenJPEG 2.5.4 through the
    /// fo-dicom.Codecs parameter set (COM marker, 6 resolutions, 64x64 code
    /// blocks, 8 quality layers ending in the lossless layer). Byte equality
    /// pins the rate allocator, packet headers and Tier-1 output together.
    #[test]
    fn lossless_multi_layer_codestream_matches_openjpeg() {
        const EXPECTED: &str = "
            ff4fff510029000000000020000000200000000000000000000000200000002000000000000000000001070101ff52000c00000008000504040001ff5c00134040484850484850484850484850484850ff640025000143726561746564206279204f70656e4a5045472076657273696f6e20322e352e34ff90000a0000000002820001ff93808080a31418256e7c8180808080808080808080808080a5d30013b168c0d8abf756abe19cf5d27449b7aea1a86faead115cb1531481dfe1afb819481a28c3fe80808080808080808080a716221f601829c5becbf6e6e1b0a8214240778c265ed47ffa2cbd71379f2b3954c87f9e80808080c5401e740e38614a45278080808090e180114f14ecde5dc290a2a4e1c6504780c02103c07c20c010c03e1040030704c1f3848074381f384007779bcf0c1b3f04e65bdfc3ea0efac2bea0a02198e719d0b9e1ee97369029aedf041b766b2f791fc58ee7a551dfd220cdd74b48a3fd631fb0da3f0440b57c19881bd895e4dda5ba9801e9f122778e12895f7a2ae40debbb34d4d8e5d888573ddf8bcd867f51f44c5b925a915e4f583fb5226262b1d12784c212d19647c1bdadc4f1ce91d49e158fe70d1f4dd329e789e0d063c92d27f1123122caf7d753a0b5f69e4c2915da5c445bd96ee2b582da0367f51884c30c87e16d197218dfe220c95155be87a5f5e90e3e96c52a82fccebcdba03000192b9300bca3386cde5ef44fd06949aca22ffcd8c7dac1f68438fc0d801f5907a00d978a0d50b2eccc2933c55b06cf62879fc583eff20f36410e15e00b6bd7cbfdaede5f901ba350afd38bd4624f760b192b67c05d9f603edd0c21e0ded6f13eb4fda386f860ccf8bb1e39b901ff4730409f989ca8ec06b278687ddd86fd33497e458db4ba2b20008263876afc3393316f4126958568368595a838bf691671f1eba2925f590b38514beb21670a297d66155ea1782778936f11779907f40737c70f374f945c2f6533b2abb143e2372347e590aa47d6e7a02fb2df70fe2bf282be060f5eb1ee7518d63250376f5bd5ff7fff7fff7fff7fffd9";
        let mut components = build_components(&[golden_samples()], 32, 32, 8, false, 5).unwrap();
        let rates = [1280.0, 640.0, 320.0, 160.0, 80.0, 40.0, 20.0, 0.0];
        let stream = encode_with_precincts(&mut components, true, None, false, &rates).unwrap();
        assert_eq!(stream, hex_bytes(EXPECTED));
    }

    /// The irreversible counterpart of the golden vector, with the layer list
    /// fo-dicom uses for the lossy syntax (`Rate = 16`, no appended zero).
    #[test]
    fn irreversible_multi_layer_codestream_matches_openjpeg() {
        const EXPECTED: &str = "
            ff4fff510029000000000020000000200000000000000000000000200000002000000000000000000001070101ff52000c00000008000504040000ff5c002342772076f076f076c06f006f006ee067506750676850055005504757d357d35762ff640025000143726561746564206279204f70656e4a5045472076657273696f6e20322e352e34ff90000a0000000000b50001ff938080808080a2e98013b168c0d8abf756abe19cf5d27449b7aea1a86faead115cb1531481dfe1afb81650862d5f758080808080808080808080808080808080b0909023f9ea5388ccdad0544a57fc3b01ccbb6380808080a2a40f4ea56d0116076d778080808080c74a8c36a41e23465d575920750b2daf4414a58080808090708012771e1fa0c6587f2f7f58165a0f1f8bf6fde980808080a0480e85feb198c015a780c8689380ffd9";
        let mut components = build_components(&[golden_samples()], 32, 32, 8, false, 5).unwrap();
        let rates = [1280.0, 640.0, 320.0, 160.0, 80.0, 40.0, 20.0, 16.0];
        let stream = encode_with_precincts(&mut components, false, None, false, &rates).unwrap();
        assert_eq!(stream, hex_bytes(EXPECTED));
    }

    /// The lossless last layer must carry every Tier-1 pass, and earlier layers
    /// may only take prefixes of the same pass sequence.
    #[test]
    fn layer_allocations_are_monotonic_and_end_lossless() {
        let samples = vec![pattern(48, 40, 23)];
        let mut components = build_components(&samples, 48, 40, 8, false, 5).unwrap();
        let rates = [1280.0, 640.0, 320.0, 160.0, 80.0, 40.0, 20.0, 0.0];
        let codestream = encode_with_precincts(&mut components, true, None, false, &rates).unwrap();
        // The decoder consumes the same layered packets, so the lossless path
        // must still reconstruct every sample.
        let decoded = decode(&codestream).unwrap();
        assert_eq!(decoded[0].samples, samples[0]);
        let mut saw_passes = false;
        for component in &components {
            for resolution in 0..=component.levels {
                for precinct in component.precincts_including_empty(resolution) {
                    for band in component.bands(resolution, precinct) {
                        for block in &band.code_blocks {
                            let assigned: u32 = block
                                .layer_passes
                                .iter()
                                .map(|&passes| u32::from(passes))
                                .sum();
                            assert_eq!(
                                assigned,
                                block.passes.len() as u32,
                                "lossless layer must include every pass"
                            );
                            saw_passes |= assigned > 0;
                        }
                    }
                }
            }
        }
        assert!(saw_passes);
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
