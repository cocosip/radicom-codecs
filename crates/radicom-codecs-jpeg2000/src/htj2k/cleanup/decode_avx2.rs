//! Decode eight MagSgn quads from a common, unstuffed bit buffer.
use super::ForwardSegmentReader;
#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[target_feature(enable = "avx2")]
fn pair_order(a: __m256i, b: __m256i) -> [__m256i; 2] {
    let lo = _mm256_unpacklo_epi32(a, b);
    let hi = _mm256_unpackhi_epi32(a, b);
    [
        _mm256_permute2x128_si256::<0x20>(lo, hi),
        _mm256_permute2x128_si256::<0x31>(lo, hi),
    ]
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn decode_rows(
    data: &[u8],
    width: usize,
    stride: usize,
    states: &[u16],
    bit_plane: i32,
    out: &mut [i32],
    north: &mut [u32],
    words: &mut Vec<u32>,
) -> Result<(), String> {
    // Beyond the segment, HT MagSgn supplies an unlimited string of one bits.
    let mut reader = ForwardSegmentReader::new(data, data.len(), 255);
    let word_count = data.len().div_ceil(4).min(out.len());
    words.clear();
    for _ in 0..word_count {
        words.push(reader.fetch(32));
        reader.advance(32);
    }
    words.push(u32::MAX);
    let end = _mm256_set1_epi32((word_count * 4) as i32);
    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let limit = _mm256_set1_epi32(32 - bit_plane);
    let mut position = 0;
    for (y, stripe) in out.chunks_exact_mut(2 * width).enumerate() {
        let (top, bottom) = stripe.split_at_mut(width);
        let mut carry = 0;
        for q in (0..width / 2).step_by(8) {
            // SAFETY: Every group has eight adjacent (VLC tuple, UVLC value) pairs.
            let info = unsafe { _mm256_loadu_si256(states[y * stride + 2 * q..].as_ptr().cast()) };
            let rho = _mm256_and_si256(_mm256_srli_epi32::<4>(info), _mm256_set1_epi32(15));
            let mut u = _mm256_srli_epi32::<16>(info);
            if y != 0 {
                // SAFETY: north includes one extra boundary after its last quad.
                let neighbors = unsafe {
                    _mm256_or_si256(
                        _mm256_loadu_si256(north[q..].as_ptr().cast()),
                        _mm256_loadu_si256(north[q + 1..].as_ptr().cast()),
                    )
                };
                // Halve the unsigned magnitude before converting to a signed float.
                // Truncating low bits above 2^24 prevents rounding to the next exponent.
                let half = _mm256_max_epi32(_mm256_srli_epi32::<1>(neighbors), one);
                let small = _mm256_cmpeq_epi32(_mm256_srli_epi32::<24>(half), zero);
                let exact = _mm256_and_si256(half, _mm256_or_si256(small, _mm256_set1_epi32(!255)));
                let log = _mm256_sub_epi32(
                    _mm256_srli_epi32::<23>(_mm256_castps_si256(_mm256_cvtepi32_ps(exact))),
                    _mm256_set1_epi32(126),
                );
                let multiple =
                    _mm256_cmpgt_epi32(_mm256_and_si256(rho, _mm256_sub_epi32(rho, one)), zero);
                u = _mm256_add_epi32(u, _mm256_blendv_epi8(one, log, multiple));
            }
            if _mm256_movemask_epi8(_mm256_cmpgt_epi32(u, limit)) != 0 {
                return Err("HTJ2K cleanup pass has an invalid U value".into());
            }
            let mut widths = [zero; 4];
            let mut sig = [zero; 4];
            let mut total = zero;
            for i in 0..4 {
                sig[i] = _mm256_cmpgt_epi32(_mm256_and_si256(rho, _mm256_set1_epi32(1 << i)), zero);
                let embed = _mm256_and_si256(
                    one,
                    _mm256_srl_epi32(info, _mm_cvtsi32_si128(12 + i as i32)),
                );
                widths[i] = _mm256_and_si256(sig[i], _mm256_sub_epi32(u, embed));
                total = _mm256_add_epi32(total, widths[i]);
            }
            // Prefix sum of preceding quads within each half, then across the halves.
            let mut prefix = _mm256_add_epi32(total, _mm256_slli_si256::<4>(total));
            prefix = _mm256_add_epi32(prefix, _mm256_slli_si256::<8>(prefix));
            prefix = _mm256_add_epi32(
                prefix,
                _mm256_set_m128i(
                    _mm_set1_epi32(_mm256_extract_epi32::<3>(prefix)),
                    _mm_setzero_si128(),
                ),
            );
            let mut starts =
                _mm256_add_epi32(_mm256_sub_epi32(prefix, total), _mm256_set1_epi32(position));
            position += _mm256_extract_epi32::<7>(prefix);
            let mut samples = [zero; 4];
            let mut vn = [zero; 4];
            for i in 0..4 {
                let offset = _mm256_min_epi32(_mm256_srli_epi32::<3>(starts), end);
                // SAFETY: At each starting byte a four-byte window is readable,
                // including the final artificial word. It covers up to 25 bits.
                let window = unsafe { _mm256_i32gather_epi32::<1>(words.as_ptr().cast(), offset) };
                let raw = _mm256_srlv_epi32(window, _mm256_and_si256(starts, _mm256_set1_epi32(7)));
                let leading =
                    _mm256_and_si256(one, _mm256_srl_epi32(info, _mm_cvtsi32_si128(8 + i as i32)));
                let mask = _mm256_sub_epi32(_mm256_sllv_epi32(one, widths[i]), one);
                let mag = _mm256_or_si256(
                    one,
                    _mm256_or_si256(
                        _mm256_and_si256(raw, mask),
                        _mm256_sllv_epi32(leading, widths[i]),
                    ),
                );
                vn[i] = _mm256_and_si256(mag, sig[i]);
                samples[i] = _mm256_and_si256(
                    sig[i],
                    _mm256_or_si256(
                        _mm256_slli_epi32::<31>(raw),
                        _mm256_sll_epi32(
                            _mm256_add_epi32(mag, _mm256_set1_epi32(2)),
                            _mm_cvtsi32_si128(bit_plane - 1),
                        ),
                    ),
                );
                starts = _mm256_add_epi32(starts, widths[i]);
            }
            let left = _mm256_insert_epi32::<0>(
                _mm256_permutevar8x32_epi32(vn[3], _mm256_setr_epi32(0, 0, 1, 2, 3, 4, 5, 6)),
                carry,
            );
            carry = _mm256_extract_epi32::<7>(vn[3]);
            // SAFETY: Each destination row has 16 samples remaining in this group.
            unsafe {
                _mm256_storeu_si256(north[q..].as_mut_ptr().cast(), _mm256_or_si256(left, vn[1]));
                for (row, pair) in [
                    (&mut *top, pair_order(samples[0], samples[2])),
                    (&mut *bottom, pair_order(samples[1], samples[3])),
                ] {
                    _mm256_storeu_si256(row[2 * q..].as_mut_ptr().cast(), pair[0]);
                    _mm256_storeu_si256(row[2 * q + 8..].as_mut_ptr().cast(), pair[1]);
                }
            }
        }
        north[width / 2] = carry as u32;
    }
    Ok(())
}
