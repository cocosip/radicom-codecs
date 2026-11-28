//! Eight independent cleanup quads per vector, in codestream order.

use super::*;
#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[target_feature(enable = "avx2")]
fn columns(row: &[i32; 16]) -> [__m256i; 2] {
    // SAFETY: Each load reads one half of the input array.
    unsafe {
        let a = _mm256_castsi256_ps(_mm256_loadu_si256(row.as_ptr().cast()));
        let b = _mm256_castsi256_ps(_mm256_loadu_si256(row.as_ptr().add(8).cast()));
        [
            _mm256_permute4x64_epi64::<0xd8>(_mm256_castps_si256(_mm256_shuffle_ps::<0x88>(a, b))),
            _mm256_permute4x64_epi64::<0xd8>(_mm256_castps_si256(_mm256_shuffle_ps::<0xdd>(a, b))),
        ]
    }
}

#[target_feature(enable = "avx2")]
fn edge_columns(row: &[i32]) -> [__m256i; 2] {
    if let Ok(full) = row.try_into() {
        return columns(full);
    }
    let mut padded = [0; 16];
    padded[..row.len()].copy_from_slice(row);
    columns(&padded)
}

#[target_feature(enable = "avx2")]
fn lanes(v: __m256i) -> [u32; 8] {
    // SAFETY: Both types contain the same eight 32-bit lanes.
    unsafe { std::mem::transmute(v) }
}

#[target_feature(enable = "avx2")]
fn previous(v: __m256i, carry: i32) -> __m256i {
    _mm256_insert_epi32::<0>(
        _mm256_permutevar8x32_epi32(v, _mm256_setr_epi32(0, 0, 1, 2, 3, 4, 5, 6)),
        carry,
    )
}

fn gather_table(first: bool) -> &'static [i32] {
    static INITIAL: OnceLock<Vec<i32>> = OnceLock::new();
    static OTHER: OnceLock<Vec<i32>> = OnceLock::new();
    let (storage, table) = if first {
        (&INITIAL, vlc_encode0())
    } else {
        (&OTHER, vlc_encode1())
    };
    storage.get_or_init(|| table.iter().map(|&v| i32::from(v)).collect())
}

