//! JPEG 2000 MQ arithmetic coder (ISO/IEC 15444-1 Annex C).
//!
//! Each context caches its probability and both transitions.

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

const PROBABILITY_STATES: [u32; 94] = {
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

const INITIAL_CONTEXTS: [u32; CONTEXT_COUNT] = {
    let mut states = [PROBABILITY_STATES[0]; CONTEXT_COUNT];
    states[CTX_UNIFORM] = PROBABILITY_STATES[46 * 2];
    states[CTX_RUN] = PROBABILITY_STATES[3 * 2];
    states[CTX_ZC] = PROBABILITY_STATES[4 * 2];
    states
};

#[derive(Debug, Clone)]
pub(crate) struct Contexts {
    states: [u32; CONTEXT_COUNT],
}

impl Default for Contexts {
    fn default() -> Self {
        Self {
            states: INITIAL_CONTEXTS,
        }
    }
}

impl Contexts {
    pub(crate) fn reset(&mut self) {
        self.states = INITIAL_CONTEXTS;
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
            contexts: INITIAL_CONTEXTS,
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
        self.contexts = INITIAL_CONTEXTS;
    }

    #[inline]
    pub(crate) fn encode(&mut self, bit: bool, context: usize) {
        let state = self.contexts[context];
        let lps = bit != (state & 0x1_0000 != 0);
        let qe = state & 0xffff;
        let interval = self.a - qe;
        self.a = interval;
        if !lps && interval >= 0x8000 {
            self.c += qe;
            return;
        }
        // With a reduced interval below Qe, MPS and LPS exchange subintervals.
        let select_qe = lps ^ (interval < qe);
        self.a = if select_qe { qe } else { interval };
        self.c += if select_qe { 0 } else { qe };
        let next = if lps {
            state >> 24
        } else {
            (state >> 17) & 0x7f
        };
        self.contexts[context] = PROBABILITY_STATES[next as usize];
        self.renorm();
    }

    #[inline]
    fn renorm(&mut self) {
        let mut remaining = self.a.leading_zeros() - 16;
        self.a <<= remaining;
        while remaining >= self.ct as u32 {
            self.c <<= self.ct;
            remaining -= self.ct as u32;
            self.byte_out();
        }
        self.c <<= remaining;
        self.ct -= remaining as i32;
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
    current_byte: u8,
    a: u32,
    c: u32,
    ct: i32,
    contexts: Contexts,
}

impl<'a> MqDecoder<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let current_byte = data.first().copied().unwrap_or(0xff);
        let mut decoder = Self {
            data,
            position: 1,
            current_byte,
            a: 0x8000,
            c: u32::from(current_byte) << 16,
            ct: 0,
            contexts: Contexts::default(),
        };
        decoder.byte_in();
        decoder.c <<= 7;
        decoder.ct -= 7;
        decoder.a = 0x8000;
        decoder
    }

    pub(crate) fn contexts_mut(&mut self) -> &mut Contexts {
        &mut self.contexts
    }

    #[inline]
    pub(crate) fn decode(&mut self, context: usize) -> bool {
        let state = self.contexts.states[context];
        let mps = state & 0x1_0000 != 0;
        let qe = state & 0xffff;
        let interval = self.a - qe;
        let select_qe = (self.c >> 16) < qe;
        self.a = if select_qe { qe } else { interval };
        if !select_qe {
            self.c -= qe << 16;
            if interval >= 0x8000 {
                return mps;
            }
        }
        let lps = select_qe ^ (interval < qe);
        let next = if lps {
            state >> 24
        } else {
            (state >> 17) & 0x7f
        };
        self.contexts.states[context] = PROBABILITY_STATES[next as usize];
        self.renorm();
        mps ^ lps
    }

    #[inline]
    fn renorm(&mut self) {
        let shift = self.a.leading_zeros() - 16;
        self.a <<= shift;
        let available = self.ct - shift as i32;
        if available >= 0 {
            self.c <<= shift;
            self.ct = available;
        } else {
            self.refill(shift);
        }
    }

    #[inline(never)]
    fn refill(&mut self, mut remaining: u32) {
        while remaining > self.ct as u32 {
            self.c <<= self.ct;
            remaining -= self.ct as u32;
            self.byte_in();
        }
        self.c <<= remaining;
        self.ct -= remaining as i32;
    }

    fn byte_in(&mut self) {
        let next = self.data.get(self.position).copied().unwrap_or(0xff);
        if self.current_byte == 0xff && next > 0x8f {
            self.c += 0xff00;
            self.ct = 8;
        } else {
            let stuffed = u32::from(self.current_byte == 0xff);
            self.position += 1;
            self.current_byte = next;
            self.c += u32::from(next) << (8 + stuffed);
            self.ct = 8 - stuffed as i32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probability_exchange_preserves_interval_and_context_transition() {
        for (initial_a, bit, expected_a, expected_c, expected_state) in [
            (0x8000, false, 0xac02, 0, 2),
            (0x8000, true, 0xa7fc, 0x15804, 3),
            (0xc000, false, 0xd3fe, 0xac02, 2),
            (0xc000, true, 0xac02, 0, 3),
            (0xd601, false, 0x8000, 0x5601, 0),
        ] {
            let mut encoder = MqEncoder::new();
            encoder.a = initial_a;
            encoder.encode(bit, 1);
            assert_eq!(encoder.a, expected_a);
            assert_eq!(encoder.c, expected_c);
            assert_eq!(encoder.contexts[1], PROBABILITY_STATES[expected_state]);
        }
    }

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
        assert_eq!(contexts.states[CTX_UNIFORM], PROBABILITY_STATES[46 * 2]);
        assert_eq!(contexts.states[CTX_RUN], PROBABILITY_STATES[3 * 2]);
        assert_eq!(contexts.states[CTX_ZC], PROBABILITY_STATES[4 * 2]);
    }
}
