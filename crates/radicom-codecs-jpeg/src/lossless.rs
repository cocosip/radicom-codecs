#![allow(clippy::chunks_exact_to_as_chunks, clippy::too_many_arguments)]

use super::bit::{BitReader, BitWriter};
use super::huffman::HuffmanTable;
use super::marker::{Marker, MarkerReader, MarkerWriter};
use super::sequential::FrameExpectation;

const HUFFMAN_COUNTS: [u8; 16] = [0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const HUFFMAN_VALUES: [u8; 17] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

/// Scan policy imposed by the transfer syntax that owns the codestream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Process 14 (`.57`): predictors 1..=7 and a point transform are allowed.
    Process14,
    /// Process 14, selection value 1 (`.70`): predictor 1, point transform 0.
    Sv1,
}

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
        let components = (0..self.components).collect::<Vec<_>>();
        self.encode_scans(
            predictor,
            point_transform,
            restart_interval,
            std::slice::from_ref(&components),
        )
    }

    /// Encode one or more scans. A scan is a list of component indices carried
    /// by that scan; T.81 permits a non-interleaved scan per component.
    fn encode_scans(
        &self,
        predictor: u8,
        point_transform: u8,
        restart_interval: Option<u16>,
        scans: &[Vec<u8>],
    ) -> Result<Vec<u8>, String> {
        validate_predictor(predictor)?;
        validate_point_transform(self.precision, point_transform)?;
        if let Some(interval) = restart_interval {
            if interval == 0 {
                return Err("JPEG restart interval must be non-zero".to_owned());
            }
            // A lossless MCU is one sample, so one MCU row is the image width.
            // libijg-based decoders (DCMTK, fo-dicom) reject a lossless DRI that
            // is not an integer number of MCU rows, so the encoder refuses it.
            if !usize::from(interval).is_multiple_of(self.width) {
                return Err(format!(
                    "JPEG lossless restart interval {interval} must be a multiple of the image width {}",
                    self.width
                ));
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
        let mut reconstructed = allocate_samples(self.samples.len())?;
        let mut seen = [false; 3];
        for scan in scans {
            if scan.is_empty() {
                return Err("JPEG lossless scan has no components".to_owned());
            }
            for &component in scan {
                let slot = usize::from(component);
                if slot >= usize::from(self.components) {
                    return Err(format!(
                        "JPEG scan references component {component} outside the frame"
                    ));
                }
                if seen[slot] {
                    return Err(format!("JPEG lossless scan repeats component {component}"));
                }
                seen[slot] = true;
            }
            output.write_segment(Marker::Sos, &sos_payload(scan, predictor, point_transform)?)?;
            let mut entropy = BitWriter::default();
            let mut restart_index = 0u8;
            for pixel in 0..self.width * self.height {
                let x = pixel % self.width;
                let y = pixel / self.width;
                let restart_pixel = restart_interval.is_some_and(|interval| {
                    pixel != 0 && pixel.is_multiple_of(usize::from(interval))
                });
                if restart_pixel {
                    entropy.flush_to_byte();
                    output_bytes(&mut output, entropy.take_bytes());
                    output.write_marker(Marker::Restart(restart_index))?;
                    restart_index = (restart_index + 1) & 7;
                }
                for &component in scan {
                    let index = pixel * usize::from(self.components) + usize::from(component);
                    let shifted = i32::from(self.samples[index] >> point_transform);
                    let prediction = predict(
                        &reconstructed,
                        index,
                        x,
                        y,
                        self.width,
                        self.components,
                        usize::from(component),
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
        }
        output.write_marker(Marker::Eoi)?;
        Ok(output.into_bytes())
    }

    pub(crate) fn decode(
        data: &[u8],
        selection: Selection,
        expected: Option<&FrameExpectation>,
    ) -> Result<Self, String> {
        let mut reader = MarkerReader::new(data);
        expect_marker(&mut reader, Marker::Soi)?;
        let mut tables: [Option<HuffmanTable>; 4] = [None, None, None, None];
        let mut restart_interval = None;
        let mut decoded = [false; 3];
        let mut header = None;
        let mut samples = Vec::new();
        let mut shifted_samples = Vec::new();
        loop {
            let (marker, payload) = reader.read_segment()?;
            match marker {
                Marker::Sof3 => {
                    if header.is_some() {
                        return Err("JPEG frame has more than one SOF3 segment".to_owned());
                    }
                    let (width, height, precision, components, component_ids) =
                        parse_sof(&payload)?;
                    if !(2..=16).contains(&precision) {
                        return Err(format!(
                            "JPEG lossless precision {precision} is outside 2..=16"
                        ));
                    }
                    if let Some(expected) = expected {
                        if width != expected.width
                            || height != expected.height
                            || precision != expected.precision
                            || components != expected.components
                        {
                            return Err("codestream layout does not match destination".to_owned());
                        }
                    }
                    let expected = width
                        .checked_mul(height)
                        .and_then(|pixels| pixels.checked_mul(usize::from(components)))
                        .ok_or_else(|| "JPEG sample count overflows usize".to_owned())?;
                    samples = allocate_samples(expected)?;
                    shifted_samples = allocate_samples(expected)?;
                    header = Some((width, height, precision, components, component_ids));
                }
                Marker::Dht => parse_dht(&payload, &mut tables)?,
                // Lossless JPEG carries no quantization table; T.81 fixes no
                // order for the table segments before SOS (libijg writes DQT
                // and DHT ahead of SOF), so a DQT is legal and unused here.
                Marker::Dqt => {}
                Marker::Dri => restart_interval = parse_restart_interval(&payload)?,
                Marker::App(_) | Marker::Com => {}
                Marker::Sos => {
                    let Some((width, height, precision, components, component_ids)) = header else {
                        return Err("missing JPEG SOF3".to_owned());
                    };
                    let scan = parse_sos(&payload, precision, components, &component_ids)?;
                    if selection == Selection::Sv1
                        && (scan.predictor != 1 || scan.point_transform != 0)
                    {
                        return Err(format!(
                            "JPEG SV1 scan uses predictor {} and point transform {}; SV1 requires predictor 1 and point transform 0",
                            scan.predictor, scan.point_transform
                        ));
                    }
                    for &(component, _) in &scan.components {
                        if decoded[usize::from(component)] {
                            return Err(format!(
                                "JPEG lossless component {} appears in more than one scan",
                                component_ids[usize::from(component)]
                            ));
                        }
                        decoded[usize::from(component)] = true;
                    }
                    let consumed = decode_scan(
                        data,
                        reader.position(),
                        &scan,
                        &tables,
                        restart_interval,
                        width,
                        height,
                        components,
                        precision,
                        &mut samples,
                        &mut shifted_samples,
                    )?;
                    reader.seek(reader.position() + consumed);
                }
                Marker::Eoi => break,
                other => return Err(format!("unexpected JPEG marker {other:?}")),
            }
        }
        let Some((width, height, precision, components, _)) = header else {
            return Err("missing JPEG SOF3".to_owned());
        };
        if decoded[..usize::from(components)].iter().any(|&seen| !seen) {
            return Err("JPEG lossless stream does not decode every component".to_owned());
        }
        Self::new(width, height, precision, components, samples)
    }
}

/// One lossless scan: the components it carries (as frame component indices plus
/// their DC Huffman table selectors), the predictor and the point transform.
struct Scan {
    components: Vec<(u8, u8)>,
    predictor: u8,
    point_transform: u8,
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    data: &[u8],
    start: usize,
    scan: &Scan,
    tables: &[Option<HuffmanTable>; 4],
    restart_interval: Option<u16>,
    width: usize,
    height: usize,
    components: u8,
    precision: u8,
    samples: &mut [u16],
    shifted_samples: &mut [u16],
) -> Result<usize, String> {
    let entropy = data
        .get(start..)
        .ok_or_else(|| "JPEG entropy stream starts outside input".to_owned())?;
    let mut bits = BitReader::new(entropy);
    let effective_precision = precision - scan.point_transform;
    let initial = 1i32 << (effective_precision - 1);
    let mcus = width
        .checked_mul(height)
        .ok_or_else(|| "JPEG sample count overflows usize".to_owned())?;
    let stride = usize::from(components);
    let mut restart_index = 0u8;
    for mcu in 0..mcus {
        let x = mcu % width;
        let y = mcu / width;
        let restart = restart_interval
            .is_some_and(|interval| mcu != 0 && mcu.is_multiple_of(usize::from(interval)));
        if restart {
            bits.align_to_byte();
            let marker = bits.take_marker()?.ok_or_else(|| {
                "expected JPEG restart marker at restart interval boundary".to_owned()
            })?;
            let expected = 0xd0 + restart_index;
            if marker != expected {
                return Err(format!(
                    "unexpected JPEG restart marker 0xff{marker:02x}, expected 0xff{expected:02x}"
                ));
            }
            restart_index = (restart_index + 1) & 7;
        }
        for &(component, table) in &scan.components {
            let huffman = tables[usize::from(table)]
                .as_ref()
                .ok_or_else(|| format!("missing JPEG DC Huffman table {table}"))?;
            let index = mcu * stride + usize::from(component);
            let prediction = predict(
                shifted_samples,
                index,
                x,
                y,
                width,
                components,
                usize::from(component),
                scan.predictor,
                initial,
                restart,
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
                    "JPEG lossless sample is outside the declared precision at pixel {mcu}, component {component}: prediction={prediction}, amplitude={amplitude}, shifted={shifted}, max={max}"
                ));
            }
            shifted_samples[index] = shifted as u16;
            samples[index] = u16::try_from(shifted << scan.point_transform)
                .map_err(|_| "JPEG lossless sample exceeds u16".to_owned())?;
        }
    }
    bits.align_to_byte();
    Ok(bits.position())
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

fn sos_payload(scan: &[u8], predictor: u8, point_transform: u8) -> Result<Vec<u8>, String> {
    validate_predictor(predictor)?;
    let count =
        u8::try_from(scan.len()).map_err(|_| "JPEG scan has too many components".to_owned())?;
    let mut payload = Vec::with_capacity(2 + scan.len() * 2 + 3);
    payload.push(count);
    for &component in scan {
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

fn parse_dht(payload: &[u8], tables: &mut [Option<HuffmanTable>; 4]) -> Result<(), String> {
    let mut position = 0usize;
    while position < payload.len() {
        let info = payload[position];
        position += 1;
        if info >> 4 != 0 {
            return Err("JPEG lossless only supports DC Huffman tables".to_owned());
        }
        let id = usize::from(info & 0x0f);
        if id > 3 {
            return Err(format!(
                "JPEG DC Huffman table id {id} is outside the 0..=3 range"
            ));
        }
        let counts = payload
            .get(position..position + 16)
            .ok_or_else(|| "JPEG DHT payload is truncated".to_owned())?;
        position += 16;
        let total = counts
            .iter()
            .map(|&count| usize::from(count))
            .sum::<usize>();
        let values = payload
            .get(position..position + total)
            .ok_or_else(|| "JPEG DHT payload has an invalid symbol count".to_owned())?;
        position += total;
        tables[id] = Some(HuffmanTable::new(counts, values)?);
    }
    if tables.iter().all(Option::is_none) {
        return Err("JPEG lossless stream has no DC Huffman table".to_owned());
    }
    Ok(())
}

fn parse_restart_interval(payload: &[u8]) -> Result<Option<u16>, String> {
    if payload.len() != 2 {
        return Err("JPEG DRI payload must contain two bytes".to_owned());
    }
    let interval = u16::from_be_bytes([payload[0], payload[1]]);
    Ok(if interval == 0 { None } else { Some(interval) })
}

fn parse_sos(
    payload: &[u8],
    precision: u8,
    components: u8,
    component_ids: &[u8; 3],
) -> Result<Scan, String> {
    let count = usize::from(
        *payload
            .first()
            .ok_or_else(|| "JPEG SOS payload is truncated".to_owned())?,
    );
    if count == 0 || count > usize::from(components) {
        return Err("JPEG lossless SOS component count is invalid".to_owned());
    }
    if payload.len() != 1 + count * 2 + 3 {
        return Err("JPEG lossless SOS payload has an invalid length".to_owned());
    }
    let mut scan_components = Vec::with_capacity(count);
    for pair in payload[1..1 + count * 2].chunks_exact(2) {
        let index = component_ids[..usize::from(components)]
            .iter()
            .position(|&id| id == pair[0])
            .ok_or_else(|| format!("JPEG lossless SOS references unknown component {}", pair[0]))?;
        if scan_components
            .iter()
            .any(|&(component, _): &(u8, u8)| usize::from(component) == index)
        {
            return Err("JPEG lossless SOS repeats a component".to_owned());
        }
        let table = pair[1] >> 4;
        if table > 3 {
            return Err(format!(
                "JPEG DC Huffman table selector {table} is outside the 0..=3 range"
            ));
        }
        scan_components.push((index as u8, table));
    }
    let scan = &payload[1 + count * 2..];
    if scan[1] != 0 {
        return Err("JPEG lossless SOS spectral end must be zero".to_owned());
    }
    if scan[2] >> 4 != 0 {
        return Err("JPEG lossless successive approximation is unsupported".to_owned());
    }
    validate_predictor(scan[0])?;
    let point_transform = scan[2] & 0x0f;
    validate_point_transform(precision, point_transform)?;
    Ok(Scan {
        components: scan_components,
        predictor: scan[0],
        point_transform,
    })
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

    /// Process 14 accepts every predictor and point transform, so it is the
    /// widest policy for exercising the marker and entropy layers.
    fn decode_any(data: &[u8]) -> Result<LosslessFrame, String> {
        LosslessFrame::decode(data, Selection::Process14, None)
    }

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
        let decoded = decode_any(&encoded).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn round_trips_12_and_16_bit_with_all_predictors() {
        for precision in [12, 16] {
            for predictor in 1..=7 {
                let frame = sample_frame(precision, 1);
                let encoded = frame.encode(predictor, 0, None).unwrap();
                let decoded = decode_any(&encoded).unwrap_or_else(|error| {
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
    fn rejects_restart_interval_that_is_not_a_multiple_of_the_width() {
        let frame = sample_frame(8, 1);
        // libijg-based decoders reject a lossless restart interval that is not
        // an integer number of MCU rows; a lossless MCU row is the image width.
        let error = frame.encode(1, 0, Some(3)).unwrap_err();
        assert!(error.contains("multiple of the image width"), "{error}");
        assert!(frame.encode(1, 0, Some(5)).is_ok());
    }

    #[test]
    fn round_trips_interleaved_components_across_restart_markers() {
        let frame = sample_frame(8, 3);
        let encoded = frame.encode(4, 0, Some(5)).unwrap();
        assert!(encoded
            .windows(2)
            .any(|window| (0xd0..=0xd7).contains(&window[1])));
        assert_eq!(decode_any(&encoded).unwrap(), frame);
    }

    #[test]
    fn accepts_multiple_dht_segments_and_dc_table_selectors() {
        let frame = sample_frame(8, 1);
        let encoded = frame.encode(1, 0, None).unwrap();
        let sos = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xda])
            .unwrap();
        let sos_length = usize::from(u16::from_be_bytes([encoded[sos + 2], encoded[sos + 3]]));
        let entropy = &encoded[sos + 2 + sos_length..encoded.len() - 2];
        for table in 0..=3u8 {
            let mut stream = MarkerWriter::default();
            stream.write_marker(Marker::Soi).unwrap();
            stream
                .write_segment(Marker::Sof3, &sof_payload(&frame).unwrap())
                .unwrap();
            for id in 0..=3u8 {
                let mut payload = dht_payload();
                payload[0] = id;
                stream.write_segment(Marker::Dht, &payload).unwrap();
            }
            let mut sos_payload = sos_payload(&[0], 1, 0).unwrap();
            sos_payload[2] = table << 4;
            stream.write_segment(Marker::Sos, &sos_payload).unwrap();
            stream.write_raw(entropy);
            stream.write_marker(Marker::Eoi).unwrap();
            assert_eq!(
                decode_any(&stream.into_bytes()).unwrap(),
                frame,
                "table selector {table}"
            );
        }
    }

    #[test]
    fn decodes_non_interleaved_multi_scan_streams() {
        let frame = sample_frame(8, 3);
        let per_component = frame
            .encode_scans(1, 0, None, &[vec![0], vec![1], vec![2]])
            .unwrap();
        assert_eq!(
            decode_any(&per_component).unwrap(),
            frame,
            "one scan per component"
        );
        let mixed = frame
            .encode_scans(1, 0, None, &[vec![0, 1], vec![2]])
            .unwrap();
        assert_eq!(
            decode_any(&mixed).unwrap(),
            frame,
            "interleaved subset scan"
        );
    }

    #[test]
    fn rejects_repeated_or_missing_components_across_scans() {
        let frame = sample_frame(8, 3);
        let per_component = frame
            .encode_scans(1, 0, None, &[vec![0], vec![1], vec![2]])
            .unwrap();
        let sos: Vec<usize> = per_component
            .windows(2)
            .enumerate()
            .filter(|(_, window)| *window == [0xff, 0xda])
            .map(|(index, _)| index)
            .collect();
        assert_eq!(sos.len(), 3);
        let mut repeated = per_component.clone();
        repeated[sos[1] + 5] = 1;
        let error = decode_any(&repeated).unwrap_err();
        assert!(error.contains("more than one scan"), "{error}");
        let missing = frame.encode_scans(1, 0, None, &[vec![0], vec![1]]).unwrap();
        let error = decode_any(&missing).unwrap_err();
        assert!(error.contains("every component"), "{error}");
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
        assert!(decode_any(&encoded).is_err());
    }

    #[test]
    fn accepts_table_segments_before_sof3() {
        let frame = sample_frame(8, 1);
        let encoded = frame.encode(1, 0, None).unwrap();
        let dht = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xc4])
            .unwrap();
        let dht_length = usize::from(u16::from_be_bytes([encoded[dht + 2], encoded[dht + 3]]));
        let dht_end = dht + 2 + dht_length;
        // T.81 fixes no order for the table segments before SOS; libijg writes
        // DQT and DHT ahead of SOF, so both must be accepted there.
        let mut stream = encoded[..2].to_vec();
        stream.extend_from_slice(&[0xff, 0xdb, 0x00, 0x43, 0x00]);
        stream.extend_from_slice(&[16u8; 64]);
        stream.extend_from_slice(&encoded[dht..dht_end]);
        stream.extend_from_slice(&encoded[2..dht]); // SOF3 after the tables
        stream.extend_from_slice(&encoded[dht_end..]);
        assert_eq!(decode_any(&stream).unwrap(), frame);
    }

    #[test]
    fn skips_app_and_com_segments_before_the_lossless_header() {
        let frame = sample_frame(8, 1);
        let encoded = frame.encode(1, 0, None).unwrap();
        let mut with_metadata = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xfe, 0, 3, 7];
        with_metadata.extend_from_slice(&encoded[2..]);
        assert_eq!(decode_any(&with_metadata).unwrap(), frame);
    }

    #[test]
    fn sv1_selection_rejects_a_point_transform_and_a_foreign_predictor() {
        let frame = sample_frame(8, 1);
        let transformed = frame.encode(1, 4, None).unwrap();
        let predictor_two = frame.encode(2, 0, None).unwrap();
        for stream in [&transformed, &predictor_two] {
            let error = LosslessFrame::decode(stream, Selection::Sv1, None).unwrap_err();
            assert!(error.contains("SV1"), "{error}");
            assert!(LosslessFrame::decode(stream, Selection::Process14, None).is_ok());
        }
    }

    #[test]
    fn rejects_a_sof_that_disagrees_with_the_destination() {
        let frame = sample_frame(8, 1);
        let encoded = frame.encode(1, 0, None).unwrap();
        let expected = FrameExpectation {
            width: 5,
            height: 4,
            components: 1,
            precision: 8,
        };
        assert!(LosslessFrame::decode(&encoded, Selection::Process14, Some(&expected)).is_ok());
        // The destination contract is checked against the SOF header, so a
        // hostile frame size is refused before its sample buffer is reserved.
        let hostile = FrameExpectation {
            width: 65535,
            height: 65535,
            components: 1,
            precision: 8,
        };
        let error =
            LosslessFrame::decode(&encoded, Selection::Process14, Some(&hostile)).unwrap_err();
        assert!(error.contains("destination"), "{error}");
    }

    #[test]
    fn round_trips_a_point_transform_without_losing_samples() {
        let mut frame = sample_frame(12, 1);
        for sample in &mut frame.samples {
            *sample &= !3;
        }
        let encoded = frame.encode(1, 2, None).unwrap();
        assert_eq!(decode_any(&encoded).unwrap(), frame);
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
        assert_eq!(decode_any(&encoded).unwrap(), frame);
    }
}
