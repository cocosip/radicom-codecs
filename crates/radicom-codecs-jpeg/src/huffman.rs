use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::bit::{BitReader, BitWriter};

#[derive(Debug, Clone, Copy, Default)]
struct Code {
    value: u16,
    length: u8,
}

#[derive(Debug, Clone, Copy, Default)]
struct CodeRange {
    first_code: u32,
    first_symbol: usize,
    count: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct HuffmanTable {
    codes: [Code; 256],
    prefixes: [u16; 256],
    ranges: [CodeRange; 16],
    symbols: Vec<u8>,
}

impl HuffmanTable {
    pub(crate) fn codeword(&self, symbol: u8) -> Option<(u16, u8)> {
        let code = self.codes[usize::from(symbol)];
        (code.length != 0).then_some((code.value, code.length))
    }

    /// Returns a short canonical code as its length and symbol, packed into a u16.
    #[inline]
    pub(crate) fn short_code(&self, prefix: u8) -> u16 {
        self.prefixes[usize::from(prefix)]
    }

    pub(crate) fn short_amplitudes<const BITS: u8, const LEN: usize>(&self) -> [u32; LEN] {
        let mut table = [0; LEN];
        for (category, code) in self.codes[..=16].iter().enumerate() {
            let amplitude_bits = if category == 16 { 0 } else { category as u8 };
            let width = code.length + amplitude_bits;
            if code.length == 0 || width > BITS {
                continue;
            }
            let unused = BITS - width;
            let start = usize::from(code.value) << (amplitude_bits + unused);
            for raw in 0..(1 << amplitude_bits) {
                let difference = match category {
                    0 => 0,
                    16 => i16::MIN,
                    _ if raw < (1 << (amplitude_bits - 1)) => {
                        (raw as i32 + 1 - (1 << amplitude_bits)) as i16
                    }
                    _ => raw as i16,
                };
                let packed =
                    u32::from(difference as u16) << 16 | (category as u32) << 8 | u32::from(width);
                let offset = start + (raw << unused);
                table[offset..offset + (1 << unused)].fill(packed);
            }
        }
        table
    }

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
        let mut codes = [Code::default(); 256];
        let mut prefixes = [0; 256];
        let mut ranges = [CodeRange::default(); 16];
        let mut code = 0u32;
        let mut value_index = 0usize;
        for (length_index, &count) in lengths.iter().enumerate() {
            let length = u8::try_from(length_index + 1).unwrap();
            code <<= 1;
            ranges[length_index] = CodeRange {
                first_code: code,
                first_symbol: value_index,
                count: count.into(),
            };
            let limit = 1u32 << length;
            for _ in 0..count {
                if code >= limit {
                    return Err("Huffman code lengths are oversubscribed".to_owned());
                }
                let symbol = values[value_index];
                let entry = &mut codes[usize::from(symbol)];
                if entry.length != 0 {
                    return Err(format!("Huffman symbol 0x{symbol:02x} is duplicated"));
                }
                *entry = Code {
                    value: u16::try_from(code).unwrap(),
                    length,
                };
                if length <= 8 {
                    let shift = 8 - length;
                    let start = (code << shift) as usize;
                    let end = start + (1usize << shift);
                    prefixes[start..end].fill(u16::from(length) << 8 | u16::from(symbol));
                }
                value_index += 1;
                code += 1;
            }
        }
        Ok(Self {
            codes,
            prefixes,
            ranges,
            symbols: values.to_vec(),
        })
    }

    /// Builds the per-image optimal DC table from symbol frequencies, the way
    /// libjpeg's `jpeg_gen_optimal_table` does (T.81 Annex K.2): merge the two
    /// least frequent trees repeatedly, count the resulting code lengths, then
    /// fold lengths above 16 bits back into the shorter lengths.
    pub(crate) fn optimal(frequencies: &[u32; 256]) -> (Vec<u8>, Vec<u8>) {
        // libjpeg reserves symbol 256 as an always-present pseudo-symbol so the
        // real symbols never receive the all-ones code, and it chains codes with
        // `others`; both are local to the construction.
        // Equal frequencies pick the greater symbol index, as libjpeg does.
        let mut trees: BinaryHeap<_> = frequencies
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count != 0)
            .map(|(symbol, &count)| (Reverse(u64::from(count)), symbol))
            .chain([(Reverse(1), 256)])
            .collect();
        let mut codesize = [0u32; 257];
        let mut others = [None; 257];

        while let Some((Reverse(first_count), first)) = trees.pop() {
            let Some((Reverse(second_count), second)) = trees.pop() else {
                break;
            };
            trees.push((Reverse(first_count + second_count), first));

            let mut node = first;
            codesize[node] += 1;
            while let Some(next) = others[node] {
                node = next;
                codesize[node] += 1;
            }
            others[node] = Some(second);

            let mut node = second;
            codesize[node] += 1;
            while let Some(next) = others[node] {
                node = next;
                codesize[node] += 1;
            }
        }

        // `bits[length]` counts symbols of that length; index 0 is unused. The
        // array covers every reachable depth so pathological frequency sets
        // cannot index past the folding range. Signed counts mirror libjpeg's
        // `int bits[]`: pairing an odd count can leave a negative entry at a
        // length that is being abandoned.
        let mut bits = [0i32; 257];
        let mut max_length = 0;
        for length in codesize.map(|length| length as usize) {
            if length != 0 {
                bits[length] += 1;
                max_length = max_length.max(length);
            }
        }

