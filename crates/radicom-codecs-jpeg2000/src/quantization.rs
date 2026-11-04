//! JPEG 2000 quantization parameters and 9/7 subband normalization
//! (ISO/IEC 15444-1 Annex E).

use crate::geometry::floor_log2;

/// Table E-1 DWT normalization factors for the irreversible 9/7 transform.
const DWT_NORMS_97: [[f64; 10]; 4] = [
    [
        1.000, 1.965, 4.177, 8.403, 16.90, 33.84, 67.69, 135.3, 270.6, 540.9,
    ],
    [
        2.022, 3.989, 8.355, 17.04, 34.27, 68.63, 137.3, 274.6, 549.0, 0.0,
    ],
    [
        2.022, 3.989, 8.355, 17.04, 34.27, 68.63, 137.3, 274.6, 549.0, 0.0,
    ],
    [
        2.080, 3.865, 8.307, 17.18, 34.71, 69.59, 139.3, 278.6, 557.2, 0.0,
    ],
];

/// Table E-1 DWT normalization factors for the reversible 5-3 transform
/// (OpenJPEG `opj_dwt_norms`). Used only by the rate allocator's weighted
/// distortion estimate.
const DWT_NORMS_53: [[f64; 10]; 4] = [
    [
        1.000, 1.500, 2.750, 5.375, 10.68, 21.34, 42.67, 85.33, 170.7, 341.3,
    ],
    [
        1.038, 1.592, 2.919, 5.703, 11.33, 22.64, 45.25, 90.48, 180.9, 0.0,
    ],
    [
        1.038, 1.592, 2.919, 5.703, 11.33, 22.64, 45.25, 90.48, 180.9, 0.0,
    ],
    [
        0.7186, 0.9218, 1.586, 3.043, 6.019, 12.01, 24.00, 47.97, 95.93, 0.0,
    ],
];

/// Reversible multiple-component-transform norms (OpenJPEG `opj_mct_norms`).
pub(crate) const MCT_NORMS_RCT: [f64; 3] = [1.732, 0.8292, 0.8292];
/// Irreversible multiple-component-transform norms
/// (OpenJPEG `opj_mct_norms_real`).
pub(crate) const MCT_NORMS_ICT: [f64; 3] = [1.732, 1.805, 1.573];

pub(crate) fn gain_bits(orientation: u8) -> u8 {
    match orientation {
        0 => 0,
        1 | 2 => 1,
        _ => 2,
    }
}

pub(crate) fn subband_index(resolution: i32, orientation: u8) -> i32 {
    if resolution == 0 {
        return if orientation == 0 { 0 } else { -1 };
    }
    if !(1..=3).contains(&orientation) {
        return -1;
    }
    1 + (resolution - 1) * 3 + i32::from(orientation) - 1
}

pub(crate) fn subband_count(levels: u8) -> usize {
    if levels == 0 {
        1
    } else {
        1 + usize::from(levels) * 3
    }
}

/// Default irreversible step sizes: one per subband, `1 / norm`, multiplied by
/// `scale`. `scale == 1` is the near-lossless default; larger scales trade
/// bit-planes for size, which is how the classic encoder implements `rate`.
/// The scale is continuous because the QCD carries an 11-bit mantissa.
pub(crate) fn irreversible_steps(levels: u8, scale: f64) -> Vec<f64> {
    let count = subband_count(levels);
    let scale = scale.max(f64::MIN_POSITIVE);
    let mut steps = Vec::with_capacity(count);
    for index in 0..count {
        let resolution = if index == 0 {
            0
        } else {
            ((index - 1) / 3 + 1) as i32
        };
        let orientation = if index == 0 {
            0
        } else {
            ((index - 1) % 3 + 1) as u8
        };
        let level = i32::from(levels) - resolution;
        let norm = dwt_norm_97(level, orientation);
        steps.push(if norm <= 0.0 { scale } else { scale / norm });
    }
    steps
}

pub(crate) fn dwt_norm_97(level: i32, orientation: u8) -> f64 {
    let orientation = usize::from(orientation.min(3));
    let max_level = if orientation == 0 { 9 } else { 8 };
    let level = level.clamp(0, max_level);
    DWT_NORMS_97[orientation][level as usize]
}

