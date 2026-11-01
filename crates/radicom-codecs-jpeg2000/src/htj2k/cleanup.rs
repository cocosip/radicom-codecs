//! HTJ2K standard cleanup-pass codec (ISO/IEC 15444-15 Annex C).
//!
//! Ported from the reference cleanup pass in `fo-dicom.PureCodecs`
//! (`Jpeg2000HtStandardCleanupPassDecoder` / `...Encoder`).

use std::sync::OnceLock;

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

/// Decodes one HT cleanup pass into sign-magnitude coefficients.
pub(crate) fn decode_cleanup(
    data: &[u8],
    width: usize,
    height: usize,
    missing_msb: u32,
) -> Result<Vec<i32>, String> {
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
    let mut scratch = vec![0u16; sstr * (height.div_ceil(2) + 1)];
    decode_mel_vlc(data, width, height, lcup, scup, sstr, &mut scratch)?;
    decode_mag_sgn(
        data,
        width,
        height,
        lcup,
        scup,
        sstr,
        &scratch,
        bit_plane,
        missing_msb + 2,
    )
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

    let mut x = 0usize;
    while x < width {
        let mut vlc_value = vlc.fetch();
        let mut t0 = vlc0()[context as usize + (vlc_value & 0x7F) as usize];
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

        let mut t1 = vlc0()[context as usize + (vlc_value & 0x7F) as usize];
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
        let mut uvlc_entry = u32::from(uvlc0()[uvlc_index]);
        let bias = u32::from(uvlc_bias_table()[uvlc_index]);
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

    let mut y = 2usize;
    while y < height {
        context = 0;
        sp = (y >> 1) * sstr;
        let mut x = 0usize;
        while x < width {
            context |= u32::from(scratch[sp - sstr] & 0xA0) << 2;
            context |= u32::from(scratch[sp + 2 - sstr] & 0x20) << 4;

            let mut vlc_value = vlc.fetch();
            let mut t0 = vlc1()[context as usize + (vlc_value & 0x7F) as usize];
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

            let mut t1 = vlc1()[context as usize + (vlc_value & 0x7F) as usize];
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
                u32::from(uvlc1()[uvlc_mode as usize + (vlc_value & 0x3F) as usize]);
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
    lcup: usize,
    scup: usize,
    sstr: usize,
    scratch: &[u16],
    bit_plane: i32,
    mmsbp2: u32,
) -> Result<Vec<i32>, String> {
    let mut reader = ForwardSegmentReader::new(&data[..lcup - scup], lcup - scup, 0xFF);
    let mut coefficients = vec![0i32; width * height];
    let mut v_scratch = vec![0u32; width + 4];
    decode_mag_sgn_row(
        &mut reader,
        &mut coefficients,
        width,
        height,
        sstr,
        scratch,
        &mut v_scratch,
        0,
        bit_plane,
        mmsbp2,
        true,
    )?;
    let mut y = 2usize;
    while y < height {
        decode_mag_sgn_row(
            &mut reader,
            &mut coefficients,
            width,
            height,
            sstr,
            scratch,
            &mut v_scratch,
            y,
            bit_plane,
            mmsbp2,
            false,
        )?;
        y += 2;
    }
    Ok(coefficients)
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
    let mut sp = (y >> 1) * sstr;
    let mut dp = y * width;
    let mut prev_vn = 0u32;
    let mut vp = 0usize;
    let mut x = 0usize;
    while x < width {
        let inf = scratch[sp];
        let uq = u32::from(scratch[sp + 1]);
        let u = if initial {
            uq
        } else {
            uq + kappa(inf, v_scratch[vp], v_scratch[vp + 1])
        };
        if u > mmsbp2 {
            return Err(format!(
                "HTJ2K cleanup pass has an invalid U value (U={u}, max={mmsbp2}, x={x}, y={y})"
            ));
        }
        let mut vn = 0;
        decode_sample(reader, coefficients, dp, inf, u, 0, bit_plane, &mut vn);
        if y + 1 < height {
            decode_sample(
                reader,
                coefficients,
                dp + width,
                inf,
                u,
                1,
                bit_plane,
                &mut vn,
            );
        }
        v_scratch[vp] = prev_vn | vn;
        prev_vn = 0;
        dp += 1;
        x += 1;
        if x >= width {
            vp += 1;
            v_scratch[vp] = 0;
            break;
        }
        decode_sample(reader, coefficients, dp, inf, u, 2, bit_plane, &mut vn);
        if y + 1 < height {
            decode_sample(
                reader,
                coefficients,
                dp + width,
                inf,
                u,
                3,
                bit_plane,
                &mut prev_vn,
            );
        } else {
            prev_vn = 0;
        }
        dp += 1;
        x += 1;
        sp += 2;
        vp += 1;
    }
    let last = vp.min(v_scratch.len() - 1);
    v_scratch[last] = prev_vn;
    Ok(())
}

fn kappa(inf: u16, current_vn: u32, next_vn: u32) -> u32 {
    let mut gamma = u32::from(inf) & 0xF0;
    gamma &= gamma.wrapping_sub(0x10);
    if gamma == 0 {
        return 1;
    }
    31 - ((current_vn | next_vn) | 2).leading_zeros()
}

#[allow(clippy::too_many_arguments)]
fn decode_sample(
    reader: &mut ForwardSegmentReader<'_>,
    coefficients: &mut [i32],
    index: usize,
    inf: u16,
    uq: u32,
    bit: u32,
    bit_plane: i32,
    vn: &mut u32,
) {
    if inf & (1 << (4 + bit)) == 0 {
        coefficients[index] = 0;
        *vn = 0;
        return;
    }
    let mag_sgn = reader.fetch();
    let mn = uq - u32::from((inf >> (12 + bit)) & 1);
    reader.advance(mn);
    let sign = mag_sgn & 1 != 0;
    *vn = mag_sgn & mask_low_bits(mn);
    *vn |= u32::from((inf >> (8 + bit)) & 1) << mn;
    *vn |= 1;
    let magnitude = (*vn + 2) << (bit_plane - 1);
    coefficients[index] = if sign {
        (0x8000_0000u32 | magnitude) as i32
    } else {
        magnitude as i32
    };
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

    fn fetch(&mut self) -> u32 {
        while self.bits < 32 {
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
    e: [i32; 4],
    s: [u32; 4],
}

impl QuadInfo {
    fn with_non_initial_u(mut self, u: i32, kappa: i32) -> Self {
        self.u = u;
        self.eps = if u > kappa {
            build_eps(self.rho, &self.e, self.e_max)
        } else {
            0
        };
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

    let mut mel = MelWriter::new();
    let mut vlc = ReverseVlcWriter::new();
    let mut ms = MagSgnWriter::new();
    let mut e_values = vec![0i32; width.div_ceil(2) + 2];
    let mut context_values = vec![0i32; width.div_ceil(2) + 2];

    encode_initial_quad_row(
        coefficients,
        width,
        height,
        bit_plane,
        &mut e_values,
        &mut context_values,
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
        &mut mel,
        &mut vlc,
        &mut ms,
    )?;

    mel.terminate_with(&mut vlc);
    ms.terminate();
    Ok(assemble(
        ms.into_bytes(),
        mel.into_bytes(),
        vlc.into_bytes(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn encode_initial_quad_row(
    coefficients: &[i32],
    width: usize,
    height: usize,
    bit_plane: i32,
    e_values: &mut [i32],
    context_values: &mut [i32],
    mel: &mut MelWriter,
    vlc: &mut ReverseVlcWriter,
    ms: &mut MagSgnWriter,
) -> Result<(), String> {
    let mut context = 0u16;
    let mut x = 0usize;
    while x < width {
        let first = read_quad(coefficients, width, height, x, 0, bit_plane);
        let tuple0 = lookup_vlc(true, context, first.rho, first.eps)?;
        vlc.encode(u32::from(tuple0.codeword), tuple0.length);
        if context == 0 {
            mel.encode(first.rho != 0);
        }
        write_mag_sgn(ms, &first, tuple0);
        let uq0 = first.u - 1;
        store_initial_state(&first, e_values, context_values, x / 2);

        let mut uq1 = 0i32;
        if x + 2 < width {
            let context1 = (first.rho >> 1) | (first.rho & 1);
            let second = read_quad(coefficients, width, height, x + 2, 0, bit_plane);
            let tuple1 = lookup_vlc(true, context1, second.rho, second.eps)?;
            vlc.encode(u32::from(tuple1.codeword), tuple1.length);
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
        write_initial_uvlc(vlc, uq0, uq1)?;
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
    mel: &mut MelWriter,
    vlc: &mut ReverseVlcWriter,
    ms: &mut MagSgnWriter,
) -> Result<(), String> {
    let mut y = 2usize;
    while y < height {
        let mut e_index = 0usize;
        let mut max_e = e_values[0].max(e_values[1]) - 1;
        e_values[0] = 0;
        let mut context_index = 0usize;
        let mut context0 = context_values[0] + (context_values[1] << 2);
        context_values[0] = 0;

        let mut x = 0usize;
        while x < width {
            let mut first = read_quad(coefficients, width, height, x, y, bit_plane);
            let kappa0 = if (first.rho & first.rho.wrapping_sub(1)) != 0 {
                max_e.max(1)
            } else {
                1
            };
            first = first.with_non_initial_u(first.e_max.max(kappa0), kappa0);
            let uq0 = first.u - kappa0;
            let tuple0 = lookup_vlc(false, context0 as u16, first.rho, first.eps)?;
            vlc.encode(u32::from(tuple0.codeword), tuple0.length);
            if context0 == 0 {
                mel.encode(first.rho != 0);
            }
            write_mag_sgn(ms, &first, tuple0);
            e_values[e_index] = e_values[e_index].max(first.e[1]);
            e_index += 1;
            max_e = e_values[e_index].max(e_values[e_index + 1]) - 1;
            e_values[e_index] = first.e[3];
            context_values[context_index] |= i32::from((first.rho & 2) >> 1);
            context_index += 1;
            let mut context1 =
                context_values[context_index] + (context_values[context_index + 1] << 2);
            context_values[context_index] = i32::from((first.rho & 8) >> 3);

            let mut uq1 = 0i32;
            let mut second = QuadInfo::default();
            if x + 2 < width {
                second = read_quad(coefficients, width, height, x + 2, y, bit_plane);
                let kappa1 = if (second.rho & second.rho.wrapping_sub(1)) != 0 {
                    max_e.max(1)
                } else {
                    1
                };
                context1 |= i32::from((first.rho & 4) >> 1) | i32::from((first.rho & 8) >> 2);
                second = second.with_non_initial_u(second.e_max.max(kappa1), kappa1);
                uq1 = second.u - kappa1;
                let tuple1 = lookup_vlc(false, context1 as u16, second.rho, second.eps)?;
                vlc.encode(u32::from(tuple1.codeword), tuple1.length);
                if context1 == 0 {
                    mel.encode(second.rho != 0);
                }
                write_mag_sgn(ms, &second, tuple1);
                e_values[e_index] = e_values[e_index].max(second.e[1]);
                e_index += 1;
                max_e = e_values[e_index].max(e_values[e_index + 1]) - 1;
                e_values[e_index] = second.e[3];
                context_values[context_index] |= i32::from((second.rho & 2) >> 1);
                context_index += 1;
                context0 = context_values[context_index] + (context_values[context_index + 1] << 2);
                context_values[context_index] = i32::from((second.rho & 8) >> 3);
            }

            write_uvlc(vlc, uq0, uq1)?;
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
    e_values[index] = e_values[index].max(quad.e[1]);
    e_values[index + 1] = quad.e[3];
    context_values[index] |= i32::from((quad.rho & 2) >> 1);
    context_values[index + 1] = i32::from((quad.rho & 8) >> 3);
}

fn read_quad(
    coefficients: &[i32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    bit_plane: i32,
) -> QuadInfo {
    let mut rho = 0u16;
    let mut e = [0i32; 4];
    let mut s = [0u32; 4];
    read_sample(
        coefficients,
        width,
        height,
        x,
        y,
        bit_plane,
        0,
        &mut e,
        &mut s,
        &mut rho,
    );
    read_sample(
        coefficients,
        width,
        height,
        x,
        y + 1,
        bit_plane,
        1,
        &mut e,
        &mut s,
        &mut rho,
    );
    read_sample(
        coefficients,
        width,
        height,
        x + 1,
        y,
        bit_plane,
        2,
        &mut e,
        &mut s,
        &mut rho,
    );
    read_sample(
        coefficients,
        width,
        height,
        x + 1,
        y + 1,
        bit_plane,
        3,
        &mut e,
        &mut s,
        &mut rho,
    );
    let e_max = e[0].max(e[1]).max(e[2].max(e[3]));
    let eps = build_eps(rho, &e, e_max);
    QuadInfo {
        rho,
        u: e_max.max(1),
        e_max,
        eps,
        e,
        s,
    }
}

#[allow(clippy::too_many_arguments)]
fn read_sample(
    coefficients: &[i32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    bit_plane: i32,
    bit: usize,
    e: &mut [i32; 4],
    s: &mut [u32; 4],
    rho: &mut u16,
) {
    if x >= width || y >= height {
        return;
    }
    let sample = coefficients[y * width + x] as u32;
    let mut val = sample.wrapping_add(sample) >> bit_plane;
    val &= !1;
    if val == 0 {
        return;
    }
    *rho |= 1 << bit;
    let first = val - 1;
    e[bit] = 32 - first.leading_zeros() as i32;
    let second = first - 1;
    s[bit] = second.wrapping_add(sample >> 31);
}

fn build_eps(rho: u16, e: &[i32; 4], e_max: i32) -> u16 {
    if e_max <= 1 {
        return 0;
    }
    let mut eps = 0u16;
    for (bit, &e_bit) in e.iter().enumerate() {
        if (rho & (1 << bit)) != 0 && e_bit == e_max {
            eps |= 1 << bit;
        }
    }
    eps
}

fn write_mag_sgn(writer: &mut MagSgnWriter, quad: &QuadInfo, tuple: VlcTuple) {
    for bit in 0..4 {
        if (quad.rho & (1 << bit)) == 0 {
            continue;
        }
        let m = quad.u - i32::from((tuple.embedding >> bit) & 1);
        let mask = if m >= 32 { u32::MAX } else { (1u32 << m) - 1 };
        writer.encode(quad.s[bit] & mask, m as u32);
    }
}

fn write_initial_uvlc(writer: &mut ReverseVlcWriter, uq0: i32, uq1: i32) -> Result<(), String> {
    if uq0 > 2 && uq1 > 2 {
        let first = UvlcTuple::create((uq0 - 2) as u32)?;
        let second = UvlcTuple::create((uq1 - 2) as u32)?;
        writer.encode(first.prefix, first.prefix_length);
        writer.encode(second.prefix, second.prefix_length);
        writer.encode(first.suffix, first.suffix_length);
        writer.encode(second.suffix, second.suffix_length);
        return Ok(());
    }
    if uq0 > 2 && uq1 > 0 {
        let first = UvlcTuple::create(uq0 as u32)?;
        writer.encode(first.prefix, first.prefix_length);
        writer.encode((uq1 - 1) as u32, 1);
        writer.encode(first.suffix, first.suffix_length);
        return Ok(());
    }
    write_uvlc(writer, uq0, uq1)
}

fn write_uvlc(writer: &mut ReverseVlcWriter, uq0: i32, uq1: i32) -> Result<(), String> {
    let first = UvlcTuple::create(uq0 as u32)?;
    let second = UvlcTuple::create(uq1 as u32)?;
    writer.encode(first.prefix, first.prefix_length);
    writer.encode(second.prefix, second.prefix_length);
    writer.encode(first.suffix, first.suffix_length);
    writer.encode(second.suffix, second.suffix_length);
    Ok(())
}

fn lookup_vlc(initial: bool, context: u16, rho: u16, eps: u16) -> Result<VlcTuple, String> {
    let table = if initial {
        vlc_encode0()
    } else {
        vlc_encode1()
    };
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

fn assemble(mag_sgn: Vec<u8>, mel: Vec<u8>, vlc: Vec<u8>) -> Vec<u8> {
    let mut result = Vec::with_capacity(mag_sgn.len() + mel.len() + vlc.len());
    result.extend_from_slice(&mag_sgn);
    result.extend_from_slice(&mel);
    result.extend_from_slice(&vlc);
    let scup = mel.len() + vlc.len();
    let last = result.len() - 1;
    result[last] = (scup >> 4) as u8;
    result[last - 1] = (result[last - 1] & 0xF0) | (scup & 0x0F) as u8;
    result
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
        if self.run > 0 {
            self.emit_bit(1);
        }
        let mel_tmp = u16::from(self.tmp) << self.remaining_bits;
        let mel_mask = (0xFFu16 << self.remaining_bits) & 0xFF;
        let vlc_mask = 0xFFu16 >> (8 - vlc.used_bits);
        if (mel_mask | vlc_mask) == 0 {
            return;
        }
        let fuse = mel_tmp | vlc.tmp;
        if (((fuse ^ mel_tmp) & mel_mask) | ((fuse ^ vlc.tmp) & vlc_mask)) == 0
            && fuse != 0xFF
            && vlc.position > 1
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
    tmp: u16,
    used_bits: u32,
    position: usize,
}

impl ReverseVlcWriter {
    fn new() -> Self {
        Self {
            tail: Vec::new(),
            last_greater_than_8f: true,
            tmp: 0xF,
            used_bits: 4,
            position: 1,
        }
    }

    fn encode(&mut self, mut codeword: u32, mut length: u32) {
        while length > 0 {
            let mut available = 8 - u32::from(self.last_greater_than_8f) - self.used_bits;
            let take = available.min(length);
            self.tmp |= ((codeword & ((1 << take) - 1)) << self.used_bits) as u16;
            self.used_bits += take;
            available -= take;
            length -= take;
            codeword >>= take;
            if available != 0 {
                continue;
            }
            if self.last_greater_than_8f && self.tmp != 0x7F {
                self.last_greater_than_8f = false;
                continue;
            }
            self.flush_tmp();
        }
    }

    fn flush_tmp(&mut self) {
        self.tail.push(self.tmp as u8);
        self.position += 1;
        self.last_greater_than_8f = self.tmp > 0x8F;
        self.tmp = 0;
        self.used_bits = 0;
    }

    fn into_bytes(self) -> Vec<u8> {
        let mut result = vec![0u8; self.tail.len() + 1];
        for (i, &byte) in self.tail.iter().rev().enumerate() {
            result[i] = byte;
        }
        let len = result.len();
        result[len - 1] = 0xFF;
        result
    }
}

struct MagSgnWriter {
    bytes: Vec<u8>,
    max_bits: u32,
    used_bits: u32,
    tmp: u32,
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

    fn encode(&mut self, mut codeword: u32, mut length: u32) {
        while length > 0 {
            let take = (self.max_bits - self.used_bits).min(length);
            self.tmp |= (codeword & ((1 << take) - 1)) << self.used_bits;
            self.used_bits += take;
            codeword >>= take;
            length -= take;
            if self.used_bits < self.max_bits {
                continue;
            }
            self.bytes.push(self.tmp as u8);
            self.max_bits = if self.tmp == 0xFF { 7 } else { 8 };
            self.tmp = 0;
            self.used_bits = 0;
        }
    }

    fn terminate(&mut self) {
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
        let decoded = decode_cleanup(&encoded, width, height, 20).unwrap();
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
        let decoded = decode_cleanup(&encoded, 4, 4, 10).unwrap();
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
        for &(width, height) in &[(4usize, 4usize), (8, 8), (6, 10), (16, 16), (32, 32)] {
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
            let decoded = decode_cleanup(&encoded, width, height, missing).unwrap();
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