        // T.81 limits codes to 16 bits: pair up the longest codes, move one to
        // the next shorter length and turn the shorter prefix into two codes one
        // bit longer.
        for length in (17..=max_length).rev() {
            while bits[length] > 0 {
                let mut shorter = length - 2;
                while bits[shorter] <= 0 {
                    shorter -= 1;
                }
                bits[length] -= 2;
                bits[length - 1] += 1;
                bits[shorter + 1] += 2;
                bits[shorter] -= 1;
            }
        }

        // Drop the pseudo-symbol from the longest remaining length. A table that
        // never saw a real symbol (an unused component slot) still needs a
        // structurally valid one-bit code, which is also what libjpeg emits.
        let mut longest = 16;
        while longest > 0 && bits[longest] <= 0 {
            longest -= 1;
        }
        if longest == 0 {
            let mut counts = vec![0u8; 16];
            counts[0] = 1;
            return (counts, vec![0]);
        }
        bits[longest] -= 1;

        let counts: Vec<u8> = bits[1..=16]
            .iter()
            .map(|&count| count.clamp(0, 255) as u8)
            .collect();
        let mut values = Vec::new();
        for length in 1..=max_length as u32 {
            for (symbol, &size) in codesize.iter().enumerate().take(256) {
                if size == length {
                    values.push(symbol as u8);
                }
            }
        }
        (counts, values)
    }

    pub(crate) fn encode(&self, symbol: u8, writer: &mut BitWriter) -> Result<(), String> {
        self.encode_with_suffix(symbol, 0, 0, writer)
    }

    #[inline]
    pub(crate) fn encode_with_suffix(
        &self,
        symbol: u8,
        suffix: u32,
        suffix_bits: u8,
        writer: &mut BitWriter,
    ) -> Result<(), String> {
        let code = self.codes[usize::from(symbol)];
        if code.length == 0 {
            return Err(format!("Huffman symbol 0x{symbol:02x} is not defined"));
        }
        let value = (u32::from(code.value) << suffix_bits) | suffix;
        writer.write_bits(value, code.length + suffix_bits)
    }

    pub(crate) fn decode(&self, reader: &mut BitReader<'_>) -> Result<u8, String> {
        let (mut value, first_range) = if let Some(prefix) = reader.peek_byte() {
            let entry = self.prefixes[usize::from(prefix)];
            if entry != 0 {
                reader.consume_prefix((entry >> 8) as u8);
                return Ok(entry as u8);
            }
            (reader.read_bits(8)?, 8)
        } else {
            (0, 0)
        };
        for range in &self.ranges[first_range..] {
            value = (value << 1) | reader.read_bits(1)?;
            let offset = value.wrapping_sub(range.first_code);
            if offset < range.count {
                return Ok(self.symbols[range.first_symbol + offset as usize]);
            }
        }
        Err("JPEG Huffman code is not defined".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimal_table_is_still_valid_for_an_unused_table() {
        // A component that no block referenced (or a flat image with no AC
        // symbols) leaves every count at zero; libjpeg emits a one-bit code.
        let (counts, values) = HuffmanTable::optimal(&[0u32; 256]);
        assert_eq!(counts.len(), 16);
        assert_eq!(counts[0], 1);
        assert_eq!(values, vec![0]);
        HuffmanTable::new(&counts, &values).unwrap();
    }

    #[test]
    fn optimal_table_follows_the_symbol_frequencies() {
        let mut frequencies = [0u32; 256];
        frequencies[0] = 1000;
        frequencies[1] = 10;
        frequencies[2] = 1;
        let (counts, values) = HuffmanTable::optimal(&frequencies);
        // The dominant symbol takes the single 1-bit code; the other two split
        // the 2-bit codes, and the pseudo-symbol is dropped from the longest.
        assert_eq!(counts[0], 1);
        assert_eq!(values, vec![0, 1, 2]);
        let table = HuffmanTable::new(&counts, &values).unwrap();
        let mut writer = BitWriter::default();
        for symbol in values {
            table.encode(symbol, &mut writer).unwrap();
        }
        let encoded = writer.finish().unwrap();
        let mut reader = BitReader::new(&encoded);
        assert_eq!(reader.read_bits(1).unwrap(), 0);
        assert_eq!(table.decode(&mut reader).unwrap(), 1);
        assert_eq!(table.decode(&mut reader).unwrap(), 2);
    }

    #[test]
    fn optimal_table_caps_code_lengths_at_sixteen_bits() {
        // Exponential frequencies force a depth far beyond the 16-bit limit, so
        // the T.81 length-folding path has to run.
        let mut frequencies = [0u32; 256];
        for (symbol, slot) in frequencies.iter_mut().enumerate().take(40) {
            *slot = 1 << symbol.min(31);
        }
        let (counts, values) = HuffmanTable::optimal(&frequencies);
        assert!(counts.len() == 16);
        assert_eq!(
            counts
                .iter()
                .map(|&count| usize::from(count))
                .sum::<usize>(),
            values.len()
        );
        HuffmanTable::new(&counts, &values).expect("a valid canonical table");
    }

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
