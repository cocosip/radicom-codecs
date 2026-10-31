#![allow(clippy::chunks_exact_to_as_chunks, clippy::too_many_arguments)]

use super::bit::{BitReader, BitWriter};
use super::huffman::HuffmanTable;
use super::marker::{Marker, MarkerReader, MarkerWriter};

const HUFFMAN_COUNTS: [u8; 16] = [0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const HUFFMAN_VALUES: [u8; 17] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LosslessFrame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) precision: u8,
    pub(crate) components: u8,
    pub(crate) samples: Vec<u16>,
}

impl LosslessFrame {
    pub(crate) fn new(
        width: usize,
        height: usize,
        precision: u8,
        components: u8,
        samples: Vec<u16>,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("JPEG dimensions must be non-zero".to_owned());
        }
        if width > u16::MAX as usize || height > u16::MAX as usize {
            return Err("JPEG dimensions exceed 16-bit SOF limits".to_owned());
        }
        if !(2..=16).contains(&precision) {
            return Err(format!(
                "JPEG lossless precision {precision} is outside 2..=16"
            ));
        }
        if !(1..=3).contains(&components) {
            return Err(format!(
                "JPEG component count {components} is outside 1..=3"
            ));
        }
        let expected = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(usize::from(components)))
            .ok_or_else(|| "JPEG sample count overflows usize".to_owned())?;
        if samples.len() != expected {
            return Err(format!(
                "JPEG sample count {} does not match expected {expected}",
                samples.len()
            ));
        }
        let max = (1u32 << precision) - 1;
        if samples.iter().any(|&sample| u32::from(sample) > max) {
            return Err("JPEG sample exceeds the declared precision".to_owned());
        }
        Ok(Self {
            width,
            height,
            precision,
            components,
            samples,
        })
    }

    pub(crate) fn encode(
        &self,
        predictor: u8,
        point_transform: u8,
        restart_interval: Option<u16>,
    ) -> Result<Vec<u8>, String> {
        validate_predictor(predictor)?;
        validate_point_transform(self.precision, point_transform)?;
        if let Some(interval) = restart_interval {
            if interval == 0 {
                return Err("JPEG restart interval must be non-zero".to_owned());
            }
        }
        let effective_precision = self.precision - point_transform;
        let initial = 1i32 << (effective_precision - 1);
        let huffman = HuffmanTable::new(&HUFFMAN_COUNTS, &HUFFMAN_VALUES)?;

        let mut output = MarkerWriter::default();
        output.write_marker(Marker::Soi)?;
        output.write_segment(Marker::Sof3, &sof_payload(self)?)?;
        output.write_segment(Marker::Dht, &dht_payload())?;
        if let Some(interval) = restart_interval {
            output.write_segment(Marker::Dri, &interval.to_be_bytes())?;
        }
        output.write_segment(
            Marker::Sos,
            &sos_payload(self.components, predictor, point_transform)?,
        )?;

        let mut entropy = BitWriter::default();
        let mut reconstructed = allocate_samples(self.samples.len())?;
        let mut restart_index = 0u8;
        for pixel in 0..self.width * self.height {
            let x = pixel % self.width;
            let y = pixel / self.width;
            let restart_pixel = restart_interval
                .is_some_and(|interval| pixel != 0 && pixel % usize::from(interval) == 0);
            if restart_pixel {
                entropy.flush_to_byte();
                output_bytes(&mut output, entropy.take_bytes());
                output.write_marker(Marker::Restart(restart_index))?;
                restart_index = (restart_index + 1) & 7;
            }
            for component in 0..usize::from(self.components) {
                let index = pixel * usize::from(self.components) + component;
                let shifted = i32::from(self.samples[index] >> point_transform);
                let prediction = predict(
                    &reconstructed,
                    index,
                    x,
                    y,
                    self.width,
                    self.components,
                    component,
                    predictor,
                    initial,
                    restart_pixel,
                );
                let difference = encode_difference(shifted - prediction, effective_precision);
                let category = category(difference);
                huffman.encode(category, &mut entropy)?;
                if category != 0 && category != 16 {
                    let amplitude = encode_amplitude(difference, category);
                    entropy.write_bits(amplitude, category)?;
                }
                reconstructed[index] = u16::try_from(shifted)
                    .map_err(|_| "JPEG reconstructed sample is outside u16".to_owned())?;
            }
        }
        entropy.flush_to_byte();
        output_bytes(&mut output, entropy.take_bytes());
        output.write_marker(Marker::Eoi)?;
        Ok(output.into_bytes())
    }

    pub(crate) fn decode(data: &[u8]) -> Result<Self, String> {
        let mut reader = MarkerReader::new(data);
        expect_marker(&mut reader, Marker::Soi)?;
        let sof = loop {
            let (marker, payload) = reader.read_segment()?;
            match marker {
                Marker::Sof3 => break payload,
                Marker::App(_) | Marker::Com => {}
                other => return Err(format!("expected JPEG SOF3, got {other:?}")),
            }
        };
        let (width, height, precision, components, component_ids) = parse_sof(&sof)?;
        if !(2..=16).contains(&precision) {
            return Err(format!(
                "JPEG lossless precision {precision} is outside 2..=16"
            ));
        }
        let dht = loop {
            let (marker, payload) = reader.read_segment()?;
            match marker {
                Marker::Dht => break payload,
                Marker::App(_) | Marker::Com => {}
                other => return Err(format!("expected JPEG DHT, got {other:?}")),
            }
        };
        let huffman = parse_dht(&dht)?;
        let mut restart_interval = None;
        let (predictor, point_transform) = loop {
            let (marker, payload) = reader.read_segment()?;
            match marker {
                Marker::Dri => {
                    if payload.len() != 2 {
                        return Err("JPEG DRI payload must contain two bytes".to_owned());
                    }
                    let interval = u16::from_be_bytes([payload[0], payload[1]]);
                    if interval == 0 {
                        return Err("JPEG restart interval must be non-zero".to_owned());
                    }
                    restart_interval = Some(interval);
                }
                Marker::Sos => break parse_sos(&payload, components, &component_ids)?,
                Marker::App(_) | Marker::Com => {}
                other => return Err(format!("unexpected JPEG marker before SOS: {other:?}")),
            }
        };
        validate_point_transform(precision, point_transform)?;
        let entropy_start = reader.position();
        let entropy = data
            .get(entropy_start..)
            .ok_or_else(|| "JPEG entropy stream starts outside input".to_owned())?;
        let mut bits = BitReader::new(entropy);
        let effective_precision = precision - point_transform;
        let initial = 1i32 << (effective_precision - 1);
        let expected = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(usize::from(components)))
            .ok_or_else(|| "JPEG sample count overflows usize".to_owned())?;
        let mut samples = allocate_samples(expected)?;
        let mut shifted_samples = allocate_samples(expected)?;
        let mut restart_index = 0u8;
        for pixel in 0..width * height {
            let x = pixel % width;
            let y = pixel / width;
            let restart_pixel = restart_interval
                .is_some_and(|interval| pixel != 0 && pixel % usize::from(interval) == 0);
            if restart_pixel {
                bits.align_to_byte();
                let marker = bits.take_marker()?.ok_or_else(|| {
                    "expected JPEG restart marker at restart interval boundary".to_owned()
                })?;
                if marker != 0xd0 + restart_index {
                    return Err(format!(
                        "unexpected JPEG restart marker 0xff{marker:02x}, expected 0xff{:02x}",
                        0xd0 + restart_index
                    ));
                }
                restart_index = (restart_index + 1) & 7;
            }
            for component in 0..usize::from(components) {
                let index = pixel * usize::from(components) + component;
                let prediction = predict(
                    &shifted_samples,
                    index,
                    x,
                    y,
                    width,
                    components,
                    component,
                    predictor,
                    initial,
                    restart_pixel,
                );
                let category = huffman.decode(&mut bits)?;
                if category > effective_precision + 1 {
                    return Err(format!(
                        "JPEG lossless category {category} exceeds effective precision {effective_precision}"
                    ));
                }
                let amplitude = if category == 0 {
                    0
                } else if category == 16 {
                    -32768
                } else {
                    let bits_value = bits.read_bits(category)?;
                    decode_amplitude(bits_value, category)
                };
                let shifted = prediction
                    .checked_add(amplitude)
                    .ok_or_else(|| "JPEG lossless reconstruction overflows i32".to_owned())?;
                let max = (1i32 << effective_precision) - 1;
                let shifted = if effective_precision == 16 {
                    shifted & max
                } else {
                    shifted
                };
                if !(0..=max).contains(&shifted) {
                    return Err(format!(
                        "JPEG lossless sample is outside the declared precision at pixel {pixel}, component {component}: prediction={prediction}, amplitude={amplitude}, shifted={shifted}, max={max}"
                    ));
                }
                shifted_samples[index] = shifted as u16;
                samples[index] = u16::try_from(shifted << point_transform)
                    .map_err(|_| "JPEG lossless sample exceeds u16".to_owned())?;
            }
        }
        bits.align_to_byte();
        if bits.take_marker()? != Some(0xd9) {
            return Err("JPEG lossless scan is missing EOI".to_owned());
        }
        Self::new(width, height, precision, components, samples)
    }
}

