#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    pub(crate) fn write_bits(&mut self, value: u32, count: u8) -> Result<(), String> {
        if count == 0 || count > 32 || (count < 32 && value >= (1u32 << count)) {
            return Err(format!("invalid bit value {value:#x} with width {count}"));
        }
        for shift in (0..count).rev() {
            self.current = (self.current << 1) | ((value >> shift) as u8 & 1);
            self.used += 1;
            if self.used == 8 {
                self.push_byte(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
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
    current: u8,
    remaining: u8,
    marker: Option<u8>,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            current: 0,
            remaining: 0,
            marker: None,
        }
    }

    pub(crate) fn read_bits(&mut self, count: u8) -> Result<u32, String> {
        if count == 0 || count > 32 {
            return Err(format!("invalid bit width {count}"));
        }
        let mut value = 0u32;
        for _ in 0..count {
            if self.remaining == 0 {
                self.fill_byte()?;
            }
            value = (value << 1) | u32::from((self.current >> (self.remaining - 1)) & 1);
            self.remaining -= 1;
        }
        Ok(value)
    }

    pub(crate) fn take_marker(&mut self) -> Result<Option<u8>, String> {
        if self.remaining != 0 {
            return Ok(self.marker.take());
        }
        if let Some(marker) = self.marker.take() {
            return Ok(Some(marker));
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
        self.marker = Some(code);
        Ok(self.marker.take())
    }

    pub(crate) fn align_to_byte(&mut self) {
        self.remaining = 0;
    }

    fn fill_byte(&mut self) -> Result<(), String> {
        let byte = *self
            .data
            .get(self.position)
            .ok_or_else(|| "truncated entropy data".to_owned())?;
        self.position += 1;
        if byte != 0xff {
            self.current = byte;
            self.remaining = 8;
            return Ok(());
        }
        let code = *self
            .data
            .get(self.position)
            .ok_or_else(|| "truncated 0xff byte in entropy data".to_owned())?;
        self.position += 1;
        match code {
            0x00 => {
                self.current = 0xff;
                self.remaining = 8;
                Ok(())
            }
            marker => {
                self.marker = Some(marker);
                Err(format!(
                    "JPEG marker 0xff{marker:02x} encountered in entropy data"
                ))
            }
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
}
