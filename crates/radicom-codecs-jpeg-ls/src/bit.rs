#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    pending: u64,
    used: u8,
    stuff_pending: bool,
}
impl BitWriter {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            ..Self::default()
        }
    }

    pub(crate) fn write_bit(&mut self, bit: bool) {
        self.append(u32::from(bit), 1);
    }
    #[inline]
    pub(crate) fn write_bits(&mut self, value: u32, count: u8) -> Result<(), String> {
        if count == 0 || count > 32 || (count < 32 && value >= (1u32 << count)) {
            return Err("invalid bit width/value".into());
        }
        self.append(value, count);
        Ok(())
    }

    #[inline]
    fn append(&mut self, value: u32, count: u8) {
        self.pending = (self.pending << count) | u64::from(value);
        self.used += count;
        if self.used >= 32 {
            self.flush_words();
        }
    }

    fn flush_words(&mut self) {
        while self.used >= 32 {
            if !self.stuff_pending {
                let word = (self.pending >> (self.used - 32)) as u32;
                let inverted = !word;
                if inverted.wrapping_sub(0x0101_0101) & !inverted & 0x8080_8080 == 0 {
                    self.bytes.extend_from_slice(&word.to_be_bytes());
                    self.used -= 32;
                    continue;
                }
            }
            self.flush_byte();
        }
    }

    #[inline]
    pub(crate) fn write_unary_with_suffix(
        &mut self,
        mut zeroes: u32,
        suffix: u32,
        suffix_bits: u8,
    ) {
        debug_assert!(suffix_bits <= 16 && suffix < (1u32 << suffix_bits));
        let payload_bits = u32::from(suffix_bits) + 1;
        while zeroes + payload_bits > 32 {
            let leading = zeroes.min(32);
            self.append(0, leading as u8);
            zeroes -= leading;
        }
        self.append((1 << suffix_bits) | suffix, (zeroes + payload_bits) as u8);
    }
    pub(crate) fn finish(mut self) -> Vec<u8> {
        while self.used >= self.next_byte_width() {
            self.flush_byte();
        }
        if self.stuff_pending || self.used != 0 {
            let tail = (self.pending as u8) & ((1 << self.used) - 1);
            self.bytes
                .push(tail << (self.next_byte_width() - self.used));
        }
        self.bytes
    }

    fn next_byte_width(&self) -> u8 {
        8 - u8::from(self.stuff_pending)
    }

    fn flush_byte(&mut self) {
        let width = self.next_byte_width();
        self.used -= width;
        let byte = ((self.pending >> self.used) as u8) & (u8::MAX >> (8 - width));
        self.bytes.push(byte);
        self.stuff_pending = byte == 0xff;
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum EntropyError {
    InvalidWidth,
    Truncated,
    InvalidStuffedBit,
    UnaryExceedsLimit,
    PredictionErrorExceedsRange,
}

impl From<EntropyError> for String {
    #[cold]
    fn from(error: EntropyError) -> Self {
        match error {
            EntropyError::InvalidWidth => "invalid bit width",
            EntropyError::Truncated => "truncated scan",
            EntropyError::InvalidStuffedBit => "invalid JPEG-LS stuffed bit",
            EntropyError::UnaryExceedsLimit => "Golomb unary exceeds limit",
            EntropyError::PredictionErrorExceedsRange => "prediction error exceeds RANGE",
        }
        .into()
    }
}

// The only unit Rice codes are 1 (mapped 0) and 01 (mapped 1).
// Each entry gives their three-symbol length and the three mapped values.
const UNIT_RICE_TRIPLETS: [u8; 64] = {
    let mut table = [0; 64];
    let mut head = 0;
    while head < 64 {
        let mut value = head;
        let mut length = 0;
        let mut mapped = 0;
        let mut channel = 0;
        while channel < 3 {
            if value & 32 != 0 {
                length += 1;
                value <<= 1;
            } else if value & 16 != 0 {
                length += 2;
                mapped |= 1 << channel;
                value <<= 2;
            } else {
                break;
            }
            channel += 1;
        }
        if channel == 3 {
            table[head] = (length << 3) | mapped;
        }
        head += 1;
    }
    table
};

#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    current: u64,
    left: u8,
    previous_ff: bool,
    #[cfg(target_arch = "x86_64")]
    can_unpack_stuffed_word: bool,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            current: 0,
            left: 0,
            previous_ff: false,
            #[cfg(target_arch = "x86_64")]
            can_unpack_stuffed_word: std::arch::is_x86_feature_detected!("bmi2")
                && std::arch::is_x86_feature_detected!("popcnt"),
        }
    }
    #[cfg(test)]
    pub(crate) fn read_bit(&mut self) -> Result<bool, EntropyError> {
        if self.left == 0 {
            self.fill()?
        }
        let bit = (self.current & (1 << (self.left - 1))) != 0;
        self.left -= 1;
        Ok(bit)
    }

    #[inline]
    pub(crate) fn run_prefix(&mut self) -> Result<u8, EntropyError> {
        if self.left == 0 {
            self.fill()?;
        }
        let aligned = self.current << (64 - self.left);
        Ok((!aligned).leading_zeros().min(u32::from(self.left)) as u8)
    }

    #[inline]
    pub(crate) fn skip_buffered(&mut self, count: u8) {
        debug_assert!(count <= self.left);
        self.left -= count;
    }

    #[inline]
    pub(crate) fn buffered_bits(&self) -> u8 {
        self.left
    }
    #[inline]
    pub(crate) fn read_bits(&mut self, mut n: u8) -> Result<u32, EntropyError> {
        if n == 0 || n > 32 {
            return Err(EntropyError::InvalidWidth);
        }
        let mut v = 0u32;
        while n != 0 {
            if self.left == 0 {
                self.fill()?;
            }
            let take = n.min(self.left);
            self.left -= take;
            let mask = u64::from(u32::MAX >> (32 - take));
            v = v.wrapping_shl(u32::from(take)) | ((self.current >> self.left) & mask) as u32;
            n -= take;
        }
        Ok(v)
    }

    #[inline(always)]
    pub(crate) fn take_zero_rice(&mut self) -> bool {
        if self.left != 0 && (self.current >> (self.left - 1)) & 1 != 0 {
            self.left -= 1;
            true
        } else {
            false
        }
    }

    #[inline(always)]
    pub(crate) fn take_nonpositive_unit_rice(&mut self) -> Option<i32> {
        if self.left >= 2 {
            let head = (self.current >> (self.left - 2)) & 3;
            if head != 0 {
                let mapped = i32::from(head == 1);
                self.left -= 1 + mapped as u8;
                return Some(-mapped);
            }
        }
        None
    }

    #[inline(always)]
    pub(crate) fn take_unit_rice_triplet(&mut self) -> Option<u8> {
        if self.left < 6 {
            return None;
        }
        let head = ((self.current >> (self.left - 6)) & 63) as usize;
        let packed = UNIT_RICE_TRIPLETS[head];
        if packed == 0 {
            return None;
        }
        self.left -= packed >> 3;
        Some(packed & 7)
    }

    #[inline]
    pub(crate) fn read_rice(&mut self, k: u8, limit: u32, qbpp: u8) -> Result<u32, EntropyError> {
        if self.left == 0 {
            self.fill()?;
        }
        let pending = self.current << (64 - self.left);
        let zeroes = (pending | 1).leading_zeros();
        let width = zeroes + 1 + u32::from(k);
        if width <= u32::from(self.left) && zeroes < limit.min(63) {
            self.left -= width as u8;
            // The aligned code starts with unary zeroes and a one above the suffix.
            let suffix = ((pending >> (64 - width)) as u32) ^ (1 << k);
            return Ok((zeroes << k) | suffix);
        }
        self.read_long_rice(k, limit, qbpp)
    }

    #[inline(never)]
    fn read_long_rice(&mut self, k: u8, limit: u32, qbpp: u8) -> Result<u32, EntropyError> {
        let mut prefix = 0;
        loop {
            if self.left == 0 {
                self.fill()?;
            }
            let pending = self.current << (64 - self.left);
            let zeroes = (pending.leading_zeros() as u8).min(self.left);
            prefix += u32::from(zeroes);
            if prefix > limit {
                return Err(EntropyError::UnaryExceedsLimit);
            }
            self.left -= zeroes;
            if self.left != 0 {
                self.left -= 1;
                break;
            }
        }
        if prefix == limit {
            return Ok(self.read_bits(qbpp)?.saturating_add(1));
        }
        if k == 0 {
            return Ok(prefix);
        }
        let suffix = if self.left >= k {
            self.left -= k;
            ((self.current >> self.left) as u32) & ((1 << k) - 1)
        } else {
            self.read_bits(k)?
        };
        Ok((prefix << k) | suffix)
    }

    /// Verifies that the segment ended on a byte boundary with zero padding and
    /// no unconsumed bytes. Takes `&mut self` so a restart boundary can finish
    /// one scan segment and continue with a fresh reader.
    pub(crate) fn finish(&mut self) -> Result<(), String> {
        if self.previous_ff && self.left == 0 {
            // A full final 0xff still needs a stuffed zero and zero padding.
            self.fill()?;
        }
        let mut tail = self.pos;
        let mut padding = self.left;
        while padding >= 7 && tail != 0 {
            let stuffed = tail >= 2 && self.data[tail - 2] == 0xff;
            let width = 8 - u8::from(stuffed);
            if padding < width {
                break;
            }
            padding -= width;
            tail -= 1;
        }
        if padding != 0 {
            let padding_mask = (1u64 << padding) - 1;
            if (self.current >> (self.left - padding)) & padding_mask != 0 {
                return Err("non-zero JPEG-LS padding bits".into());
            }
        }
        if tail < self.data.len() {
            // Writers may pad an entropy segment with a single zero byte before
            // the next marker (fo-dicom pads a fragment to an even length) and
            // CharLS accepts one trailing 0x00 as well. Anything longer, or a
            // non-zero byte, is trailing entropy/data.
            let trailing = &self.data[tail..];
            if trailing.len() > 1 || trailing[0] != 0 {
                return Err("trailing JPEG-LS entropy data".into());
            }
        }
        Ok(())
    }
    fn fill(&mut self) -> Result<(), EntropyError> {
        if !self.previous_ff {
            if let Some(bytes) = self.data[self.pos..].first_chunk::<8>() {
                let word = u64::from_be_bytes(*bytes);
                let inverted = !word;
                if inverted.wrapping_sub(0x0101_0101_0101_0101) & !inverted & 0x8080_8080_8080_8080
                    == 0
                {
                    self.pos += 8;
                    self.current = word;
                    self.left = 64;
                    return Ok(());
                }
            }
            if let Some(bytes) = self.data[self.pos..].first_chunk::<4>() {
                let word = u32::from_be_bytes(*bytes);
                let inverted = !word;
                if inverted.wrapping_sub(0x0101_0101) & !inverted & 0x8080_8080 == 0 {
                    self.pos += 4;
                    self.current = u64::from(word);
                    self.left = 32;
                    return Ok(());
                }
            }
        }
        #[cfg(target_arch = "x86_64")]
        if self.can_unpack_stuffed_word {
            if let Some(bytes) = self.data[self.pos..].first_chunk::<8>() {
                // SAFETY: Availability was checked when this reader was created.
                if let Some((word, width)) =
                    unsafe { unpack_stuffed_word(u64::from_be_bytes(*bytes), self.previous_ff) }
                {
                    self.pos += 8;
                    self.current = word;
                    self.left = width;
                    self.previous_ff = bytes[7] == 0xff;
                    return Ok(());
                }
            }
        }
        let b = *self.data.get(self.pos).ok_or(EntropyError::Truncated)?;
        self.pos += 1;
        if self.previous_ff {
            if b & 0x80 != 0 {
                return Err(EntropyError::InvalidStuffedBit);
            }
            self.current = u64::from(b);
            self.left = 7;
            self.previous_ff = false
        } else {
            self.current = u64::from(b);
            self.left = 8;
        }
        if b == 0xff {
            self.previous_ff = true
        }
        Ok(())
    }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "bmi2,popcnt")]