fn validate_predictor(predictor: u8) -> Result<(), String> {
    if (1..=7).contains(&predictor) {
        Ok(())
    } else {
        Err(format!(
            "JPEG lossless predictor {predictor} is outside 1..=7"
        ))
    }
}

fn validate_point_transform(precision: u8, point_transform: u8) -> Result<(), String> {
    if point_transform < precision {
        Ok(())
    } else {
        Err(format!(
            "JPEG point transform {point_transform} must be less than precision {precision}"
        ))
    }
}

fn sof_payload(frame: &LosslessFrame) -> Result<Vec<u8>, String> {
    let mut payload = Vec::with_capacity(6 + usize::from(frame.components) * 3);
    payload.push(frame.precision);
    payload.extend((frame.height as u16).to_be_bytes());
    payload.extend((frame.width as u16).to_be_bytes());
    payload.push(frame.components);
    for component in 0..frame.components {
        payload.extend([component + 1, 0x11, 0]);
    }
    Ok(payload)
}

fn dht_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 16 + HUFFMAN_VALUES.len());
    payload.push(0);
    payload.extend(HUFFMAN_COUNTS);
    payload.extend(HUFFMAN_VALUES);
    payload
}

fn sos_payload(components: u8, predictor: u8, point_transform: u8) -> Result<Vec<u8>, String> {
    validate_predictor(predictor)?;
    let mut payload = Vec::with_capacity(2 + usize::from(components) * 2 + 3);
    payload.push(components);
    for component in 0..components {
        payload.extend([component + 1, 0]);
    }
    payload.extend([predictor, 0, point_transform]);
    Ok(payload)
}

