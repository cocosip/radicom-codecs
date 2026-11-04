//! OpenJPH-compatible irreversible HTJ2K quantization helpers.

const GAIN97_L: [f32; 34] = [
    1.0000e+00, 1.4021e+00, 2.0304e+00, 2.9012e+00, 4.1153e+00, 5.8245e+00, 8.2388e+00, 1.1652e+01,
    1.6479e+01, 2.3304e+01, 3.2957e+01, 4.6609e+01, 6.5915e+01, 9.3217e+01, 1.3183e+02, 1.8643e+02,
    2.6366e+02, 3.7287e+02, 5.2732e+02, 7.4574e+02, 1.0546e+03, 1.4915e+03, 2.1093e+03, 2.9830e+03,
    4.2185e+03, 5.9659e+03, 8.4371e+03, 1.1932e+04, 1.6874e+04, 2.3864e+04, 3.3748e+04, 4.7727e+04,
    6.7496e+04, 9.5454e+04,
];

const GAIN97_H: [f32; 34] = [
    1.4425e+00, 1.9669e+00, 2.8839e+00, 4.1475e+00, 5.8946e+00, 8.3472e+00, 1.1809e+01, 1.6701e+01,
    2.3620e+01, 3.3403e+01, 4.7240e+01, 6.6807e+01, 9.4479e+01, 1.3361e+02, 1.8896e+02, 2.6723e+02,
    3.7792e+02, 5.3446e+02, 7.5583e+02, 1.0689e+03, 1.5117e+03, 2.1378e+03, 3.0233e+03, 4.2756e+03,
    6.0467e+03, 8.5513e+03, 1.2093e+04, 1.7103e+04, 2.4187e+04, 3.4205e+04, 4.8373e+04, 6.8410e+04,
    9.6747e+04, 1.3682e+05,
];

fn gain_l(decompositions: usize) -> f32 {
    GAIN97_L[decompositions.min(GAIN97_L.len() - 1)]
}

fn gain_h(decompositions: usize) -> f32 {
    GAIN97_H[decompositions.min(GAIN97_H.len() - 1)]
}

fn orientation_gain(orientation: u8) -> f64 {
    match orientation {
        3 => 4.0,
        0 => 1.0,
        _ => 2.0,
    }
}

fn encode_openjph_step(delta: f32) -> u16 {
    let mut exponent = 0u32;
    let mut delta = delta;
    while delta < 1.0 {
        exponent += 1;
        delta *= 2.0;
    }
    if exponent > 29 {
        exponent = 29;
        delta = 1.0;
    }
    let mut mantissa = (delta * 2048.0).round() as i32 - 2048;
    if mantissa >= 2048 {
        mantissa = 0x7FF;
    } else if mantissa < 0 {
        mantissa = 0;
    }
    ((exponent << 11) | mantissa as u32) as u16
}

/// Builds the OpenJPH scalar-expounded step table for the irreversible path.
pub(crate) fn create_openjph_steps(
    decomposition_levels: u8,
    bit_depth: u8,
    base_delta: f32,
) -> Vec<u16> {
    let mut base_delta = base_delta;
    if base_delta <= 0.0 {
        // OpenJPH caps the reference delta at 16 bits (`ojph_min(16, bit_depth)`).
        base_delta = 1.0 / (1u32 << bit_depth.min(16)) as f32;
    }
    let levels = usize::from(decomposition_levels);
    let mut steps = Vec::with_capacity(3 * levels + 1);
    let gain = gain_l(levels);
    steps.push(encode_openjph_step(base_delta / (gain * gain)));
    for d in (1..=levels).rev() {
        let gain_low = gain_l(d);
        let gain_high = gain_h(d - 1);
        let lh = encode_openjph_step(base_delta / (gain_low * gain_high));
        steps.push(lh);
        steps.push(lh);
        steps.push(encode_openjph_step(base_delta / (gain_high * gain_high)));
    }
    steps
}

pub(crate) fn kmax(encoded_step: u16, guard_bits: u8) -> i32 {
    i32::from((encoded_step >> 11) & 0x1F) - 1 + i32::from(guard_bits)
}

/// Base step size for an explicit 1..=100 quality level.
fn quality_scale(quality: u32) -> f64 {
    let quality = f64::from(quality.clamp(1, 100));
    (2f64.powf((100.0 - quality) / 12.5)).max(0.01) * 0.18
}

/// Builds the step table for an explicit quality level.
pub(crate) fn create_quality_steps(
    decomposition_levels: u8,
    bit_depth: u8,
    quality: u32,
) -> Vec<u16> {
    use crate::quantization::{dwt_norm_97, encode_step_size};

    let scale = quality_scale(quality);
    let levels = usize::from(decomposition_levels);
    let mut steps = Vec::with_capacity(3 * levels + 1);
    for index in 0..=3 * levels {
        let resolution = if index == 0 { 0 } else { (index - 1) / 3 + 1 };
        let orientation = if index == 0 {
            0
        } else {
            ((index - 1) % 3 + 1) as u8
        };
        let level = (levels as i32 - resolution as i32).max(0);
        let norm = dwt_norm_97(level, orientation);
        let step = if norm <= 0.0 { scale } else { scale / norm };
        steps.push(encode_step_size(step, bit_depth));
    }
    steps
}

