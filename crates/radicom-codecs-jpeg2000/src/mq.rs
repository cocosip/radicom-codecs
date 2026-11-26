//! JPEG 2000 MQ arithmetic coder (ISO/IEC 15444-1 Annex C).
//!
//! The decoder stores the probability index and most probable symbol in one
//! byte. The encoder caches each context's probability and both transitions.

const QE: [u16; 47] = [
    0x5601, 0x3401, 0x1801, 0x0AC1, 0x0521, 0x0221, 0x5601, 0x5401, 0x4801, 0x3801, 0x3001, 0x2401,
    0x1C01, 0x1601, 0x5601, 0x5401, 0x5101, 0x4801, 0x3801, 0x3401, 0x3001, 0x2801, 0x2401, 0x2201,
    0x1C01, 0x1801, 0x1601, 0x1401, 0x1201, 0x1101, 0x0AC1, 0x09C1, 0x08A1, 0x0521, 0x0441, 0x02A1,
    0x0221, 0x0141, 0x0111, 0x0085, 0x0049, 0x0025, 0x0015, 0x0009, 0x0005, 0x0001, 0x5601,
];

const NMPS: [u8; 47] = [
    1, 2, 3, 4, 5, 38, 7, 8, 9, 10, 11, 12, 13, 29, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 45, 46,
];

const NLPS: [u8; 47] = [
    1, 6, 9, 12, 29, 33, 6, 14, 14, 14, 17, 18, 20, 21, 14, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22,
    23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 46,
];

const SWITCH: [bool; 47] = [
    true, false, false, false, false, false, true, false, false, false, false, false, false, false,
    true, false, false, false, false, false, false, false, false, false, false, false, false,
    false, false, false, false, false, false, false, false, false, false, false, false, false,
    false, false, false, false, false, false, false,
];

pub(crate) const CONTEXT_COUNT: usize = 19;
pub(crate) const CTX_ZC: usize = 0;
pub(crate) const CTX_RUN: usize = 17;
pub(crate) const CTX_UNIFORM: usize = 18;

const ENCODE_STATES: [u32; 94] = {
    let mut table = [0; 94];
    let mut index = 0;
    while index < table.len() {
        let state = index / 2;
        let mps = (index & 1) as u32;
        let next_mps = NMPS[state] as u32 * 2 + mps;
        let next_lps = NLPS[state] as u32 * 2 + (mps ^ SWITCH[state] as u32);
        // Qe in bits 0..15, MPS in bit 16, and two 7-bit transition indices.
        table[index] = QE[state] as u32 | (mps << 16) | (next_mps << 17) | (next_lps << 24);
        index += 1;
    }
    table
};

const INITIAL_ENCODE_CONTEXTS: [u32; CONTEXT_COUNT] = {
    let mut states = [ENCODE_STATES[0]; CONTEXT_COUNT];
    states[CTX_UNIFORM] = ENCODE_STATES[46 * 2];
    states[CTX_RUN] = ENCODE_STATES[3 * 2];
    states[CTX_ZC] = ENCODE_STATES[4 * 2];
    states
};

#[derive(Debug, Clone)]
pub(crate) struct Contexts {
    states: [u8; CONTEXT_COUNT],
}

impl Default for Contexts {
    fn default() -> Self {
        Self {
            states: [0; CONTEXT_COUNT],
        }
    }
}

impl Contexts {
    pub(crate) fn reset(&mut self) {
        self.states = [0; CONTEXT_COUNT];
        self.states[CTX_UNIFORM] = 46;
        self.states[CTX_RUN] = 3;
        self.states[CTX_ZC] = 4;
    }

    #[allow(dead_code)]
    pub(crate) fn set(&mut self, context: usize, state: u8) {
        self.states[context] = state;
    }
}

#[derive(Debug)]
pub(crate) struct MqEncoder {
    data: Vec<u8>,
    a: u32,
    c: u32,
    ct: i32,
    contexts: [u32; CONTEXT_COUNT],
}

impl MqEncoder {
    pub(crate) fn new() -> Self {
        Self {
            data: vec![0],
            a: 0x8000,
            c: 0,
            ct: 12,
            contexts: INITIAL_ENCODE_CONTEXTS,
        }
    }

    pub(crate) fn restart(&mut self, capacity: usize) {
        self.data.clear();
        self.data.reserve(capacity + 1);
        self.data.push(0);
        self.a = 0x8000;
        self.c = 0;
        self.ct = 12;
        self.reset_contexts();
    }

    pub(crate) fn reset_contexts(&mut self) {
        self.contexts = INITIAL_ENCODE_CONTEXTS;
    }

    #[inline]
    pub(crate) fn encode(&mut self, bit: bool, context: usize) {
        let cx = self.contexts[context];
        let mps = cx & 0x1_0000 != 0;
        let qe = cx & 0xFFFF;
        self.a -= qe;
        if bit == mps {
            if self.a & 0x8000 == 0 {
                if self.a < qe {
                    self.a = qe;
                } else {
                    self.c += qe;
                }
                self.contexts[context] = ENCODE_STATES[((cx >> 17) & 0x7F) as usize];
                self.renorm();
            } else {
                self.c += qe;
            }
        } else {
            if self.a < qe {
                self.c += qe;
            } else {
                self.a = qe;
            }
            self.contexts[context] = ENCODE_STATES[(cx >> 24) as usize];
            self.renorm();
        }
    }