fn parse_sof(payload: &[u8]) -> Result<(usize, usize, u8, u8, [u8; 3]), String> {
    if payload.len() < 6 {
        return Err("JPEG SOF3 payload is truncated".to_owned());
    }
    let precision = payload[0];
    let height = usize::from(u16::from_be_bytes([payload[1], payload[2]]));
    let width = usize::from(u16::from_be_bytes([payload[3], payload[4]]));
    let components = payload[5];
    if payload.len() != 6 + usize::from(components) * 3 {
        return Err("JPEG SOF3 component payload has an invalid length".to_owned());
    }
    if !(1..=3).contains(&components) || width == 0 || height == 0 {
        return Err("JPEG SOF3 dimensions or component count are invalid".to_owned());
    }
    let mut ids = [0u8; 3];
    for (index, component) in payload[6..].chunks_exact(3).enumerate() {
        if component[1] != 0x11 || component[2] != 0 {
            return Err("JPEG lossless only supports 1x1 component sampling".to_owned());
        }
        if ids[..index].contains(&component[0]) {
            return Err("JPEG lossless component identifiers must be unique".to_owned());
        }
        ids[index] = component[0];
    }
    Ok((width, height, precision, components, ids))
}

fn parse_dht(payload: &[u8]) -> Result<HuffmanTable, String> {
    if payload.len() < 17 || payload[0] != 0 {
        return Err("JPEG lossless requires one DC Huffman table with id 0".to_owned());
    }
    let counts = &payload[1..17];
    let total = counts
        .iter()
        .map(|&count| usize::from(count))
        .sum::<usize>();
    if payload.len() != 17 + total {
        return Err("JPEG DHT payload has an invalid symbol count".to_owned());
    }
    HuffmanTable::new(counts, &payload[17..])
}

