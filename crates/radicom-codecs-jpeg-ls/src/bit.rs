#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
    stuff_pending: bool,
}
impl BitWriter {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn write_bit(&mut self, bit: bool) {
        if self.stuff_pending {
            self.used += 1;
            self.stuff_pending = false;
        }
        self.current = (self.current << 1) | u8::from(bit);
        self.used += 1;
        if self.used == 8 {
            self.push(self.current);
            self.current = 0;
            self.used = 0;
        }
    }
    pub(crate) fn write_bits(&mut self, value: u32, count: u8) -> Result<(), String> {
        if count == 0 || count > 32 || (count < 32 && value >= (1u32 << count)) {
            return Err("invalid bit width/value".into());
        }
        for i in (0..count).rev() {
            self.write_bit(((value >> i) & 1) != 0);
        }
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.stuff_pending {
            self.used += 1;
            self.stuff_pending = false;
        }
        if self.used != 0 {
            self.current <<= 8 - self.used;
            self.push(self.current);
        }
        self.bytes
    }
    fn push(&mut self, b: u8) {
        self.bytes.push(b);
        if b == 0xff {
            self.stuff_pending = true;
        }
    }
}
#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    current: u8,
    left: u8,
    previous_ff: bool,
}
impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            current: 0,
            left: 0,
            previous_ff: false,
        }
    }
    pub(crate) fn read_bit(&mut self) -> Result<bool, String> {
        if self.left == 0 {
            self.fill()?
        }
        let bit = (self.current & (1 << (self.left - 1))) != 0;
        self.left -= 1;
        Ok(bit)
    }
    pub(crate) fn read_bits(&mut self, n: u8) -> Result<u32, String> {
        if n == 0 || n > 32 {
            return Err("invalid bit width".into());
        }
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.read_bit()?)
        }
        Ok(v)
    }
    /// Verifies that the segment ended on a byte boundary with zero padding and
    /// no unconsumed bytes. Takes `&mut self` so a restart boundary can finish
    /// one scan segment and continue with a fresh reader.
    pub(crate) fn finish(&mut self) -> Result<(), String> {
        if self.previous_ff && self.left == 0 {
            // A full final 0xff still needs a stuffed zero and zero padding.
            self.fill()?;
        }
        if self.left > 0 {
            let mask = if self.left == 8 {
                u8::MAX
            } else {
                (1u8 << self.left) - 1
            };
            if self.current & mask != 0 {
                return Err("non-zero JPEG-LS padding bits".into());
            }
        }
        if self.pos < self.data.len() {
            // Writers may pad an entropy segment with zero bytes before the
            // next marker (fo-dicom pads a fragment to an even length) and
            // CharLS accepts a trailing 0x00 as well.
            if self.data[self.pos..].iter().any(|byte| *byte != 0) {
                return Err("trailing JPEG-LS entropy data".into());
            }
        }
        Ok(())
    }
    fn fill(&mut self) -> Result<(), String> {
        let b = *self.data.get(self.pos).ok_or("truncated scan")?;
        self.pos += 1;
        if self.previous_ff {
            if b & 0x80 != 0 {
                return Err("invalid JPEG-LS stuffed bit".into());
            }
            self.current = b;
            self.left = 7;
            self.previous_ff = false
        } else {
            self.current = b;
            self.left = 8
        }
        if b == 0xff {
            self.previous_ff = true
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bit_stuffing_roundtrip() {
        let mut w = BitWriter::new();
        w.write_bits(0xff, 8).unwrap();
        w.write_bits(0b101, 3).unwrap();
        let b = w.finish();
        assert_eq!(b, vec![0xff, 0x50]);
        let mut r = BitReader::new(&b);
        assert_eq!(r.read_bits(8).unwrap(), 0xff);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
    }

    #[test]
    fn rejects_non_zero_padding_bits() {
        let mut reader = BitReader::new(&[0b1000_0001]);
        assert!(reader.read_bit().unwrap());
        assert!(reader.finish().is_err());
    }

    #[test]
    fn accepts_zero_stuffing_after_a_final_full_ff_byte() {
        let mut writer = BitWriter::new();
        writer.write_bits(255, 8).unwrap();
        let bytes = writer.finish();
        assert_eq!(bytes, [255, 0]);
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(8).unwrap(), 255);
        reader.finish().unwrap();
    }

    #[test]
    fn rejects_nonzero_padding_in_a_partially_consumed_ff_byte() {
        let mut reader = BitReader::new(&[255, 0]);
        assert!(reader.read_bit().unwrap());
        assert!(reader.finish().is_err());
    }
}