/// OpenJPH reversible `SPqcd` exponents, in QCD subband order, plus the guard
/// bit count that OpenJPH derives from the magnitude bound.
///
/// OpenJPH's `param_qcd::set_rev_quant` starts from `B = bit_depth` (plus one
/// bit when the reversible colour transform is employed) and adds the integer
/// part of the 5/3 bibo gain, then subtracts the guard bits:
/// - LL: `B + 2` (for any decomposition level above zero);
/// - HL/LH/HH at the highest decomposition: `B + 3`, except at the coarsest
///   level (`d == 1`) where both gains round to `B + 2`.
pub(crate) fn openjph_reversible_exponents(
    decomposition_levels: u8,
    bit_depth: u8,
    color_transform: bool,
) -> (Vec<u8>, u8) {
    let levels = u32::from(decomposition_levels);
    let b = u32::from(bit_depth) + u32::from(color_transform);
    let mut raw = Vec::with_capacity(3 * levels as usize + 1);
    // With no decomposition there is no bibo gain, but the LL band must still
    // hold the full sample range (the 5/3 samples are level shifted), so keep
    // one bit-plane of headroom instead of OpenJPH's degenerate `B`.
    raw.push(b + if levels > 0 { 2 } else { 1 });
    for d in (1..=levels).rev() {
        let x = if d == 1 { 2 } else { 3 };
        raw.extend_from_slice(&[b + x, b + x, b + x]);
    }
    let max = raw.iter().copied().max().unwrap_or(0);
    let guard = (max as i32 - 31).max(1) as u8;
    let exponents = raw
        .iter()
        .map(|&value| (value - u32::from(guard)) as u8)
        .collect();
    (exponents, guard)
}

/// CAP `Ccap` derived from the quantization table, as OpenJPH's
/// `param_cap::check_validity` computes it from `param_qcd::get_MAGB`.
pub(crate) fn cap_ccap(
    encoded_steps: &[u16],
    decomposition_levels: u8,
    reversible: bool,
    guard_bits: u8,
) -> u16 {
    let levels = i32::from(decomposition_levels);
    let mut bound = 0u32;
    for (index, &step) in encoded_steps.iter().enumerate() {
        let exponent = u32::from(step >> 11);
        let value = if reversible {
            exponent + u32::from(guard_bits) - 1
        } else {
            let index = index as i32;
            let resolution = if index > 0 { (index - 1) / 3 } else { 0 };
            let nb = (levels - resolution).max(0) as u32;
            exponent + u32::from(guard_bits) - nb
        };
        bound = bound.max(value);
    }
    let bucket = if bound <= 8 {
        0
    } else if bound < 28 {
        bound - 8
    } else {
        13 + (bound >> 2)
    };
    let mut ccap = (bucket as u16) & 0x1F;
    if !reversible {
        ccap |= 0x20;
    }
    ccap
}

/// OpenJPH's irreversible `delta_inv`, computed exactly as `ojph_subband`
/// does: the QCD mantissa in single precision, divided by `2^exponent` and by
/// `2^(31 - Kmax)`, then inverted in single precision.
pub(crate) fn irrev_delta_inv_f32(encoded_step: u16, orientation: u8, k_max: i32) -> f32 {
    let exponent = u32::from((encoded_step >> 11) & 0x1F);
    let gain = match orientation {
        3 => 4.0f32,
        0 => 1.0f32,
        _ => 2.0f32,
    };
    let mantissa = ((u32::from(encoded_step & 0x7FF) | 0x800) as f32) * gain;
    let delta = mantissa / 2048.0f32 / (1u32 << exponent) as f32 / (1u32 << (31 - k_max)) as f32;
    1.0f32 / delta
}

/// Quantizes float coefficients to OpenJPH's sign-magnitude integers.
///
/// fo-dicom builds OpenJPH with its SSE2/AVX2 codestream kernels, and those
/// quantize with `_mm_cvtps_epi32` (round to nearest, ties to even) rather than
/// the scalar path's truncation. The references therefore carry the rounded
/// values, so rounding is part of byte-exact interoperability.
pub(crate) fn to_sign_magnitude_f32(coefficients: &[f32], delta_inv: f32) -> Vec<i32> {
    coefficients
        .iter()
        .map(|&value| {
            let t = (value * delta_inv).round_ties_even() as i32;
            let magnitude = t.unsigned_abs();
            if t < 0 {
                (0x8000_0000u32 | magnitude) as i32
            } else {
                magnitude as i32
            }
        })
        .collect()
}

