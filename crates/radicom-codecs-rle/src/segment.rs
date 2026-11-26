use crate::MAX_DECODED_BYTES;

pub(crate) fn encode_segment(source: &[u8], output: &mut Vec<u8>) {
    let mut literal_start = 0;
    let mut position = 0;
    while position < source.len() {
        let value = source[position];
        let mut end = position + 1;
        let limit = source.len().min(position + 128);
        while end < limit && source[end] == value {
            end += 1;
        }
        let length = end - position;
        // Match fo-dicom's packetization for a two-byte run at the end.
        if length >= 3 || (length == 2 && end == source.len()) {
            for chunk in source[literal_start..position].chunks(128) {
                write_literal(output, chunk);
            }
            output.extend_from_slice(&[(1i16 - length as i16) as u8, value]);
            literal_start = end;
        } else if end - literal_start >= 128 {
            let next_literal = literal_start + 128;
            write_literal(output, &source[literal_start..next_literal]);
            literal_start = next_literal;
        }
        position = end;
    }
    if literal_start < source.len() {
        write_literal(output, &source[literal_start..]);
    }
}

/// Rejects an output size that the segment input cannot justify, before the
/// output buffer is allocated. See `frame::ensure_decodable_size` for the
/// 128-bytes-per-input-byte argument.
pub(crate) fn ensure_decoded_size_fits(
    expected_length: usize,
    encoded_length: usize,
) -> Result<(), String> {
    if expected_length > MAX_DECODED_BYTES {
        return Err(format!(
            "declared segment output {expected_length} exceeds the {MAX_DECODED_BYTES}-byte limit"
        ));
    }
    let expansion_limit = encoded_length
        .checked_mul(128)
        .ok_or_else(|| "RLE segment size overflow".to_owned())?;
    if expected_length > expansion_limit {
        return Err(format!(
            "declared segment output {expected_length} exceeds the {expansion_limit}-byte expansion bound for a {encoded_length}-byte segment"
        ));
    }
    Ok(())
}

pub(crate) fn decode_segment(encoded: &[u8], expected_length: usize) -> Result<Vec<u8>, String> {
    ensure_decoded_size_fits(expected_length, encoded.len())?;
    let mut output = Vec::with_capacity(expected_length);
    let mut input = 0usize;
    while input < encoded.len() {
        let control = *encoded
            .get(input)
            .ok_or_else(|| "missing RLE control byte".to_owned())? as i8;
        input += 1;
        if control >= 0 {
            let length = control as usize + 1;
            let end = input
                .checked_add(length)
                .ok_or_else(|| "literal run length overflow".to_owned())?;
            if end > encoded.len() {
                return Err("literal run exceeds segment input".to_owned());
            }
            if output
                .len()
                .checked_add(length)
                .is_none_or(|end| end > expected_length)
            {
                return Err("literal run exceeds segment output".to_owned());
            }
            output.extend_from_slice(
                encoded
                    .get(input..end)
                    .ok_or_else(|| "literal run exceeds segment input".to_owned())?,
            );
            input = end;
        } else if control == i8::MIN {
            continue;
        } else {
            let length = (1i16 - i16::from(control)) as usize;
            let value = *encoded
                .get(input)
                .ok_or_else(|| "repeat run is missing its repeated byte".to_owned())?;
            input += 1;
            if output
                .len()
                .checked_add(length)
                .is_none_or(|end| end > expected_length)
            {
                return Err("repeat run exceeds segment output".to_owned());
            }
            output.extend(std::iter::repeat_n(value, length));
        }
    }
    if output.len() != expected_length {
        return Err(format!(
            "decoded segment length {} does not match expected length {expected_length}",
            output.len()
        ));
    }
    Ok(output)
}

fn write_literal(output: &mut Vec<u8>, bytes: &[u8]) {
    output.push((bytes.len() - 1) as u8);
    output.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::{decode_segment, ensure_decoded_size_fits};

    fn encode_segment(source: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        super::encode_segment(source, &mut output);
        output
    }

    #[test]
    fn encodes_and_decodes_literal_repeat_and_mixed_runs() {
        assert_eq!(encode_segment(&[10, 11, 12]), [2, 10, 11, 12]);
        assert_eq!(encode_segment(&[7, 7, 7]), [254, 7]);
        let source = [1, 2, 9, 9, 9, 9, 3];
        assert_eq!(encode_segment(&source), [1, 1, 2, 253, 9, 0, 3]);
        assert_eq!(decode_segment(&[1, 1, 2, 253, 9, 0, 3], 7).unwrap(), source);
    }

    #[test]
    fn handles_boundaries_and_noop() {
        let source = vec![4u8; 129];
        let encoded = encode_segment(&source);
        assert_eq!(decode_segment(&encoded, source.len()).unwrap(), source);
        assert_eq!(decode_segment(&[0x80, 0, 4], 1).unwrap(), [4]);
        assert!(decode_segment(&[2, 1, 2], 3).is_err());
        assert!(decode_segment(&[254, 7], 2).is_err());
    }

    #[test]
    fn matches_the_reference_133_byte_packetization() {
        let mut source: Vec<u8> = (0..128).map(|value| value as u8).collect();
        source.extend_from_slice(&[200, 200, 200, 5, 6]);
        let encoded = encode_segment(&source);
        let mut expected = vec![127u8];
        expected.extend(0..128);
        expected.extend_from_slice(&[254, 200, 1, 5, 6]);
        assert_eq!(encoded, expected);
    }

    #[test]
    fn encodes_a_two_byte_tail_run_as_a_repeat_like_the_reference() {
        let encoded = encode_segment(&[1, 2, 3, 3]);
        assert_eq!(encoded, [1, 1, 2, 0xff, 3]);
        assert_eq!(decode_segment(&encoded, 4).unwrap(), [1, 2, 3, 3]);
        assert_eq!(encode_segment(&[7, 7]), [0xff, 7]);
        // A run of two followed by another byte stays a literal run.
        assert_eq!(encode_segment(&[7, 7, 9]), [2, 7, 7, 9]);
        assert_eq!(
            decode_segment(&[1, 1, 2, 0xff, 3], 4).unwrap(),
            [1, 2, 3, 3]
        );
    }

    #[test]
    fn rejects_an_output_size_beyond_the_segment_input() {
        let error = decode_segment(&[0x00, 0x2a], 1_000_000).unwrap_err();
        assert!(error.contains("expansion"), "{error}");
        let error = ensure_decoded_size_fits((1 << 30) + 1, 2).unwrap_err();
        assert!(error.contains("limit"), "{error}");
    }
}