fn parse_sos(payload: &[u8], components: u8, component_ids: &[u8; 3]) -> Result<(u8, u8), String> {
    let expected = 1 + usize::from(components) * 2 + 3;
    if payload.len() != expected || payload[0] != components {
        return Err("JPEG lossless SOS component count is invalid".to_owned());
    }
    for component in payload[1..1 + usize::from(components) * 2].chunks_exact(2) {
        if !component_ids[..usize::from(components)].contains(&component[0]) || component[1] != 0 {
            return Err("JPEG lossless only supports DC Huffman table 0".to_owned());
        }
    }
    let scan = &payload[1 + usize::from(components) * 2..];
    if scan[1] != 0 {
        return Err("JPEG lossless SOS spectral end must be zero".to_owned());
    }
    if scan[2] >> 4 != 0 {
        return Err("JPEG lossless successive approximation is unsupported".to_owned());
    }
    validate_predictor(scan[0])?;
    Ok((scan[0], scan[2] & 0x0f))
}

fn expect_marker(reader: &mut MarkerReader<'_>, expected: Marker) -> Result<(), String> {
    let (actual, payload) = reader.read_segment()?;
    if actual != expected || !payload.is_empty() {
        return Err(format!("expected JPEG marker {expected:?}, got {actual:?}"));
    }
    Ok(())
}

fn output_bytes(writer: &mut MarkerWriter, bytes: Vec<u8>) {
    writer.write_raw(&bytes);
}

fn allocate_samples(length: usize) -> Result<Vec<u16>, String> {
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(length)
        .map_err(|_| "JPEG frame is too large to allocate".to_owned())?;
    samples.resize(length, 0);
    Ok(samples)
}

fn category(value: i32) -> u8 {
    if value == 0 {
        return 0;
    }
    let magnitude = value.unsigned_abs();
    (32 - magnitude.leading_zeros()) as u8
}

fn encode_difference(value: i32, precision: u8) -> i32 {
    if precision == 16 {
        ((value + 32768) & 0xffff) - 32768
    } else {
        value
    }
}

fn encode_amplitude(value: i32, category: u8) -> u32 {
    if value >= 0 {
        value as u32
    } else {
        ((1i32 << category) + value - 1) as u32
    }
}

fn decode_amplitude(value: u32, category: u8) -> i32 {
    let threshold = 1u32 << (category - 1);
    if value < threshold {
        value as i32 + 1 - (1i32 << category)
    } else {
        value as i32
    }
}