/// Decodes the coefficient-domain delta for one subband and code-block.
pub(crate) fn decode_delta(encoded_step: u16, orientation: u8, k_max: i32) -> f64 {
    let exponent = i32::from((encoded_step >> 11) & 0x1F);
    let mantissa = f64::from((encoded_step & 0x7FF) | 0x800) * orientation_gain(orientation);
    let mantissa = mantissa / 2048.0 / (1u32 << exponent) as f64;
    mantissa / 2f64.powi(31 - k_max)
}

pub(crate) fn from_sign_magnitude(coefficients: &[i32], delta: f64) -> Vec<f64> {
    coefficients
        .iter()
        .map(|&value| {
            let bits = value as u32;
            let magnitude = f64::from(bits & 0x7FFF_FFFF) * delta;
            if bits & 0x8000_0000 != 0 {
                -magnitude
            } else {
                magnitude
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_table_has_one_entry_per_subband() {
        let steps = create_openjph_steps(5, 8, -1.0);
        assert_eq!(steps.len(), 16);
        assert!(steps.iter().all(|&step| step != 0));
    }

    #[test]
    fn reversible_exponents_match_the_fo_dicom_payloads() {
        // Expected payloads are the QCD segments of the native reference files
        // (SPqcd bytes = exponent << 3), for 5 decomposition levels.
        let cases: [(u8, bool, [u8; 16]); 3] = [
            (
                16,
                false,
                [
                    0x88, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
                    0x88, 0x88, 0x88,
                ],
            ),
            (
                8,
                false,
                [
                    0x48, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50,
                    0x48, 0x48, 0x48,
                ],
            ),
            (
                8,
                true,
                [
                    0x50, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58, 0x58,
                    0x50, 0x50, 0x50,
                ],
            ),
        ];
        for (bits, color, expected) in cases {
            let (exponents, guard) = openjph_reversible_exponents(5, bits, color);
            assert_eq!(guard, 1);
            let bytes: Vec<u8> = exponents.iter().map(|&e| e << 3).collect();
            assert_eq!(bytes, expected, "bits {bits} color {color}");
            let encoded: Vec<u16> = exponents.iter().map(|&e| u16::from(e) << 11).collect();
            let ccap = cap_ccap(&encoded, 5, true, guard);
            let expected_ccap = if color {
                0x0003
            } else if bits == 16 {
                0x000A
            } else {
                0x0002
            };
            assert_eq!(ccap, expected_ccap, "bits {bits} color {color}");
        }
    }

    #[test]
    fn irreversible_cap_matches_the_fo_dicom_payloads() {
        // 16-bit mono (1.dcm) and 8-bit RGB (7.dcm) lossy references.
        let mono = create_openjph_steps(5, 16, -1.0);
        assert_eq!(cap_ccap(&mono, 5, false, 1), 0x002A);
        let color = create_openjph_steps(5, 8, -1.0);
        assert_eq!(cap_ccap(&color, 5, false, 1), 0x0022);
    }

    #[test]
    fn quantization_rounds_to_nearest_even_like_the_simd_kernels() {
        // OpenJPH's SSE2/AVX2 kernels use _mm_cvtps_epi32, so .5 ties round
        // to the even integer instead of truncating.
        let coefficients = [0.625f32, 0.875, -0.625, -0.875];
        let sign_magnitude = to_sign_magnitude_f32(&coefficients, 4.0);
        assert_eq!(sign_magnitude[0], 2);
        assert_eq!(sign_magnitude[1], 4);
        assert_eq!(sign_magnitude[2], (0x8000_0000u32 | 2) as i32);
        assert_eq!(sign_magnitude[3], (0x8000_0000u32 | 4) as i32);
    }

    #[test]
    fn quality_steps_match_the_reference_tables() {
        // Expected values come from the fo-dicom reference implementation.
        let expected = [
            (
                (5u8, 8u8, 90u32),
                vec![
                    31072, 31040, 31040, 31008, 29024, 29024, 29008, 27032, 27032, 27048, 25100,
                    25100, 25184, 23018, 23018, 22948,
                ],
            ),
            (
                (5, 8, 75),
                vec![
                    29408, 29376, 29376, 29328, 27344, 27344, 27320, 25348, 25348, 25368, 23436,
                    23436, 23532, 21349, 21349, 21267,
                ],
            ),
            (
                (3, 16, 80),
                vec![
                    41036, 41052, 41052, 41064, 39104, 39104, 39176, 37026, 37026, 36965,
                ],
            ),
        ];
        for ((levels, bits, quality), steps) in expected {
            assert_eq!(
                create_quality_steps(levels, bits, quality),
                steps,
                "levels {levels} bits {bits} quality {quality}"
            );
        }
    }

    #[test]
    fn higher_quality_uses_finer_steps() {
        let coarse = create_quality_steps(5, 8, 30);
        let fine = create_quality_steps(5, 8, 90);
        assert!(fine[0] > coarse[0]);
    }
}
