use std::arch::x86_64::*;

use super::DC_SCALE;

#[target_feature(enable = "avx2")]
pub(super) fn forward(input: &[i32; 64], precision: u8) -> [i32; 64] {
    let rows = std::array::from_fn(|row| {
        // SAFETY: Every row has eight i32 lanes and unaligned loads are valid.
        unsafe { _mm256_loadu_si256(input.as_ptr().add(row * 8).cast()) }
    });
    let pass1_bits = if precision == 8 { 2 } else { 1 };
    let columns = forward_lanes::<true>(transpose_i32(rows), pass1_bits);
    let rows = forward_lanes::<false>(transpose_i32(columns), pass1_bits);
    let mut output = [0; 64];
    for (row, lanes) in rows.into_iter().enumerate() {
        // SAFETY: The output has room for eight contiguous i32 lanes per row.
        unsafe { _mm256_storeu_si256(output.as_mut_ptr().add(row * 8).cast(), lanes) };
    }
    output
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
pub(super) fn inverse(input: &[i32; 64], basis: &[[f64; 8]; 8]) -> ([i32; 64], u64) {
    let vertical = |column| {
        let coefficients = std::array::from_fn(|row| {
            // SAFETY: Each row has eight coefficients and column is 0 or 4.
            let lanes = unsafe { _mm_loadu_si128(input.as_ptr().add(row * 8 + column).cast()) };
            _mm256_cvtepi32_pd(lanes)
        });
        inverse_lanes(coefficients, basis)
    };
    let left = vertical(0);
    let right = vertical(4);
    let mut output = [0; 64];
    let mut boundaries = 0u64;
    for row in [0, 4] {
        let [a, b, c, d] = transpose([left[row], left[row + 1], left[row + 2], left[row + 3]]);
        let [e, f, g, h] = transpose([right[row], right[row + 1], right[row + 2], right[row + 3]]);
        let columns = inverse_lanes([a, b, c, d, e, f, g, h], basis);
        for column in [0, 4] {
            let rows = transpose([
                columns[column],
                columns[column + 1],
                columns[column + 2],
                columns[column + 3],
            ]);
            for (dy, values) in rows.into_iter().enumerate() {
                let index = (row + dy) * 8 + column;
                let values = _mm256_mul_pd(values, _mm256_set1_pd(0.25));
                let rounded =
                    _mm256_round_pd::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(values);
                let difference =
                    _mm256_andnot_pd(_mm256_set1_pd(-0.0), _mm256_sub_pd(values, rounded));
                let near_half = _mm256_cmp_pd::<_CMP_GT_OQ>(difference, _mm256_set1_pd(0.497));
                boundaries |= (_mm256_movemask_pd(near_half) as u64) << index;
                let clamped = _mm256_min_pd(
                    _mm256_max_pd(rounded, _mm256_set1_pd(f64::from(i32::MIN))),
                    _mm256_set1_pd(f64::from(i32::MAX)),
                );
                // SAFETY: index is the start of four adjacent pixels in output.
                unsafe {
                    _mm_storeu_si128(
                        output.as_mut_ptr().add(index).cast(),
                        _mm256_cvttpd_epi32(clamped),
                    );
                }
            }
        }
    }
    (output, boundaries)
}

#[target_feature(enable = "avx2")]
fn inverse_lanes(values: [__m256d; 8], basis: &[[f64; 8]; 8]) -> [__m256d; 8] {
    let [dc, a, b, c, d, e, f, g] = values;
    let [_, _, cos2, _, cos4, _, cos6, _] = basis[0];
    let dc = _mm256_mul_pd(dc, _mm256_set1_pd(DC_SCALE));
    let d = _mm256_mul_pd(d, _mm256_set1_pd(cos4));
    let even_outer = _mm256_add_pd(dc, d);
    let even_inner = _mm256_sub_pd(dc, d);
    let even0 = _mm256_add_pd(
        _mm256_mul_pd(b, _mm256_set1_pd(cos2)),
        _mm256_mul_pd(f, _mm256_set1_pd(cos6)),
    );
    let even1 = _mm256_sub_pd(
        _mm256_mul_pd(b, _mm256_set1_pd(cos6)),
        _mm256_mul_pd(f, _mm256_set1_pd(cos2)),
    );
    let even = [
        _mm256_add_pd(even_outer, even0),
        _mm256_add_pd(even_inner, even1),
        _mm256_sub_pd(even_inner, even1),
        _mm256_sub_pd(even_outer, even0),
    ];
    let mut output = [_mm256_setzero_pd(); 8];
    for (x, even) in even.into_iter().enumerate() {
        let weights = &basis[x];
        let odd = _mm256_add_pd(
            _mm256_add_pd(
                _mm256_add_pd(
                    _mm256_mul_pd(a, _mm256_set1_pd(weights[1])),
                    _mm256_mul_pd(c, _mm256_set1_pd(weights[3])),
                ),
                _mm256_mul_pd(e, _mm256_set1_pd(weights[5])),
            ),
            _mm256_mul_pd(g, _mm256_set1_pd(weights[7])),
        );
        output[x] = _mm256_add_pd(even, odd);
        output[7 - x] = _mm256_sub_pd(even, odd);
    }
    output
}

#[target_feature(enable = "avx2")]
fn transpose([a, b, c, d]: [__m256d; 4]) -> [__m256d; 4] {
    let ab_low = _mm256_unpacklo_pd(a, b);
    let ab_high = _mm256_unpackhi_pd(a, b);
    let cd_low = _mm256_unpacklo_pd(c, d);
    let cd_high = _mm256_unpackhi_pd(c, d);
    [
        _mm256_permute2f128_pd::<0x20>(ab_low, cd_low),
        _mm256_permute2f128_pd::<0x20>(ab_high, cd_high),
        _mm256_permute2f128_pd::<0x31>(ab_low, cd_low),
        _mm256_permute2f128_pd::<0x31>(ab_high, cd_high),
    ]
}
