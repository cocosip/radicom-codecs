//! Bounded bit-level I/O for JPEG 2000 marker segments and packet headers.
//!
//! Packet-header bit streams use JPEG 2000 bit stuffing: after a `0xFF` byte
//! only seven bits of the next byte are available, because its most
//! significant bit is the stuffed zero. The reader and writer below implement
//! that rule exactly.

/// MSB-first bit reader over a byte slice.
#[derive(Debug, Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u16,
    bit_count: i32,
    stuffed: bool,
}

impl<'a> BitReader<'a> {
    #[cfg(test)]
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            bit_count: 0,
            stuffed: false,
        }
    }

    pub(crate) fn new_stuffed(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            bit_count: 0,
            stuffed: true,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn read_bit(&mut self) -> Result<bool, String> {
        if self.bit_count == 0 {
            self.byte_in()?;
        }
        self.bit_count -= 1;
        Ok((self.buffer >> self.bit_count) & 1 != 0)
    }

    pub(crate) fn read_bits(&mut self, count: u8) -> Result<u32, String> {
        if count > 32 {
            return Err("bit count exceeds 32".into());
        }
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }

    pub(crate) fn align_to_byte(&mut self) {
        if self.stuffed && (self.buffer & 0xFF) == 0xFF {
            // Consume the stuffed byte so the next read starts at a boundary.
            let _ = self.byte_in();
        }
        self.bit_count = 0;
    }

    fn byte_in(&mut self) -> Result<(), String> {
        if self.position >= self.data.len() {
            return Err("bitstream exhausted".into());
        }
        self.buffer <<= 8;
        self.bit_count = if self.stuffed && self.buffer == 0xFF00 {
            7
        } else {
            8
        };
        self.buffer |= u16::from(self.data[self.position]);
        self.position += 1;
        Ok(())
    }
}

/// MSB-first bit writer with optional packet-header byte stuffing.
#[derive(Debug, Default, Clone)]
pub(crate) struct BitWriter {
    data: Vec<u8>,
    current: u8,
    bit: u8,
    stuffed: bool,
}

impl BitWriter {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn new_stuffed() -> Self {
        Self {
            data: Vec::new(),
            current: 0,
            bit: 0,
            stuffed: true,
        }
    }

    pub(crate) fn write_bit(&mut self, value: bool) {
        if value {
            self.current |= 1 << (7 - self.bit);
        }
        self.bit += 1;
        if self.bit == 8 {
            self.flush_byte();
        }
    }

    pub(crate) fn write_bits(&mut self, value: u32, count: u8) {
        for i in (0..count).rev() {
            self.write_bit((value >> i) & 1 != 0);
        }
    }

    fn flush_byte(&mut self) {
        let value = self.current;
        self.data.push(value);
        self.current = 0;
        self.bit = if self.stuffed && value == 0xFF { 1 } else { 0 };
    }

    pub(crate) fn into_bytes(mut self) -> Vec<u8> {
        if self.bit > 0 {
            self.flush_byte();
        }
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first_bits() {
        let mut r = BitReader::new(&[0b1011_0010, 0b0100_0000]);
        assert!(r.read_bit().unwrap());
        assert!(!r.read_bit().unwrap());
        assert!(r.read_bit().unwrap());
        assert!(r.read_bit().unwrap());
        assert_eq!(r.read_bits(4).unwrap(), 0b0010);
        assert_eq!(r.read_bits(3).unwrap(), 0b010);
        assert_eq!(r.read_bits(5).unwrap(), 0);
        assert!(r.read_bit().is_err());
    }

    #[test]
    fn stuffed_reader_skips_the_stuffed_msb() {
        let data = [0xFF, 0x81, 0x80];
        let mut r = BitReader::new_stuffed(&data);
        for _ in 0..8 {
            assert!(r.read_bit().unwrap());
        }
        // Only seven bits of 0x81 are visible: its MSB is the stuffed zero.
        assert!(!r.read_bit().unwrap());
        for _ in 0..5 {
            assert!(!r.read_bit().unwrap());
        }
        assert!(r.read_bit().unwrap());
        assert!(r.read_bit().unwrap());
    }

    #[test]
    fn writer_round_trips_and_stuffs() {
        let mut w = BitWriter::new();
        w.write_bits(0b1011, 4);
        w.write_bits(0b0011, 4);
        assert_eq!(w.into_bytes(), vec![0b1011_0011]);

        let mut w = BitWriter::new_stuffed();
        w.write_bits(0xFF, 8);
        w.write_bits(0b1010, 4);
        let bytes = w.into_bytes();
        assert_eq!(bytes, vec![0xFF, 0b0101_0000]);

        let mut r = BitReader::new_stuffed(&bytes);
        assert_eq!(r.read_bits(8).unwrap(), 0xFF);
        assert_eq!(r.read_bits(4).unwrap(), 0b1010);
    }

    #[test]
    fn align_to_byte_consumes_stuffing() {
        let data = [0xFF, 0x80, 0xAA];
        let mut r = BitReader::new_stuffed(&data);
        assert_eq!(r.read_bits(8).unwrap(), 0xFF);
        r.align_to_byte();
        assert_eq!(r.read_bits(8).unwrap(), 0xAA);
    }
}
