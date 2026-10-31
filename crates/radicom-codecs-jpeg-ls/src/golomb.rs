use crate::bit::{BitReader, BitWriter};
pub(crate) fn encode_mapped(
    w: &mut BitWriter,
    k: u8,
    m: u32,
    limit: u32,
    qbpp: u8,
) -> Result<(), String> {
    let high = m >> k;
    let threshold = limit.saturating_sub(u32::from(qbpp) + 1);
    if high < threshold {
        for _ in 0..high {
            w.write_bit(false)
        }
        w.write_bit(true);
        if k > 0 {
            w.write_bits(m & ((1u32 << k) - 1), k)?
        }
    } else {
        for _ in 0..threshold {
            w.write_bit(false)
        }
        w.write_bit(true);
        let v = m.checked_sub(1).ok_or("invalid mapped error")?;
        w.write_bits(v & ((1u32 << qbpp) - 1), qbpp)?
    }
    Ok(())
}
pub(crate) fn decode_mapped(
    r: &mut BitReader<'_>,
    k: u8,
    limit: u32,
    qbpp: u8,
) -> Result<u32, String> {
    let threshold = limit.saturating_sub(u32::from(qbpp) + 1);
    let mut high = 0;
    while !r.read_bit()? {
        high += 1;
        if high > threshold {
            return Err("Golomb unary exceeds limit".into());
        }
    }
    if high == threshold {
        Ok(r.read_bits(qbpp)?.saturating_add(1))
    } else {
        let rem = if k > 0 { r.read_bits(k)? } else { 0 };
        Ok((high << k) | rem)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn golomb_roundtrip() {
        for k in 0..6 {
            for m in 0..100 {
                let mut w = BitWriter::new();
                encode_mapped(&mut w, k, m, 64, 8).unwrap();
                let b = w.finish();
                let mut r = BitReader::new(&b);
                assert_eq!(decode_mapped(&mut r, k, 64, 8).unwrap(), m)
            }
        }
    }

    #[test]
    fn rejects_unary_prefix_longer_than_the_escape_threshold() {
        let mut writer = BitWriter::new();
        // LIMIT=32, QBPP=8 permits exactly 23 leading zero bits.
        writer.write_bits(0, 24).unwrap();
        writer.write_bit(true);
        writer.write_bits(0, 8).unwrap();
        let bytes = writer.finish();
        assert!(decode_mapped(&mut BitReader::new(&bytes), 0, 32, 8).is_err());
    }
}
