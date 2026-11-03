//! Classic JPEG 2000 EBCOT Tier-1 coding (ISO/IEC 15444-1 Annex D).
//!
//! The implementation follows the pure C# reference implementation's flag
//! layout and context derivation. Coefficients are carried in a fixed-point
//! representation with six fractional bits, exactly as the reference decoder
//! expects; the public entry points return plain integer coefficients.

use crate::mq::{MqDecoder, MqEncoder};

const FRACTIONAL_BITS: i32 = 6;

const SIG: u32 = 0x0001;
const REFINED: u32 = 0x0002;
const VISITED: u32 = 0x0004;
const SIG_N: u32 = 0x0010;
const SIG_S: u32 = 0x0020;
const SIG_W: u32 = 0x0040;
const SIG_E: u32 = 0x0080;
const SIG_NW: u32 = 0x0100;
const SIG_NE: u32 = 0x0200;
const SIG_SW: u32 = 0x0400;
const SIG_SE: u32 = 0x0800;
const SIG_NEIGHBORS: u32 = SIG_N | SIG_S | SIG_W | SIG_E | SIG_NW | SIG_NE | SIG_SW | SIG_SE;
const SIGN: u32 = 0x1000;
const SIGN_N: u32 = 0x2000;
const SIGN_S: u32 = 0x4000;
const SIGN_W: u32 = 0x8000;
const SIGN_E: u32 = 0x10000;

pub(crate) const CTX_ZC: usize = 0;
pub(crate) const CTX_SIGN: usize = 9;
pub(crate) const CTX_MAG: usize = 14;
pub(crate) const CTX_RUN: usize = 17;
pub(crate) const CTX_UNIFORM: usize = 18;

#[derive(Debug, Clone)]
pub(crate) struct EncodedCodeBlock {
    pub(crate) data: Vec<u8>,
    pub(crate) pass_count: u32,
    pub(crate) max_bit_plane: i32,
}

/// Encodes one code-block. `coefficients` must already be in the reference
/// fixed-point domain (reversible values shifted left by six, irreversible
/// quantized values multiplied by 64).
pub(crate) fn encode_code_block(
    coefficients: &[i32],
    width: usize,
    height: usize,
    orientation: u8,
    style: u8,
) -> Result<EncodedCodeBlock, String> {
    if width == 0 || height == 0 || coefficients.len() != width * height {
        return Err("invalid code-block dimensions".into());
    }
    let max = coefficients
        .iter()
        .map(|v| v.unsigned_abs())
        .max()
        .unwrap_or(0);
    if max == 0 {
        return Ok(EncodedCodeBlock {
            data: Vec::new(),
            pass_count: 0,
            max_bit_plane: -1,
        });
    }
    let max_bit_plane = 32 - max.leading_zeros() as i32 - 1;
    let bit_count = max_bit_plane - (FRACTIONAL_BITS - 1);
    if bit_count <= 0 {
        return Ok(EncodedCodeBlock {
            data: Vec::new(),
            pass_count: 0,
            max_bit_plane,
        });
    }
    let pass_count = (bit_count * 3 - 2) as u32;
    let mut encoder = Tier1Encoder::new(width, height, orientation, style);
    encoder.load(coefficients);
    let data = encoder.run(pass_count as i32);
    Ok(EncodedCodeBlock {
        data,
        pass_count,
        max_bit_plane,
    })
}

/// Decodes one code-block from its (already concatenated) coding segments.
pub(crate) fn decode_code_block(
    segments: &[crate::geometry::CodeBlockSegment],
    width: usize,
    height: usize,
    orientation: u8,
    max_bit_plane: i32,
    style: u8,
) -> Result<Vec<i32>, String> {
    if width == 0 || height == 0 {
        return Err("invalid code-block dimensions".into());
    }
    if max_bit_plane < 0 {
        return Ok(vec![0; width * height]);
    }
    let mut decoder = Tier1Decoder::new(width, height, orientation, style);
    decoder.run(segments, max_bit_plane)?;
    Ok(decoder.finish())
}

