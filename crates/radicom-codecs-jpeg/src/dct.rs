#![allow(clippy::chunks_exact_to_as_chunks)]

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
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: The feature check guards the AVX2 kernel.
        return unsafe { avx2::forward(input, precision) };
    }
    islow_forward_scalar(input, precision)
}

fn islow_forward_scalar(input: &[i32; 64], precision: u8) -> [i32; 64] {
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
/// The reciprocal performs the rounded division for both precisions. The 8-bit
/// build truncates its divisor to UINT16, and both truncate the result to JCOEF.
pub(crate) struct ForwardQuantizer {
    reciprocal: [u32; 64],
    correction: [u32; 64],
    shift: [i32; 64],
}

impl ForwardQuantizer {
    pub(crate) fn new(table: &[u16; 64], precision: u8) -> Self {
        let mut quantizer = Self {
            reciprocal: [0; 64],
            correction: [0; 64],
            shift: [0; 64],
        };
        for (i, &quant) in table.iter().enumerate() {
            let divisor = u32::from(quant) << 3;
            let divisor = if precision == 8 {
                // The 8-bit IJG build passes the divisor through UINT16.
                divisor & 0xffff
            } else {
                divisor
            };
            let (reciprocal, correction, shift) = compute_reciprocal(divisor);
            quantizer.reciprocal[i] = reciprocal;
            quantizer.correction[i] = correction;
            quantizer.shift[i] = shift + 32;
        }
        quantizer
    }

    pub(crate) fn quantize(&self, coefficients: &[i32; 64]) -> [i32; 64] {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: The feature check guards the AVX2 kernel.
            return unsafe { avx2::quantize(coefficients, self) };
        }
        std::array::from_fn(|i| self.quantize_one(i, coefficients[i]))
    }

    #[inline]
    pub(crate) fn quantize_one(&self, i: usize, coefficient: i32) -> i32 {
        let magnitude = u64::from(coefficient.unsigned_abs()) + u64::from(self.correction[i]);
        let rounded = ((magnitude * u64::from(self.reciprocal[i])) >> self.shift[i]) as i32;
        let value = if coefficient < 0 { -rounded } else { rounded };
        i32::from(value as i16)
    }
}