unsafe fn unpack_stuffed_word(word: u64, previous_ff: bool) -> Option<(u64, u8)> {
    use std::arch::x86_64::_pext_u64;

    const LOW_BITS: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    const HIGH_BITS: u64 = !LOW_BITS;
    let inverted = !word;
    // The addition sets each byte's high bit when its low seven bits are
    // nonzero. Including the original high bit identifies exact 0xff bytes.
    let ff_positions = !(((inverted & LOW_BITS) + LOW_BITS) | inverted) & HIGH_BITS;
    let stuffed = (ff_positions >> 8) | (u64::from(previous_ff) << 63);
    if word & stuffed != 0 {
        // Preserve bytewise error handling for an invalid stuffing bit.
        return None;
    }
    Some((_pext_u64(word, !stuffed), 64 - stuffed.count_ones() as u8))
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
    fn buffered_unit_rice_matches_the_general_reader() {
        for head in 0..=u8::MAX {
            for left in 0..=8 {
                let mut fast = BitReader::new(&[]);
                fast.current = u64::from(head);
                fast.left = left;
                let mut generic = BitReader::new(&[]);
                generic.current = fast.current;
                generic.left = left;
                let original_left = fast.left;
                if let Some(error) = fast.take_nonpositive_unit_rice() {
                    assert_eq!(generic.read_rice(0, 23, 8).unwrap(), (-error) as u32);
                    assert_eq!(fast.left, generic.left);
                } else {
                    assert_eq!(fast.left, original_left);
                    assert!(left < 2 || ((head >> (left - 2)) & 3) == 0);
                }
            }
        }
    }

    #[test]
    fn buffered_unit_rice_triplets_match_the_general_reader() {
        for head in 0..64 {
            let mut fast = BitReader::new(&[]);
            fast.current = head;
            fast.left = 6;
            let mut generic = BitReader::new(&[]);
            generic.current = head;
            generic.left = 6;
            let values: Vec<_> = (0..3)
                .filter_map(|_| generic.read_rice(0, 23, 8).ok())
                .collect();
            let expected = match values.as_slice() {
                [first @ 0..=1, second @ 0..=1, third @ 0..=1] => {
                    Some((first | second << 1 | third << 2) as u8)
                }
                _ => None,
            };
            assert_eq!(fast.take_unit_rice_triplet(), expected);
            assert_eq!(fast.left, if expected.is_some() { generic.left } else { 6 });
        }
    }

    #[test]
    fn unary_across_a_stuffed_byte_ignores_the_stuffed_bit() {
        let mut writer = BitWriter::new();
        writer.write_bits(255, 8).unwrap();
        writer.write_unary_with_suffix(39, 0, 0);
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(8).unwrap(), 255);
        assert_eq!(reader.read_rice(0, 40, 8).unwrap(), 39);
        reader.finish().unwrap();
    }

    #[test]
    fn rejects_non_zero_padding_bits() {
        let mut reader = BitReader::new(&[0b1000_0001]);
        assert!(reader.read_bit().unwrap());
        assert!(reader.finish().is_err());
    }

    #[test]
    fn validates_prefetched_padding_and_trailing_bytes() {
        for (bytes, bits, valid) in [
            ([0x80, 0, 0, 0], 17, true),
            ([0x80, 0, 1, 0], 17, false),
            ([0x80, 0, 0, 1], 17, false),
            ([0x80, 0, 0, 0], 9, false),
            ([0x80, 0, 0, 0], 32, true),
        ] {
            let mut reader = BitReader::new(&bytes);
            assert_eq!(reader.read_bits(bits).unwrap(), 1u32 << (bits - 1));
            assert_eq!(reader.finish().is_ok(), valid);
        }
    }

    #[test]
    fn prefetched_stuffing_preserves_bits_and_padding() {
        let data = [0xff, 0x7f, 0xff, 0x7f, 0xff, 0x7f, 0xa5, 0];
        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read_bits(32).unwrap(), u32::MAX);
        assert_eq!(reader.read_bits(13).unwrap(), 0x1fff);
        assert_eq!(reader.read_bits(8).unwrap(), 0xa5);
        reader.finish().unwrap();

        let mut reader = BitReader::new(&[0x80, 1, 2, 3, 4, 5, 0xff, 0]);
        assert_eq!(reader.read_bits(32).unwrap(), 0x8001_0203);
        assert_eq!(reader.read_bits(24).unwrap(), 0x0004_05ff);
        reader.finish().unwrap();
    }

    #[test]
    fn prefetches_eight_clean_bytes_without_consuming_trailing_data() {
        let mut reader = BitReader::new(&[0x80, 1, 2, 3, 4, 5, 0, 0]);
        assert_eq!(reader.read_bits(32).unwrap(), 0x8001_0203);
        assert_eq!(reader.buffered_bits(), 32);
        assert_eq!(reader.read_bits(24).unwrap(), 0x0004_0500);
        reader.finish().unwrap();
    }

    #[test]
    fn packed_words_preserve_stuffing_across_flushes() {
        let mut writer = BitWriter::new();
        writer.write_bits(0x0123_45ff, 32).unwrap();
        writer.write_bits(0x89ab_cdef, 32).unwrap();
        writer.write_bit(true);
        let bytes = writer.finish();
        assert_eq!(
            bytes,
            [0x01, 0x23, 0x45, 0xff, 0x44, 0xd5, 0xe6, 0xf7, 0xc0]
        );

        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(32).unwrap(), 0x0123_45ff);
        assert_eq!(reader.read_bits(32).unwrap(), 0x89ab_cdef);
        assert!(reader.read_bit().unwrap());
        reader.finish().unwrap();
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

    #[test]
    fn permits_at_most_one_trailing_zero_byte() {
        // A single 0x00 between entropy and the next marker is T.87 padding.
        let mut writer = BitWriter::new();
        writer.write_bits(0b1010_1010, 8).unwrap();
        let mut one = writer.finish();
        one.push(0);
        let mut reader = BitReader::new(&one);
        assert_eq!(reader.read_bits(8).unwrap(), 0b1010_1010);
        reader.finish().unwrap();

        // Two zero bytes are trailing data, not padding.
        let mut two = one.clone();
        two.push(0);
        let mut reader = BitReader::new(&two);
        let _ = reader.read_bits(8).unwrap();
        assert!(reader.finish().is_err());
    }
}
