use super::header::RleHeader;
use super::layout::RleLayout;
use super::segment::{decode_segment, encode_segment};

pub(crate) fn encode_frame(layout: &RleLayout, raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.len() < layout.frame_length {
        return Err(format!(
            "raw frame length {} is shorter than expected {}",
            raw.len(),
            layout.frame_length
        ));
    }
    let mut frame = vec![0u8; 64];
    let mut offsets = Vec::with_capacity(layout.segment_count);
    for segment_index in 0..layout.segment_count {
        let (sample, plane) = layout.segment_indices(segment_index)?;
        let mut samples = Vec::with_capacity(layout.pixel_count);
        for pixel in 0..layout.pixel_count {
            samples.push(raw[layout.raw_offset(pixel, sample, plane)?]);
        }
        let encoded = encode_segment(&samples)?;
        offsets.push(frame.len());
        frame.extend_from_slice(&encoded);
        if encoded.len() % 2 == 1 {
            frame.push(0);
        }
    }
    let header = RleHeader::write(&offsets)?;
    frame[..64].copy_from_slice(&header);
    Ok(frame)
}

pub(crate) fn decode_frame(layout: &RleLayout, rle: &[u8]) -> Result<Vec<u8>, String> {
    let header = RleHeader::parse(rle)?;
    if header.offsets().len() != layout.segment_count {
        return Err(format!(
            "segment count {} does not match expected {}",
            header.offsets().len(),
            layout.segment_count
        ));
    }
    let mut raw = vec![0u8; layout.frame_length];
    for (segment_index, &start) in header.offsets().iter().enumerate() {
        let end = header
            .offsets()
            .get(segment_index + 1)
            .copied()
            .unwrap_or(rle.len());
        if end <= start {
            return Err(format!("segment {segment_index} is empty"));
        }
        let encoded = rle
            .get(start..end)
            .ok_or_else(|| format!("segment {segment_index} is outside the frame"))?;
        let decoded = match decode_segment(encoded, layout.pixel_count) {
            Ok(decoded) => decoded,
            Err(error) if encoded.last() == Some(&0) => {
                let unpadded = encoded
                    .get(..encoded.len() - 1)
                    .ok_or_else(|| "invalid empty padded segment".to_owned())?;
                decode_segment(unpadded, layout.pixel_count).map_err(|_| error)?
            }
            Err(error) => return Err(error),
        };
        let (sample, plane) = layout.segment_indices(segment_index)?;
        for (pixel, &value) in decoded.iter().enumerate() {
            let offset = layout.raw_offset(pixel, sample, plane)?;
            *raw.get_mut(offset)
                .ok_or_else(|| "decoded pixel offset is outside the output frame".to_owned())? =
                value;
        }
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_header_segments_and_decodes_exactly() {
        let layout = RleLayout::new_for_test(1, 3, 8, 1, 0).unwrap();
        let encoded = encode_frame(&layout, &[1, 2, 3]).unwrap();
        assert_eq!(u32::from_le_bytes(encoded[..4].try_into().unwrap()), 1);
        assert_eq!(decode_frame(&layout, &encoded).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn rejects_truncated_and_wrong_segment_frames() {
        let layout = RleLayout::new_for_test(1, 3, 8, 1, 0).unwrap();
        assert!(decode_frame(&layout, &[0; 63]).is_err());
        let mut encoded = encode_frame(&layout, &[1, 2, 3]).unwrap();
        encoded[0] = 2;
        assert!(decode_frame(&layout, &encoded).is_err());
    }
}
