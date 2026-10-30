const HEADER_LENGTH: usize = 64;
const MAX_SEGMENTS: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RleHeader {
    offsets: Vec<usize>,
}

impl RleHeader {
    pub(crate) fn parse(frame: &[u8]) -> Result<Self, String> {
        if frame.len() < HEADER_LENGTH {
            return Err("frame is shorter than the 64-byte RLE header".to_owned());
        }
        let count = usize::try_from(read_u32(frame, 0)?)
            .map_err(|_| "segment count is too large".to_owned())?;
        if !(1..=MAX_SEGMENTS).contains(&count) {
            return Err(format!("segment count {count} is outside 1..=15"));
        }
        let mut offsets = Vec::with_capacity(count);
        let mut previous = HEADER_LENGTH;
        for index in 0..count {
            let offset = usize::try_from(read_u32(frame, 4 + index * 4)?)
                .map_err(|_| "segment offset is too large".to_owned())?;
            if offset < HEADER_LENGTH || offset >= frame.len() {
                return Err(format!(
                    "segment {index} offset {offset} is outside the frame"
                ));
            }
            if index > 0 && offset <= previous {
                return Err("segment offsets must be strictly increasing".to_owned());
            }
            offsets.push(offset);
            previous = offset;
        }
        Ok(Self { offsets })
    }

    pub(crate) fn write(offsets: &[usize]) -> Result<[u8; HEADER_LENGTH], String> {
        if !(1..=MAX_SEGMENTS).contains(&offsets.len()) {
            return Err(format!("segment count {} is outside 1..=15", offsets.len()));
        }
        let mut bytes = [0u8; HEADER_LENGTH];
        write_u32(
            &mut bytes,
            0,
            u32::try_from(offsets.len()).map_err(|_| "segment count is too large".to_owned())?,
        );
        let mut previous = HEADER_LENGTH;
        for (index, &offset) in offsets.iter().enumerate() {
            if offset < HEADER_LENGTH || (index > 0 && offset <= previous) {
                return Err(
                    "segment offsets must be at least 64 and strictly increasing".to_owned(),
                );
            }
            write_u32(
                &mut bytes,
                4 + index * 4,
                u32::try_from(offset).map_err(|_| "segment offset is too large".to_owned())?,
            );
            previous = offset;
        }
        Ok(bytes)
    }

    pub(crate) fn offsets(&self) -> &[usize] {
        &self.offsets
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "header offset overflow".to_owned())?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| "truncated RLE header".to_owned())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_little_endian_header_and_zeroes_reserved_offsets() {
        let bytes = RleHeader::write(&[64, 70]).unwrap();
        assert_eq!(&bytes[..4], &[2, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &[64, 0, 0, 0]);
        assert_eq!(&bytes[8..12], &[70, 0, 0, 0]);
        assert!(bytes[12..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn parses_and_rejects_header_boundaries() {
        let mut frame = vec![0u8; 70];
        frame[..64].copy_from_slice(&RleHeader::write(&[64]).unwrap());
        assert_eq!(RleHeader::parse(&frame).unwrap().offsets(), &[64]);
        assert!(RleHeader::parse(&[0; 63]).is_err());
        let mut invalid = vec![0u8; 65];
        invalid[..64].copy_from_slice(&RleHeader::write(&[64]).unwrap());
        invalid[0] = 0;
        assert!(RleHeader::parse(&invalid).is_err());
    }
}