struct Tier1Encoder {
    width: usize,
    height: usize,
    stride: usize,
    data: Vec<i32>,
    flags: Vec<u32>,
    orientation: u8,
    style: u8,
    mq: MqEncoder,
    bit_plane: i32,
}

impl Tier1Encoder {
    fn new(width: usize, height: usize, orientation: u8, style: u8) -> Self {
        Self {
            width,
            height,
            stride: width + 2,
            data: vec![0; (width + 2) * (height + 2)],
            flags: vec![0; (width + 2) * (height + 2)],
            orientation,
            style,
            mq: MqEncoder::new(),
            bit_plane: 0,
        }
    }

    fn load(&mut self, coefficients: &[i32]) {
        for y in 0..self.height {
            for x in 0..self.width {
                let index = self.index(x, y);
                self.data[index] = coefficients[y * self.width + x];
            }
        }
    }

    fn index(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.stride + (x + 1)
    }

    fn index_i(&self, x: i32, y: i32) -> usize {
        ((y + 1) as usize) * self.stride + (x + 1) as usize
    }

    fn run(mut self, pass_count: i32) -> Vec<u8> {
        let max = self
            .data
            .iter()
            .map(|v| v.unsigned_abs())
            .max()
            .unwrap_or(0);
        if max == 0 || pass_count <= 0 {
            return Vec::new();
        }
        self.bit_plane = 32 - max.leading_zeros() as i32 - 1;
        let mut pass = 0;
        let mut pass_type = 2;
        while self.bit_plane >= 0 && pass < pass_count {
            if pass_type == 0 || (pass_type == 2 && pass == 0) {
                for flag in &mut self.flags {
                    *flag &= !VISITED;
                }
            }
            match pass_type {
                0 => self.encode_sigpass(),
                1 => self.encode_refpass(),
                _ => self.encode_cleanup(),
            }
            if self.style & 0x02 != 0 {
                self.mq.reset_contexts();
            }
            pass += 1;
            if pass_type == 2 {
                pass_type = 0;
                self.bit_plane -= 1;
            } else {
                pass_type += 1;
            }
        }
        self.mq.flush()
    }

    fn encode_sigpass(&mut self) {
        for stripe in (0..self.height).step_by(4) {
            for x in 0..self.width {
                for dy in 0..4 {
                    let y = stripe + dy;
                    if y >= self.height {
                        break;
                    }
                    let index = self.index(x, y);
                    let flags = self.flags[index];
                    if flags & SIG != 0 || flags & SIG_NEIGHBORS == 0 {
                        continue;
                    }
                    let significant = self.magnitude_bit(index);
                    self.mq
                        .encode(significant, zero_context(flags, self.orientation));
                    self.flags[index] |= VISITED;
                    if significant {
                        self.set_significant(x, y, index, false);
                    }
                }
            }
        }
    }

    fn encode_refpass(&mut self) {
        for stripe in (0..self.height).step_by(4) {
            for x in 0..self.width {
                for dy in 0..4 {
                    let y = stripe + dy;
                    if y >= self.height {
                        break;
                    }
                    let index = self.index(x, y);
                    let flags = self.flags[index];
                    if flags & SIG == 0 || flags & VISITED != 0 {
                        continue;
                    }
                    self.mq
                        .encode(self.magnitude_bit(index), magnitude_context(flags));
                    self.flags[index] |= REFINED;
                }
            }
        }
    }