fn predict(
    samples: &[u16],
    index: usize,
    x: usize,
    y: usize,
    width: usize,
    components: u8,
    component: usize,
    predictor: u8,
    initial: i32,
    reset: bool,
) -> i32 {
    let first_row = reset || y == 0;
    let stride = usize::from(components);
    if first_row {
        return if x == 0 {
            initial
        } else {
            i32::from(samples[index - stride])
        };
    }
    if x == 0 {
        return i32::from(samples[index - width * stride]);
    }
    let left = if x == 0 {
        initial
    } else {
        i32::from(samples[index - stride])
    };
    let above = if y == 0 {
        initial
    } else {
        i32::from(samples[index - width * stride])
    };
    let upper_left = if x == 0 || y == 0 {
        initial
    } else {
        i32::from(samples[index - (width + 1) * stride])
    };
    let _ = component;
    match predictor {
        1 => left,
        2 => above,
        3 => upper_left,
        4 => left + above - upper_left,
        5 => left + ((above - upper_left) >> 1),
        6 => above + ((left - upper_left) >> 1),
        7 => (left + above) >> 1,
        _ => initial,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame(precision: u8, components: u8) -> LosslessFrame {
        let width = 5;
        let height = 4;
        let mut samples = Vec::new();
        let mask = (1u32 << precision) - 1;
        for y in 0..height {
            for x in 0..width {
                for component in 0..usize::from(components) {
                    let value = ((x * 37 + y * 53 + component * 71) as u32 * 17) & mask;
                    samples.push(value as u16);
                }
            }
        }
        LosslessFrame::new(width, height, precision, components, samples).unwrap()
    }

    #[test]
    fn round_trips_8_bit_predictor_one() {
        let frame = sample_frame(8, 1);
        let encoded = frame.encode(1, 0, None).unwrap();
        let decoded = LosslessFrame::decode(&encoded).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn round_trips_12_and_16_bit_with_all_predictors() {
        for precision in [12, 16] {
            for predictor in 1..=7 {
                let frame = sample_frame(precision, 1);
                let encoded = frame.encode(predictor, 0, None).unwrap();
                let decoded = LosslessFrame::decode(&encoded).unwrap_or_else(|error| {
                    panic!("precision={precision} predictor={predictor}: {error}")
                });
                assert_eq!(
                    decoded, frame,
                    "precision={precision} predictor={predictor}"
                );
            }
        }
    }

    #[test]
    fn round_trips_interleaved_components_across_restart_markers() {
        let frame = sample_frame(8, 3);
        let encoded = frame.encode(4, 0, Some(3)).unwrap();
        assert!(encoded
            .windows(2)
            .any(|window| (0xd0..=0xd7).contains(&window[1])));
        assert_eq!(LosslessFrame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn rejects_missing_huffman_table_and_invalid_scan() {
        let frame = sample_frame(8, 1);
        let mut encoded = frame.encode(1, 0, None).unwrap();
        let dht = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xc4])
            .unwrap();
        encoded.drain(dht..dht + 2);
        assert!(LosslessFrame::decode(&encoded).is_err());
    }

    #[test]
    fn skips_app_and_com_segments_before_the_lossless_header() {
        let frame = sample_frame(8, 1);
        let encoded = frame.encode(1, 0, None).unwrap();
        let mut with_metadata = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xfe, 0, 3, 7];
        with_metadata.extend_from_slice(&encoded[2..]);
        assert_eq!(LosslessFrame::decode(&with_metadata).unwrap(), frame);
    }

    #[test]
    fn round_trips_a_point_transform_without_losing_samples() {
        let mut frame = sample_frame(12, 1);
        for sample in &mut frame.samples {
            *sample &= !3;
        }
        let encoded = frame.encode(1, 2, None).unwrap();
        assert_eq!(LosslessFrame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn accepts_zero_based_external_component_identifiers() {
        let frame = sample_frame(8, 3);
        let mut encoded = frame.encode(1, 0, None).unwrap();
        let sof = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xc3])
            .unwrap();
        for (offset, id) in [10usize, 13, 16].into_iter().zip([0u8, 1, 2]) {
            encoded[sof + offset] = id;
        }
        let sos = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xda])
            .unwrap();
        for (offset, id) in [5usize, 7, 9].into_iter().zip([0u8, 1, 2]) {
            encoded[sos + offset] = id;
        }
        assert_eq!(LosslessFrame::decode(&encoded).unwrap(), frame);
    }
}
