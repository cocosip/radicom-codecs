//! HTJ2K standard cleanup-pass codec (ISO/IEC 15444-15 Annex C).
//!
//! Ported from the reference cleanup pass in `fo-dicom.PureCodecs`
//! (`Jpeg2000HtStandardCleanupPassDecoder` / `...Encoder`).

use std::sync::OnceLock;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod decode_avx2;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod encode_avx2;

use super::tables::{
    uvlc_bias, uvlc_table_0, uvlc_table_1, vlc_decode_table, VLC_TABLE_0_SOURCE, VLC_TABLE_1_SOURCE,
};

const MEL_EXPONENTS: [u32; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

fn vlc0() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| vlc_decode_table(VLC_TABLE_0_SOURCE))
}

fn vlc1() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| vlc_decode_table(VLC_TABLE_1_SOURCE))
}

fn uvlc0() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(uvlc_table_0)
}

fn uvlc1() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(uvlc_table_1)
}

fn uvlc_bias_table() -> &'static [u8] {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    TABLE.get_or_init(uvlc_bias)
}

#[derive(Default)]
pub(crate) struct CleanupDecoder {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    magnitude_words: Vec<u32>,
    vlc_scratch: Vec<u16>,
    magnitude_scratch: Vec<u32>,
    coefficients: Vec<i32>,
}

impl CleanupDecoder {
    /// Decodes one HT cleanup pass, reusing buffers across code blocks.
    pub(crate) fn decode(
        &mut self,
        data: &[u8],
        width: usize,
        height: usize,
        missing_msb: u32,
    ) -> Result<&[i32], String> {
        if width == 0 || height == 0 {
            return Err("HTJ2K cleanup pass dimensions are invalid".into());
        }
        if data.len() < 2 {
            return Err("HTJ2K cleanup pass is too short".into());
        }
        let lcup = data.len();
        let scup = (usize::from(data[lcup - 1]) << 4) + usize::from(data[lcup - 2] & 0x0F);
        if scup < 2 || scup > lcup || scup > 4079 {
            return Err("HTJ2K cleanup pass has an invalid Scup locator".into());
        }
        let bit_plane = 30i32 - missing_msb as i32;
        if bit_plane <= 0 {
            return Err("HTJ2K cleanup pass requires a positive CUP bit-plane".into());
        }

        let sstr = ((width + 2) + 7) & !7;
        self.vlc_scratch.clear();
        self.vlc_scratch.resize(sstr * (height.div_ceil(2) + 1), 0);
        decode_mel_vlc(data, width, height, lcup, scup, sstr, &mut self.vlc_scratch)?;
        self.coefficients.resize(width * height, 0);
        self.magnitude_scratch.clear();
        self.magnitude_scratch.resize(width + 4, 0);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if width.is_multiple_of(16)
            && height.is_multiple_of(2)
            && missing_msb + 2 <= 25
            && crate::wavelet::avx2_available()
        {
            // SAFETY: Full groups of eight quads and AVX2 are available. At most
            // 25 bits per sample can straddle four bytes at any bit alignment.
            unsafe {
                decode_avx2::decode_rows(
                    &data[..lcup - scup],
                    width,
                    sstr,
                    &self.vlc_scratch,
                    bit_plane,
                    &mut self.coefficients,
                    &mut self.magnitude_scratch,
                    &mut self.magnitude_words,
                )?;
            }
            return Ok(&self.coefficients);
        }
        decode_mag_sgn(
            &data[..lcup - scup],
            width,
            height,
            sstr,
            &self.vlc_scratch,
            bit_plane,
            missing_msb + 2,
            &mut self.coefficients,
            &mut self.magnitude_scratch,
        )?;
        Ok(&self.coefficients)
    }
}