    fn encode_cleanup(&mut self) {
        for stripe in (0..self.height).step_by(4) {
            for x in 0..self.width {
                let full_stripe = stripe + 3 < self.height;
                let mut eligible = full_stripe;
                let mut run_length = -1i32;
                if full_stripe {
                    for dy in 0..4 {
                        let flags = self.flags[self.index(x, stripe + dy)];
                        if flags & (VISITED | SIG | SIG_NEIGHBORS) != 0 {
                            eligible = false;
                            break;
                        }
                    }
                }
                if eligible {
                    for dy in 0..4 {
                        if self.magnitude_bit(self.index(x, stripe + dy)) {
                            run_length = dy as i32;
                            break;
                        }
                    }
                    self.mq.encode(run_length >= 0, CTX_RUN);
                    if run_length < 0 {
                        continue;
                    }
                    self.mq.encode((run_length >> 1) & 1 != 0, CTX_UNIFORM);
                    self.mq.encode(run_length & 1 != 0, CTX_UNIFORM);
                }
                for dy in 0..4 {
                    let y = stripe + dy;
                    if y >= self.height {
                        break;
                    }
                    if eligible && (dy as i32) < run_length {
                        continue;
                    }
                    let index = self.index(x, y);
                    let flags = self.flags[index];
                    if flags & VISITED != 0 || flags & SIG != 0 {
                        self.flags[index] &= !VISITED;
                        continue;
                    }
                    let mut significant = eligible && dy as i32 == run_length;
                    if !significant {
                        significant = self.magnitude_bit(index);
                        self.mq
                            .encode(significant, zero_context(flags, self.orientation));
                    }
                    if significant {
                        self.set_significant(x, y, index, self.style & 0x20 != 0);
                    }
                    self.flags[index] &= !VISITED;
                }
            }
        }
    }

    fn set_significant(&mut self, x: usize, y: usize, index: usize, uniform_sign: bool) {
        let sign = self.data[index] < 0;
        if uniform_sign {
            // SEGSYM: signs of coefficients that become significant in the
            // cleanup pass use the uniform context, without prediction.
            self.mq.encode(sign, CTX_UNIFORM);
        } else {
            let context = sign_context(self.flags[index]);
            let prediction = sign_prediction(self.flags[index]);
            self.mq.encode(sign ^ prediction, context);
        }
        if sign {
            self.flags[index] |= SIGN;
        }
        self.flags[index] |= SIG;
        self.update_neighbors(x, y);
    }

    fn magnitude_bit(&self, index: usize) -> bool {
        (self.data[index].unsigned_abs() >> self.bit_plane) & 1 != 0
    }

    fn update_neighbors(&mut self, x: usize, y: usize) {
        let negative = self.flags[self.index(x, y)] & SIGN != 0;
        if !(self.style & 0x08 != 0 && y & 3 == 0) {
            self.mark(x as i32, y as i32 - 1, SIG_S, negative, SIGN_S);
            let idx_nw = self.index_i(x as i32 - 1, y as i32 - 1);
            let idx_ne = self.index_i(x as i32 + 1, y as i32 - 1);
            self.flags[idx_nw] |= SIG_SE;
            self.flags[idx_ne] |= SIG_SW;
        }
        self.mark(x as i32, y as i32 + 1, SIG_N, negative, SIGN_N);
        self.mark(x as i32 - 1, y as i32, SIG_E, negative, SIGN_E);
        self.mark(x as i32 + 1, y as i32, SIG_W, negative, SIGN_W);
        let idx_sw = self.index_i(x as i32 - 1, y as i32 + 1);
        let idx_se = self.index_i(x as i32 + 1, y as i32 + 1);
        self.flags[idx_sw] |= SIG_NE;
        self.flags[idx_se] |= SIG_NW;
    }

    fn mark(&mut self, x: i32, y: i32, sig_flag: u32, negative: bool, sign_flag: u32) {
        let index = self.index_i(x, y);
        self.flags[index] |= sig_flag;
        if negative {
            self.flags[index] |= sign_flag;
        }
    }
}

struct Tier1Decoder {
    width: usize,
    height: usize,
    stride: usize,
    data: Vec<i32>,
    flags: Vec<u32>,
    orientation: u8,
    style: u8,
    bit_plane: i32,
    in_cleanup: bool,
}

impl Tier1Decoder {
    fn new(width: usize, height: usize, orientation: u8, style: u8) -> Self {
        Self {
            width,
            height,
            stride: width + 2,
            data: vec![0; (width + 2) * (height + 2)],
            flags: vec![0; (width + 2) * (height + 2)],
            orientation,
            style,
            bit_plane: 0,
            in_cleanup: false,
        }
    }

