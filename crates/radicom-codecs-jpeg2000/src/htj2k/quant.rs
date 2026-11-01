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
        base_delta = 1.0 / (1u32 << bit_depth) as f32;
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

/// Decodes the coefficient-domain delta for one subband and code-block.
pub(crate) fn decode_delta(encoded_step: u16, orientation: u8, k_max: i32) -> f64 {
    let exponent = i32::from((encoded_step >> 11) & 0x1F);
    let mantissa = f64::from((encoded_step & 0x7FF) | 0x800) * orientation_gain(orientation);
    let mantissa = mantissa / 2048.0 / (1u32 << exponent) as f64;
    mantissa / 2f64.powi(31 - k_max)
}

pub(crate) fn to_sign_magnitude(coefficients: &[f64], delta: f64) -> Vec<i32> {
    let delta_inv = if delta > 0.0 { 1.0 / delta } else { 1.0 };
    coefficients
        .iter()
        .map(|&value| {
            let t = truncate_to_i32(value * delta_inv);
            let magnitude = t.unsigned_abs();
            if t < 0 {
                (0x8000_0000u32 | magnitude) as i32
            } else {
                magnitude as i32
            }
        })
        .collect()
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

fn truncate_to_i32(value: f64) -> i32 {
    if value >= f64::from(i32::MAX) {
        i32::MAX
    } else if value <= f64::from(i32::MIN) {
        i32::MIN
    } else if value >= 0.0 {
        value.floor() as i32
    } else {
        value.ceil() as i32
    }
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
    fn sign_magnitude_round_trips_through_delta() {
        let delta = decode_delta(0x7000, 1, 9);
        assert!(delta > 0.0);
        let values = [delta * 100.0, -delta * 300.0, 0.0, delta * 700.0];
        let sign_magnitude = to_sign_magnitude(&values, delta);
        let restored = from_sign_magnitude(&sign_magnitude, delta);
        for (a, b) in values.iter().zip(&restored) {
            assert!((a - b).abs() <= delta * 1.1);
        }
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