/// `compute_reciprocal` from `jcdctmgr.c`: reciprocal, correction+round factor,
/// and bit shift for dividing by `divisor` with rounding in 32-bit arithmetic.
fn compute_reciprocal(divisor: u32) -> (u32, u32, i32) {
    if divisor <= 1 {
        return (1, 0, -32);
    }
    let b = divisor.ilog2() as i32;
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

// Loeffler's inverse butterfly, with IJG's 13-bit integer constants. Both
// arithmetic implementations delay rounding until the end of each pass.
macro_rules! inverse_butterfly {
    ($values:expr, $add:ident, $sub:ident, $mul:ident) => {{
        let [dc, a, b, c, d, e, f, g] = $values;
        let rot = $mul($add(b, f), 4433);
        let even2 = $add(rot, $mul(f, -15137));
        let even3 = $add(rot, $mul(b, 6270));
        let lo = $mul($add(dc, d), 8192);
        let hi = $mul($sub(dc, d), 8192);
        let even = [
            $add(lo, even3),
            $add(hi, even2),
            $sub(hi, even2),
            $sub(lo, even3),
        ];
        let z1 = $mul($add(g, a), -7373);
        let z2 = $mul($add(e, c), -20995);
        let z3 = $add(g, c);
        let z4 = $add(e, a);
        let z5 = $mul($add(z3, z4), 9633);
        let z3 = $add($mul(z3, -16069), z5);
        let z4 = $add($mul(z4, -3196), z5);
        let odd = [
            $add($mul(a, 12299), $add(z1, z4)),
            $add($mul(c, 25172), $add(z2, z3)),
            $add($mul(e, 16819), $add(z2, z4)),
            $add($mul(g, 2446), $add(z1, z3)),
        ];
        [
            $add(even[0], odd[0]),
            $add(even[1], odd[1]),
            $add(even[2], odd[2]),
            $add(even[3], odd[3]),
            $sub(even[3], odd[3]),
            $sub(even[2], odd[2]),
            $sub(even[1], odd[1]),
            $sub(even[0], odd[0]),
        ]
    }};
}

#[cfg(target_arch = "x86_64")]
#[path = "dct_avx2.rs"]
mod avx2;

pub(crate) trait DecodedSample: Copy + Default {
    fn from_word(value: u16) -> Self;

    #[cfg(target_arch = "x86_64")]
    unsafe fn store_row(output: *mut Self, values: std::arch::x86_64::__m128i);

    #[cfg(target_arch = "x86_64")]
    unsafe fn load_row(input: *const Self) -> std::arch::x86_64::__m128i;
}

impl DecodedSample for u16 {
    #[inline]
    fn from_word(value: u16) -> Self {
        value
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn store_row(output: *mut Self, values: std::arch::x86_64::__m128i) {
        // SAFETY: The caller reserves eight writable sample slots.
        unsafe { std::arch::x86_64::_mm_storeu_si128(output.cast(), values) }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn load_row(input: *const Self) -> std::arch::x86_64::__m128i {
        unsafe { std::arch::x86_64::_mm_loadu_si128(input.cast()) }
    }
}

impl DecodedSample for u8 {
    #[inline]
    fn from_word(value: u16) -> Self {
        value as u8
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn store_row(output: *mut Self, values: std::arch::x86_64::__m128i) {
        use std::arch::x86_64::{_mm_packus_epi16, _mm_storel_epi64};
        // SAFETY: The caller reserves eight byte samples. SSE2 is part of x86-64.
        unsafe { _mm_storel_epi64(output.cast(), _mm_packus_epi16(values, values)) }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn load_row(input: *const Self) -> std::arch::x86_64::__m128i {
        use std::arch::x86_64::{_mm_loadl_epi64, _mm_setzero_si128, _mm_unpacklo_epi8};
        unsafe { _mm_unpacklo_epi8(_mm_loadl_epi64(input.cast()), _mm_setzero_si128()) }
    }
}

impl DecodedSample for [u8; 2] {
    #[inline]
    fn from_word(value: u16) -> Self {
        value.to_le_bytes()
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn store_row(output: *mut Self, values: std::arch::x86_64::__m128i) {
        // SAFETY: The caller reserves eight writable words. x86-64 is little-endian.
        unsafe { std::arch::x86_64::_mm_storeu_si128(output.cast(), values) }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn load_row(input: *const Self) -> std::arch::x86_64::__m128i {
        unsafe { std::arch::x86_64::_mm_loadu_si128(input.cast()) }
    }
}

pub(crate) struct BlockOutput<'a, T> {
    samples: &'a mut [T],
    stride: usize,
}

impl<'a, T: DecodedSample> BlockOutput<'a, T> {
    pub(crate) fn new(samples: &'a mut [T], stride: usize) -> Self {
        assert!(samples.len() >= 8 && stride >= 8 && stride <= (samples.len() - 8) / 7);
        Self { samples, stride }
    }

    pub(crate) fn fill_dc(&mut self, dc: i32, precision: u8) {
        let center = 1 << (precision - 1);
        let value = T::from_word(
            inverse_dc(dc)
                .saturating_add(center)
                .clamp(0, 2 * center - 1) as u16,
        );
        for y in 0..8 {
            self.samples[y * self.stride..y * self.stride + 8].fill(value);
        }
    }
}

pub(crate) fn inverse<T: DecodedSample>(
    input: &[i32; 64],
    precision: u8,
    count: usize,
    output: &mut BlockOutput<'_, T>,
) {
    let pass_bits = if precision == 8 { 2 } else { 1 };
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: The feature check guards the AVX2 kernel.
        if unsafe { avx2::inverse(input, pass_bits, count, precision, output) } {
            return;
        }
    }
    let center = 1 << (precision - 1);
    let block = inverse_scalar(input, pass_bits, count);
    for (y, row) in block.as_chunks::<8>().0.iter().enumerate() {
        let values = row.map(|value| {
            T::from_word(value.saturating_add(center).clamp(0, 2 * center - 1) as u16)
        });
        output.samples[y * output.stride..y * output.stride + 8].copy_from_slice(&values);
    }
}

pub(crate) fn inverse_dc(coefficient: i32) -> i32 {
    ((i64::from(coefficient) + 4) >> 3) as i32
}

fn inverse_scalar(input: &[i32; 64], pass_bits: i32, count: usize) -> [i32; 64] {
    let pass = |values, shift: i32| {
        let add = |a: i64, b| a + b;
        let sub = |a: i64, b| a - b;
        let mul = |a: i64, c| a * c;
        let output = inverse_butterfly!(values, add, sub, mul);
        output.map(|v| (v + (1i64 << (shift - 1))) >> shift)
    };
    let mut workspace = [[0i64; 8]; 8];
    let columns = match count {
        0..=3 => 2,
        4..=10 => 4,
        _ => 8,
    };
    for x in 0..columns {
        let column = std::array::from_fn(|y| i64::from(input[y * 8 + x]));
        let transformed = pass(column, 13 - pass_bits);
        for (row, value) in workspace.iter_mut().zip(transformed) {
            row[x] = value;
        }
    }
    let mut output = [0; 64];
    for (row, values) in output.chunks_exact_mut(8).zip(workspace) {
        for (dest, value) in row.iter_mut().zip(pass(values, 16 + pass_bits)) {
            *dest = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        }
    }
    output
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
        let mut output = [0u16; 64];
        inverse(&coefficients, 8, 64, &mut BlockOutput::new(&mut output, 8));
        assert!(input
            .iter()
            .zip(output)
            .all(|(a, b)| (*a + 128 - i32::from(b)).abs() <= 2));
    }

    #[test]
    fn inverse_matches_wide_integer_arithmetic_for_both_precisions() {
        let mut state = 0x1234_5678u32;
        let cases = (0..128).map(|index| {
            let mut input = [0i32; 64];
            input[0] = 8 * index - 508;
            if index % 3 != 0 {
                for (slot, coefficient) in input.iter_mut().enumerate().skip(1) {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    if index % 3 == 1 || slot % 5 == 0 {
                        *coefficient = (state as i32) >> if index < 96 { 21 } else { 0 };
                    }
                }
            }
            input
        });
        for input in cases {
            for (precision, pass_bits) in [(8, 2), (12, 1)] {
                let center = 1 << (precision - 1);
                for count in [3, 10, 64] {
                    let mut truncated = input;
                    for &index in &ZIGZAG[count..] {
                        truncated[index] = 0;
                    }
                    let mut actual = [0; 64];
                    inverse(
                        &truncated,
                        precision,
                        count,
                        &mut BlockOutput::new(&mut actual, 8),
                    );
                    let expected = inverse_scalar(&truncated, pass_bits, 64)
                        .map(|value| value.saturating_add(center).clamp(0, 2 * center - 1) as u16);
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    #[test]
    fn dc_half_steps_round_toward_positive_infinity() {
        for (dc, expected) in [(-12, -1), (-4, 0), (4, 1), (12, 2)] {
            let mut block = [0; 64];
            block[0] = dc;
            assert_eq!(inverse_dc(dc), expected);
            for precision in [8, 12] {
                let mut actual = [0; 64];
                inverse(&block, precision, 1, &mut BlockOutput::new(&mut actual, 8));
                assert_eq!(actual, [(expected + (1 << (precision - 1))) as u16; 64]);
            }
        }
    }

    #[test]
    fn reciprocal_quantization_matches_rounded_division_for_extended_tables() {
        let table = std::array::from_fn(|i| [1, 7, 255, 256, 4095, 8192, 16383, 32767][i % 8]);
        for precision in [8, 12] {
            let quantizer = ForwardQuantizer::new(&table, precision);
            for n in -260..=260 {
                let input = std::array::from_fn(|i| n * 503 + i as i32);
                let expected = std::array::from_fn(|i| {
                    let divisor = u32::from(table[i]) * 8;
                    let divisor = if precision == 8 {
                        divisor & 0xffff
                    } else {
                        divisor
                    }
                    .max(1);
                    let coefficient = input[i];
                    let value = (coefficient.unsigned_abs() + divisor / 2) / divisor;
                    i32::from((coefficient.signum() * value as i32) as i16)
                });
                assert_eq!(quantizer.quantize(&input), expected);
            }
        }
    }
}
