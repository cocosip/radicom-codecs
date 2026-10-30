pub(crate) fn encode_segment(source: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(source.len().saturating_add(source.len().div_ceil(128)));
    let mut literals = Vec::with_capacity(132);
    let mut previous = None;
    let mut repeat_count = 0usize;
    for &value in source {
        if Some(value) == previous {
            repeat_count += 1;
            if repeat_count > 2 && !literals.is_empty() {
                flush_literals(&mut output, &mut literals);
            } else if repeat_count > 128 {
                write_repeat(
                    &mut output,
                    previous.ok_or_else(|| "missing previous byte".to_owned())?,
                    128,
                );
                repeat_count -= 128;
            }
            continue;
        }
        match repeat_count {
            0 => {}
            1 | 2 => {
                for _ in 0..repeat_count {
                    literals.push(previous.ok_or_else(|| "missing previous byte".to_owned())?);
                }
            }
            count => write_repeats(
                &mut output,
                previous.ok_or_else(|| "missing previous byte".to_owned())?,
                count,
            ),
        }
        flush_oversized_literals(&mut output, &mut literals);
        previous = Some(value);
        repeat_count = 1;
    }
    match repeat_count {
        0 => {}
        1 | 2 => {
            for _ in 0..repeat_count {
                literals.push(previous.ok_or_else(|| "missing previous byte".to_owned())?);
            }
        }
        count => write_repeats(
            &mut output,
            previous.ok_or_else(|| "missing previous byte".to_owned())?,
            count,
        ),
    }
    flush_literals(&mut output, &mut literals);
    Ok(output)
}

pub(crate) fn decode_segment(encoded: &[u8], expected_length: usize) -> Result<Vec<u8>, String> {
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

fn flush_oversized_literals(output: &mut Vec<u8>, literals: &mut Vec<u8>) {
    while literals.len() > 128 {
        write_literal(output, &literals[..128]);
        literals.drain(..128);
    }
}
fn flush_literals(output: &mut Vec<u8>, literals: &mut Vec<u8>) {
    while !literals.is_empty() {
        let length = literals.len().min(128);
        write_literal(output, &literals[..length]);
        literals.drain(..length);
    }
}
fn write_literal(output: &mut Vec<u8>, bytes: &[u8]) {
    output.push((bytes.len() - 1) as u8);
    output.extend_from_slice(bytes);
}
fn write_repeats(output: &mut Vec<u8>, value: u8, mut count: usize) {
    while count != 0 {
        let length = count.min(128);
        write_repeat(output, value, length);
        count -= length;
    }
}
fn write_repeat(output: &mut Vec<u8>, value: u8, count: usize) {
    output.push((1i16 - count as i16) as u8);
    output.push(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_literal_repeat_and_mixed_runs() {
        assert_eq!(encode_segment(&[10, 11, 12]).unwrap(), [2, 10, 11, 12]);
        assert_eq!(encode_segment(&[7, 7, 7]).unwrap(), [254, 7]);
        let source = [1, 2, 9, 9, 9, 9, 3];
        assert_eq!(encode_segment(&source).unwrap(), [1, 1, 2, 253, 9, 0, 3]);
        assert_eq!(decode_segment(&[1, 1, 2, 253, 9, 0, 3], 7).unwrap(), source);
    }

    #[test]
    fn handles_boundaries_and_noop() {
        let source = vec![4u8; 129];
        let encoded = encode_segment(&source).unwrap();
        assert_eq!(decode_segment(&encoded, source.len()).unwrap(), source);
        assert_eq!(decode_segment(&[0x80, 0, 4], 1).unwrap(), [4]);
        assert!(decode_segment(&[2, 1, 2], 3).is_err());
        assert!(decode_segment(&[254, 7], 2).is_err());
    }

    #[test]
    fn matches_the_reference_133_byte_packetization() {
        let mut source: Vec<u8> = (0..128).map(|value| value as u8).collect();
        source.extend_from_slice(&[200, 200, 200, 5, 6]);
        let encoded = encode_segment(&source).unwrap();
        let mut expected = vec![127u8];
        expected.extend(0..128);
        expected.extend_from_slice(&[254, 200, 1, 5, 6]);
        assert_eq!(encoded, expected);
    }
}
