use super::header::{RleHeader, HEADER_LENGTH};
use super::layout::RleLayout;
use super::segment::{decode_segment, encode_segment};
use crate::MAX_DECODED_BYTES;

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
        let samples = if layout.bit_packed {
            unpack_bits(layout, raw)?
        } else {
            let (sample, plane) = layout.segment_indices(segment_index)?;
            let mut samples = Vec::with_capacity(layout.pixel_count);
            for pixel in 0..layout.pixel_count {
                samples.push(raw[layout.raw_offset(pixel, sample, plane)?]);
            }
            samples
        };
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
    ensure_decodable_size(layout, rle.len())?;
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
        if layout.bit_packed {
            pack_bits(layout, &mut raw, &decoded)?;
            continue;
        }
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

/// Rejects a declared decoded size that the compressed frame cannot justify,
/// before any allocation. A PackBits input byte contributes at most 128 output
/// bytes (one literal control plus up to 128 payload bytes, or two repeat bytes
/// for 128 output), so the bound is conservative for every legal segment.
fn ensure_decodable_size(layout: &RleLayout, rle_len: usize) -> Result<(), String> {
    if layout.frame_length > MAX_DECODED_BYTES || layout.pixel_count > MAX_DECODED_BYTES {
        return Err(format!(
            "declared decoded size (frame {} bytes, segment {} bytes) exceeds the {MAX_DECODED_BYTES}-byte limit",
            layout.frame_length, layout.pixel_count
        ));
    }
    let payload = rle_len.saturating_sub(HEADER_LENGTH);
    let expansion_limit = payload
        .checked_mul(128)
        .ok_or_else(|| "RLE frame size overflow".to_owned())?;
    if layout.frame_length > expansion_limit || layout.pixel_count > expansion_limit {
        return Err(format!(
            "declared decoded size (frame {} bytes, segment {} bytes) exceeds the {expansion_limit}-byte expansion bound for a {payload}-byte frame",
            layout.frame_length, layout.pixel_count
        ));
    }
    Ok(())
}

/// Expands the native LSB-first bit-packed frame to one byte per pixel.
fn unpack_bits(layout: &RleLayout, raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.len() < layout.frame_length {
        return Err(format!(
            "raw frame length {} is shorter than expected {}",
            raw.len(),
            layout.frame_length
        ));
    }
    let mut samples = Vec::with_capacity(layout.pixel_count);
    for pixel in 0..layout.pixel_count {
        let byte = *raw
            .get(pixel / 8)
            .ok_or_else(|| "bit-packed pixel is outside the raw frame".to_owned())?;
        samples.push((byte >> (pixel % 8)) & 1);
    }
    Ok(samples)
}

/// Packs one byte per pixel back into the native LSB-first frame; only 0 and 1
/// are valid wire values for a 1-bit sample.
fn pack_bits(layout: &RleLayout, raw: &mut [u8], samples: &[u8]) -> Result<(), String> {
    if samples.len() != layout.pixel_count {
        return Err(format!(
            "decoded segment length {} does not match expected {}",
            samples.len(),
            layout.pixel_count
        ));
    }
    raw.fill(0);
    for (pixel, &value) in samples.iter().enumerate() {
        match value {
            0 => {}
            1 => raw[pixel / 8] |= 1 << (pixel % 8),
            other => {
                return Err(format!(
                    "1-bit sample value {other} at pixel {pixel} is not 0 or 1"
                ))
            }
        }
    }
    Ok(())
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

    #[test]
    fn encodes_one_bit_frame_as_a_single_byte_per_pixel_segment() {
        let layout = RleLayout::new_for_test(2, 7, 1, 1, 0).unwrap();
        let raw = [0b1010_1010, 0b0011_0011];
        let encoded = encode_frame(&layout, &raw).unwrap();
        let header = RleHeader::parse(&encoded).unwrap();
        assert_eq!(header.offsets().len(), 1);
        let start = header.offsets()[0];
        let segment = &encoded[start..];
        let decoded = match decode_segment(segment, layout.pixel_count) {
            Ok(decoded) => decoded,
            Err(_) => decode_segment(&segment[..segment.len() - 1], layout.pixel_count).unwrap(),
        };
        assert_eq!(decoded.len(), 14);
        assert!(decoded.iter().all(|&value| value <= 1));
        assert_eq!(decode_frame(&layout, &encoded).unwrap(), raw);
    }

    #[test]
    fn rejects_one_bit_segment_values_above_one() {
        let layout = RleLayout::new_for_test(1, 3, 1, 1, 0).unwrap();
        let mut frame = vec![0u8; 64];
        frame[..64].copy_from_slice(&RleHeader::write(&[64]).unwrap());
        frame.extend_from_slice(&encode_segment(&[0, 1, 2]).unwrap());
        let error = decode_frame(&layout, &frame).unwrap_err();
        assert!(error.contains("is not 0 or 1"), "{error}");
    }

    #[test]
    fn rejects_a_frame_beyond_the_packbits_expansion_bound() {
        let layout = RleLayout::new_for_test(1000, 1000, 8, 1, 0).unwrap();
        let mut frame = RleHeader::write(&[64]).unwrap().to_vec();
        frame.extend_from_slice(&[0x00, 0x2a]);
        let error = decode_frame(&layout, &frame).unwrap_err();
        assert!(error.contains("expansion"), "{error}");
    }

    #[test]
    fn rejects_a_frame_beyond_the_absolute_output_limit() {
        let layout = RleLayout::new_for_test(40_000, 40_000, 8, 1, 0).unwrap();
        let error = ensure_decodable_size(&layout, 130).unwrap_err();
        assert!(error.contains("limit"), "{error}");
    }

    #[test]
    fn rejects_a_segment_count_mismatch() {
        let layout = RleLayout::new_for_test(1, 1, 8, 1, 0).unwrap();
        let mut frame = RleHeader::write(&[64, 66]).unwrap().to_vec();
        frame.extend_from_slice(&[0x00, 0x2a, 0x00]);
        let error = decode_frame(&layout, &frame).unwrap_err();
        assert!(
            error.contains("segment count 2 does not match expected 1"),
            "{error}"
        );

        let layout = RleLayout::new_for_test(2, 1, 16, 1, 0).unwrap();
        let mut frame = RleHeader::write(&[64]).unwrap().to_vec();
        frame.extend_from_slice(&[0x00, 1, 0x00, 2]);
        let error = decode_frame(&layout, &frame).unwrap_err();
        assert!(
            error.contains("segment count 1 does not match expected 2"),
            "{error}"
        );
    }
}
