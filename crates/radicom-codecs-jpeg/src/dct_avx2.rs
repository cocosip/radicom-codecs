use std::arch::x86_64::*;

use super::{ForwardQuantizer, ZIGZAG};

// Map the eight nonzero flags of each natural-order row to entropy order.
static ROW_AC_MASKS: [[u64; 256]; 8] = {
    let mut masks = [[0u64; 256]; 8];
    let mut position = 1;
    while position < ZIGZAG.len() {
        let index = ZIGZAG[position];
        let mut pattern = 0;
        while pattern < 256 {
            if pattern & (1 << (index % 8)) != 0 {
                masks[index / 8][pattern] |= 1 << position;
            }
            pattern += 1;
        }
        position += 1;
    }
    masks
};

#[cfg(test)]
#[target_feature(enable = "avx2")]
pub(super) fn quantize(input: &[i32; 64], quantizer: &ForwardQuantizer) -> [i32; 64] {
    let mut rows = load_rows(input);
    for (i, row) in rows.iter_mut().enumerate() {
        *row = quantize_row(*row, quantizer, i * 8);
    }
    store_rows(rows)
}

#[inline]
#[target_feature(enable = "avx2")]
fn quantize_row(coefficient: __m256i, quantizer: &ForwardQuantizer, i: usize) -> __m256i {
    // SAFETY: The caller supplies one of the eight row offsets in each table.
    unsafe {
        let correction = _mm256_loadu_si256(quantizer.correction.as_ptr().add(i).cast());
        let reciprocal = _mm256_loadu_si256(quantizer.reciprocal.as_ptr().add(i).cast());
        let shift = _mm256_loadu_si256(quantizer.shift.as_ptr().add(i).cast());
        let magnitude = _mm256_add_epi32(_mm256_abs_epi32(coefficient), correction);
        let even = _mm256_srlv_epi64(
            _mm256_mul_epu32(magnitude, reciprocal),
            _mm256_and_si256(shift, _mm256_set1_epi64x(0xffff_ffff)),
        );
        let odd = _mm256_srlv_epi64(
            _mm256_mul_epu32(
                _mm256_srli_epi64::<32>(magnitude),
                _mm256_srli_epi64::<32>(reciprocal),
            ),
            _mm256_srli_epi64::<32>(shift),
        );
        let result = _mm256_sign_epi32(
            _mm256_or_si256(even, _mm256_slli_epi64::<32>(odd)),
            coefficient,
        );
        _mm256_srai_epi32::<16>(_mm256_slli_epi32::<16>(result))
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn load_rows(input: &[i32; 64]) -> [__m256i; 8] {
    std::array::from_fn(|row| {
        // SAFETY: Every row has eight i32 lanes and unaligned loads are valid.
        unsafe { _mm256_loadu_si256(input.as_ptr().add(row * 8).cast()) }
    })
}

#[inline]
#[target_feature(enable = "avx2")]
fn forward_rows(input: &[i32; 64], precision: u8) -> [__m256i; 8] {
    let rows = load_rows(input);
    let pass1_bits = if precision == 8 { 2 } else { 1 };
    let columns = forward_lanes::<true>(transpose_i32(rows), pass1_bits);
    forward_lanes::<false>(transpose_i32(columns), pass1_bits)
}

#[inline]
#[target_feature(enable = "avx2")]
fn store_rows(rows: [__m256i; 8]) -> [i32; 64] {
    let mut output = [0; 64];
    for (row, lanes) in rows.into_iter().enumerate() {
        // SAFETY: The output has room for eight contiguous i32 lanes per row.
        unsafe { _mm256_storeu_si256(output.as_mut_ptr().add(row * 8).cast(), lanes) };
    }
    output
}

#[cfg(test)]
#[target_feature(enable = "avx2")]
pub(super) fn forward(input: &[i32; 64], precision: u8) -> [i32; 64] {
    store_rows(forward_rows(input, precision))
}

#[target_feature(enable = "avx2")]
pub(super) fn forward_quantized(
    input: &[i32; 64],
    precision: u8,
    quantizer: &ForwardQuantizer,
) -> ([i32; 64], u64) {
    let mut rows = forward_rows(input, precision);
    let mut nonzero = 0;
    for (i, row) in rows.iter_mut().enumerate() {
        *row = quantize_row(*row, quantizer, i * 8);
        let zero_lanes = _mm256_cmpeq_epi32(*row, _mm256_setzero_si256());
        let row_mask = !_mm256_movemask_ps(_mm256_castsi256_ps(zero_lanes)) as u8;
        nonzero |= ROW_AC_MASKS[i][usize::from(row_mask)];
    }
    (store_rows(rows), nonzero)
}

#[target_feature(enable = "avx2")]
fn forward_lanes<const FIRST: bool>(
    [s0, s1, s2, s3, s4, s5, s6, s7]: [__m256i; 8],
    pass1_bits: i32,
) -> [__m256i; 8] {
    use std::arch::x86_64::{_mm256_add_epi32 as add, _mm256_sub_epi32 as sub};
    let multiply = |a, constant| _mm256_mullo_epi32(a, _mm256_set1_epi32(constant));
    let descale = |value, shift| {
        _mm256_sra_epi32(
            add(value, _mm256_set1_epi32(1 << (shift - 1))),
            _mm_cvtsi32_si128(shift),
        )
    };
    let even = |v| {
        if FIRST {
            _mm256_sll_epi32(v, _mm_cvtsi32_si128(pass1_bits))
        } else {
            descale(v, pass1_bits)
        }
    };
    let odd_shift = if FIRST {
        13 - pass1_bits
    } else {
        13 + pass1_bits
    };
    let odd = |v| descale(v, odd_shift);

    let tmp0 = add(s0, s7);
    let tmp7 = sub(s0, s7);
    let tmp1 = add(s1, s6);
    let tmp6 = sub(s1, s6);
    let tmp2 = add(s2, s5);
    let tmp5 = sub(s2, s5);
    let tmp3 = add(s3, s4);
    let tmp4 = sub(s3, s4);
    let tmp10 = add(tmp0, tmp3);
    let tmp13 = sub(tmp0, tmp3);
    let tmp11 = add(tmp1, tmp2);
    let tmp12 = sub(tmp1, tmp2);
    let z1 = multiply(add(tmp12, tmp13), 4433);
    let even2 = odd(add(z1, multiply(tmp13, 6270)));
    let even6 = odd(add(z1, multiply(tmp12, -15137)));

    let z1 = multiply(add(tmp4, tmp7), -7373);
    let z2 = multiply(add(tmp5, tmp6), -20995);
    let z3 = add(tmp4, tmp6);
    let z4 = add(tmp5, tmp7);
    let z5 = multiply(add(z3, z4), 9633);
    let z3 = add(multiply(z3, -16069), z5);
    let z4 = add(multiply(z4, -3196), z5);
    [
        even(add(tmp10, tmp11)),
        odd(add(add(multiply(tmp7, 12299), z1), z4)),
        even2,
        odd(add(add(multiply(tmp6, 25172), z2), z3)),
        even(sub(tmp10, tmp11)),
        odd(add(add(multiply(tmp5, 16819), z2), z4)),
        even6,
        odd(add(add(multiply(tmp4, 2446), z1), z3)),
    ]
}

#[target_feature(enable = "avx2")]
fn transpose_i32([a, b, c, d, e, f, g, h]: [__m256i; 8]) -> [__m256i; 8] {
    let ab_low = _mm256_unpacklo_epi32(a, b);
    let ab_high = _mm256_unpackhi_epi32(a, b);
    let cd_low = _mm256_unpacklo_epi32(c, d);
    let cd_high = _mm256_unpackhi_epi32(c, d);
    let ef_low = _mm256_unpacklo_epi32(e, f);
    let ef_high = _mm256_unpackhi_epi32(e, f);
    let gh_low = _mm256_unpacklo_epi32(g, h);
    let gh_high = _mm256_unpackhi_epi32(g, h);
    let abcd0 = _mm256_unpacklo_epi64(ab_low, cd_low);
    let abcd1 = _mm256_unpackhi_epi64(ab_low, cd_low);
    let abcd2 = _mm256_unpacklo_epi64(ab_high, cd_high);
    let abcd3 = _mm256_unpackhi_epi64(ab_high, cd_high);
    let efgh0 = _mm256_unpacklo_epi64(ef_low, gh_low);
    let efgh1 = _mm256_unpackhi_epi64(ef_low, gh_low);
    let efgh2 = _mm256_unpacklo_epi64(ef_high, gh_high);
    let efgh3 = _mm256_unpackhi_epi64(ef_high, gh_high);
    [
        _mm256_permute2x128_si256::<0x20>(abcd0, efgh0),
        _mm256_permute2x128_si256::<0x20>(abcd1, efgh1),
        _mm256_permute2x128_si256::<0x20>(abcd2, efgh2),
        _mm256_permute2x128_si256::<0x20>(abcd3, efgh3),
        _mm256_permute2x128_si256::<0x31>(abcd0, efgh0),
        _mm256_permute2x128_si256::<0x31>(abcd1, efgh1),
        _mm256_permute2x128_si256::<0x31>(abcd2, efgh2),
        _mm256_permute2x128_si256::<0x31>(abcd3, efgh3),
    ]
}

#[target_feature(enable = "avx2")]
pub(super) fn inverse<T: super::DecodedSample>(
    input: &[i32; 64],
    pass_bits: i32,
    count: usize,
    precision: u8,
    output: &mut super::BlockOutput<'_, T>,
) -> bool {
    let rows = std::array::from_fn(|y| {
        // SAFETY: All eight rows contain eight i32 coefficients.
        unsafe { _mm256_loadu_si256(input.as_ptr().add(y * 8).cast()) }
    });
    let Some(rows) = (match (pass_bits, count) {
        (2, 0..=3) => inverse_block::<2, 11, 18>(rows),
        (2, 4..=10) => inverse_block::<4, 11, 18>(rows),
        (2, _) => inverse_block::<8, 11, 18>(rows),
        (_, 0..=3) => inverse_block::<2, 12, 17>(rows),
        (_, 4..=10) => inverse_block::<4, 12, 17>(rows),
        _ => inverse_block::<8, 12, 17>(rows),
    }) else {
        return false;
    };
    let center = _mm256_set1_epi32(1 << (precision - 1));
    let max = _mm256_set1_epi32((1 << precision) - 1);
    for (y, row) in rows.into_iter().enumerate() {
        let row = _mm256_min_epi32(_mm256_add_epi32(row, center), max);
        let row = _mm_packus_epi32(
            _mm256_castsi256_si128(row),
            _mm256_extracti128_si256::<1>(row),
        );
        // SAFETY: BlockOutput validates eight writable samples in all eight rows.
        unsafe {
            T::store_row(output.samples.as_mut_ptr().add(y * output.stride), row);
        }
    }
    true
}

#[target_feature(enable = "avx2")]
#[inline]
fn inverse_block<const N: usize, const FIRST: i32, const LAST: i32>(
    rows: [__m256i; 8],
) -> Option<[__m256i; 8]> {
    let vertical = inverse_pass::<N, FIRST>(rows)?;
    let horizontal = inverse_pass::<N, LAST>(transpose_i32(vertical))?;
    Some(transpose_i32(horizontal))
}

#[target_feature(enable = "avx2")]
#[inline]
fn inverse_pass<const N: usize, const SHIFT: i32>(v: [__m256i; 8]) -> Option<[__m256i; 8]> {
    use std::arch::x86_64::{_mm256_add_epi32 as add, _mm256_sub_epi32 as sub};
    // With |DC| < 65536 and each |AC| < 16384, the DC term is below
    // 2^29 and the other seven output weights are at most 11364.
    let mut ac = _mm256_setzero_si256();
    for value in &v[1..N] {
        ac = _mm256_or_si256(ac, _mm256_abs_epi32(*value));
    }
    let wide = _mm256_or_si256(
        _mm256_and_si256(ac, _mm256_set1_epi32(!16383)),
        _mm256_and_si256(_mm256_abs_epi32(v[0]), _mm256_set1_epi32(!65535)),
    );
    if _mm256_testz_si256(wide, wide) == 0 {
        return None;
    }
    let pair = |left, right| _mm256_blend_epi16::<0xaa>(left, _mm256_slli_epi32::<16>(right));
    let dot = |pair, left: i16, right: i16| {
        _mm256_madd_epi16(
            pair,
            _mm256_set1_epi32(i32::from(left as u16) | (i32::from(right) << 16)),
        )
    };
    let zero = _mm256_setzero_si256();
    let v = |i| if i < N { v[i] } else { zero };
    let sum = _mm256_slli_epi32::<13>(add(v(0), v(4)));
    let difference = _mm256_slli_epi32::<13>(sub(v(0), v(4)));
    let bf = pair(v(2), v(6));
    let outer = dot(bf, 10703, 4433);
    let inner = dot(bf, 4433, -10704);
    let even = [
        add(sum, outer),
        add(difference, inner),
        sub(difference, inner),
        sub(sum, outer),
    ];
    // Fold the ISLOW odd rotations into integer dot products. Keeping their
    // exact integer weights preserves the rounding of both transform passes.
    let ac = pair(v(1), v(3));
    let eg = pair(v(5), v(7));
    let odd = [
        add(dot(ac, 11363, 9633), dot(eg, 6437, 2260)),
        add(dot(ac, 9633, -2259), dot(eg, -11362, -6436)),
        add(dot(ac, 6437, -11362), dot(eg, 2261, 9633)),
        add(dot(ac, 2260, -6436), dot(eg, 9633, -11363)),
    ];
    let rounded = even.map(|value| add(value, _mm256_set1_epi32(1 << (SHIFT - 1))));
    Some(std::array::from_fn(|i| {
        if i < 4 {
            _mm256_srai_epi32::<SHIFT>(add(rounded[i], odd[i]))
        } else {
            _mm256_srai_epi32::<SHIFT>(sub(rounded[7 - i], odd[7 - i]))
        }
    }))
}