// A positive cleanup bit plane leaves at most 31 magnitude bits, so both
// offsets in a quad pair fit in five bits. Pack the UVLC word and its length.
fn pair_codes(initial: bool) -> &'static [u32; 1024] {
    static FIRST: OnceLock<[u32; 1024]> = OnceLock::new();
    static LATER: OnceLock<[u32; 1024]> = OnceLock::new();
    let storage = if initial { &FIRST } else { &LATER };
    storage.get_or_init(|| {
        std::array::from_fn(|i| {
            let (a, b) = ((i & 31) as i32, (i >> 5) as i32);
            let (bits, len) = if initial {
                initial_uvlc_code(a, b)
            } else {
                uvlc_code(a, b)
            }
            .expect("five-bit cleanup offsets always have a UVLC code");
            bits | (len << 16)
        })
    })
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn encode_rows(
    coefficients: &[i32],
    width: usize,
    bit_plane: i32,
    mel: &mut MelWriter,
    vlc: &mut ReverseVlcWriter,
    ms: &mut MagSgnWriter,
) {
    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let shift = _mm_cvtsi32_si128(bit_plane);
    // At each quad boundary keep the larger of its two neighboring bottom exponents.
    let padded_quads = width.div_ceil(16) * 8;
    let mut north = vec![0i32; padded_quads + 1];
    for (y, stripe) in coefficients.chunks(2 * width).enumerate() {
        let table = gather_table(y == 0);
        let uvlc = pair_codes(y == 0);
        let (top, bottom) = stripe.split_at(width);
        let mut left_rho = 0;
        let mut left_exp = 0;
        for (group, t) in top.chunks(16).enumerate() {
            let valid = t.len().div_ceil(2);
            let [t0, t1] = edge_columns(t);
            let [b0, b1] = edge_columns(
                bottom
                    .get(group * 16..group * 16 + t.len())
                    .unwrap_or_default(),
            );
            let mut exponents = [zero; 4];
            let mut codes = [zero; 4];
            let mut rho = zero;
            for (i, sample) in [t0, b0, t1, b1].into_iter().enumerate() {
                let value = _mm256_andnot_si256(
                    one,
                    _mm256_srl_epi32(_mm256_add_epi32(sample, sample), shift),
                );
                codes[i] = _mm256_or_si256(
                    _mm256_sub_epi32(value, _mm256_set1_epi32(2)),
                    _mm256_srli_epi32::<31>(sample),
                );
                let magnitude = _mm256_max_epi32(zero, _mm256_sub_epi32(value, one));
                let small = _mm256_cmpeq_epi32(_mm256_srli_epi32::<24>(magnitude), zero);
                let exact =
                    _mm256_and_si256(magnitude, _mm256_or_si256(small, _mm256_set1_epi32(!255)));
                exponents[i] = _mm256_max_epi32(
                    zero,
                    _mm256_sub_epi32(
                        _mm256_srli_epi32::<23>(_mm256_castps_si256(_mm256_cvtepi32_ps(exact))),
                        _mm256_set1_epi32(126),
                    ),
                );
                rho = _mm256_or_si256(
                    rho,
                    _mm256_and_si256(_mm256_cmpgt_epi32(value, zero), _mm256_set1_epi32(1 << i)),
                );
            }
            let left = previous(rho, left_rho);
            left_rho = _mm256_extract_epi32::<7>(rho);
            let base = group * 8;
            let (ctx, kappa) = if y == 0 {
                (
                    _mm256_or_si256(_mm256_srli_epi32::<1>(left), _mm256_and_si256(left, one)),
                    one,
                )
            } else {
                // SAFETY: north has one extra entry beyond the last complete group.
                let (a, b) = unsafe {
                    (
                        _mm256_loadu_si256(north[base..].as_ptr().cast()),
                        _mm256_loadu_si256(north[base + 1..].as_ptr().cast()),
                    )
                };
                let ctx = _mm256_or_si256(
                    _mm256_and_si256(one, _mm256_cmpgt_epi32(a, zero)),
                    _mm256_or_si256(
                        _mm256_and_si256(
                            _mm256_set1_epi32(2),
                            _mm256_cmpgt_epi32(left, _mm256_set1_epi32(3)),
                        ),
                        _mm256_and_si256(_mm256_set1_epi32(4), _mm256_cmpgt_epi32(b, zero)),
                    ),
                );
                let multiple =
                    _mm256_cmpgt_epi32(_mm256_and_si256(rho, _mm256_sub_epi32(rho, one)), zero);
                let predicted = _mm256_sub_epi32(_mm256_max_epi32(a, b), one);
                (
                    ctx,
                    _mm256_max_epi32(one, _mm256_and_si256(multiple, predicted)),
                )
            };
            let boundary = _mm256_max_epi32(previous(exponents[3], left_exp), exponents[1]);
            left_exp = _mm256_extract_epi32::<7>(exponents[3]);
            // SAFETY: Eight boundary entries correspond to the eight complete quads.
            unsafe {
                _mm256_storeu_si256(north[base..].as_mut_ptr().cast(), boundary);
            }
            let high = _mm256_max_epi32(
                _mm256_max_epi32(exponents[0], exponents[1]),
                _mm256_max_epi32(exponents[2], exponents[3]),
            );
            let u = _mm256_max_epi32(kappa, high);
            let mut eps = zero;
            for (i, e) in exponents.into_iter().enumerate() {
                eps = _mm256_or_si256(
                    eps,
                    _mm256_and_si256(_mm256_cmpeq_epi32(e, high), _mm256_set1_epi32(1 << i)),
                );
            }
            eps = _mm256_and_si256(eps, _mm256_cmpgt_epi32(high, kappa));
            let index = _mm256_or_si256(
                _mm256_slli_epi32::<8>(ctx),
                _mm256_or_si256(_mm256_slli_epi32::<4>(rho), eps),
            );
            // SAFETY: ctx is three bits, rho and eps are four bits, giving indices 0..2048.
            let tuples = unsafe { _mm256_i32gather_epi32::<4>(table.as_ptr(), index) };
            let mut packed = [zero; 2];
            let mut lengths = [zero; 2];
            let mut fields = [zero; 4];
            let mut widths = [zero; 4];
            for i in 0..4 {
                let lane_shift = _mm_cvtsi32_si128(i as i32);
                let embed = _mm256_and_si256(one, _mm256_srl_epi32(tuples, lane_shift));
                let sig =
                    _mm256_cmpgt_epi32(_mm256_and_si256(rho, _mm256_set1_epi32(1 << i)), zero);
                let bits = _mm256_and_si256(sig, _mm256_sub_epi32(u, embed));
                let field = _mm256_and_si256(
                    codes[i],
                    _mm256_sub_epi32(_mm256_sllv_epi32(one, bits), one),
                );
                fields[i] = field;
                widths[i] = bits;
                packed[i / 2] =
                    _mm256_or_si256(packed[i / 2], _mm256_sllv_epi32(field, lengths[i / 2]));
                lengths[i / 2] = _mm256_add_epi32(lengths[i / 2], bits);
            }
            let overwide = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpgt_epi32(
                _mm256_max_epi32(lengths[0], lengths[1]),
                _mm256_set1_epi32(32),
            )));
            let words = [lanes(packed[0]), lanes(packed[1])];
            let lens = [lanes(lengths[0]), lanes(lengths[1])];
            let uq = lanes(_mm256_sub_epi32(u, kappa));
            let tuples = lanes(tuples);
            let mel_events = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpeq_epi32(ctx, zero)));
            let significant =
                _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpgt_epi32(rho, zero)));
            for i in (0..valid).step_by(2) {
                for j in i..(i + 2).min(valid) {
                    if mel_events & (1 << j) != 0 {
                        mel.encode(significant & (1 << j) != 0);
                    }
                    if overwide & (1 << j) != 0 {
                        for k in 0..4 {
                            ms.encode(lanes(fields[k])[j], lanes(widths[k])[j]);
                        }
                    } else if lens[0][j] + lens[1][j] <= 32 {
                        ms.encode(
                            words[0][j] | words[1][j].wrapping_shl(lens[0][j]),
                            lens[0][j] + lens[1][j],
                        );
                    } else {
                        ms.encode(words[0][j], lens[0][j]);
                        ms.encode(words[1][j], lens[1][j]);
                    }
                }
                let (u0, u1) = (uq[i], uq[i + 1]);
                if y == 0 && u0 > 0 && u1 > 0 {
                    mel.encode(u0.min(u1) > 2);
                }
                let uv = uvlc[(u0 | (u1 << 5)) as usize];
                let (a, b) = (tuples[i], if i + 1 < valid { tuples[i + 1] } else { 0 });
                let len0 = (a >> 4) & 7;
                let len = len0 + ((b >> 4) & 7);
                vlc.encode(
                    (a >> 8) | ((b >> 8) << len0) | ((uv & 65535) << len),
                    len + (uv >> 16),
                );
            }
        }
        north[padded_quads] = left_exp;
    }
}
