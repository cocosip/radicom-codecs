#![allow(clippy::chunks_exact_to_as_chunks)]

use std::f64::consts::PI;

pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub(crate) const LUMA_Q: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
pub(crate) const CHROMA_Q: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// Largest quantization entry libjpeg allows when it is not forced to keep the
/// tables 8-bit (`jpeg_set_quality(..., force_baseline = FALSE)`), which is what
/// fo-dicom's IJG path always uses.
pub(crate) const MAX_QUANTIZATION: u16 = 32767;

pub(crate) fn scaled_table(base: &[u8; 64], quality: u32, max_qval: u16) -> [u16; 64] {
    let q = quality.clamp(1, 100);
    let scale = if q < 50 { 5000 / q } else { 200 - q * 2 };
    std::array::from_fn(|i| {
        (((u32::from(base[i]) * scale + 50) / 100).clamp(1, u32::from(max_qval))) as u16
    })
}

/// libjpeg's forward DCT for `JDCT_ISLOW` (`jfdctint.c`). The result is scaled
/// up by 8, exactly like libjpeg, so it is paired with [`ForwardQuantizer`].
///
/// This is a direct port of the fixed-point LL&M algorithm: every intermediate
/// is a JLONG (32-bit) and the constants are `jfdctint.c`'s precomputed
/// `FIX_*` values. Rust's `wrapping_*` mirrors the wrap-around MSVC produces
/// for the C signed arithmetic. `precision` selects `PASS1_BITS` (2 for 8-bit
/// samples, 1 for 12-bit) exactly as `jfdctint.c` does.
pub(crate) fn islow_forward(input: &[i32; 64], precision: u8) -> [i32; 64] {
    const FIX_0_298631336: i32 = 2446;
    const FIX_0_390180644: i32 = 3196;
    const FIX_0_541196100: i32 = 4433;
    const FIX_0_765366865: i32 = 6270;
    const FIX_0_899976223: i32 = 7373;
    const FIX_1_175875602: i32 = 9633;
    const FIX_1_501321110: i32 = 12299;
    const FIX_1_847759065: i32 = 15137;
    const FIX_1_961570560: i32 = 16069;
    const FIX_2_053119869: i32 = 16819;
    const FIX_2_562915447: i32 = 20995;
    const FIX_3_072711026: i32 = 25172;
    const CONST_BITS: u32 = 13;

    #[inline]
    fn descale(value: i32, shift: u32) -> i32 {
        // `DESCALE`: add half before the arithmetic right shift (towards minus
        // infinity), then shift.
        value.wrapping_add(1 << (shift - 1)) >> shift
    }

    #[inline]
    fn left_shift(value: i32, shift: u32) -> i32 {
        // `LEFT_SHIFT`: `(JLONG)((unsigned long)(a) << (b))`.
        ((value as u32) << shift) as i32
    }

    let pass1_bits: u32 = if precision == 8 { 2 } else { 1 };

    // One 1-D pass. The even (DC/4) outputs are only left-shifted in pass 1 and
    // descaled in pass 2; the AC outputs are descaled by different shifts.
    let transform = |s: [i32; 8], pass1: bool| -> [i32; 8] {
        let tmp0 = s[0].wrapping_add(s[7]);
        let tmp7 = s[0].wrapping_sub(s[7]);
        let tmp1 = s[1].wrapping_add(s[6]);
        let tmp6 = s[1].wrapping_sub(s[6]);
        let tmp2 = s[2].wrapping_add(s[5]);
        let tmp5 = s[2].wrapping_sub(s[5]);
        let tmp3 = s[3].wrapping_add(s[4]);
        let tmp4 = s[3].wrapping_sub(s[4]);

        let tmp10 = tmp0.wrapping_add(tmp3);
        let tmp13 = tmp0.wrapping_sub(tmp3);
        let tmp11 = tmp1.wrapping_add(tmp2);
        let tmp12 = tmp1.wrapping_sub(tmp2);

        let even = |v: i32| {
            if pass1 {
                left_shift(v, pass1_bits)
            } else {
                descale(v, pass1_bits)
            }
        };
        let odd_shift = if pass1 {
            CONST_BITS - pass1_bits
        } else {
            CONST_BITS + pass1_bits
        };
        let odd = |v: i32| descale(v, odd_shift);

        let z1 = tmp12.wrapping_add(tmp13).wrapping_mul(FIX_0_541196100);
        let even2 = odd(z1.wrapping_add(tmp13.wrapping_mul(FIX_0_765366865)));
        let even6 = odd(z1.wrapping_add(tmp12.wrapping_mul(-FIX_1_847759065)));

        let z1 = tmp4.wrapping_add(tmp7);
        let z2 = tmp5.wrapping_add(tmp6);
        let z3 = tmp4.wrapping_add(tmp6);
        let z4 = tmp5.wrapping_add(tmp7);
        let z5 = z3.wrapping_add(z4).wrapping_mul(FIX_1_175875602);

        let tmp4 = tmp4.wrapping_mul(FIX_0_298631336);
        let tmp5 = tmp5.wrapping_mul(FIX_2_053119869);
        let tmp6 = tmp6.wrapping_mul(FIX_3_072711026);
        let tmp7 = tmp7.wrapping_mul(FIX_1_501321110);
        let z1 = z1.wrapping_mul(-FIX_0_899976223);
        let z2 = z2.wrapping_mul(-FIX_2_562915447);
        let z3 = z3.wrapping_mul(-FIX_1_961570560);
        let z4 = z4.wrapping_mul(-FIX_0_390180644);

        let z3 = z3.wrapping_add(z5);
        let z4 = z4.wrapping_add(z5);

        [
            even(tmp10.wrapping_add(tmp11)),
            odd(tmp7.wrapping_add(z1).wrapping_add(z4)),
            even2,
            odd(tmp6.wrapping_add(z2).wrapping_add(z3)),
            even(tmp10.wrapping_sub(tmp11)),
            odd(tmp5.wrapping_add(z2).wrapping_add(z4)),
            even6,
            odd(tmp4.wrapping_add(z1).wrapping_add(z3)),
        ]
    };

    let mut data = *input;

    // Pass 1: rows.
    for row in data.chunks_exact_mut(8) {
        let out = transform(
            [
                row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7],
            ],
            true,
        );
        row.copy_from_slice(&out);
    }

    // Pass 2: columns.
    for x in 0..8 {
        let column = std::array::from_fn(|y| data[y * 8 + x]);
        let out = transform(column, false);
        for y in 0..8 {
            data[y * 8 + x] = out[y];
        }
    }
    data
}