    fn index(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.stride + (x + 1)
    }

    fn index_i(&self, x: i32, y: i32) -> usize {
        ((y + 1) as usize) * self.stride + (x + 1) as usize
    }

    fn run(
        &mut self,
        segments: &[crate::geometry::CodeBlockSegment],
        max_bit_plane: i32,
    ) -> Result<(), String> {
        self.bit_plane = max_bit_plane;
        let mut bytes = Vec::new();
        let mut total_passes = 0u32;
        for segment in segments {
            if segment.pass_count == 0 {
                continue;
            }
            bytes.extend_from_slice(&segment.data);
            total_passes += segment.pass_count;
        }
        if total_passes == 0 {
            return Ok(());
        }
        let mut mq = MqDecoder::new(&bytes);
        let mut raw = RawBitReader::new(&bytes);
        let mut pass_type = 2;
        for pass in 0..total_passes {
            if self.bit_plane < 0 {
                break;
            }
            if pass_type == 0 || (pass_type == 2 && pass == 0) {
                for flag in &mut self.flags {
                    *flag &= !VISITED;
                }
            }
            let lazy = self.is_lazy_pass(max_bit_plane, pass_type);
            match pass_type {
                0 => self.decode_sigpass(&mut mq, &mut raw, lazy),
                1 => self.decode_refpass(&mut mq, &mut raw, lazy),
                _ => {
                    self.in_cleanup = true;
                    self.decode_cleanup(&mut mq)?;
                    self.in_cleanup = false;
                }
            }
            if self.style & 0x02 != 0 && !lazy {
                mq.contexts_mut().reset();
            }
            if pass_type == 2 {
                pass_type = 0;
                self.bit_plane -= 1;
            } else {
                pass_type += 1;
            }
        }
        Ok(())
    }

    fn is_lazy_pass(&self, max_bit_plane: i32, pass_type: i32) -> bool {
        if self.style & 0x01 == 0 {
            return false;
        }
        let pass_index_from_msb = (max_bit_plane - self.bit_plane) * 3 + pass_type;
        pass_index_from_msb >= 10 && pass_type != 2
    }

    fn decode_sigpass(&mut self, mq: &mut MqDecoder<'_>, raw: &mut RawBitReader<'_>, lazy: bool) {
        for stripe in (0..self.height).step_by(4) {
            for x in 0..self.width {
                for dy in 0..4 {
                    let y = stripe + dy;
                    if y >= self.height {
                        break;
                    }
                    let index = self.index(x, y);
                    let flags = self.flags[index];
                    if flags & SIG != 0 || flags & SIG_NEIGHBORS == 0 {
                        continue;
                    }
                    let bit = if lazy {
                        raw.read_bit()
                    } else {
                        mq.decode(zero_context(flags, self.orientation))
                    };
                    self.flags[index] |= VISITED;
                    if bit {
                        if lazy {
                            self.set_significant_raw(x, y, index, raw);
                        } else {
                            self.set_significant_mq(x, y, index, mq);
                        }
                    }
                }
            }
        }
    }

    fn decode_refpass(&mut self, mq: &mut MqDecoder<'_>, raw: &mut RawBitReader<'_>, lazy: bool) {
        for stripe in (0..self.height).step_by(4) {
            for x in 0..self.width {
                for dy in 0..4 {
                    let y = stripe + dy;
                    if y >= self.height {
                        break;
                    }
                    let index = self.index(x, y);
                    let flags = self.flags[index];
                    if flags & SIG == 0 || flags & VISITED != 0 {
                        continue;
                    }
                    let bit = if lazy {
                        raw.read_bit()
                    } else {
                        mq.decode(magnitude_context(flags))
                    };
                    let half = (1i32 << self.bit_plane) >> 1;
                    if self.data[index] < 0 {
                        self.data[index] += if bit { -half } else { half };
                    } else {
                        self.data[index] += if bit { half } else { -half };
                    }
                    self.flags[index] |= REFINED;
                }
            }
        }
    }

