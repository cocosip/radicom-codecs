#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    current: u64,
    used: u8,
}

impl BitWriter {
    #[inline]
    pub(crate) fn write_bits(&mut self, value: u32, count: u8) -> Result<(), String> {
        if count == 0 || count > 32 || (count < 32 && value >= (1u32 << count)) {
            return Err(format!("invalid bit value {value:#x} with width {count}"));
        }
        self.write_codeword(value, count);
        Ok(())
    }

    /// Appends a previously constructed Huffman code and its amplitude.
    #[inline]
    pub(crate) fn write_codeword(&mut self, value: u32, count: u8) {
        debug_assert!((1..=32).contains(&count) && (count == 32 || value < 1 << count));
        self.current = (self.current << count) | u64::from(value);
        self.used += count;
        if self.used >= 32 {
            self.used -= 32;
            let word = (self.current >> self.used) as u32;
            let bytes = word.to_be_bytes();
            if bytes.contains(&0xff) {
                for byte in bytes {
                    self.push_byte(byte);
                }
            } else {
                self.bytes.extend_from_slice(&bytes);
            }
        }
    }

    pub(crate) fn finish(mut self) -> Result<Vec<u8>, String> {
        self.flush_to_byte();
        Ok(self.bytes)
    }

    pub(crate) fn flush_to_byte(&mut self) {
        while self.used >= 8 {
            self.used -= 8;
            self.push_byte((self.current >> self.used) as u8);
        }
        if self.used != 0 {
            let padding = 8 - self.used;
            self.push_byte(((self.current as u8) << padding) | ((1u8 << padding) - 1));
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
    // Unread bits are at the high end. Slots below them are zero.
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
        let value = (self.buffer >> (64 - count)) as u32;
        self.consume_prefix(count);
        Ok(value)
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
            Some((self.buffer >> (64 - count)) as usize)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn consume_prefix(&mut self, count: u8) {
        self.remaining -= count;
        self.buffer = self.buffer.unbounded_shl(u32::from(count));
    }

    /// Left-aligned unstuffed bits, for decoding several symbols per refill.
    #[inline]
    pub(crate) fn window(&mut self) -> (u64, u8) {
        if self.remaining < 32 {
            self.refill();
        }
        (self.buffer, self.remaining)
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
        self.buffer = 0;
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
        if let Some(bytes) = self.data[self.position..].first_chunk::<8>() {
            let word = u64::from_be_bytes(*bytes);
            let inverted = !word;
            if inverted.wrapping_sub(0x0101_0101_0101_0101) & !inverted & 0x8080_8080_8080_8080 == 0
            {
                let count = (64 - self.remaining) / 8;
                let mask = u64::MAX << (64 - count * 8);
                self.buffer |= (word & mask) >> self.remaining;
                self.stuffed = (u16::from(self.stuffed) << count) as u8;
                self.remaining += count * 8;
                self.position += usize::from(count);
                return;
            }
        }
        while self.remaining <= 56 {
            if self.remaining <= 32 {
                if let Some(bytes) = self.data[self.position..].first_chunk::<4>() {
                    let word = u32::from_be_bytes(*bytes);
                    let inverted = !word;
                    let has_ff = inverted.wrapping_sub(0x0101_0101) & !inverted & 0x8080_8080 != 0;
                    if !has_ff {
                        self.position += 4;
                        self.stuffed <<= 4;
                        self.buffer |= u64::from(word) << (32 - self.remaining);
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
            self.buffer |= u64::from(byte) << (56 - self.remaining);
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
    fn stuffs_bytes_across_word_and_restart_boundaries() {
        let mut writer = BitWriter::default();
        writer.write_bits(0x0f, 4).unwrap();
        writer.write_bits(0xff00_0fff, 32).unwrap();
        writer.flush_to_byte();
        assert_eq!(writer.take_bytes(), [0xff, 0, 0xf0, 0, 0xff, 0, 0xff, 0]);
        writer.write_bits(0x12, 8).unwrap();
        assert_eq!(writer.finish().unwrap(), [0x12]);
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

    #[test]
    fn full_window_refills_across_stuffing_and_a_restart() {
        let data = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0xff, 0x00, 0x88, 0x99, 0xff, 0xd2,
            0xaa, 0xbb,
        ];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.window(), (0x0011_2233_4455_6677, 64));
        assert_eq!(reader.read_bits(4).unwrap(), 0);
        assert_eq!(reader.read_bits(28).unwrap(), 0x0011_2233);
        assert_eq!(reader.read_bits(32).unwrap(), 0x4455_6677);
        assert_eq!(reader.read_bits(16).unwrap(), 0xff88);
        assert_eq!(reader.read_bits(3).unwrap(), 4);
        reader.align_to_byte();
        assert_eq!(reader.take_marker().unwrap(), Some(0xd2));
        assert_eq!(reader.read_bits(16).unwrap(), 0xaabb);
    }
}