/// Per-coefficient forward quantization, ported from `jcdctmgr.c`.
///
/// The 8-bit and 12-bit libjpeg builds use different algorithms: 8-bit uses the
/// fixed-point reciprocal table from `compute_reciprocal`, while 12-bit divides
/// by `qval << 3` with explicit rounding. The coefficient results (JCOEF, a
/// 16-bit value) are truncated the way the C cast truncates.
pub(crate) struct ForwardQuantizer {
    precision: u8,
    reciprocal: [u32; 64],
    correction: [u32; 64],
    shift: [i32; 64],
    divisor: [i32; 64],
}

impl ForwardQuantizer {
    pub(crate) fn new(table: &[u16; 64], precision: u8) -> Self {
        let mut quantizer = Self {
            precision,
            reciprocal: [0; 64],
            correction: [0; 64],
            shift: [0; 64],
            divisor: [0; 64],
        };
        for (i, &quant) in table.iter().enumerate() {
            if precision == 8 {
                // `compute_reciprocal` takes a UINT16 divisor, so the
                // `quantval << 3` argument is truncated to 16 bits by the C
                // call. Reproduce that conversion.
                let divisor = ((u32::from(quant) << 3) & 0xffff) as u16;
                let (reciprocal, correction, shift) = compute_reciprocal(divisor);
                quantizer.reciprocal[i] = reciprocal;
                quantizer.correction[i] = correction;
                quantizer.shift[i] = shift;
            } else {
                quantizer.divisor[i] = (i32::from(quant)) << 3;
            }
        }
        quantizer
    }

    pub(crate) fn quantize(&self, coefficients: &[i32; 64]) -> [i32; 64] {
        let mut output = [0i32; 64];
        for (i, &coefficient) in coefficients.iter().enumerate() {
            let value = if self.precision == 8 {
                let mut temp = coefficient;
                let reciprocal = u64::from(self.reciprocal[i]);
                let correction = u64::from(self.correction[i]);
                // `shift + sizeof(DCTELEM) * 8`; the negative shift used by the
                // divisor <= 1 path (`-32`) turns into a zero shift.
                let shift = (self.shift[i] + 32) as u32;
                if temp < 0 {
                    temp = -temp;
                    let product = (temp as u64 + correction) * reciprocal;
                    -((product >> shift) as i32)
                } else {
                    let product = (temp as u64 + correction) * reciprocal;
                    (product >> shift) as i32
                }
            } else {
                let divisor = self.divisor[i];
                let mut temp = coefficient;
                if temp < 0 {
                    temp = -temp;
                    temp += divisor >> 1;
                    temp = if temp >= divisor { temp / divisor } else { 0 };
                    -temp
                } else {
                    temp += divisor >> 1;
                    if temp >= divisor {
                        temp / divisor
                    } else {
                        0
                    }
                }
            };
            output[i] = i32::from(value as i16);
        }
        output
    }
}

/// `flss` from `jcdctmgr.c`: one plus the index of the highest set bit.
fn flss(mut value: u16) -> i32 {
    let mut bit = 16;
    if value == 0 {
        return 0;
    }
    if value & 0xff00 == 0 {
        bit -= 8;
        value <<= 8;
    }
    if value & 0xf000 == 0 {
        bit -= 4;
        value <<= 4;
    }
    if value & 0xc000 == 0 {
        bit -= 2;
        value <<= 2;
    }
    if value & 0x8000 == 0 {
        bit -= 1;
    }
    bit
}