    fn decode_cleanup(&mut self, mq: &mut MqDecoder<'_>) -> Result<(), String> {
        for stripe in (0..self.height).step_by(4) {
            for x in 0..self.width {
                let mut can_run_length = stripe + 3 < self.height;
                if can_run_length {
                    for dy in 0..4 {
                        let flags = self.flags[self.index(x, stripe + dy)];
                        if flags & (VISITED | SIG | SIG_NEIGHBORS) != 0 {
                            can_run_length = false;
                            break;
                        }
                    }
                }
                if can_run_length && !mq.decode(CTX_RUN) {
                    continue;
                }
                let run_length = if can_run_length {
                    (i32::from(mq.decode(CTX_UNIFORM)) << 1) | i32::from(mq.decode(CTX_UNIFORM))
                } else {
                    -1
                };
                for dy in 0..4 {
                    let y = stripe + dy;
                    if y >= self.height {
                        break;
                    }
                    if can_run_length && (dy as i32) < run_length {
                        continue;
                    }
                    let index = self.index(x, y);
                    let flags = self.flags[index];
                    if flags & VISITED != 0 || flags & SIG != 0 {
                        self.flags[index] &= !VISITED;
                        continue;
                    }
                    let significant = if can_run_length && dy as i32 == run_length {
                        true
                    } else {
                        mq.decode(zero_context(flags, self.orientation))
                    };
                    if significant {
                        self.set_significant_mq(x, y, index, mq);
                    }
                    self.flags[index] &= !VISITED;
                }
            }
        }
        Ok(())
    }

    fn set_significant_mq(&mut self, x: usize, y: usize, index: usize, mq: &mut MqDecoder<'_>) {
        if self.style & 0x20 != 0 && self.in_cleanup {
            // SEGSYM: signs of coefficients that become significant in the
            // cleanup pass are coded with the uniform context.
            let sign_bit = mq.decode(CTX_UNIFORM);
            self.set_significant_value(x, y, index, sign_bit);
            return;
        }
        let sign_bit =
            mq.decode(sign_context(self.flags[index])) ^ sign_prediction(self.flags[index]);
        self.set_significant_value(x, y, index, sign_bit);
    }

    fn set_significant_raw(
        &mut self,
        x: usize,
        y: usize,
        index: usize,
        raw: &mut RawBitReader<'_>,
    ) {
        let sign_bit = raw.read_bit();
        self.set_significant_value(x, y, index, sign_bit);
    }

    fn set_significant_value(&mut self, x: usize, y: usize, index: usize, sign_bit: bool) {
        let one = 1i32 << self.bit_plane;
        let value = one | (one >> 1);
        self.data[index] = if sign_bit { -value } else { value };
        if sign_bit {
            self.flags[index] |= SIGN;
        }
        self.flags[index] |= SIG;
        self.update_neighbors(x, y);
    }

    fn update_neighbors(&mut self, x: usize, y: usize) {
        let negative = self.flags[self.index(x, y)] & SIGN != 0;
        if !(self.style & 0x08 != 0 && y & 3 == 0) {
            self.mark(x as i32, y as i32 - 1, SIG_S, negative, SIGN_S);
            let idx_nw = self.index_i(x as i32 - 1, y as i32 - 1);
            let idx_ne = self.index_i(x as i32 + 1, y as i32 - 1);
            self.flags[idx_nw] |= SIG_SE;
            self.flags[idx_ne] |= SIG_SW;
        }
        self.mark(x as i32, y as i32 + 1, SIG_N, negative, SIGN_N);
        self.mark(x as i32 - 1, y as i32, SIG_E, negative, SIGN_E);
        self.mark(x as i32 + 1, y as i32, SIG_W, negative, SIGN_W);
        let idx_sw = self.index_i(x as i32 - 1, y as i32 + 1);
        let idx_se = self.index_i(x as i32 + 1, y as i32 + 1);
        self.flags[idx_sw] |= SIG_NE;
        self.flags[idx_se] |= SIG_NW;
    }

