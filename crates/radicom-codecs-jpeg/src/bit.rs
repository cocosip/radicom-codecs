#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    #[inline]
    pub(crate) fn write_bits(&mut self, value: u32, count: u8) -> Result<(), String> {
        if count == 0 || count > 32 || (count < 32 && value >= (1u32 << count)) {
            return Err(format!("invalid bit value {value:#x} with width {count}"));
        }
        let bits = (u64::from(self.current) << count) | u64::from(value);
        self.used += count;
        while self.used >= 8 {
            self.used -= 8;
            self.push_byte((bits >> self.used) as u8);
        }
        self.current = bits as u8 & ((1u8 << self.used) - 1);
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<Vec<u8>, String> {
        self.flush_to_byte();
        Ok(self.bytes)
    }

    pub(crate) fn flush_to_byte(&mut self) {
        if self.used != 0 {
            self.current = (self.current << (8 - self.used)) | ((1u8 << (8 - self.used)) - 1);
            self.push_byte(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    pub(crate) fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    fn push_byte(&mut self, byte: u8) {
        self.bytes.push(byte);
        if byte == 0xff {
            self.bytes.push(0);
        }
    }
}

#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    remaining: u8,
    stuffed: u8,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            remaining: 0,
            stuffed: 0,
        }
    }

    #[inline]
    pub(crate) fn read_bits(&mut self, count: u8) -> Result<u32, String> {
        if count == 0 || count > 32 {
            return Err(format!("invalid bit width {count}"));
        }
        if self.remaining < count {
            self.refill();
            if self.remaining < count {
                return Err("truncated JPEG entropy data or premature marker".to_owned());
            }
        }
        self.remaining -= count;
        Ok(((self.buffer >> self.remaining) & ((1u64 << count) - 1)) as u32)
    }

    #[inline]
    pub(crate) fn peek_byte(&mut self) -> Option<u8> {
        self.peek_prefix(8).map(|value| value as u8)
    }

    #[inline]
    pub(crate) fn peek_prefix(&mut self, count: u8) -> Option<usize> {
        if self.remaining < count {
            self.refill();
        }
        if self.remaining >= count {
            Some(((self.buffer >> (self.remaining - count)) & ((1 << count) - 1)) as usize)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn consume_prefix(&mut self, count: u8) {
        self.remaining -= count;
    }

    pub(crate) fn take_marker(&mut self) -> Result<Option<u8>, String> {
        if self.remaining != 0 {
            return Ok(None);
        }
        if self.position >= self.data.len() {
            return Ok(None);
        }
        if self.data[self.position] != 0xff {
            return Ok(None);
        }
        self.position += 1;
        while self.position < self.data.len() && self.data[self.position] == 0xff {
            self.position += 1;
        }
        let code = *self
            .data
            .get(self.position)
            .ok_or_else(|| "truncated marker after entropy data".to_owned())?;
        self.position += 1;
        if code == 0 {
            return Err("byte-stuffed 0xff is not a marker boundary".to_owned());
        }
        Ok(Some(code))
    }

    pub(crate) fn align_to_byte(&mut self) {
        self.position = self.position();
        self.remaining = 0;
    }

    /// Byte offset of the next unread byte in the stream.
    pub(crate) fn position(&self) -> usize {
        // Each unused whole byte is still unread. A stuffed byte also occupies
        // a following zero in the input.
        let unread = self.remaining / 8;
        let mask = ((1u16 << unread) - 1) as u8;
        self.position - usize::from(unread) - (self.stuffed & mask).count_ones() as usize
    }

    fn refill(&mut self) {
        while self.remaining <= 56 {
            if self.remaining <= 32 {
                if let Some(bytes) = self.data[self.position..].first_chunk::<4>() {
                    let word = u32::from_be_bytes(*bytes);
                    let inverted = !word;
                    let has_ff = inverted.wrapping_sub(0x0101_0101) & !inverted & 0x8080_8080 != 0;
                    if !has_ff {
                        self.position += 4;
                        self.stuffed <<= 4;
                        self.buffer = (self.buffer << 32) | u64::from(word);
                        self.remaining += 32;
                        continue;
                    }
                }
            }
            let Some(&byte) = self.data.get(self.position) else {
                break;
            };
            let stuffed = byte == 0xff;
            if stuffed && self.data.get(self.position + 1) != Some(&0) {
                break;
            }
            self.position += 1 + usize::from(stuffed);
            self.stuffed = (self.stuffed << 1) | u8::from(stuffed);
            self.buffer = (self.buffer << 8) | u64::from(byte);
            self.remaining += 8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_msb_bits_with_byte_stuffing() {
        let mut writer = BitWriter::default();
        writer.write_bits(0xff, 8).unwrap();
        writer.write_bits(0b101, 3).unwrap();
        let encoded = writer.finish().unwrap();
        assert_eq!(encoded, vec![0xff, 0x00, 0b1011_1111]);

        let mut reader = BitReader::new(&encoded);
        assert_eq!(reader.read_bits(8).unwrap(), 0xff);
        assert_eq!(reader.read_bits(3).unwrap(), 0b101);
        assert!(reader.read_bits(1).is_ok());
    }

    #[test]
    fn reports_a_marker_after_entropy_data() {
        let mut reader = BitReader::new(&[0x12, 0xff, 0xd9]);
        assert_eq!(reader.read_bits(8).unwrap(), 0x12);
        assert_eq!(reader.take_marker().unwrap(), Some(0xd9));
    }

    #[test]
    fn rejects_invalid_bit_widths_and_truncated_input() {
        let mut reader = BitReader::new(&[0x80]);
        assert!(reader.read_bits(0).is_err());
        assert!(reader.read_bits(33).is_err());
        assert!(reader.read_bits(2).is_ok());
        assert!(reader.read_bits(7).is_err());
    }

    #[test]
    fn lookahead_handles_stuffing_without_consuming_a_marker() {
        let bytes = [0xaa, 0xff, 0x00, 0xf0, 0xff, 0xd0];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(3).unwrap(), 0b101);
        assert_eq!(reader.peek_byte(), Some(0b0101_0111));
        assert_eq!(reader.position(), 1);
        assert_eq!(reader.read_bits(16).unwrap(), 0x57ff);
        assert_eq!(reader.peek_byte(), None);
        reader.align_to_byte();
        assert_eq!(reader.take_marker().unwrap(), Some(0xd0));
    }
}