/// `compute_reciprocal` from `jcdctmgr.c`: reciprocal, correction+round factor,
/// and bit shift for dividing by `divisor` with rounding in 32-bit arithmetic.
fn compute_reciprocal(divisor: u16) -> (u32, u32, i32) {
    if divisor <= 1 {
        return (1, 0, -32);
    }
    let b = flss(divisor) - 1;
    let mut r = 32 + b;
    let mut fq = (1u64 << r) / u64::from(divisor);
    let fr = (1u64 << r) % u64::from(divisor);
    let mut c = u64::from(divisor) / 2;
    if fr == 0 {
        fq >>= 1;
        r -= 1;
    } else if fr <= u64::from(divisor) / 2 {
        c += 1;
    } else {
        fq += 1;
    }
    (fq as u32, c as u32, r - 32)
}

pub(crate) fn inverse(input: &[i32; 64]) -> [i32; 64] {
    std::array::from_fn(|v| {
        let y = v / 8;
        let x = v % 8;
        let mut sum = 0.0;
        for j in 0..8 {
            for i in 0..8 {
                let a = if i == 0 { 1.0 / 2.0_f64.sqrt() } else { 1.0 };
                let b = if j == 0 { 1.0 / 2.0_f64.sqrt() } else { 1.0 };
                sum += a
                    * b
                    * f64::from(input[j * 8 + i])
                    * (((2 * x + 1) as f64 * i as f64 * PI) / 16.0).cos()
                    * (((2 * y + 1) as f64 * j as f64 * PI) / 16.0).cos();
            }
        }
        (sum / 4.0).round() as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn islow_forward_of_a_constant_block_is_dc_only() {
        let output = islow_forward(&[37; 64], 8);
        // libjpeg scales the forward DCT by 8, and the 2-D DC gain is
        // 8 * 8 = 64 relative to the sample value.
        assert_eq!(output[0], 64 * 37);
        assert!(output[1..].iter().all(|&value| value == 0));
    }

    #[test]
    fn islow_forward_matches_the_reference_integer_dct() {
        // Expected values from an independent transcription of
        // libjpeg-turbo 3.2.0 jfdctint.c `_jpeg_fdct_islow`.
        let input: [i32; 64] = std::array::from_fn(|i| ((i * 37) % 251) as i32 - 125);
        let expected = [
            -174, -669, -1120, 558, 0, 103, -79, -232, -433, 398, -615, -464, -53, -45, -230, 104,
            -464, -840, -2069, 992, -735, -59, 356, 167, -301, -1320, 1471, 1108, -364, 606, -251,
            192, 0, 364, 464, -266, -502, -53, -192, -545, 1, -192, -431, -606, -545, 1108, -562,
            393, 192, -46, 355, -122, 848, 82, 61, 521, -325, -104, 16, 339, -266, -464, -934, 398,
        ];
        assert_eq!(islow_forward(&input, 8), expected);
    }

    #[test]
    fn islow_forward_uses_the_twelve_bit_pass_shift() {
        let input: [i32; 64] = std::array::from_fn(|i| ((i * 53) % 4091) as i32 - 2048);
        let expected = [
            -24224, -7728, 0, -808, 0, -240, 0, -60, -61810, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, -6465, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1932, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, -487, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(islow_forward(&input, 12), expected);
    }

    #[test]
    fn quantizer_matches_libjpeg_rounding() {
        // One table entry for every coefficient makes the expected value easy
        // to state. The 8-bit divisor is `quantval << 3` because the ISLOW DCT
        // is scaled by eight.
        let table = [3u16; 64];
        let quantizer = ForwardQuantizer::new(&table, 8);
        let coefficients = std::array::from_fn(|i| i as i32 - 32);
        let output = quantizer.quantize(&coefficients);
        for (i, &actual) in output.iter().enumerate() {
            let expected = ((i as i32 - 32) as f64 / 24.0).round() as i32;
            assert_eq!(actual, expected, "coefficient {i}");
        }
    }

    #[test]
    fn twelve_bit_quantizer_rounds_with_division() {
        let table = [3u16; 64];
        let quantizer = ForwardQuantizer::new(&table, 12);
        // The ISLOW output is the orthogonal coefficient scaled by eight, so
        // `value * 8 / (qval * 8)` is `value / qval`.
        let coefficients = std::array::from_fn(|i| (i as i32 - 32) * 8);
        let output = quantizer.quantize(&coefficients);
        for (i, &actual) in output.iter().enumerate() {
            let expected = ((i as i32 - 32) as f64 / 3.0).round() as i32;
            assert_eq!(actual, expected, "coefficient {i}");
        }
    }

    #[test]
    fn dct_round_trip_stays_within_two_samples() {
        let input: [i32; 64] = std::array::from_fn(|i| (i as i32 * 13 % 255) - 128);
        let coefficients: [i32; 64] = islow_forward(&input, 8).map(|value| value / 8);
        let output = inverse(&coefficients);
        assert!(input.iter().zip(output).all(|(a, b)| (*a - b).abs() <= 2));
    }
}