    fn mark(&mut self, x: i32, y: i32, sig_flag: u32, negative: bool, sign_flag: u32) {
        let index = self.index_i(x, y);
        self.flags[index] |= sig_flag;
        if negative {
            self.flags[index] |= sign_flag;
        }
    }

    fn finish(self) -> Vec<i32> {
        let mut result = vec![0i32; self.width * self.height];
        for y in 0..self.height {
            for x in 0..self.width {
                let value = self.data[self.index(x, y)];
                result[y * self.width + x] = if value >= 0 {
                    value >> FRACTIONAL_BITS
                } else {
                    -((-value) >> FRACTIONAL_BITS)
                };
            }
        }
        result
    }
}

struct RawBitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u32,
    bit_count: i32,
}

impl<'a> RawBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            bit_count: 0,
        }
    }

    fn read_bit(&mut self) -> bool {
        if self.bit_count == 0 {
            if self.buffer == 0xFF {
                let next = self.read_byte();
                if next > 0x8F {
                    self.buffer = 0xFF;
                    self.bit_count = 8;
                } else {
                    self.buffer = u32::from(next);
                    self.bit_count = 7;
                }
            } else {
                self.buffer = u32::from(self.read_byte());
                self.bit_count = 8;
            }
        }
        self.bit_count -= 1;
        (self.buffer >> self.bit_count) & 1 != 0
    }

    fn read_byte(&mut self) -> u8 {
        let byte = self.data.get(self.position).copied().unwrap_or(0xFF);
        self.position += 1;
        byte
    }
}

fn zero_context(flags: u32, orientation: u8) -> usize {
    let mut horizontal = u32::from(flags & SIG_W != 0) + u32::from(flags & SIG_E != 0);
    let mut vertical = u32::from(flags & SIG_N != 0) + u32::from(flags & SIG_S != 0);
    let diagonal = u32::from(flags & SIG_NW != 0)
        + u32::from(flags & SIG_NE != 0)
        + u32::from(flags & SIG_SW != 0)
        + u32::from(flags & SIG_SE != 0);
    if orientation == 1 {
        std::mem::swap(&mut horizontal, &mut vertical);
    }
    let n = if orientation == 3 {
        let hv = horizontal + vertical;
        match diagonal {
            0 => match hv {
                0 => 0,
                1 => 1,
                _ => 2,
            },
            1 => match hv {
                0 => 3,
                1 => 4,
                _ => 5,
            },
            2 => {
                if hv == 0 {
                    6
                } else {
                    7
                }
            }
            _ => 8,
        }
    } else {
        match horizontal {
            0 => match vertical {
                0 => match diagonal {
                    0 => 0,
                    1 => 1,
                    _ => 2,
                },
                1 => 3,
                _ => 4,
            },
            1 => match vertical {
                0 => {
                    if diagonal == 0 {
                        5
                    } else {
                        6
                    }
                }
                _ => 7,
            },
            _ => 8,
        }
    };
    CTX_ZC + n
}

fn sign_context(flags: u32) -> usize {
    let (mut horizontal, mut vertical) = sign_contributions(flags);
    if horizontal < 0 {
        horizontal = -horizontal;
        vertical = -vertical;
    }
    let n = if horizontal == 0 {
        if vertical == 0 {
            0
        } else {
            1
        }
    } else {
        match vertical {
            -1 => 2,
            0 => 3,
            _ => 4,
        }
    };
    CTX_SIGN + n
}

fn sign_prediction(flags: u32) -> bool {
    let (horizontal, vertical) = sign_contributions(flags);
    if horizontal == 0 && vertical == 0 {
        return false;
    }
    !(horizontal > 0 || (horizontal == 0 && vertical > 0))
}

