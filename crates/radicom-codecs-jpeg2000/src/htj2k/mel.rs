//! HTJ2K MEL (Magnitude Event Length) coding.
//!
//! Ported from the reference MEL coder in `fo-dicom.PureCodecs`
//! (`Jpeg2000HtMel`). MEL codes a run of zero events terminated by a one
//! event, using an adaptive exponent and JPEG 2000 bit stuffing.

const MEL_EXPONENTS: [u32; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

/// MEL bit writer with 0xFF stuffing.
pub(crate) struct MelBitWriter {
    bytes: Vec<u8>,
    current: u8,
    remaining_bits: u8,
}

impl MelBitWriter {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            remaining_bits: 8,
        }
    }

    pub(crate) fn write_bit(&mut self, bit: bool) {
        self.current = (self.current << 1) | u8::from(bit);
        self.remaining_bits -= 1;
        if self.remaining_bits == 0 {
            self.flush();
        }
    }

    pub(crate) fn write_bits(&mut self, value: u32, count: u32) {
        for bit in (0..count).rev() {
            self.write_bit((value >> bit) & 1 != 0);
        }
    }

    pub(crate) fn into_bytes(mut self) -> Vec<u8> {
        if self.remaining_bits < 8 {
            self.current <<= self.remaining_bits;
            self.flush();
        }
        self.bytes
    }

    fn flush(&mut self) {
        let value = self.current;
        self.bytes.push(value);
        self.remaining_bits = if value == 0xFF { 7 } else { 8 };
        self.current = 0;
    }
}

/// MEL bit reader with 0xFF stuffing.
pub(crate) struct MelBitReader<'a> {
    bytes: &'a [u8],
    offset: usize,
    current: u8,
    remaining_bits: u8,
    unstuff_next: bool,
}

impl<'a> MelBitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            current: 0,
            remaining_bits: 0,
            unstuff_next: false,
        }
    }

    pub(crate) fn read_bit(&mut self) -> Result<bool, String> {
        if self.remaining_bits == 0 {
            self.fill_byte()?;
        }
        self.remaining_bits -= 1;
        Ok((self.current >> self.remaining_bits) & 1 != 0)
    }

    pub(crate) fn read_bits(&mut self, count: u32) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }

    fn fill_byte(&mut self) -> Result<(), String> {
        if self.offset >= self.bytes.len() {
            return Err("HTJ2K MEL bitstream ended unexpectedly".into());
        }
        self.current = self.bytes[self.offset];
        self.offset += 1;
        self.remaining_bits = if self.unstuff_next { 7 } else { 8 };
        self.unstuff_next = self.current == 0xFF;
        Ok(())
    }
}

/// Encodes a MEL event sequence (false = zero, true = one).
pub(crate) fn encode_events(events: &[bool]) -> Vec<u8> {
    let mut writer = MelBitWriter::new();
    let mut state = 0usize;
    let mut run = 0u32;
    let mut threshold = 1u32;

    for &event in events {
        if !event {
            run += 1;
            if run >= threshold {
                writer.write_bit(true);
                run = 0;
                state = (state + 1).min(12);
                threshold = 1 << MEL_EXPONENTS[state];
            }
        } else {
            writer.write_bit(false);
            for bit in (0..MEL_EXPONENTS[state]).rev() {
                writer.write_bit((run >> bit) & 1 != 0);
            }
            run = 0;
            state = state.saturating_sub(1);
            threshold = 1 << MEL_EXPONENTS[state];
        }
    }
    if run > 0 {
        writer.write_bit(true);
    }
    writer.into_bytes()
}

/// Decodes exactly `event_count` MEL events.
pub(crate) fn decode_events(bytes: &[u8], event_count: usize) -> Result<Vec<bool>, String> {
    let mut events = Vec::with_capacity(event_count);
    let mut reader = MelBitReader::new(bytes);
    let mut state = 0usize;

    while events.len() < event_count {
        let exponent = MEL_EXPONENTS[state];
        if reader.read_bit()? {
            let zeroes = 1usize << exponent;
            append_zeroes(&mut events, event_count, zeroes);
            state = (state + 1).min(12);
        } else {
            let zeroes = if exponent == 0 {
                0
            } else {
                reader.read_bits(exponent)? as usize
            };
            append_zeroes(&mut events, event_count, zeroes);
            if events.len() < event_count {
                events.push(true);
            }
            state = state.saturating_sub(1);
        }
    }
    Ok(events)
}

fn append_zeroes(events: &mut Vec<bool>, limit: usize, count: usize) {
    for _ in 0..count {
        if events.len() >= limit {
            break;
        }
        events.push(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_event_sequences() {
        let cases: Vec<Vec<bool>> = vec![
            vec![],
            vec![true],
            vec![false, true],
            vec![false; 64],
            vec![true; 32],
            (0..200).map(|i| i % 3 == 0 || i % 7 == 0).collect(),
            (0..1000).map(|i| i % 11 == 0).collect(),
        ];
        for events in cases {
            let bytes = encode_events(&events);
            let decoded = decode_events(&bytes, events.len()).unwrap();
            assert_eq!(decoded, events);
        }
    }

    #[test]
    fn stuffing_is_visible_in_the_stream() {
        // A long run of zero events produces 0xFF bytes and must round trip.
        let events = vec![false; 500];
        let bytes = encode_events(&events);
        assert!(bytes.contains(&0xFF));
        assert_eq!(decode_events(&bytes, events.len()).unwrap(), events);
    }
}