/// Reversible 5-3 band norm, clamped exactly like OpenJPEG's
/// `opj_dwt_getnorm()`.
pub(crate) fn dwt_norm_53(level: i32, orientation: u8) -> f64 {
    let orientation = usize::from(orientation.min(3));
    let max_level = if orientation == 0 { 9 } else { 8 };
    let level = level.clamp(0, max_level);
    DWT_NORMS_53[orientation][level as usize]
}

/// Band step size as OpenJPEG computes it in `opj_tcd_init_tile`: a double
/// expression cast to `OPJ_FLOAT32` (so the encoder's coefficient division and
/// the rate allocator's distortion weights both see the rounded f32 value).
pub(crate) fn band_stepsize(encoded: u16, precision: u8, gain: u8) -> f32 {
    let exponent = (encoded >> 11) & 0x1F;
    let mantissa = encoded & 0x07FF;
    let rb = i32::from(precision) + i32::from(gain);
    ((1.0 + f64::from(mantissa) / 2048.0) * 2f64.powi(rb - i32::from(exponent))) as f32
}

/// Encodes a step size into the 16-bit scalar-expounded representation.
pub(crate) fn encode_step_size(step_size: f64, precision: u8) -> u16 {
    if step_size <= 0.0 {
        return 0;
    }
    let fixed_point = (step_size * 8192.0).floor() as i64;
    let fixed_point = fixed_point.max(1);
    let log2 = floor_log2(fixed_point.min(i64::from(i32::MAX)) as i32);
    let power = log2 - 13;
    let shift = 11 - log2;
    let mantissa = if shift < 0 {
        fixed_point >> -shift
    } else {
        fixed_point << shift
    } as u16
        & 0x07FF;
    let exponent = (i32::from(precision) - power).clamp(0, 0x1F) as u16;
    (exponent << 11) | mantissa
}

/// Decodes the scalar-expounded representation with the given nominal range.
pub(crate) fn decode_step_size(encoded: u16, precision: u8) -> f64 {
    let exponent = (encoded >> 11) & 0x1F;
    let mantissa = encoded & 0x07FF;
    (1.0 + f64::from(mantissa) / 2048.0) * 2f64.powi(i32::from(precision) - i32::from(exponent))
}

pub(crate) fn bit_plane_depth(encoded: u16, guard_bits: u8) -> i32 {
    i32::from((encoded >> 11) & 0x1F) + i32::from(guard_bits) - 1
}

/// Reversible QCD step bytes: exponent = precision + gain, mantissa zero.
pub(crate) fn reversible_steps(precision: u8, levels: u8) -> Vec<u8> {
    let count = subband_count(levels);
    let mut steps = Vec::with_capacity(count);
    for index in 0..count {
        let orientation = if index == 0 {
            0
        } else {
            ((index - 1) % 3 + 1) as u8
        };
        steps.push((precision + gain_bits(orientation)) << 3);
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_size_round_trips_through_the_wire_format() {
        for precision in [8u8, 12, 16] {
            for step in [1.0, 0.5, 0.25, 2.0] {
                let encoded = encode_step_size(step, precision);
                let decoded = decode_step_size(encoded, precision);
                assert!(
                    (decoded - step).abs() <= step * 1e-3,
                    "precision={precision} step={step} encoded={encoded} decoded={decoded}"
                );
            }
        }
    }

    #[test]
    fn reversible_steps_match_the_default_payload() {
        // 8-bit, 5 levels: LL=8, then 9,9,10 repeated.
        let steps = reversible_steps(8, 5);
        assert_eq!(steps[0], 0x40);
        assert_eq!(steps[1], 0x48);
        assert_eq!(steps[2], 0x48);
        assert_eq!(steps[3], 0x50);
        assert_eq!(steps.len(), 16);
    }

    #[test]
    fn subband_indices_are_ordered_by_resolution() {
        assert_eq!(subband_index(0, 0), 0);
        assert_eq!(subband_index(1, 1), 1);
        assert_eq!(subband_index(1, 2), 2);
        assert_eq!(subband_index(1, 3), 3);
        assert_eq!(subband_index(2, 1), 4);
    }
}