fn sign_contributions(flags: u32) -> (i32, i32) {
    let pos = |sig: u32, sign: u32| -> i32 {
        if flags & sig != 0 && flags & sign == 0 {
            1
        } else {
            0
        }
    };
    let neg = |sig: u32, sign: u32| -> i32 {
        if flags & sig != 0 && flags & sign != 0 {
            1
        } else {
            0
        }
    };
    let horizontal = (pos(SIG_E, SIGN_E) + pos(SIG_W, SIGN_W)).min(1)
        - (neg(SIG_E, SIGN_E) + neg(SIG_W, SIGN_W)).min(1);
    let vertical = (pos(SIG_N, SIGN_N) + pos(SIG_S, SIGN_S)).min(1)
        - (neg(SIG_N, SIGN_N) + neg(SIG_S, SIGN_S)).min(1);
    (horizontal, vertical)
}

fn magnitude_context(flags: u32) -> usize {
    if flags & REFINED != 0 {
        CTX_MAG + 2
    } else if flags & SIG_NEIGHBORS != 0 {
        CTX_MAG + 1
    } else {
        CTX_MAG
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(width: usize, height: usize) -> Vec<i32> {
        (0..width * height)
            .map(|i| ((i * 37 + i * i * 11) % 200) as i32 - 100)
            .collect()
    }

    #[test]
    fn round_trips_code_blocks_exactly() {
        for (width, height, orientation) in [
            (1, 1, 0),
            (1, 17, 1),
            (17, 1, 2),
            (13, 9, 3),
            (32, 24, 1),
            (64, 64, 3),
        ] {
            let values = pattern(width, height);
            let coefficients: Vec<i32> = values.iter().map(|v| v * 64).collect();
            let encoded = encode_code_block(&coefficients, width, height, orientation, 0).unwrap();
            let segment = crate::geometry::CodeBlockSegment {
                data: encoded.data.clone(),
                pass_count: encoded.pass_count,
            };
            let decoded = decode_code_block(
                &[segment],
                width,
                height,
                orientation,
                encoded.max_bit_plane,
                0,
            )
            .unwrap();
            assert_eq!(decoded, values, "{width}x{height} orient={orientation}");
        }
    }

    #[test]
    fn all_zero_block_has_no_passes() {
        let encoded = encode_code_block(&[0i32; 16], 4, 4, 0, 0).unwrap();
        assert_eq!(encoded.pass_count, 0);
        assert!(encoded.data.is_empty());
    }

    #[test]
    fn reset_vertical_causal_and_sign_segment_styles_round_trip() {
        for style in [0x02u8, 0x08, 0x20, 0x02 | 0x20] {
            for (width, height, orientation) in [(13, 9, 3), (32, 24, 1), (17, 17, 0)] {
                let values = pattern(width, height);
                let coefficients: Vec<i32> = values.iter().map(|v| v * 64).collect();
                let encoded =
                    encode_code_block(&coefficients, width, height, orientation, style).unwrap();
                let segment = crate::geometry::CodeBlockSegment {
                    data: encoded.data.clone(),
                    pass_count: encoded.pass_count,
                };
                let decoded = decode_code_block(
                    &[segment],
                    width,
                    height,
                    orientation,
                    encoded.max_bit_plane,
                    style,
                )
                .unwrap();
                assert_eq!(
                    decoded, values,
                    "style=0x{style:02X} {width}x{height} orient={orientation}"
                );
            }
        }
    }

    #[test]
    fn context_derivation_matches_known_shapes() {
        assert_eq!(zero_context(0, 0), 0);
        assert_eq!(zero_context(SIG_E, 0), 5);
        assert_eq!(zero_context(SIG_N, 0), 3);
        assert_eq!(zero_context(SIG_N | SIG_S | SIG_E | SIG_W, 3), 2);
        assert_eq!(zero_context(SIG_N | SIG_S | SIG_E | SIG_W | SIG_NW, 3), 5);
        assert_eq!(sign_context(0), 9);
        assert!(sign_prediction(SIG_W | SIGN_W));
    }
}
