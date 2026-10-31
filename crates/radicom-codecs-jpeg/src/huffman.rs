use super::bit::{BitReader, BitWriter};

#[derive(Debug, Clone)]
struct Code {
    symbol: u8,
    value: u16,
    length: u8,
}

#[derive(Debug, Clone)]
pub(crate) struct HuffmanTable {
    codes: Vec<Code>,
}

impl HuffmanTable {
    pub(crate) fn new(lengths: &[u8], values: &[u8]) -> Result<Self, String> {
        if lengths.len() > 16 {
            return Err("JPEG Huffman tables support at most 16 code lengths".to_owned());
        }
        let total = lengths.iter().try_fold(0usize, |sum, count| {
            sum.checked_add(usize::from(*count))
                .ok_or_else(|| "Huffman symbol count overflows usize".to_owned())
        })?;
        if total != values.len() {
            return Err(format!(
                "Huffman code counts describe {total} symbols, got {}",
                values.len()
            ));
        }
        let mut codes = Vec::with_capacity(total);
        let mut seen = [false; 256];
        let mut code = 0u32;
        let mut value_index = 0usize;
        for (length_index, &count) in lengths.iter().enumerate() {
            let length = u8::try_from(length_index + 1).unwrap();
            code <<= 1;
            let limit = 1u32 << length;
            for _ in 0..count {
                if code >= limit {
                    return Err("Huffman code lengths are oversubscribed".to_owned());
                }
                let symbol = values[value_index];
                if seen[usize::from(symbol)] {
                    return Err(format!("Huffman symbol 0x{symbol:02x} is duplicated"));
                }
                seen[usize::from(symbol)] = true;
                codes.push(Code {
                    symbol,
                    value: u16::try_from(code).unwrap(),
                    length,
                });
                value_index += 1;
                code += 1;
            }
        }
        Ok(Self { codes })
    }

    pub(crate) fn encode(&self, symbol: u8, writer: &mut BitWriter) -> Result<(), String> {
        let code = self
            .codes
            .iter()
            .find(|code| code.symbol == symbol)
            .ok_or_else(|| format!("Huffman symbol 0x{symbol:02x} is not defined"))?;
        writer.write_bits(u32::from(code.value), code.length)
    }

    pub(crate) fn decode(&self, reader: &mut BitReader<'_>) -> Result<u8, String> {
        let mut value = 0u16;
        for length in 1..=16 {
            value = (value << 1) | reader.read_bits(1)? as u16;
            if let Some(code) = self
                .codes
                .iter()
                .find(|code| code.length == length && code.value == value)
            {
                return Ok(code.symbol);
            }
        }
        Err("JPEG Huffman code is not defined".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_canonical_codes_and_round_trips_symbols() {
        let lengths = [0, 2, 2];
        let values = [0xa, 0xb, 0xc, 0xd];
        let table = HuffmanTable::new(&lengths, &values).unwrap();
        let mut writer = BitWriter::default();
        for symbol in values {
            table.encode(symbol, &mut writer).unwrap();
        }
        let encoded = writer.finish().unwrap();
        let mut reader = BitReader::new(&encoded);
        let decoded: Vec<_> = (0..values.len())
            .map(|_| table.decode(&mut reader).unwrap())
            .collect();
        assert_eq!(decoded, values);
    }

    #[test]
    fn rejects_oversubscribed_and_missing_symbol_tables() {
        assert!(HuffmanTable::new(&[3], &[1, 2, 3]).is_err());
        assert!(HuffmanTable::new(&[2], &[1, 1]).is_err());
        let table = HuffmanTable::new(&[1], &[7]).unwrap();
        let mut writer = BitWriter::default();
        assert!(table.encode(8, &mut writer).is_err());
    }
}