    fn renorm(&mut self) {
        while self.a < 0x8000 {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byte_out();
            }
        }
    }

    fn byte_out(&mut self) {
        let previous = self.data.last_mut().expect("MQ sentinel or previous byte");
        if *previous != 0xFF && self.c & 0x0800_0000 != 0 {
            *previous += 1;
            self.c &= 0x07FF_FFFF;
        }
        if *previous == 0xFF {
            self.data.push((self.c >> 20) as u8);
            self.c &= 0xFFFFF;
            self.ct = 7;
        } else {
            self.data.push((self.c >> 19) as u8);
            self.c &= 0x7FFFF;
            self.ct = 8;
        }
    }

    /// Bytes emitted so far, matching OpenJPEG's `opj_mqc_numbytes()`. The
    /// sentinel and the pending byte are excluded, with wrapping when only
    /// the sentinel is present.
    pub(crate) fn numbytes(&self) -> u32 {
        (self.data.len() as u32).wrapping_sub(2)
    }

    pub(crate) fn flush(&mut self) -> Vec<u8> {
        let temp = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= temp {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byte_out();
        self.c <<= self.ct;
        self.byte_out();
        if self.data.last() == Some(&0xFF) {
            self.data.pop();
        }
        self.data.remove(0);
        std::mem::take(&mut self.data)
    }
}

#[derive(Debug)]
pub(crate) struct MqDecoder<'a> {
    data: &'a [u8],
    position: usize,
    a: u32,
    c: u32,
    ct: i32,
    contexts: Contexts,
}

impl<'a> MqDecoder<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut decoder = Self {
            data,
            position: 0,
            a: 0x8000,
            c: 0,
            ct: 0,
            contexts: Contexts::default(),
        };
        decoder.contexts.reset();
        decoder.c = u32::from(decoder.byte_at(0)) << 16;
        decoder.byte_in();
        decoder.c <<= 7;
        decoder.ct -= 7;
        decoder.a = 0x8000;
        decoder
    }

    pub(crate) fn contexts_mut(&mut self) -> &mut Contexts {
        &mut self.contexts
    }

    pub(crate) fn decode(&mut self, context: usize) -> bool {
        let cx = self.contexts.states[context];
        let state = usize::from(cx & 0x7F);
        let mps = cx >> 7 != 0;
        let qe = u32::from(QE[state]);
        self.a -= qe;
        if (self.c >> 16) < qe {
            let bit;
            if self.a < qe {
                self.a = qe;
                bit = mps;
                self.contexts.states[context] = NMPS[state] | (u8::from(mps) << 7);
            } else {
                self.a = qe;
                bit = !mps;
                let next_mps = if SWITCH[state] { !mps } else { mps };
                self.contexts.states[context] = NLPS[state] | (u8::from(next_mps) << 7);
            }
            self.renorm();
            return bit;
        }
        self.c -= qe << 16;
        if self.a & 0x8000 != 0 {
            return mps;
        }
        let bit;
        if self.a < qe {
            bit = !mps;
            let next_mps = if SWITCH[state] { !mps } else { mps };
            self.contexts.states[context] = NLPS[state] | (u8::from(next_mps) << 7);
        } else {
            bit = mps;
            self.contexts.states[context] = NMPS[state] | (u8::from(mps) << 7);
        }
        self.renorm();
        bit
    }

    fn renorm(&mut self) {
        while self.a < 0x8000 {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
        }
    }

    fn byte_in(&mut self) {
        let next = self.byte_at(self.position + 1);
        if self.byte_at(self.position) == 0xFF {
            if next > 0x8F {
                self.c += 0xFF00;
                self.ct = 8;
            } else {
                self.position += 1;
                self.c += u32::from(next) << 9;
                self.ct = 7;
            }
        } else {
            self.position += 1;
            self.c += u32::from(next) << 8;
            self.ct = 8;
        }
    }

    fn byte_at(&self, index: usize) -> u8 {
        self.data.get(index).copied().unwrap_or(0xFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_mixed_decision_stream() {
        let mut encoder = MqEncoder::new();
        let mut expected = Vec::new();
        let mut seed = 0x1234_5678u32;
        for i in 0..2000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let bit = seed >> 31 != 0;
            let context = (i * 7 + usize::from(bit)) % CONTEXT_COUNT;
            encoder.encode(bit, context);
            expected.push((context, bit));
        }
        let bytes = encoder.flush();
        let mut decoder = MqDecoder::new(&bytes);
        for (context, bit) in expected {
            assert_eq!(decoder.decode(context), bit, "context={context}");
        }
    }

    #[test]
    fn handles_zero_length_and_truncated_streams() {
        let mut decoder = MqDecoder::new(&[]);
        // The decoder pads with an artificial 0xFF 0xFF terminator.
        let _ = decoder.decode(CTX_UNIFORM);
        let mut decoder = MqDecoder::new(&[0xFF]);
        let _ = decoder.decode(CTX_ZC);
    }

    #[test]
    fn context_state_round_trips_through_packing() {
        let mut contexts = Contexts::default();
        contexts.reset();
        assert_eq!(contexts.states[CTX_UNIFORM] & 0x7F, 46);
        assert_eq!(contexts.states[CTX_RUN] & 0x7F, 3);
        assert_eq!(contexts.states[CTX_ZC] & 0x7F, 4);
    }
}