fn decode_mel_vlc(
    data: &[u8],
    width: usize,
    height: usize,
    lcup: usize,
    scup: usize,
    sstr: usize,
    scratch: &mut [u16],
) -> Result<(), String> {
    let mut mel = MelSegmentReader::new(data, lcup, scup);
    let mut vlc = ReverseSegmentReader::new(data, lcup, scup);
    let mut run = mel.get_run();
    let mut context = 0u32;
    let mut sp = 0usize;
    let initial_vlc = vlc0();
    let initial_uvlc = uvlc0();
    let initial_bias = uvlc_bias_table();

    let mut x = 0usize;
    while x < width {
        let mut vlc_value = vlc.fetch();
        let mut t0 = initial_vlc[context as usize + (vlc_value & 0x7F) as usize];
        if context == 0 {
            run -= 2;
            t0 = if run == -1 { t0 } else { 0 };
            if run < 0 {
                run = mel.get_run();
            }
        }
        scratch[sp] = t0;
        x += 2;
        context = (u32::from(t0 & 0x10) << 3) | (u32::from(t0 & 0xE0) << 2);
        vlc_value = vlc.advance(u32::from(t0 & 0x7));

        let mut t1 = initial_vlc[context as usize + (vlc_value & 0x7F) as usize];
        if context == 0 && x < width {
            run -= 2;
            t1 = if run == -1 { t1 } else { 0 };
            if run < 0 {
                run = mel.get_run();
            }
        }
        t1 = if x < width { t1 } else { 0 };
        scratch[sp + 2] = t1;
        x += 2;
        context = (u32::from(t1 & 0x10) << 3) | (u32::from(t1 & 0xE0) << 2);
        vlc_value = vlc.advance(u32::from(t1 & 0x7));

        let mut uvlc_mode = (u32::from(t0 & 0x8) << 3) | (u32::from(t1 & 0x8) << 4);
        if uvlc_mode == 0xC0 {
            run -= 2;
            uvlc_mode += if run == -1 { 0x40 } else { 0 };
            if run < 0 {
                run = mel.get_run();
            }
        }
        let uvlc_index = uvlc_mode as usize + (vlc_value & 0x3F) as usize;
        let mut uvlc_entry = u32::from(initial_uvlc[uvlc_index]);
        let bias = u32::from(initial_bias[uvlc_index]);
        vlc_value = vlc.advance(uvlc_entry & 0x7);
        uvlc_entry >>= 3;
        let suffix_length = uvlc_entry & 0xF;
        let suffix = vlc_value & mask_low_bits(suffix_length);
        vlc_value = vlc.advance(suffix_length);
        uvlc_entry >>= 4;
        let quad0_suffix_length = uvlc_entry & 0x7;
        uvlc_entry >>= 3;
        let mut u_q0 = (uvlc_entry & 0x7) + (suffix & !(0xFFu32 << quad0_suffix_length));
        let mut u_q1 = (uvlc_entry >> 3) + (suffix >> quad0_suffix_length);
        if u_q0.wrapping_sub(bias & 0x3) > 32 {
            u_q0 += (vlc_value & 0xF) << 2;
            vlc_value = vlc.advance(4);
        }
        if u_q1.wrapping_sub(bias >> 2) > 32 {
            u_q1 += (vlc_value & 0xF) << 2;
            vlc.advance(4);
        }
        scratch[sp + 1] = (u_q0 + 1) as u16;
        scratch[sp + 3] = (u_q1 + 1) as u16;
        sp += 4;
    }
    scratch[sp] = 0;
    scratch[sp + 1] = 0;

    let later_vlc = vlc1();
    let later_uvlc = uvlc1();
    let mut y = 2usize;
    while y < height {
        context = 0;
        sp = (y >> 1) * sstr;
        let mut x = 0usize;
        while x < width {
            context |= u32::from(scratch[sp - sstr] & 0xA0) << 2;
            context |= u32::from(scratch[sp + 2 - sstr] & 0x20) << 4;

            let mut vlc_value = vlc.fetch();
            let mut t0 = later_vlc[context as usize + (vlc_value & 0x7F) as usize];
            if context == 0 {
                run -= 2;
                t0 = if run == -1 { t0 } else { 0 };
                if run < 0 {
                    run = mel.get_run();
                }
            }
            scratch[sp] = t0;
            x += 2;
            context = (u32::from(t0 & 0x40) << 2) | (u32::from(t0 & 0x80) << 1);
            context |= u32::from(scratch[sp - sstr] & 0x80);
            context |= u32::from(scratch[sp + 2 - sstr] & 0xA0) << 2;
            context |= u32::from(scratch[sp + 4 - sstr] & 0x20) << 4;
            vlc_value = vlc.advance(u32::from(t0 & 0x7));

            let mut t1 = later_vlc[context as usize + (vlc_value & 0x7F) as usize];
            if context == 0 && x < width {
                run -= 2;
                t1 = if run == -1 { t1 } else { 0 };
                if run < 0 {
                    run = mel.get_run();
                }
            }
            t1 = if x < width { t1 } else { 0 };
            scratch[sp + 2] = t1;
            x += 2;
            context = (u32::from(t1 & 0x40) << 2) | (u32::from(t1 & 0x80) << 1);
            context |= u32::from(scratch[sp + 2 - sstr] & 0x80);
            vlc_value = vlc.advance(u32::from(t1 & 0x7));

            let uvlc_mode = (u32::from(t0 & 0x8) << 3) | (u32::from(t1 & 0x8) << 4);
            let mut uvlc_entry =
                u32::from(later_uvlc[uvlc_mode as usize + (vlc_value & 0x3F) as usize]);
            vlc_value = vlc.advance(uvlc_entry & 0x7);
            uvlc_entry >>= 3;
            let suffix_length = uvlc_entry & 0xF;
            let suffix = vlc_value & mask_low_bits(suffix_length);
            vlc.advance(suffix_length);
            uvlc_entry >>= 4;
            let quad0_suffix_length = uvlc_entry & 0x7;
            uvlc_entry >>= 3;
            let mut u_q0 = (uvlc_entry & 0x7) + (suffix & !(0xFFu32 << quad0_suffix_length));
            let mut u_q1 = (uvlc_entry >> 3) + (suffix >> quad0_suffix_length);
            if u_q0 > 32 {
                u_q0 += (vlc_value & 0xF) << 2;
                vlc_value = vlc.advance(4);
            }
            if u_q1 > 32 {
                u_q1 += (vlc_value & 0xF) << 2;
                vlc.advance(4);
            }
            scratch[sp + 1] = u_q0 as u16;
            scratch[sp + 3] = u_q1 as u16;
            sp += 4;
        }
        scratch[sp] = 0;
        scratch[sp + 1] = 0;
        y += 2;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_mag_sgn(
    data: &[u8],
    width: usize,
    height: usize,
    sstr: usize,
    scratch: &[u16],
    bit_plane: i32,
    mmsbp2: u32,
    coefficients: &mut [i32],
    v_scratch: &mut [u32],
) -> Result<(), String> {
    let mut reader = ForwardSegmentReader::new(data, data.len(), 0xFF);
    decode_mag_sgn_row(
        &mut reader,
        coefficients,
        width,
        height,
        sstr,
        scratch,
        v_scratch,
        0,
        bit_plane,
        mmsbp2,
        true,
    )?;
    let mut y = 2usize;
    while y < height {
        decode_mag_sgn_row(
            &mut reader,
            coefficients,
            width,
            height,
            sstr,
            scratch,
            v_scratch,
            y,
            bit_plane,
            mmsbp2,
            false,
        )?;
        y += 2;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_mag_sgn_row(
    reader: &mut ForwardSegmentReader<'_>,
    coefficients: &mut [i32],
    width: usize,
    height: usize,
    sstr: usize,
    scratch: &[u16],
    v_scratch: &mut [u32],
    y: usize,
    bit_plane: i32,
    mmsbp2: u32,
    initial: bool,
) -> Result<(), String> {
    let row_start = (y >> 1) * sstr;
    let scratch = &scratch[row_start..row_start + width.div_ceil(2) * 2];
    let row_count = (height - y).min(2);
    let rows = &mut coefficients[y * width..(y + row_count) * width];
    let (top, bottom) = rows.split_at_mut(width);
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if !bottom.is_empty() && crate::wavelet::avx2_available() {
        // SAFETY: AVX2 was detected and the row pair is complete.
        return unsafe {
            decode_row_pair_avx2(
                reader, top, bottom, scratch, v_scratch, y, bit_plane, mmsbp2, initial,
            )
        };
    }
    if bottom.is_empty() {
        decode_mag_sgn_row_pairs::<false, false>(
            reader, top, bottom, scratch, v_scratch, y, bit_plane, mmsbp2, initial,
        )
    } else {
        decode_mag_sgn_row_pairs::<true, false>(
            reader, top, bottom, scratch, v_scratch, y, bit_plane, mmsbp2, initial,
        )
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn decode_row_pair_avx2(
    reader: &mut ForwardSegmentReader<'_>,
    top: &mut [i32],
    bottom: &mut [i32],
    scratch: &[u16],
    v_scratch: &mut [u32],
    y: usize,
    bit_plane: i32,
    mmsbp2: u32,
    initial: bool,
) -> Result<(), String> {
    decode_mag_sgn_row_pairs::<true, true>(
        reader, top, bottom, scratch, v_scratch, y, bit_plane, mmsbp2, initial,
    )
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn decode_mag_sgn_row_pairs<const TWO_ROWS: bool, const AVX2: bool>(
    reader: &mut ForwardSegmentReader<'_>,
    top: &mut [i32],
    bottom: &mut [i32],
    scratch: &[u16],
    v_scratch: &mut [u32],
    y: usize,
    bit_plane: i32,
    mmsbp2: u32,
    initial: bool,
) -> Result<(), String> {
    let mut prev_vn = 0u32;
    let mut bottom_pairs = bottom.chunks_mut(2);
    let quad_count = top.len().div_ceil(2);
    let quads = top.chunks_mut(2).zip(scratch.as_chunks::<2>().0);
    for (quad, (top, state)) in quads.enumerate() {
        let inf = state[0];
        let uq = u32::from(state[1]);
        let u = if initial {
            uq
        } else {
            uq + kappa(inf, v_scratch[quad], v_scratch[quad + 1])
        };
        if u > mmsbp2 {
            let x = 2 * quad;
            return Err(format!(
                "HTJ2K cleanup pass has an invalid U value (U={u}, max={mmsbp2}, x={x}, y={y})"
            ));
        }
        let bottom = if TWO_ROWS { bottom_pairs.next() } else { None };
        let mut left_vn = 0;
        let previous_right_vn = prev_vn;
        prev_vn = 0;
        if let Some(bottom) = bottom {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            if AVX2 && top.len() == 2 && (1..=8).contains(&u) {
                // SAFETY: AVX2 was detected before entering the loop.
                let (samples, magnitudes) =
                    unsafe { decode_short_quad_avx2(reader, inf, u, bit_plane) };
                [top[0], bottom[0], top[1], bottom[1]] = samples;
                v_scratch[quad] = previous_right_vn | magnitudes[0];
                prev_vn = magnitudes[1];
                continue;
            }
            if top.len() == 2 && inf & 0xF0 == 0xF0 && (1..=8).contains(&u) {
                let (top_samples, bottom_samples, magnitudes) =
                    decode_dense_short_quad(reader, inf, u, bit_plane);
                top.copy_from_slice(&top_samples);
                bottom.copy_from_slice(&bottom_samples);
                [left_vn, prev_vn] = magnitudes;
            } else {
                top[0] = decode_sample::<0>(reader, inf, u, bit_plane).0;
                (bottom[0], left_vn) = decode_sample::<1>(reader, inf, u, bit_plane);
                if let [_, right] = top {
                    *right = decode_sample::<2>(reader, inf, u, bit_plane).0;
                    (bottom[1], prev_vn) = decode_sample::<3>(reader, inf, u, bit_plane);
                }
            }
        } else {
            top[0] = decode_sample::<0>(reader, inf, u, bit_plane).0;
            if let [_, right] = top {
                *right = decode_sample::<2>(reader, inf, u, bit_plane).0;
            }
        }
        v_scratch[quad] = previous_right_vn | left_vn;
    }
    v_scratch[quad_count] = prev_vn;
    Ok(())
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const SHORT_QUAD_ENDS: [[[u32; 16]; 16]; 8] = {
    // Each byte is the end offset of one of the four magnitude fields.
    let mut ends = [[[0; 16]; 16]; 8];
    let mut u = 1;
    while u <= 8 {
        let mut rho = 0;
        while rho < 16 {
            let mut embedded = 0;
            while embedded < 16 {
                let mut end = 0;
                let mut lane = 0;
                while lane < 4 {
                    if rho & (1 << lane) != 0 {
                        end += u - ((embedded >> lane) & 1);
                    }
                    ends[u - 1][rho][embedded] |= (end as u32) << (lane * 8);
                    lane += 1;
                }
                embedded += 1;
            }
            rho += 1;
        }
        u += 1;
    }
    ends
};

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn decode_short_quad_avx2(
    reader: &mut ForwardSegmentReader<'_>,
    inf: u16,
    u: u32,
    bit_plane: i32,
) -> ([i32; 4], [u32; 2]) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    if inf & 0xf0 == 0 {
        return ([0; 4], [0; 2]);
    }
    let info = _mm_set1_epi32(i32::from(inf));
    let lanes = _mm_setr_epi32(0, 1, 2, 3);
    let ones = _mm_set1_epi32(1);
    let sig_bits = _mm_setr_epi32(0x10, 0x20, 0x40, 0x80);
    let significance = _mm_cmpeq_epi32(_mm_and_si128(info, sig_bits), sig_bits);
    let ends =
        SHORT_QUAD_ENDS[u as usize - 1][usize::from((inf >> 4) & 15)][usize::from(inf >> 12)];
    let total = ends >> 24;
    let sums = _mm_cvtepu8_epi32(_mm_cvtsi32_si128(ends as i32));
    let starts = _mm_slli_si128::<4>(sums);
    let widths = _mm_sub_epi32(sums, starts);
    // A significant sample with zero width still reads its sign bit.
    let stream = _mm_set1_epi32(reader.fetch((total + 1).min(32)) as i32);
    reader.advance(total);
    let raw = _mm_srlv_epi32(stream, starts);
    let sign = _mm_slli_epi32::<31>(raw);
    let mask = _mm_sub_epi32(_mm_sllv_epi32(ones, widths), ones);
    let e_1 = _mm_and_si128(_mm_srlv_epi32(_mm_srli_epi32::<8>(info), lanes), ones);
    let vn = _mm_or_si128(
        _mm_or_si128(_mm_and_si128(raw, mask), _mm_sllv_epi32(e_1, widths)),
        ones,
    );
    let magnitude = _mm_sll_epi32(
        _mm_add_epi32(vn, _mm_set1_epi32(2)),
        _mm_cvtsi32_si128(bit_plane - 1),
    );
    let result = _mm_and_si128(_mm_or_si128(magnitude, sign), significance);
    let vn = _mm_and_si128(vn, significance);
    let mut samples = [0; 4];
    // SAFETY: The array has four lanes and the store allows unaligned data.
    unsafe { _mm_storeu_si128(samples.as_mut_ptr().cast(), result) };
    (
        samples,
        [
            _mm_extract_epi32::<1>(vn) as u32,
            _mm_extract_epi32::<3>(vn) as u32,
        ],
    )
}

#[inline]
fn decode_dense_short_quad(
    reader: &mut ForwardSegmentReader<'_>,
    inf: u16,
    uq: u32,
    bit_plane: i32,
) -> ([i32; 2], [i32; 2], [u32; 2]) {
    let embedding = u32::from(inf >> 12);
    let m0 = uq - (embedding & 1);
    let m1 = uq - ((embedding >> 1) & 1);
    let m2 = uq - ((embedding >> 2) & 1);
    let m3 = uq - (embedding >> 3);
    let consumed = m0 + m1 + m2 + m3;
    // When the final sample consumes zero bits, its sign is the next bit.
    let word = reader.fetch(consumed + u32::from(m3 == 0));
    reader.advance(consumed);
    let (x0, _) = decode_sample_word::<0>(word, inf, m0, bit_plane);
    let (x1, v1) = decode_sample_word::<1>(word >> m0, inf, m1, bit_plane);
    let (x2, _) = decode_sample_word::<2>(word >> (m0 + m1), inf, m2, bit_plane);
    let (x3, v3) = decode_sample_word::<3>(word >> (m0 + m1 + m2), inf, m3, bit_plane);
    ([x0, x2], [x1, x3], [v1, v3])
}

#[inline]
fn decode_sample<const BIT: u32>(
    reader: &mut ForwardSegmentReader<'_>,
    inf: u16,
    uq: u32,
    bit_plane: i32,
) -> (i32, u32) {
    if inf & (1 << (4 + BIT)) == 0 {
        return (0, 0);
    }
    let mn = uq - u32::from((inf >> (12 + BIT)) & 1);
    // A zero-length magnitude still inspects one sign bit without consuming it.
    let mag_sgn = reader.fetch(mn.max(1));
    reader.advance(mn);
    decode_sample_word::<BIT>(mag_sgn, inf, mn, bit_plane)
}

#[inline]
fn decode_sample_word<const BIT: u32>(
    mag_sgn: u32,
    inf: u16,
    mn: u32,
    bit_plane: i32,
) -> (i32, u32) {
    let sign = (mag_sgn & 1) << 31;
    let vn = (mag_sgn & mask_low_bits(mn)) | (u32::from((inf >> (8 + BIT)) & 1) << mn) | 1;
    let magnitude = (vn + 2) << (bit_plane - 1);
    ((sign | magnitude) as i32, vn)
}

fn kappa(inf: u16, current_vn: u32, next_vn: u32) -> u32 {
    let mut gamma = u32::from(inf) & 0xF0;
    gamma &= gamma.wrapping_sub(0x10);
    if gamma == 0 {
        1
    } else {
        31 - ((current_vn | next_vn) | 2).leading_zeros()
    }
}

fn mask_low_bits(count: u32) -> u32 {
    if count >= 32 {
        u32::MAX
    } else {
        (1u32 << count) - 1
    }
}

fn read_le32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

struct MelSegmentReader<'a> {
    data: &'a [u8],
    offset: usize,
    tmp: u64,
    bits: i32,
    size: i32,
    unstuff: bool,
    k: usize,
    num_runs: u32,
    runs: u64,
}

impl<'a> MelSegmentReader<'a> {
    fn new(data: &'a [u8], lcup: usize, scup: usize) -> Self {
        let mut reader = Self {
            data,
            offset: lcup - scup,
            tmp: 0,
            bits: 0,
            size: scup as i32 - 1,
            unstuff: false,
            k: 0,
            num_runs: 0,
            runs: 0,
        };
        let count = reader.size.clamp(0, 4) + if reader.size == 0 { 1 } else { 0 };
        for _ in 0..count {
            let mut d = if reader.size > 0 {
                reader.data[reader.offset]
            } else {
                0xFF
            };
            if reader.size == 1 {
                d |= 0x0F;
            }
            if reader.size > 0 {
                reader.offset += 1;
                reader.size -= 1;
            }
            let d_bits = 8 - i32::from(reader.unstuff);
            reader.tmp = (reader.tmp << d_bits) | u64::from(d);
            reader.bits += d_bits;
            reader.unstuff = d == 0xFF;
        }
        reader.tmp <<= 64 - reader.bits;
        reader
    }

    fn get_run(&mut self) -> i32 {
        if self.num_runs == 0 {
            self.decode();
        }
        let run = (self.runs & 0x7F) as i32;
        self.runs >>= 7;
        self.num_runs -= 1;
        run
    }

    fn decode(&mut self) {
        if self.bits < 6 {
            self.read();
        }
        while self.bits >= 6 && self.num_runs < 8 {
            let exponent = MEL_EXPONENTS[self.k];
            let mut run;
            if self.tmp & (1u64 << 63) != 0 {
                run = (1 << exponent) - 1;
                self.k = (self.k + 1).min(12);
                self.tmp <<= 1;
                self.bits -= 1;
                run <<= 1;
            } else {
                run = ((self.tmp >> (63 - exponent)) & ((1u64 << exponent) - 1)) as i32;
                self.k = self.k.saturating_sub(1);
                self.tmp <<= exponent + 1;
                self.bits -= exponent as i32 + 1;
                run = (run << 1) + 1;
            }
            let shift = self.num_runs * 7;
            self.runs &= !(0x3Fu64 << shift);
            self.runs |= (run as u64) << shift;
            self.num_runs += 1;
        }
    }

    fn read(&mut self) {
        if self.bits > 32 {
            return;
        }
        let mut value = 0xFFFF_FFFFu32;
        if self.size > 4 {
            value = read_le32(self.data, self.offset);
            self.offset += 4;
            self.size -= 4;
        } else if self.size > 0 {
            let mut shift = 0;
            while self.size > 1 {
                value =
                    (value & !(0xFFu32 << shift)) | (u32::from(self.data[self.offset]) << shift);
                self.offset += 1;
                self.size -= 1;
                shift += 8;
            }
            let v = u32::from(self.data[self.offset] | 0x0F);
            value = (value & !(0xFFu32 << shift)) | (v << shift);
            self.offset += 1;
            self.size -= 1;
        }

        let mut bits = 32 - i32::from(self.unstuff);
        let mut t = value & 0xFF;
        let mut unstuff = (value & 0xFF) == 0xFF;
        bits -= i32::from(unstuff);
        t <<= 8 - u32::from(unstuff);
        t |= (value >> 8) & 0xFF;
        unstuff = ((value >> 8) & 0xFF) == 0xFF;
        bits -= i32::from(unstuff);
        t <<= 8 - u32::from(unstuff);
        t |= (value >> 16) & 0xFF;
        unstuff = ((value >> 16) & 0xFF) == 0xFF;
        bits -= i32::from(unstuff);
        t <<= 8 - u32::from(unstuff);
        t |= (value >> 24) & 0xFF;
        self.unstuff = ((value >> 24) & 0xFF) == 0xFF;

        self.tmp |= u64::from(t) << (64 - bits as u32 - self.bits as u32);
        self.bits += bits;
    }
}

struct ReverseSegmentReader<'a> {
    data: &'a [u8],
    offset: isize,
    tmp: u64,
    bits: i32,
    size: i32,
    unstuff: bool,
}

impl<'a> ReverseSegmentReader<'a> {
    fn new(data: &'a [u8], lcup: usize, scup: usize) -> Self {
        let mut reader = Self {
            data,
            offset: (lcup - 2) as isize,
            tmp: 0,
            bits: 0,
            size: scup as i32 - 2,
            unstuff: false,
        };
        let d = reader.data[reader.offset as usize];
        reader.offset -= 1;
        reader.tmp = u64::from(d >> 4);
        reader.bits = 4 - i32::from((reader.tmp & 7) == 7);
        reader.unstuff = d > 0x8F;
        reader.read();
        reader
    }

    fn fetch(&mut self) -> u32 {
        if self.bits < 32 {
            self.read();
            if self.bits < 32 {
                self.read();
            }
        }
        self.tmp as u32
    }

    fn advance(&mut self, bits: u32) -> u32 {
        self.tmp >>= bits;
        self.bits -= bits as i32;
        self.tmp as u32
    }

    fn read(&mut self) {
        if self.bits > 32 {
            return;
        }
        let mut value = 0u32;
        if self.size > 3 {
            value = read_le32(self.data, (self.offset - 3) as usize);
            self.offset -= 4;
            self.size -= 4;
        } else if self.size > 0 {
            let mut shift = 24;
            while self.size > 0 {
                value |= u32::from(self.data[self.offset as usize]) << shift;
                self.offset -= 1;
                self.size -= 1;
                shift -= 8;
            }
        }

        // A stuffed bit requires seven trailing ones in the following byte.
        let candidates = !value & 0x7F7F_7F7F;
        if candidates.wrapping_sub(0x0101_0101) & !candidates & 0x8080_8080 == 0 {
            self.tmp |= u64::from(value.swap_bytes()) << self.bits;
            self.bits += 32;
            self.unstuff = (value & 0xFF) > 0x8F;
            return;
        }

        let mut tmp = value >> 24;
        let mut bits = 8 - i32::from(self.unstuff && ((value >> 24) & 0x7F) == 0x7F);
        let mut unstuff = (value >> 24) > 0x8F;
        tmp |= ((value >> 16) & 0xFF) << bits;
        bits += 8 - i32::from(unstuff && ((value >> 16) & 0x7F) == 0x7F);
        unstuff = ((value >> 16) & 0xFF) > 0x8F;
        tmp |= ((value >> 8) & 0xFF) << bits;
        bits += 8 - i32::from(unstuff && ((value >> 8) & 0x7F) == 0x7F);
        unstuff = ((value >> 8) & 0xFF) > 0x8F;
        tmp |= (value & 0xFF) << bits;
        bits += 8 - i32::from(unstuff && (value & 0x7F) == 0x7F);
        self.unstuff = (value & 0xFF) > 0x8F;
        self.tmp |= u64::from(tmp) << self.bits;
        self.bits += bits;
    }
}

struct ForwardSegmentReader<'a> {
    data: &'a [u8],
    offset: usize,
    size: usize,
    unstuff_limit: u8,
    tmp: u64,
    bits: i32,
    last_byte: i32,
}

impl<'a> ForwardSegmentReader<'a> {
    fn new(data: &'a [u8], size: usize, unstuff_limit: u8) -> Self {
        let mut reader = Self {
            data,
            offset: 0,
            size,
            unstuff_limit,
            tmp: 0,
            bits: 0,
            last_byte: -1,
        };
        for _ in 0..4 {
            reader.read_byte();
        }
        reader.read_word();
        reader
    }

    fn fetch(&mut self, count: u32) -> u32 {
        while self.bits < count as i32 {
            self.read_word();
        }
        self.tmp as u32
    }

    fn advance(&mut self, bits: u32) {
        self.tmp >>= bits;
        self.bits -= bits as i32;
    }

    fn read_byte(&mut self) {
        let value = if self.offset < self.size {
            self.data[self.offset]
        } else {
            self.unstuff_limit
        };
        if self.offset < self.size {
            self.offset += 1;
        }
        let bits = if self.last_byte == i32::from(self.unstuff_limit) {
            7
        } else {
            8
        };
        let mask = if bits == 8 { 0xFFu8 } else { (1u8 << bits) - 1 };
        self.tmp |= u64::from(value & mask) << self.bits;
        self.bits += bits;
        self.last_byte = i32::from(value);
    }

    fn read_word(&mut self) {
        if self.bits > 32 {
            return;
        }
        // The common case needs no stuffed-bit removal between bytes.
        if self.last_byte != i32::from(self.unstuff_limit) && self.size - self.offset >= 4 {
            let word = read_le32(self.data, self.offset);
            let repeated_marker = u32::from(self.unstuff_limit) * 0x0101_0101;
            let match_byte = word ^ repeated_marker;
            let has_stuffing =
                match_byte.wrapping_sub(0x0101_0101) & !match_byte & 0x8080_8080 != 0;
            if !has_stuffing {
                self.tmp |= u64::from(word) << self.bits;
                self.bits += 32;
                self.offset += 4;
                self.last_byte = (word >> 24) as i32;
                return;
            }
        }
        for _ in 0..4 {
            self.read_byte();
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

fn vlc_encode0() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| build_vlc_encode_table(VLC_TABLE_0_SOURCE))
}

fn vlc_encode1() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| build_vlc_encode_table(VLC_TABLE_1_SOURCE))
}

fn build_vlc_encode_table(source: &[u16]) -> Vec<u16> {
    let mut table = vec![0u16; 2048];
    for (i, slot) in table.iter_mut().enumerate() {
        let context = (i >> 8) as u16;
        let rho = ((i >> 4) & 0xF) as u16;
        let emb = (i & 0xF) as u16;
        if (emb & rho) != emb || (rho == 0 && context == 0) {
            continue;
        }
        let mut best = usize::MAX;
        if emb != 0 {
            let mut best_bits = -1i32;
            let mut offset = 0;
            while offset + 6 < source.len() {
                if source[offset] == context && source[offset + 1] == rho && source[offset + 2] == 1
                {
                    let embedding = source[offset + 3];
                    if (emb & embedding) == source[offset + 4] {
                        let bits = embedding.count_ones() as i32;
                        if bits >= best_bits {
                            best = offset;
                            best_bits = bits;
                        }
                    }
                }
                offset += 7;
            }
        } else {
            let mut offset = 0;
            while offset + 6 < source.len() {
                if source[offset] == context && source[offset + 1] == rho && source[offset + 2] == 0
                {
                    best = offset;
                    break;
                }
                offset += 7;
            }
        }
        if best != usize::MAX {
            *slot = (source[best + 5] << 8) | (source[best + 6] << 4) | source[best + 3];
        }
    }
    table
}

#[derive(Clone, Copy, Default)]
struct QuadInfo {
    rho: u16,
    u: i32,
    e_max: i32,
    eps: u16,
    bottom_exponents: [i32; 2],
    s: [u32; 4],
}

impl QuadInfo {
    fn with_non_initial_u(mut self, u: i32, kappa: i32) -> Self {
        self.u = u;
        if u <= kappa {
            self.eps = 0;
        }
        self
    }
}

#[derive(Clone, Copy)]
struct VlcTuple {
    codeword: u16,
    length: u32,
    embedding: u16,
}

struct UvlcTuple {
    prefix: u32,
    prefix_length: u32,
    suffix: u32,
    suffix_length: u32,
}

impl UvlcTuple {
    fn create(value: u32) -> Result<Self, String> {
        Ok(match value {
            0 => Self {
                prefix: 0,
                prefix_length: 0,
                suffix: 0,
                suffix_length: 0,
            },
            1 => Self {
                prefix: 1,
                prefix_length: 1,
                suffix: 0,
                suffix_length: 0,
            },
            2 => Self {
                prefix: 2,
                prefix_length: 2,
                suffix: 0,
                suffix_length: 0,
            },
            3 => Self {
                prefix: 4,
                prefix_length: 3,
                suffix: 0,
                suffix_length: 1,
            },
            4 => Self {
                prefix: 4,
                prefix_length: 3,
                suffix: 1,
                suffix_length: 1,
            },
            _ if value < 33 => Self {
                prefix: 0,
                prefix_length: 3,
                suffix: value - 5,
                suffix_length: 5,
            },
            _ => return Err("HTJ2K UVLC value is outside the implemented encoder range".into()),
        })
    }
}

/// Encodes one HT cleanup pass from sign-magnitude coefficients.
pub(crate) fn encode_cleanup(
    coefficients: &[i32],
    width: usize,
    height: usize,
    missing_msb: u32,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || coefficients.len() != width * height {
        return Err("HTJ2K cleanup pass dimensions are invalid".into());
    }
    let bit_plane = 30i32 - missing_msb as i32;
    if bit_plane <= 0 {
        return Err("HTJ2K cleanup pass requires a positive CUP bit-plane".into());
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if width >= 16 && crate::wavelet::avx2_available() {
        let mut mel = MelWriter::new();
        let mut vlc = ReverseVlcWriter::new();
        let mut ms = MagSgnWriter::new();
        // SAFETY: The CPU supports AVX2. The kernel pads partial groups before vector loads.
        unsafe {
            encode_avx2::encode_rows(coefficients, width, bit_plane, &mut mel, &mut vlc, &mut ms)
        };
        return Ok(finish_cleanup(mel, vlc, ms));
    }
    encode_quad_rows(coefficients, width, height, bit_plane)
}

fn encode_quad_rows(
    coefficients: &[i32],
    width: usize,
    height: usize,
    bit_plane: i32,
) -> Result<Vec<u8>, String> {
    let mut mel = MelWriter::new();
    let mut vlc = ReverseVlcWriter::new();
    let mut ms = MagSgnWriter::new();
    let mut e_values = vec![0i32; width.div_ceil(2) + 2];
    let mut context_values = vec![0i32; width.div_ceil(2) + 2];
    let mut quads = vec![QuadInfo::default(); width.div_ceil(2)];

    encode_initial_quad_row(
        coefficients,
        width,
        height,
        bit_plane,
        &mut e_values,
        &mut context_values,
        &mut quads,
        &mut mel,
        &mut vlc,
        &mut ms,
    )?;
    encode_non_initial_quad_rows(
        coefficients,
        width,
        height,
        bit_plane,
        &mut e_values,
        &mut context_values,
        &mut quads,
        &mut mel,
        &mut vlc,
        &mut ms,
    )?;

    Ok(finish_cleanup(mel, vlc, ms))
}

fn finish_cleanup(mut mel: MelWriter, mut vlc: ReverseVlcWriter, mut ms: MagSgnWriter) -> Vec<u8> {
    mel.terminate_with(&mut vlc);
    ms.terminate();
    assemble(ms.into_bytes(), mel.into_bytes(), vlc.into_bytes())
}

#[allow(clippy::too_many_arguments)]
fn encode_initial_quad_row(
    coefficients: &[i32],
    width: usize,
    height: usize,
    bit_plane: i32,
    e_values: &mut [i32],
    context_values: &mut [i32],
    quads: &mut [QuadInfo],
    mel: &mut MelWriter,
    vlc: &mut ReverseVlcWriter,
    ms: &mut MagSgnWriter,
) -> Result<(), String> {
    let mut context = 0u16;
    let mut x = 0usize;
    let table = vlc_encode0();
    let top = &coefficients[..width];
    let bottom = if height > 1 {
        &coefficients[width..2 * width]
    } else {
        &[]
    };
    prepare_quad_row(top, bottom, bit_plane, quads);
    while x < width {
        let first = quads[x / 2];
        let tuple0 = lookup_vlc(table, context, first.rho, first.eps)?;
        if context == 0 {
            mel.encode(first.rho != 0);
        }
        write_mag_sgn(ms, &first, tuple0);
        let uq0 = first.u - 1;
        store_initial_state(&first, e_values, context_values, x / 2);

        let mut uq1 = 0i32;
        let mut second_code = (0, 0);
        if x + 2 < width {
            let context1 = (first.rho >> 1) | (first.rho & 1);
            let second = quads[x / 2 + 1];
            let tuple1 = lookup_vlc(table, context1, second.rho, second.eps)?;
            second_code = (u32::from(tuple1.codeword), tuple1.length);
            if context1 == 0 {
                mel.encode(second.rho != 0);
            }
            write_mag_sgn(ms, &second, tuple1);
            uq1 = second.u - 1;
            store_initial_state(&second, e_values, context_values, (x + 2) / 2);
            context = (second.rho >> 1) | (second.rho & 1);
        } else {
            context = 0;
        }

        if uq0 > 0 && uq1 > 0 {
            mel.encode(uq0.min(uq1) > 2);
        }
        let (uvlc, uvlc_length) = initial_uvlc_code(uq0, uq1)?;
        let (second_word, second_length) = second_code;
        let tuple_length = tuple0.length + second_length;
        let codeword =
            u32::from(tuple0.codeword) | (second_word << tuple0.length) | (uvlc << tuple_length);
        debug_assert!(tuple_length + uvlc_length <= 32);
        vlc.encode(codeword, tuple_length + uvlc_length);
        x += 4;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_non_initial_quad_rows(
    coefficients: &[i32],
    width: usize,
    height: usize,
    bit_plane: i32,
    e_values: &mut [i32],
    context_values: &mut [i32],
    quads: &mut [QuadInfo],
    mel: &mut MelWriter,
    vlc: &mut ReverseVlcWriter,
    ms: &mut MagSgnWriter,
) -> Result<(), String> {
    let table = vlc_encode1();
    let mut y = 2usize;
    while y < height {
        let top = &coefficients[y * width..(y + 1) * width];
        let bottom = if y + 1 < height {
            &coefficients[(y + 1) * width..(y + 2) * width]
        } else {
            &[]
        };
        let mut e_index = 0usize;
        let mut max_e = e_values[0].max(e_values[1]) - 1;
        e_values[0] = 0;
        let mut context_index = 0usize;
        let mut context0 = context_values[0] + (context_values[1] << 2);
        context_values[0] = 0;

        let mut x = 0usize;
        prepare_quad_row(top, bottom, bit_plane, quads);
        while x < width {
            let mut first = quads[x / 2];
            let kappa0 = if (first.rho & first.rho.wrapping_sub(1)) != 0 {
                max_e.max(1)
            } else {
                1
            };
            first = first.with_non_initial_u(first.e_max.max(kappa0), kappa0);
            let uq0 = first.u - kappa0;
            let tuple0 = lookup_vlc(table, context0 as u16, first.rho, first.eps)?;
            if context0 == 0 {
                mel.encode(first.rho != 0);
            }
            write_mag_sgn(ms, &first, tuple0);
            e_values[e_index] = e_values[e_index].max(first.bottom_exponents[0]);
            e_index += 1;
            max_e = e_values[e_index].max(e_values[e_index + 1]) - 1;
            e_values[e_index] = first.bottom_exponents[1];
            context_values[context_index] |= i32::from((first.rho & 2) >> 1);
            context_index += 1;
            let mut context1 =
                context_values[context_index] + (context_values[context_index + 1] << 2);
            context_values[context_index] = i32::from((first.rho & 8) >> 3);

            let mut uq1 = 0i32;
            let mut second_code = (0, 0);
            let mut second = QuadInfo::default();
            if x + 2 < width {
                second = quads[x / 2 + 1];
                let kappa1 = if (second.rho & second.rho.wrapping_sub(1)) != 0 {
                    max_e.max(1)
                } else {
                    1
                };
                context1 |= i32::from((first.rho & 4) >> 1) | i32::from((first.rho & 8) >> 2);
                second = second.with_non_initial_u(second.e_max.max(kappa1), kappa1);
                uq1 = second.u - kappa1;
                let tuple1 = lookup_vlc(table, context1 as u16, second.rho, second.eps)?;
                second_code = (u32::from(tuple1.codeword), tuple1.length);
                if context1 == 0 {
                    mel.encode(second.rho != 0);
                }
                write_mag_sgn(ms, &second, tuple1);
                e_values[e_index] = e_values[e_index].max(second.bottom_exponents[0]);
                e_index += 1;
                max_e = e_values[e_index].max(e_values[e_index + 1]) - 1;
                e_values[e_index] = second.bottom_exponents[1];
                context_values[context_index] |= i32::from((second.rho & 2) >> 1);
                context_index += 1;
                context0 = context_values[context_index] + (context_values[context_index + 1] << 2);
                context_values[context_index] = i32::from((second.rho & 8) >> 3);
            }

            let (uvlc, uvlc_length) = uvlc_code(uq0, uq1)?;
            let (second_word, second_length) = second_code;
            let tuple_length = tuple0.length + second_length;
            let codeword = u32::from(tuple0.codeword)
                | (second_word << tuple0.length)
                | (uvlc << tuple_length);
            debug_assert!(tuple_length + uvlc_length <= 32);
            vlc.encode(codeword, tuple_length + uvlc_length);
            context0 |= i32::from((second.rho & 4) >> 1) | i32::from((second.rho & 8) >> 2);
            x += 4;
        }
        y += 2;
    }
    Ok(())
}

fn store_initial_state(
    quad: &QuadInfo,
    e_values: &mut [i32],
    context_values: &mut [i32],
    index: usize,
) {
    e_values[index] = e_values[index].max(quad.bottom_exponents[0]);
    e_values[index + 1] = quad.bottom_exponents[1];
    context_values[index] |= i32::from((quad.rho & 2) >> 1);
    context_values[index + 1] = i32::from((quad.rho & 8) >> 3);
}

fn prepare_quad_row(top: &[i32], bottom: &[i32], bit_plane: i32, quads: &mut [QuadInfo]) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let start = if bottom.len() >= 4 && crate::wavelet::avx2_available() {
        // SAFETY: Both rows contain four samples per complete pair of quads.
        unsafe { prepare_quad_pairs_avx2(top, bottom, bit_plane, quads) }
    } else {
        0
    };
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    let start = 0;
    for (index, quad) in quads.iter_mut().enumerate().skip(start) {
        *quad = read_quad(top, bottom, index * 2, bit_plane);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn prepare_quad_pairs_avx2(
    top: &[i32],
    bottom: &[i32],
    bit_plane: i32,
    quads: &mut [QuadInfo],
) -> usize {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let two = _mm256_set1_epi32(2);
    let shift = _mm_cvtsi32_si128(bit_plane);
    let mut count = 0;
    for ((top, bottom), pair) in top
        .as_chunks::<4>()
        .0
        .iter()
        .zip(bottom.as_chunks::<4>().0)
        .zip(quads.as_chunks_mut::<2>().0)
    {
        // SAFETY: The input chunks each contain four adjacent i32 samples.
        let samples = unsafe {
            let t = _mm_loadu_si128(top.as_ptr().cast());
            let b = _mm_loadu_si128(bottom.as_ptr().cast());
            _mm256_set_m128i(_mm_unpackhi_epi32(t, b), _mm_unpacklo_epi32(t, b))
        };
        let value = _mm256_andnot_si256(
            one,
            _mm256_srl_epi32(_mm256_add_epi32(samples, samples), shift),
        );
        let code = _mm256_or_si256(
            _mm256_sub_epi32(value, two),
            _mm256_srli_epi32::<31>(samples),
        );
        let magnitude = _mm256_max_epi32(zero, _mm256_sub_epi32(value, one));
        // Retain the leading bit. Dropping eight low bits above 2^24 keeps
        // the integer-to-float conversion exact, even just below powers of two.
        let small = _mm256_cmpeq_epi32(_mm256_srli_epi32::<24>(magnitude), zero);
        let exact = _mm256_and_si256(magnitude, _mm256_or_si256(small, _mm256_set1_epi32(!255)));
        let exp = _mm256_max_epi32(
            zero,
            _mm256_sub_epi32(
                _mm256_srli_epi32::<23>(_mm256_castps_si256(_mm256_cvtepi32_ps(exact))),
                _mm256_set1_epi32(126),
            ),
        );
        let m = _mm256_max_epi32(exp, _mm256_shuffle_epi32::<0xb1>(exp));
        let m = _mm256_max_epi32(m, _mm256_shuffle_epi32::<0x4e>(m));
        let mask = |v| _mm256_movemask_ps(_mm256_castsi256_ps(v)) as u16;
        let rho = mask(_mm256_cmpgt_epi32(exp, zero));
        let eps = mask(_mm256_and_si256(
            _mm256_cmpeq_epi32(exp, m),
            _mm256_cmpgt_epi32(m, one),
        ));
        let mut codes = [[0u32; 4]; 2];
        let mut exponents = [[0i32; 4]; 2];
        // SAFETY: Each array contains eight lanes and permits unaligned stores.
        unsafe {
            _mm256_storeu_si256(codes.as_mut_ptr().cast(), code);
            _mm256_storeu_si256(exponents.as_mut_ptr().cast(), exp);
        }
        let maxima = [_mm256_extract_epi32::<0>(m), _mm256_extract_epi32::<4>(m)];
        for i in 0..2 {
            pair[i] = QuadInfo {
                rho: (rho >> (4 * i)) & 15,
                u: maxima[i].max(1),
                e_max: maxima[i],
                eps: (eps >> (4 * i)) & 15,
                bottom_exponents: [exponents[i][1], exponents[i][3]],
                s: codes[i],
            };
        }
        count += 2;
    }
    count
}

#[inline]
fn read_quad(top: &[i32], bottom: &[i32], x: usize, bit_plane: i32) -> QuadInfo {
    let (e0, s0) = sample_exponent_and_code(top[x], bit_plane);
    let (e1, s1) = sample_exponent_and_code(bottom.get(x).copied().unwrap_or(0), bit_plane);
    let (e2, s2) = sample_exponent_and_code(top.get(x + 1).copied().unwrap_or(0), bit_plane);
    let (e3, s3) = sample_exponent_and_code(bottom.get(x + 1).copied().unwrap_or(0), bit_plane);
    let e = [e0, e1, e2, e3];
    let s = [s0, s1, s2, s3];
    let rho = u16::from(e0 != 0)
        | (u16::from(e1 != 0) << 1)
        | (u16::from(e2 != 0) << 2)
        | (u16::from(e3 != 0) << 3);
    let e_max = e[0].max(e[1]).max(e[2].max(e[3]));
    let eps = build_eps(&e, e_max);
    QuadInfo {
        rho,
        u: e_max.max(1),
        e_max,
        eps,
        bottom_exponents: [e1, e3],
        s,
    }
}

#[inline]
fn sample_exponent_and_code(sample: i32, bit_plane: i32) -> (i32, u32) {
    let sample = sample as u32;
    let value = (sample.wrapping_add(sample) >> bit_plane) & !1;
    let exponent = (32 - value.saturating_sub(1).leading_zeros()) as i32;
    // Insignificant samples have exponent zero and never emit a code.
    (exponent, value.wrapping_sub(2) | (sample >> 31))
}

fn build_eps(e: &[i32; 4], e_max: i32) -> u16 {
    if e_max <= 1 {
        return 0;
    }
    u16::from(e[0] == e_max)
        | (u16::from(e[1] == e_max) << 1)
        | (u16::from(e[2] == e_max) << 2)
        | (u16::from(e[3] == e_max) << 3)
}

fn write_mag_sgn(writer: &mut MagSgnWriter, quad: &QuadInfo, tuple: VlcTuple) {
    if (1..=8).contains(&quad.u) {
        // Four fields of at most eight bits fit in one writer word.
        let mut word = 0;
        let mut length = 0;
        let mut significant = quad.rho;
        while significant != 0 {
            let sample = significant.trailing_zeros();
            significant &= significant - 1;
            let bits = quad.u as u32 - u32::from((tuple.embedding >> sample) & 1);
            let mask = (1u32 << bits) - 1;
            word |= (quad.s[sample as usize] & mask) << length;
            length += bits;
        }
        writer.encode(word, length);
        return;
    }
    for bit in 0..4 {
        if (quad.rho & (1 << bit)) == 0 {
            continue;
        }
        let m = quad.u - i32::from((tuple.embedding >> bit) & 1);
        let mask = if m >= 32 { u32::MAX } else { (1u32 << m) - 1 };
        writer.encode(quad.s[bit] & mask, m as u32);
    }
}

fn initial_uvlc_code(uq0: i32, uq1: i32) -> Result<(u32, u32), String> {
    if uq0 > 2 && uq1 > 2 {
        return uvlc_code(uq0 - 2, uq1 - 2);
    }
    if uq0 > 2 && uq1 > 0 {
        let first = UvlcTuple::create(uq0 as u32)?;
        let prefix_length = first.prefix_length + 1;
        let codeword = first.prefix
            | (((uq1 - 1) as u32) << first.prefix_length)
            | (first.suffix << prefix_length);
        return Ok((codeword, prefix_length + first.suffix_length));
    }
    uvlc_code(uq0, uq1)
}

fn uvlc_code(uq0: i32, uq1: i32) -> Result<(u32, u32), String> {
    let first = UvlcTuple::create(uq0 as u32)?;
    let second = UvlcTuple::create(uq1 as u32)?;
    let prefix_length = first.prefix_length + second.prefix_length;
    let codeword = first.prefix
        | (second.prefix << first.prefix_length)
        | (first.suffix << prefix_length)
        | (second.suffix << (prefix_length + first.suffix_length));
    Ok((
        codeword,
        prefix_length + first.suffix_length + second.suffix_length,
    ))
}

fn lookup_vlc(table: &[u16], context: u16, rho: u16, eps: u16) -> Result<VlcTuple, String> {
    let index = ((context as usize) << 8) | ((rho as usize) << 4) | eps as usize;
    let packed = table.get(index).copied().unwrap_or(0);
    if packed == 0 && !(context == 0 && rho == 0) {
        return Err(format!(
            "HTJ2K standard VLC encode tuple is not available for context {context}, rho 0x{rho:X}, eps 0x{eps:X}"
        ));
    }
    Ok(VlcTuple {
        codeword: packed >> 8,
        length: u32::from((packed >> 4) & 0x7),
        embedding: packed & 0xF,
    })
}

fn assemble(mut mag_sgn: Vec<u8>, mel: Vec<u8>, vlc: Vec<u8>) -> Vec<u8> {
    let scup = mel.len() + vlc.len();
    mag_sgn.reserve(scup);
    mag_sgn.extend_from_slice(&mel);
    mag_sgn.extend_from_slice(&vlc);
    let last = mag_sgn.len() - 1;
    mag_sgn[last] = (scup >> 4) as u8;
    mag_sgn[last - 1] = (mag_sgn[last - 1] & 0xF0) | (scup & 0x0F) as u8;
    mag_sgn
}

struct MelWriter {
    bytes: Vec<u8>,
    remaining_bits: u8,
    tmp: u8,
    run: u32,
    k: usize,
    threshold: u32,
}

impl MelWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            remaining_bits: 8,
            tmp: 0,
            run: 0,
            k: 0,
            threshold: 1,
        }
    }

    fn encode(&mut self, bit: bool) {
        if !bit {
            self.run += 1;
            if self.run < self.threshold {
                return;
            }
            self.emit_bit(1);
            self.run = 0;
            self.k = (self.k + 1).min(12);
            self.threshold = 1 << MEL_EXPONENTS[self.k];
            return;
        }
        self.emit_bit(0);
        let mut t = MEL_EXPONENTS[self.k];
        while t > 0 {
            t -= 1;
            self.emit_bit(u8::from((self.run >> t) & 1 != 0));
        }
        self.run = 0;
        self.k = self.k.saturating_sub(1);
        self.threshold = 1 << MEL_EXPONENTS[self.k];
    }

    fn terminate_with(&mut self, vlc: &mut ReverseVlcWriter) {
        vlc.flush_bytes();
        if self.run > 0 {
            self.emit_bit(1);
        }
        let mel_tmp = u16::from(self.tmp) << self.remaining_bits;
        let mel_mask = (0xFFu16 << self.remaining_bits) & 0xFF;
        let vlc_mask = 0xFFu16 >> (8 - vlc.used_bits);
        if (mel_mask | vlc_mask) == 0 {
            return;
        }
        let vlc_tmp = vlc.tmp as u16;
        let fuse = mel_tmp | vlc_tmp;
        if (((fuse ^ mel_tmp) & mel_mask) | ((fuse ^ vlc_tmp) & vlc_mask)) == 0
            && fuse != 0xFF
            && !vlc.tail.is_empty()
        {
            self.bytes.push(fuse as u8);
        } else {
            self.bytes.push(mel_tmp as u8);
            vlc.flush_tmp();
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn emit_bit(&mut self, bit: u8) {
        self.tmp = (self.tmp << 1) + bit;
        self.remaining_bits -= 1;
        if self.remaining_bits != 0 {
            return;
        }
        self.bytes.push(self.tmp);
        self.remaining_bits = if self.tmp == 0xFF { 7 } else { 8 };
        self.tmp = 0;
    }
}

struct ReverseVlcWriter {
    tail: Vec<u8>,
    last_greater_than_8f: bool,
    tmp: u64,
    used_bits: u32,
}

impl ReverseVlcWriter {
    fn new() -> Self {
        Self {
            tail: Vec::new(),
            last_greater_than_8f: true,
            tmp: 0xF,
            used_bits: 4,
        }
    }

    fn encode(&mut self, codeword: u32, length: u32) {
        debug_assert!(length <= 32 && u64::from(codeword) >> length == 0);
        self.tmp |= u64::from(codeword) << self.used_bits;
        self.used_bits += length;
        if self.used_bits < 32 {
            return;
        }
        // Stuffing requires seven low one bits. Four bytes without such a
        // suffix can be copied without inspecting their preceding bytes.
        let word = self.tmp as u32;
        let inverted = !word & 0x7f7f_7f7f;
        if inverted.wrapping_sub(0x0101_0101) & !inverted & 0x8080_8080 == 0 {
            self.tail.extend_from_slice(&word.to_le_bytes());
            self.last_greater_than_8f = word >> 24 > 0x8f;
            self.used_bits -= 32;
            self.tmp >>= 32;
        } else {
            self.flush_bytes();
        }
    }

    fn flush_bytes(&mut self) {
        let mut bits = self.tmp;
        let mut used = self.used_bits;
        let mut previous_high = self.last_greater_than_8f;
        while used >= 7 {
            // Stuff the high bit when the previous byte exceeds 0x8f and the next seven bits are ones.
            let stuffed = previous_high && bits & 0x7f == 0x7f;
            let width = 8 - u32::from(stuffed);
            if used < width {
                previous_high = false;
                break;
            }
            let byte = bits as u8 & (0xff >> u32::from(stuffed));
            self.tail.push(byte);
            bits >>= width;
            used -= width;
            previous_high = byte > 0x8f;
        }
        self.tmp = bits;
        self.used_bits = used;
        self.last_greater_than_8f = previous_high;
    }

    fn flush_tmp(&mut self) {
        self.tail.push(self.tmp as u8);
        self.last_greater_than_8f = self.tmp > 0x8F;
        self.tmp = 0;
        self.used_bits = 0;
    }

    fn into_bytes(mut self) -> Vec<u8> {
        self.tail.reverse();
        self.tail.push(0xff);
        self.tail
    }
}

struct MagSgnWriter {
    bytes: Vec<u8>,
    max_bits: u32,
    used_bits: u32,
    tmp: u64,
}

impl MagSgnWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            max_bits: 8,
            used_bits: 0,
            tmp: 0,
        }
    }

    fn encode(&mut self, codeword: u32, length: u32) {
        self.tmp |= u64::from(codeword) << self.used_bits;
        self.used_bits += length;
        if self.used_bits < 32 {
            return;
        }
        let word = self.tmp as u32;
        let inverted = !word;
        if self.max_bits == 8 && inverted.wrapping_sub(0x0101_0101) & word & 0x8080_8080 == 0 {
            self.bytes.extend_from_slice(&word.to_le_bytes());
            self.tmp >>= 32;
            self.used_bits -= 32;
        } else {
            self.flush_bytes();
        }
    }

    fn flush_bytes(&mut self) {
        while self.used_bits >= self.max_bits {
            let byte = (self.tmp as u8) & (0xFF >> (8 - self.max_bits));
            self.bytes.push(byte);
            self.tmp >>= self.max_bits;
            self.used_bits -= self.max_bits;
            self.max_bits = if byte == 0xFF { 7 } else { 8 };
        }
    }

    fn terminate(&mut self) {
        self.flush_bytes();
        if self.used_bits != 0 {
            let unused = self.max_bits - self.used_bits;
            self.tmp |= (0xFF & ((1 << unused) - 1)) << self.used_bits;
            if self.tmp != 0xFF {
                self.bytes.push(self.tmp as u8);
            }
        } else if self.max_bits == 7 && !self.bytes.is_empty() {
            self.bytes.pop();
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn magnitude_vectors_match_quad_decoder_through_segment_end() {
        if !crate::wavelet::avx2_available() {
            return;
        }
        let (width, height, stride) = (48, 8, 56);
        let mut state = 123u32;
        let samples: Vec<_> = (0..width * height)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                if state & 3 == 0 {
                    0
                } else {
                    state as i32
                }
            })
            .collect();
        for missing in [1, 7, 12, 23] {
            let segment = encode_cleanup(&samples, width, height, missing).unwrap();
            let end = segment.len();
            let scup = 16 * usize::from(segment[end - 1]) + usize::from(segment[end - 2] & 15);
            let mut states = vec![0; stride * (height / 2 + 1)];
            decode_mel_vlc(&segment, width, height, end, scup, stride, &mut states).unwrap();
            for n in [0, 1, 3, 4, (end - scup) / 2, end - scup] {
                let data = &segment[..n.min(end - scup)];
                let plane = 30 - missing as i32;
                let mut expected = vec![0; width * height];
                let mut actual = expected.clone();
                let reference = decode_mag_sgn(
                    data,
                    width,
                    height,
                    stride,
                    &states,
                    plane,
                    missing + 2,
                    &mut expected,
                    &mut vec![0; width + 4],
                );
                // SAFETY: AVX2 was detected, and all rows contain complete groups.
                let vector = unsafe {
                    decode_avx2::decode_rows(
                        data,
                        width,
                        stride,
                        &states,
                        plane,
                        &mut actual,
                        &mut vec![0; width + 4],
                        &mut Vec::new(),
                    )
                };
                assert_eq!(vector.is_ok(), reference.is_ok());
                if vector.is_ok() {
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    #[test]
    fn complete_quad_groups_match_at_every_cleanup_bit_plane() {
        for width in [16, 17, 18, 19, 30, 33, 48, 63, 64] {
            let mut state = 37u32;
            let mut samples = vec![0; width * 9];
            for (i, v) in samples.iter_mut().enumerate() {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                *v = match (i / 16) % 4 {
                    0 => 0,
                    1 => i32::MAX,
                    2 if state & 15 != 0 => 0,
                    _ => state as i32,
                };
            }
            for plane in 1..=30 {
                let expected = encode_quad_rows(&samples, width, 9, plane).unwrap();
                assert_eq!(
                    encode_cleanup(&samples, width, 9, (30 - plane) as u32).unwrap(),
                    expected,
                    "width {width}, plane {plane}"
                );
            }
        }
    }

    #[test]
    fn quad_row_matches_scalar_exponents_at_every_power_of_two() {
        let values: Vec<_> = (1..31)
            .flat_map(|n| {
                let v = 1i32 << n;
                [
                    v - 1,
                    v,
                    v + 1,
                    (v - 1) | i32::MIN,
                    v | i32::MIN,
                    (v + 1) | i32::MIN,
                ]
            })
            .chain([0, i32::MAX, i32::MIN])
            .collect();
        let bottom: Vec<_> = values.iter().rev().copied().collect();
        for bits in 1..=30 {
            let mut row = vec![QuadInfo::default(); values.len().div_ceil(2)];
            prepare_quad_row(&values, &bottom, bits, &mut row);
            for (index, q) in row.into_iter().enumerate() {
                let r = read_quad(&values, &bottom, index * 2, bits);
                assert_eq!(
                    (q.rho, q.u, q.e_max, q.eps, q.bottom_exponents, q.s),
                    (r.rho, r.u, r.e_max, r.eps, r.bottom_exponents, r.s)
                );
            }
        }
    }

    #[test]
    fn reverse_vlc_writer_preserves_stuffing_across_writes() {
        let mut writer = ReverseVlcWriter::new();
        for (word, length) in [(0, 3), (0, 0), (1, 1), (0xff, 8), (0x17f, 9), (0x3a, 6)] {
            writer.encode(word, length);
        }
        writer.encode(0x1122_3344, 32);
        assert_eq!(writer.used_bits, 0);
        assert_eq!(
            writer.into_bytes(),
            [0x11, 0x22, 0x33, 0x44, 0xea, 0x7f, 0xff, 0x8f, 0xff]
        );
    }

    #[test]
    fn short_mag_sgn_quads_match_individual_writes() {
        for prefix in 0..32 {
            let mut batch = MagSgnWriter::new();
            let mut scalar = MagSgnWriter::new();
            for writer in [&mut batch, &mut scalar] {
                writer.encode(0xa55a_19ff & ((1 << prefix) - 1), prefix);
            }
            for u in 1..=8 {
                for rho in 0..16 {
                    for embedding in 0..16 {
                        let quad = QuadInfo {
                            rho,
                            u,
                            s: [0xff, 0xff, 0x72, 0x8d],
                            ..Default::default()
                        };
                        let tuple = VlcTuple {
                            codeword: 0,
                            length: 0,
                            embedding,
                        };
                        write_mag_sgn(&mut batch, &quad, tuple);
                        for (sample, value) in quad.s.into_iter().enumerate() {
                            if rho & (1 << sample) != 0 {
                                let bits = u as u32 - u32::from((embedding >> sample) & 1);
                                scalar.encode(value & ((1 << bits) - 1), bits);
                            }
                        }
                    }
                }
            }
            batch.terminate();
            scalar.terminate();
            assert_eq!(batch.into_bytes(), scalar.into_bytes());
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_quad_preserves_samples_and_reader_position() {
        if !crate::wavelet::avx2_available() {
            return;
        }
        let data = [0xfe, 0xff, 0x7d, 0xa7, 0xff, 0x20, 0x78, 0x91, 0x32];
        for bytes in [&data[..], &data[..3], &data[..0]] {
            for u in 1..=8 {
                for significance in 0..16 {
                    for embedding in 0..16 {
                        let info = (significance << 4) | (embedding << 12) | 0x0500;
                        let mut scalar = ForwardSegmentReader::new(bytes, bytes.len(), 0xff);
                        let mut vector = ForwardSegmentReader::new(bytes, bytes.len(), 0xff);
                        scalar.advance(9);
                        vector.advance(9);
                        let a = decode_sample::<0>(&mut scalar, info, u, 17);
                        let b = decode_sample::<1>(&mut scalar, info, u, 17);
                        let c = decode_sample::<2>(&mut scalar, info, u, 17);
                        let d = decode_sample::<3>(&mut scalar, info, u, 17);
                        // SAFETY: AVX2 is available and the quad fits in one word.
                        let decoded = unsafe { decode_short_quad_avx2(&mut vector, info, u, 17) };
                        assert_eq!(decoded, ([a.0, b.0, c.0, d.0], [b.1, d.1]));
                        assert_eq!(vector.fetch(32), scalar.fetch(32));
                    }
                }
            }
        }
    }

    #[test]
    fn dense_quad_batch_matches_sample_reads_across_stuffed_bytes() {
        let data = [0xfe, 0xff, 0x7d, 0xa7, 0xff, 0x20, 0x78, 0x91, 0x32];
        for u in 1..=8 {
            for embedding in 0..16 {
                let info = 0x05f0 | (embedding << 12);
                let mut scalar = ForwardSegmentReader::new(&data, data.len(), 0xff);
                let mut grouped = ForwardSegmentReader::new(&data, data.len(), 0xff);
                scalar.advance(9);
                grouped.advance(9);
                let x0 = decode_sample::<0>(&mut scalar, info, u, 17);
                let x1 = decode_sample::<1>(&mut scalar, info, u, 17);
                let x2 = decode_sample::<2>(&mut scalar, info, u, 17);
                let x3 = decode_sample::<3>(&mut scalar, info, u, 17);
                assert_eq!(
                    decode_dense_short_quad(&mut grouped, info, u, 17),
                    ([x0.0, x2.0], [x1.0, x3.0], [x1.1, x3.1]),
                );
                assert_eq!(grouped.fetch(32), scalar.fetch(32));
            }
        }
    }

    #[test]
    fn magnitude_writer_batches_bytes_and_preserves_stuffing_and_termination() {
        let mut writer = MagSgnWriter::new();
        for (code, bits) in [
            (0x12, 8),
            (0x34, 8),
            (0x56, 8),
            (0x78, 8),
            (0xff, 8),
            (0x7e, 7),
            (0x01, 8),
            (0xff, 8),
        ] {
            writer.encode(code, bits);
        }
        writer.terminate();
        assert_eq!(
            writer.into_bytes(),
            [0x12, 0x34, 0x56, 0x78, 0xff, 0x7e, 0x01]
        );
    }

    #[test]
    fn packed_uvlc_pairs_match_the_four_separate_fields() {
        for uq0 in 0..=32 {
            for uq1 in 0..=32 {
                let mut packed = ReverseVlcWriter::new();
                let mut separate = ReverseVlcWriter::new();
                for _ in 0..3 {
                    let first = UvlcTuple::create(uq0).unwrap();
                    let second = UvlcTuple::create(uq1).unwrap();
                    separate.encode(first.prefix, first.prefix_length);
                    separate.encode(second.prefix, second.prefix_length);
                    separate.encode(first.suffix, first.suffix_length);
                    separate.encode(second.suffix, second.suffix_length);
                    let (word, length) = uvlc_code(uq0 as i32, uq1 as i32).unwrap();
                    packed.encode(word, length);
                }
                assert_eq!(packed.tmp, separate.tmp);
                assert_eq!(packed.used_bits, separate.used_bits);
                assert_eq!(packed.into_bytes(), separate.into_bytes());
            }
        }
    }

    #[test]
    fn magnitude_reader_refills_across_stuffed_bytes_and_end_of_data() {
        let data = [
            0x31, 0x32, 0x33, 0x34, 0x51, 0x52, 0x53, 0x54, 0xff, 0x23, 0x19,
        ];
        let mut reader = ForwardSegmentReader::new(&data, data.len(), 0xff);
        let mut expected = Vec::new();
        let mut previous = 0;
        for byte in data.into_iter().chain([0xff; 8]) {
            let width = if previous == 0xff { 7 } else { 8 };
            expected.extend((0..width).map(|bit| (byte >> bit) & 1));
            previous = byte;
        }
        let mut consumed = 0;
        for count in [29, 1, 17, 0, 31, 1, 26] {
            let lookahead = count.max(1);
            let expected_bits = expected[consumed..consumed + lookahead as usize]
                .iter()
                .enumerate()
                .fold(0, |bits, (i, &bit)| bits | (u32::from(bit) << i));
            let buffered = reader.bits;
            let offset = reader.offset;
            assert_eq!(
                reader.fetch(lookahead) & mask_low_bits(lookahead),
                expected_bits
            );
            if buffered >= lookahead as i32 {
                assert_eq!(reader.offset, offset);
                assert_eq!(reader.bits, buffered);
            }
            reader.advance(count);
            consumed += count as usize;
        }
    }

    fn sign_magnitude(value: i32) -> i32 {
        if value < 0 {
            (0x8000_0000u32 | value.unsigned_abs()) as i32
        } else {
            value
        }
    }

    #[test]
    fn cleanup_pass_round_trips_sign_magnitude_coefficients() {
        let width = 4usize;
        let height = 4usize;
        let magnitudes = [1i32, 4, 0, 9, 2, 0, 7, 3, 6, 0, 0, 11, 13, 5, 8, 0];
        let shift = 10;
        let coefficients: Vec<i32> = magnitudes
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                sign_magnitude(if i % 3 == 0 {
                    -(v << shift)
                } else {
                    v << shift
                })
            })
            .collect();
        let encoded = encode_cleanup(&coefficients, width, height, 20).unwrap();
        let mut decoder = CleanupDecoder::default();
        let decoded = decoder.decode(&encoded, width, height, 20).unwrap();
        let unscaled: Vec<i32> = decoded
            .iter()
            .map(|&v| {
                let magnitude = ((v as u32) & 0x7FFF_FFFF) >> shift;
                if v < 0 {
                    -(magnitude as i32)
                } else {
                    magnitude as i32
                }
            })
            .collect();
        let expected: Vec<i32> = magnitudes
            .iter()
            .enumerate()
            .map(|(i, &v)| if i % 3 == 0 { -(v) } else { v })
            .collect();
        assert_eq!(unscaled, expected);
    }

    #[test]
    fn zero_cleanup_pass_round_trips() {
        let coefficients = vec![0i32; 16];
        let encoded = encode_cleanup(&coefficients, 4, 4, 10).unwrap();
        let mut decoder = CleanupDecoder::default();
        let decoded = decoder.decode(&encoded, 4, 4, 10).unwrap();
        assert_eq!(decoded, coefficients);
    }
}

#[cfg(test)]
mod random_tests {
    use super::*;

    fn sign_magnitude(value: i32) -> i32 {
        if value < 0 {
            (0x8000_0000u32 | value.unsigned_abs()) as i32
        } else {
            value
        }
    }

    fn unscale(value: i32, shift: u32) -> i32 {
        let magnitude = ((value as u32) & 0x7FFF_FFFF) >> shift;
        if value < 0 {
            -(magnitude as i32)
        } else {
            magnitude as i32
        }
    }

    #[test]
    fn cleanup_pass_round_trips_random_blocks() {
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        let mut decoder = CleanupDecoder::default();
        for (width, height) in [
            (4usize, 4usize),
            (8, 8),
            (6, 10),
            (16, 16),
            (32, 32),
            (7, 5),
        ] {
            let k_max = 18u32;
            let shift = 31 - k_max;
            let missing = k_max - 1;
            let mut coefficients = vec![0i32; width * height];
            for (i, slot) in coefficients.iter_mut().enumerate() {
                let magnitude = if next() % 4 == 0 {
                    0
                } else {
                    (next() % (1 << (k_max - 2))) as i32
                };
                let value = magnitude << shift;
                *slot = sign_magnitude(if i % 5 == 0 { -value } else { value });
            }
            let encoded = encode_cleanup(&coefficients, width, height, missing).unwrap();
            let decoded = decoder.decode(&encoded, width, height, missing).unwrap();
            let expected: Vec<i32> = coefficients.iter().map(|&v| unscale(v, shift)).collect();
            let actual: Vec<i32> = decoded.iter().map(|&v| unscale(v, shift)).collect();
            assert_eq!(actual, expected, "block {width}x{height}");
        }
    }
}

#[cfg(test)]
mod vector_dump {
    use super::*;

    #[test]
    fn dump_reference_vector() {
        let magnitudes = [1i32, 4, 0, 9, 2, 0, 7, 3, 6, 0, 0, 11, 13, 5, 8, 0];
        let shift = 10;
        let coefficients: Vec<i32> = magnitudes
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let value = v << shift;
                if i % 3 == 0 {
                    (0x8000_0000u32 | value.unsigned_abs()) as i32
                } else {
                    value
                }
            })
            .collect();
        let encoded = encode_cleanup(&coefficients, 4, 4, 20).unwrap();
        let hex: String = encoded.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(hex, "D11DA41939FA40AEFAE5CCD700");
    }
}
