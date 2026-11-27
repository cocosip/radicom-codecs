#![allow(clippy::too_many_arguments, clippy::if_same_then_else)]
use crate::{
    bit::{BitReader, BitWriter, EntropyError},
    context::{RegularContext, RunContext},
    golomb::{decode_mapped, encode_mapped},
    marker::split_restart_segments,
    preset::Preset,
    run::{map_error, predict, unmap_error, J},
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScanConfig {
    pub width: usize,
    pub height: usize,
    pub components: usize,
    pub maxval: u16,
    pub near: u16,
    pub ilv: u8,
    pub preset: Preset,
}

#[derive(Clone, Copy)]
struct ErrorQuantizer {
    near: i32,
    range: i32,
    reciprocal: u64,
    lossless_shift: Option<u32>,
}

impl ErrorQuantizer {
    fn new(near: u16, range: i32) -> Self {
        Self {
            near: i32::from(near),
            range,
            reciprocal: (1u64 << 32).div_ceil(2 * u64::from(near) + 1),
            lossless_shift: (near == 0 && (range as u32).is_power_of_two())
                .then(|| 32 - range.trailing_zeros()),
        }
    }

    #[inline]
    fn quantize(self, value: i32) -> i32 {
        if let Some(shift) = self.lossless_shift {
            return (value << shift) >> shift;
        }
        let quantized = if self.near == 0 {
            value
        } else {
            // MAXVAL + NEAR < 2^17 and the divisor is at most 511. The
            // rounded-up reciprocal therefore gives the exact integer quotient.
            let magnitude = ((u64::from(value.unsigned_abs() + self.near as u32) * self.reciprocal)
                >> 32) as i32;
            if value < 0 {
                -magnitude
            } else {
                magnitude
            }
        };
        let reduced = quantized + if quantized < 0 { self.range } else { 0 };
        if reduced >= (self.range + 1) / 2 {
            reduced - self.range
        } else {
            reduced
        }
    }
}

#[inline]
fn reconstructed(pred: i32, e: i32, sign: i32, near: i32, maxval: i32, range: i32) -> i32 {
    if near == 0 && maxval & (maxval + 1) == 0 {
        return (pred + sign * e) & maxval;
    }
    let step = 2 * near + 1;
    let mut value = pred + sign * e * step;
    if value as u32 <= maxval as u32 {
        return value;
    }
    if value < -near {
        value += range * step;
    } else if value > maxval + near {
        value -= range * step;
    }
    correct_prediction(value, maxval)
}

fn correct_prediction(pred: i32, maxval: i32) -> i32 {
    if pred as u32 <= maxval as u32 {
        pred
    } else if pred < 0 {
        0
    } else {
        maxval
    }
}

#[cfg(test)]
fn context_id(ra: i32, rb: i32, rc: i32, rd: i32, gradients: &GradientTable) -> i32 {
    let q1 = gradients.quantize_reconstructed(rd - rb);
    let q2 = gradients.quantize_reconstructed(rb - rc);
    let q3 = gradients.quantize_reconstructed(rc - ra);
    (q1 * 9 + q2) * 9 + q3
}

/// The gradient range is bounded by MAXVAL in both encoding and reconstruction.
#[derive(Clone)]
struct GradientTable {
    values: Vec<i8>,
    center: i32,
}

impl GradientTable {
    fn for_scan(preset: Preset, near: u16) -> Rc<Self> {
        thread_local! {
            static LAST_SCAN: RefCell<Option<(Preset, u16, Rc<GradientTable>)>> =
                const { RefCell::new(None) };
        }
        LAST_SCAN.with_borrow_mut(|last| {
            if let Some((cached_preset, cached_near, table)) = last {
                if *cached_preset == preset && *cached_near == near {
                    return Rc::clone(table);
                }
            }
            let table = Rc::new(Self::new(preset, near));
            *last = Some((preset, near, Rc::clone(&table)));
            table
        })
    }

    fn new(preset: Preset, near: u16) -> Self {
        let center = usize::from(preset.maxval);
        let mut values = vec![0; center * 2 + 1];
        let starts = [
            usize::from(near) + 1,
            usize::from(preset.t1),
            usize::from(preset.t2),
            usize::from(preset.t3),
            center + 1,
        ];
        for (level, pair) in starts.windows(2).enumerate() {
            let level = level as i8 + 1;
            let (start, end) = (pair[0], pair[1]);
            values[center + start..center + end].fill(level);
            values[center + 1 - end..center + 1 - start].fill(-level);
        }
        Self {
            values,
            center: center as i32,
        }
    }

    #[inline]
    fn quantize(&self, difference: i32) -> i32 {
        i32::from(self.values[(self.center + difference) as usize])
    }

    // The scan verifies that the gradient table uses the same MAXVAL as
    // reconstruction. Every difference of two reconstructed samples is
    // therefore in -MAXVAL..=MAXVAL.
    #[inline(always)]
    fn quantize_reconstructed(&self, difference: i32) -> i32 {
        let index = (self.center + difference) as usize;
        debug_assert!(index < self.values.len());
        // SAFETY: Callers use differences of reconstructed samples only.
        unsafe { i32::from(*self.values.get_unchecked(index)) }
    }
}

fn golomb_params(c: &ScanConfig) -> (i32, u32, u8) {
    (
        c.preset.range(c.near) as i32,
        c.preset.limit(c.near),
        c.preset.qbpp(c.near),
    )
}

/// D.4 `Reset_encoder` / `Reset_decoder`: re-initialise the adaptive model and
/// treat the first line of the restart interval as the first line of a scan, so
/// the previous line and the left edge sample are all zero for prediction.
/// Reconstruction buffers keep their real samples; only prediction restarts.
fn reset_restart_state<T: Copy + Default>(
    range: i32,
    contexts: &mut [RegularContext],
    run_contexts: &mut [RunContext; 2],
    run_indexes: &mut [usize],
    previous: &mut [T],
    previous_left: &mut [T],
) {
    contexts.fill(RegularContext::new(range));
    run_contexts[0] = RunContext::new(0, range);
    run_contexts[1] = RunContext::new(1, range);
    run_indexes.fill(0);
    previous.fill(T::default());
    previous_left.fill(T::default());
}

/// Number of scan segments implied by the restart interval for an image whose
/// MCU is one line of `c.height` lines (ILV=0 and ILV=2, and ILV=1 with every
/// vertical factor equal to one).
fn expected_segments(c: &ScanConfig, restart_interval: u32) -> u64 {
    expected_segments_for(c.height.max(1) as u64, restart_interval)
}

/// Segment count for `mcus` minimum coded units. T.87 D.4 requires the restart
/// interval to be counted in MCUs, and allows the final interval to hold fewer
/// MCUs than the DRI value.
fn expected_segments_for(mcus: u64, restart_interval: u32) -> u64 {
    if restart_interval == 0 {
        return 1;
    }
    (mcus.max(1) - 1) / u64::from(restart_interval) + 1
}

/// Byte-aligns the current segment and appends the next RSTm marker.
fn end_restart_interval(writer: BitWriter, index: &mut u8) -> BitWriter {
    let mut output = writer.finish();
    output.extend([0xff, 0xd0 + *index]);
    *index = (*index + 1) & 7;
    BitWriter::from_bytes(output)
}

/// Finishes the current segment and seats the reader on the next one.
fn begin_restart_interval<'a>(
    reader: &mut BitReader<'a>,
    segments: &[&'a [u8]],
    index: &mut usize,
) -> Result<(), String> {
    reader.finish()?;
    *index += 1;
    let bytes = *segments
        .get(*index)
        .ok_or("missing JPEG-LS restart marker")?;
    *reader = BitReader::new(bytes);
    Ok(())
}

#[inline(always)]
fn encode_regular(
    w: &mut BitWriter,
    ctx: &mut RegularContext,
    qs: i32,
    x: i32,
    ra: i32,
    rb: i32,
    rc: i32,
    c: &ScanConfig,
    quantizer: ErrorQuantizer,
    limit: u32,
    qbpp: u8,
) -> i32 {
    let sign_mask = qs >> 31;
    let bias = (ctx.bias() ^ sign_mask) - sign_mask;
    let corrected = correct_prediction(predict(ra, rb, rc) + bias, c.maxval as i32);
    let error = quantizer.quantize(((x - corrected) ^ sign_mask) - sign_mask);
    let k = ctx.k();
    let correction = ctx.correction(k | c.near as u8);
    let mapped = map_error(error ^ correction);
    encode_mapped(w, k, mapped, limit, qbpp).expect("bounded Golomb parameters");
    ctx.update(error, c.near as i32, c.preset.reset as i32);
    if c.near == 0 {
        return x;
    }
    reconstructed(
        corrected,
        (error ^ sign_mask) - sign_mask,
        1,
        c.near as i32,
        c.maxval as i32,
        quantizer.range,
    )
}

#[inline(always)]
fn decode_regular(
    r: &mut BitReader<'_>,
    contexts: &mut [RegularContext; 365],
    qs: i32,
    prediction: i32,
    c: &ScanConfig,
    range: i32,
    limit: u32,
    qbpp: u8,
) -> Result<i32, EntropyError> {
    let context_index = qs.unsigned_abs() as usize;
    debug_assert!(context_index < contexts.len());
    // SAFETY: Each gradient quantizes to -4..=4, so the base-nine
    // combination is in -364..=364 for all reconstructed samples.
    let ctx = unsafe { contexts.get_unchecked_mut(context_index) };
    decode_regular_with_context(r, ctx, qs, prediction, c, range, limit, qbpp)
}

#[inline(always)]
fn decode_regular_with_context(
    r: &mut BitReader<'_>,
    ctx: &mut RegularContext,
    qs: i32,
    prediction: i32,
    c: &ScanConfig,
    range: i32,
    limit: u32,
    qbpp: u8,
) -> Result<i32, EntropyError> {
    let sign_mask = qs >> 31;
    let bias = (ctx.bias() ^ sign_mask) - sign_mask;
    let corrected = correct_prediction(prediction + bias, c.maxval as i32);
    if c.near != 0 && ctx.accepts_zero_rice() && r.take_zero_rice() {
        ctx.update_zero_error(i32::from(c.preset.reset));
        return Ok(corrected);
    }
    let k = ctx.k();
    let mapped = decode_mapped(r, k, limit, qbpp)?;
    let error = unmap_error(mapped) ^ ctx.correction(k | c.near as u8);
    if error.wrapping_add(range / 2) as u32 >= range as u32 {
        return Err(EntropyError::PredictionErrorExceedsRange);
    }
    ctx.update(error, c.near as i32, c.preset.reset as i32);
    let signed_error = (error ^ sign_mask) - sign_mask;
    Ok(reconstructed(
        corrected,
        signed_error,
        1,
        c.near as i32,
        c.maxval as i32,
        range,
    ))
}

#[inline(never)]
fn decode_byte_rgb_shared<T: OutputSample>(
    reader: &mut BitReader<'_>,
    stored: &mut RegularContext,
    context_id: i32,
    predictions: [i32; 3],
) -> Result<[T; 3], EntropyError> {
    const RANGE: i32 = 256;
    const LIMIT: u32 = 32;
    const QBPP: u8 = 8;
    let sign = context_id >> 31;
    if stored.accepts_unit_rice_triplet() {
        if let Some(mapped) = reader.take_unit_rice_triplet() {
            // With k=0 these three errors are all -1 or 0. These updates
            // preserve k and the bias, so only the correction can change.
            let bias = (stored.bias() ^ sign) - sign;
            let errors = stored.update_unit_rice_triplet(mapped);
            return Ok(std::array::from_fn(|i| {
                let predicted = correct_prediction(predictions[i] + bias, 255);
                T::from_integer((predicted + ((errors[i] ^ sign) - sign)) & 255)
            }));
        }
    }
    let mut state = *stored;
    let mut sample = [T::default(); 3];
    for i in 0..3 {
        let bias = (state.bias() ^ sign) - sign;
        let predicted = correct_prediction(predictions[i] + bias, 255);
        let k = state.k();
        let mapped = decode_mapped(reader, k, LIMIT, QBPP)?;
        let error = unmap_error(mapped) ^ state.correction(k);
        if error.wrapping_add(RANGE / 2) as u32 >= RANGE as u32 {
            return Err(EntropyError::PredictionErrorExceedsRange);
        }
        state.update(error, 0, 64);
        let corrected = (error ^ sign) - sign;
        sample[i] = T::from_integer((predicted + corrected) & 255);
    }
    *stored = state;
    Ok(sample)
}

#[inline(always)]
fn decode_lossless_byte_regular(
    reader: &mut BitReader<'_>,
    contexts: &mut [RegularContext; 365],
    context_id: i32,
    prediction: i32,
) -> Result<i32, EntropyError> {
    // SAFETY: The three quantized gradients each range from -4 through 4.
    let state = unsafe { contexts.get_unchecked_mut(context_id.unsigned_abs() as usize) };
    let sign = context_id >> 31;
    let bias = (state.bias() ^ sign) - sign;
    let predicted = correct_prediction(prediction + bias, 255);
    if state.accepts_zero_rice() {
        if let Some(code_error) = reader.take_nonpositive_unit_rice() {
            let error = code_error ^ state.correction(0);
            state.update_nonpositive_unit_error(error);
            return Ok((predicted + ((error ^ sign) - sign)) & 255);
        }
    }
    let k = state.k();
    let error = unmap_error(decode_mapped(reader, k, 32, 8)?) ^ state.correction(k);
    if error.wrapping_add(128) as u32 >= 256 {
        return Err(EntropyError::PredictionErrorExceedsRange);
    }
    state.update(error, 0, 64);
    Ok((predicted + ((error ^ sign) - sign)) & 255)
}

fn encode_run_interrupt(
    w: &mut BitWriter,
    contexts: &mut [RunContext; 2],
    run_index: usize,
    x: i32,
    ra: i32,
    rb: i32,
    c: &ScanConfig,
    quantizer: ErrorQuantizer,
    limit: u32,
    qbpp: u8,
) -> i32 {
    let near = c.near as i32;
    let (typ, error) = if (c.components == 1 || c.ilv == 1) && (ra - rb).abs() <= near {
        (1usize, quantizer.quantize(x - ra))
    } else {
        let sign = if rb >= ra { 1 } else { -1 };
        (0usize, quantizer.quantize((x - rb) * sign))
    };
    let k = contexts[typ].k();
    let mapped = contexts[typ].encode_value(error, k, c.preset.reset as i32);
    encode_mapped(w, k, mapped, limit - u32::from(J[run_index]) - 1, qbpp)
        .expect("bounded run Golomb parameters");
    let sign = if typ == 1 {
        1
    } else if rb >= ra {
        1
    } else {
        -1
    };
    let reference = if typ == 1 { ra } else { rb };
    reconstructed(
        reference,
        error,
        sign,
        near,
        c.maxval as i32,
        quantizer.range,
    )
}

#[inline(always)]
fn decode_run_interrupt(
    r: &mut BitReader<'_>,
    contexts: &mut [RunContext; 2],
    run_index: usize,
    ra: i32,
    rb: i32,
    c: &ScanConfig,
    range: i32,
    limit: u32,
    qbpp: u8,
) -> Result<i32, String> {
    let near = c.near as i32;
    let (typ, reference, sign) = if (c.components == 1 || c.ilv == 1) && (ra - rb).abs() <= near {
        (1usize, ra, 1)
    } else {
        (0usize, rb, if rb >= ra { 1 } else { -1 })
    };
    let k = contexts[typ].k();
    let mapped = decode_mapped(r, k, limit - u32::from(J[run_index]) - 1, qbpp)? as i32;
    if mapped > range {
        return Err("mapped run interruption error exceeds RANGE".into());
    }
    let error = contexts[typ].decode_value(mapped + typ as u32 as i32, k, c.preset.reset as i32);
    if error < -(range / 2) || error > (range - 1) / 2 {
        return Err("run interruption error exceeds RANGE".into());
    }
    Ok(reconstructed(
        reference,
        error,
        sign,
        near,
        c.maxval as i32,
        range,
    ))
}

fn encode_run_length(
    w: &mut BitWriter,
    mut run: usize,
    run_index: &mut usize,
    end_of_line: bool,
) -> Result<(), String> {
    let mut flags = 0u8;
    while run >= (1usize << J[*run_index]) {
        if flags == 32 {
            w.write_bits(u32::MAX, 32)?;
            flags = 0;
        }
        flags += 1;
        run -= 1usize << J[*run_index];
        *run_index = (*run_index + 1).min(31);
    }
    if flags != 0 {
        w.write_bits(u32::MAX >> (32 - flags), flags)?;
    }
    if !end_of_line {
        w.write_bits(run as u32, J[*run_index] + 1)?;
    } else if run != 0 {
        w.write_bit(true);
    }
    Ok(())
}

#[inline]
fn decode_run_length(
    reader: &mut BitReader<'_>,
    room: usize,
    index: &mut usize,
) -> Result<usize, String> {
    let mut remaining = room;
    loop {
        let ones = reader.run_prefix()?;
        for bit in 0..ones {
            let chunk = 1usize << J[*index];
            if chunk >= remaining {
                if chunk == remaining {
                    *index = (*index + 1).min(31);
                }
                reader.skip_buffered(bit + 1);
                return Ok(room);
            }
            *index = (*index + 1).min(31);
            remaining -= chunk;
        }
        if ones == reader.buffered_bits() {
            reader.skip_buffered(ones);
            continue;
        }
        reader.skip_buffered(ones + 1);
        let width = J[*index];
        let tail = if width == 0 {
            0
        } else {
            reader.read_bits(width)? as usize
        };
        if tail > remaining {
            return Err("run exceeds line".into());
        }
        return Ok(room - remaining + tail);
    }
}

/// Test-only shorthand for a scan without restart markers.
#[cfg(test)]
pub(crate) fn encode_scan(c: ScanConfig, input: &[u16]) -> Result<Vec<u8>, String> {
    encode_restart_scan(c, input, 0)
}

#[cfg(test)]
pub(crate) fn encode_restart_scan(
    c: ScanConfig,
    input: &[u16],
    restart_interval: u32,
) -> Result<Vec<u8>, String> {
    append_scan(c, input, restart_interval, Vec::new())
}

/// Encodes one scan, emitting an `RSTm` marker every `restart_interval` MCUs
/// (image lines, T.87 B.4); `0` emits none.
pub(crate) fn append_scan(
    c: ScanConfig,
    input: &[u16],
    restart_interval: u32,
    output: Vec<u8>,
) -> Result<Vec<u8>, String> {
    append_scan_input(c, ScanInput::Words(input), restart_interval, output)
}

pub(crate) fn append_byte_scan(
    c: ScanConfig,
    input: &[u8],
    restart_interval: u32,
    output: Vec<u8>,
) -> Result<Vec<u8>, String> {
    append_scan_input(c, ScanInput::Bytes(input), restart_interval, output)
}

#[derive(Clone, Copy)]
enum ScanInput<'a> {
    Words(&'a [u16]),
    Bytes(&'a [u8]),
}

impl ScanInput<'_> {
    fn len(self) -> usize {
        match self {
            Self::Words(samples) => samples.len(),
            Self::Bytes(samples) => samples.len(),
        }
    }

    fn row<'a>(&'a self, range: std::ops::Range<usize>, scratch: &'a mut Vec<u16>) -> &'a [u16] {
        match self {
            Self::Words(samples) => &samples[range],
            Self::Bytes(samples) => {
                scratch.clear();
                scratch.extend(samples[range].iter().copied().map(u16::from));
                scratch
            }
        }
    }
}

fn append_scan_input(
    c: ScanConfig,
    input: ScanInput<'_>,
    restart_interval: u32,
    output: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let expected = c
        .width
        .checked_mul(c.height)
        .and_then(|v| v.checked_mul(c.components))
        .ok_or("image size overflow")?;
    if input.len() != expected {
        return Err("sample count mismatch".into());
    }
    if c.components == 0 || c.components > 3 || c.ilv > 2 {
        return Err("unsupported interleave mode".into());
    }
    if c.ilv == 0 && c.components != 1 {
        return Err("non-interleaved scan requires one component".into());
    }
    if c.ilv == 1 {
        return encode_line_scan(c, input, restart_interval, output);
    }
    match (c.components, c.near == 0) {
        (1, true) => encode_interleaved::<1, true>(c, input, restart_interval, output),
        (1, false) => encode_interleaved::<1, false>(c, input, restart_interval, output),
        (2, _) => encode_interleaved::<2, false>(c, input, restart_interval, output),
        (3, true) => encode_interleaved::<3, true>(c, input, restart_interval, output),
        (3, false) => encode_interleaved::<3, false>(c, input, restart_interval, output),
        _ => unreachable!("validated component count"),
    }
}

fn encode_interleaved<const COMPONENTS: usize, const LOSSLESS: bool>(
    c: ScanConfig,
    input: ScanInput<'_>,
    restart_interval: u32,
    output: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let (range, limit, qbpp) = golomb_params(&c);
    let quantizer = ErrorQuantizer::new(c.near, range);
    let gradients = GradientTable::for_scan(c.preset, c.near);
    let interval = u64::from(restart_interval);
    let mut restart_index = 0u8;
    let mut writer = BitWriter::from_bytes(output);
    let mut contexts = [RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_index = 0usize;
    let stride = c.width + 2;
    let mut stack_lines = [[0u16; COMPONENTS]; 32];
    let mut allocated_lines;
    let lines = if stride * 2 <= stack_lines.len() {
        &mut stack_lines[..stride * 2]
    } else {
        allocated_lines = vec![[0u16; COMPONENTS]; stride * 2];
        &mut allocated_lines
    };
    let (mut previous, mut current) = lines.split_at_mut(stride);
    let mut previous_left = [0u16; COMPONENTS];
    let row_len = c.width * COMPONENTS;
    let mut scratch = Vec::new();
    for y in 0..c.height {
        let input_row = input.row(y * row_len..(y + 1) * row_len, &mut scratch);
        // T.87 B.4/D.4: an MCU is one image line, so a restart boundary always
        // falls between lines and never inside a run.
        if interval != 0 && y != 0 && (y as u64).is_multiple_of(interval) {
            writer = end_restart_interval(writer, &mut restart_index);
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                std::slice::from_mut(&mut run_index),
                previous.as_flattened_mut(),
                &mut previous_left,
            );
        }
        // T.87 A.2.1 defines the samples immediately outside both ends of each row.
        previous[0] = previous_left;
        previous[c.width + 1] = previous[c.width];
        current[0] = previous[1];
        let input_row = input_row.as_chunks::<COMPONENTS>().0;
        if LOSSLESS {
            current[1..=c.width].copy_from_slice(input_row);
        }
        let mut ra = current[0];
        let mut horizontal: [i32; COMPONENTS] = std::array::from_fn(|i| {
            gradients.quantize(i32::from(previous[1][i]) - i32::from(previous[0][i]))
        });
        let mut x = 0usize;
        while x < c.width {
            let [rc, rb, rd] = previous[x..x + 3] else {
                unreachable!("each row has two guard samples");
            };
            let qs: [i32; COMPONENTS] = std::array::from_fn(|i| {
                let next = gradients.quantize(i32::from(rd[i]) - i32::from(rb[i]));
                let context = (next * 9 + horizontal[i]) * 9
                    + gradients.quantize(i32::from(rc[i]) - i32::from(ra[i]));
                horizontal[i] = next;
                context
            });
            if qs.iter().all(|&q| q == 0) {
                let room = c.width - x;
                let run = matching_run(&input_row[x..], ra, c.near);
                encode_run_length(&mut writer, run, &mut run_index, run == room)?;
                if !LOSSLESS {
                    current[x + 1..x + run + 1].fill(ra);
                }
                if run == room {
                    x += run;
                } else {
                    let pos = x + run;
                    for i in 0..COMPONENTS {
                        let reconstructed = encode_run_interrupt(
                            &mut writer,
                            &mut run_contexts,
                            run_index,
                            input_row[pos][i] as i32,
                            i32::from(ra[i]),
                            i32::from(previous[pos + 1][i]),
                            &c,
                            quantizer,
                            limit,
                            qbpp,
                        ) as u16;
                        if !LOSSLESS {
                            current[pos + 1][i] = reconstructed;
                        }
                    }
                    run_index = run_index.saturating_sub(1);
                    x = pos + 1;
                    if run != 0 {
                        horizontal = std::array::from_fn(|i| {
                            gradients
                                .quantize(i32::from(previous[x + 1][i]) - i32::from(previous[x][i]))
                        });
                    }
                }
            } else {
                for i in 0..COMPONENTS {
                    let idx = qs[i].unsigned_abs() as usize;
                    let sample = input_row[x][i] as i32;
                    let reconstructed = encode_regular(
                        &mut writer,
                        &mut contexts[idx],
                        qs[i],
                        sample,
                        i32::from(ra[i]),
                        i32::from(rb[i]),
                        i32::from(rc[i]),
                        &c,
                        quantizer,
                        limit,
                        qbpp,
                    ) as u16;
                    if !LOSSLESS {
                        current[x + 1][i] = reconstructed;
                    }
                }
                x += 1;
            }
            ra = current[x];
        }
        previous_left = previous[1];
        std::mem::swap(&mut previous, &mut current);
    }
    Ok(writer.finish())
}

#[inline]
fn matching_run<const COMPONENTS: usize>(
    samples: &[[u16; COMPONENTS]],
    reference: [u16; COMPONENTS],
    near: u16,
) -> usize {
    #[cfg(target_arch = "x86_64")]
    if samples.len() >= 16 {
        if COMPONENTS == 1 && samples[0][0].abs_diff(reference[0]) <= near {
            return matching_mono_run(samples.as_flattened(), reference[0], near);
        }
        if COMPONENTS == 3
            && samples[0]
                .iter()
                .zip(reference)
                .all(|(&a, b)| a.abs_diff(b) <= near)
        {
            let reference = [reference[0], reference[1], reference[2]];
            return matching_rgb_run(samples.as_flattened(), reference, near);
        }
    }
    samples
        .iter()
        .take_while(|sample| (0..COMPONENTS).all(|i| sample[i].abs_diff(reference[i]) <= near))
        .count()
}

#[cfg(target_arch = "x86_64")]
fn matching_mono_run(samples: &[u16], reference: u16, near: u16) -> usize {
    use std::arch::x86_64::*;

    let (blocks, tail) = samples.as_chunks::<8>();
    // SAFETY: SSE2 is available on x86-64. Each load reads one complete block.
    unsafe {
        let reference = _mm_set1_epi16(reference as i16);
        let limit = _mm_set1_epi16(near as i16);
        let zero = _mm_setzero_si128();
        for (index, block) in blocks.iter().enumerate() {
            let values = _mm_loadu_si128(block.as_ptr().cast());
            let matches = if near == 0 {
                _mm_cmpeq_epi16(values, reference)
            } else {
                let difference = _mm_or_si128(
                    _mm_subs_epu16(values, reference),
                    _mm_subs_epu16(reference, values),
                );
                _mm_cmpeq_epi16(_mm_subs_epu16(difference, limit), zero)
            };
            let mask = _mm_movemask_epi8(matches) as u16;
            if mask != u16::MAX {
                return index * 8 + (mask.trailing_ones() as usize / 2);
            }
        }
    }
    blocks.len() * 8
        + tail
            .iter()
            .take_while(|&&sample| sample.abs_diff(reference) <= near)
            .count()
}

#[cfg(target_arch = "x86_64")]
fn matching_rgb_run(samples: &[u16], [r, g, b]: [u16; 3], near: u16) -> usize {
    use std::arch::x86_64::*;

    let (blocks, tail) = samples.as_chunks::<24>();
    // SAFETY: SSE2 is available on x86-64. Each block contains eight complete
    // pixels, so the channel order realigns after three unaligned vector loads.
    unsafe {
        let (r, g, b) = (r as i16, g as i16, b as i16);
        let references = [
            _mm_setr_epi16(r, g, b, r, g, b, r, g),
            _mm_setr_epi16(b, r, g, b, r, g, b, r),
            _mm_setr_epi16(g, b, r, g, b, r, g, b),
        ];
        let limit = _mm_set1_epi16(near as i16);
        let zero = _mm_setzero_si128();
        for (index, block) in blocks.iter().enumerate() {
            let mut mask = 0u64;
            for (i, (values, reference)) in
                block.as_chunks::<8>().0.iter().zip(references).enumerate()
            {
                let values = _mm_loadu_si128(values.as_ptr().cast());
                let matches = if near == 0 {
                    _mm_cmpeq_epi16(values, reference)
                } else {
                    let difference = _mm_or_si128(
                        _mm_subs_epu16(values, reference),
                        _mm_subs_epu16(reference, values),
                    );
                    _mm_cmpeq_epi16(_mm_subs_epu16(difference, limit), zero)
                };
                mask |= u64::from(_mm_movemask_epi8(matches) as u16) << (i * 16);
            }
            if mask != (1u64 << 48) - 1 {
                return index * 8 + mask.trailing_ones() as usize / 6;
            }
        }
    }
    blocks.len() * 8
        + tail
            .as_chunks::<3>()
            .0
            .iter()
            .take_while(|pixel| {
                pixel[0].abs_diff(r) <= near
                    && pixel[1].abs_diff(g) <= near
                    && pixel[2].abs_diff(b) <= near
            })
            .count()
}

fn encode_line_scan(
    c: ScanConfig,
    input: ScanInput<'_>,
    restart_interval: u32,
    output: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let (range, limit, qbpp) = golomb_params(&c);
    let quantizer = ErrorQuantizer::new(c.near, range);
    let gradients = GradientTable::for_scan(c.preset, c.near);
    let interval = u64::from(restart_interval);
    let mut restart_index = 0u8;
    let mut writer = BitWriter::from_bytes(output);
    let mut contexts = [RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_indexes = vec![0usize; c.components];
    let stride = c.width + 2;
    let mut previous = vec![0i32; stride * c.components];
    let mut current = vec![0i32; stride * c.components];
    let mut previous_left = vec![0i32; c.components];
    let mut component_samples = Vec::with_capacity(c.width);
    let row_len = c.width * c.components;
    let mut scratch = Vec::new();
    for y in 0..c.height {
        let input_row = input.row(y * row_len..(y + 1) * row_len, &mut scratch);
        // V1 = V2 = 1: one image line is one MCU, even for line interleave.
        if interval != 0 && y != 0 && (y as u64).is_multiple_of(interval) {
            writer = end_restart_interval(writer, &mut restart_index);
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                &mut run_indexes,
                &mut previous,
                &mut previous_left,
            );
        }
        for (component, run_index) in run_indexes.iter_mut().enumerate() {
            let start = component * stride;
            let row = &mut current[start + 1..start + 1 + c.width];
            let prior = &mut previous[start..start + stride];
            prior[0] = previous_left[component];
            prior[c.width + 1] = prior[c.width];
            component_samples.clear();
            component_samples.extend(input_row[component..].iter().step_by(c.components).copied());
            let samples = component_samples.as_chunks::<1>().0;
            let mut ra = prior[1];
            let mut horizontal = gradients.quantize(ra - prior[0]);
            let mut x = 0usize;
            let mut neighborhoods = prior.windows(3);
            while let Some(&[rc, rb, rd]) = neighborhoods.next() {
                let next = gradients.quantize(rd - rb);
                let qs = (next * 9 + horizontal) * 9 + gradients.quantize(rc - ra);
                horizontal = next;
                if qs == 0 {
                    let run = matching_run(&samples[x..], [ra as u16], c.near);
                    encode_run_length(&mut writer, run, run_index, run == c.width - x)?;
                    row[x..x + run].fill(ra);
                    x += run;
                    if x == c.width {
                        break;
                    }
                    ra = encode_run_interrupt(
                        &mut writer,
                        &mut run_contexts,
                        *run_index,
                        i32::from(samples[x][0]),
                        ra,
                        prior[x + 1],
                        &c,
                        quantizer,
                        limit,
                        qbpp,
                    );
                    row[x] = ra;
                    *run_index = run_index.saturating_sub(1);
                    x += 1;
                    if run != 0 {
                        horizontal = gradients.quantize(prior[x + 1] - prior[x]);
                        neighborhoods = prior[x..].windows(3);
                    }
                } else {
                    let idx = qs.unsigned_abs() as usize;
                    let sample = i32::from(samples[x][0]);
                    ra = encode_regular(
                        &mut writer,
                        &mut contexts[idx],
                        qs,
                        sample,
                        ra,
                        rb,
                        rc,
                        &c,
                        quantizer,
                        limit,
                        qbpp,
                    );
                    row[x] = ra;
                    x += 1;
                }
            }
            previous_left[component] = prior[1];
        }
        std::mem::swap(&mut previous, &mut current);
    }
    Ok(writer.finish())
}

pub(crate) fn decode_scan(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<u16>, String> {
    // Also reached through decoded scan configs; reject oversized buffers
    // before the sample vector is allocated (design §10).
    crate::checked_sample_count(c.width, c.height, c.components)?;
    if c.components == 0 || c.components > 3 || c.ilv > 2 {
        return Err("unsupported interleave mode".into());
    }
    if c.ilv == 0 && c.components != 1 {
        return Err("non-interleaved scan requires one component".into());
    }
    if c.ilv == 1 {
        return decode_line_scan(c, scan, restart_interval);
    }
    decode_sample_scan::<u16>(c, scan, restart_interval)
}

pub(crate) fn decode_byte_scan(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<u8>, String> {
    debug_assert!(c.maxval <= 255 && matches!(c.ilv, 0 | 2));
    decode_sample_scan::<u8>(c, scan, restart_interval)
}

pub(crate) fn decode_word_scan(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<u8>, String> {
    debug_assert!(matches!(c.ilv, 0 | 2));
    Ok(decode_sample_scan::<[u8; 2]>(c, scan, restart_interval)?.into_flattened())
}

trait OutputSample: Copy + Default {
    fn as_integer(self) -> i32;
    fn from_integer(value: i32) -> Self;
}

impl OutputSample for u16 {
    fn as_integer(self) -> i32 {
        i32::from(self)
    }

    fn from_integer(value: i32) -> Self {
        value as u16
    }
}

impl OutputSample for [u8; 2] {
    fn as_integer(self) -> i32 {
        i32::from(u16::from_le_bytes(self))
    }

    fn from_integer(value: i32) -> Self {
        (value as u16).to_le_bytes()
    }
}

impl OutputSample for u8 {
    fn as_integer(self) -> i32 {
        i32::from(self)
    }

    fn from_integer(value: i32) -> Self {
        debug_assert!((0..=255).contains(&value));
        value as u8
    }
}

fn decode_sample_scan<T: OutputSample>(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<T>, String> {
    let byte_lossless = is_byte_lossless(&c);
    match (c.components, c.near == 0, byte_lossless) {
        (1, true, true) => decode_interleaved::<T, 1, true, true>(c, scan, restart_interval),
        (3, true, true) => decode_interleaved::<T, 3, true, true>(c, scan, restart_interval),
        (1, true, false) => decode_interleaved::<T, 1, true, false>(c, scan, restart_interval),
        (1, false, _) => decode_interleaved::<T, 1, false, false>(c, scan, restart_interval),
        (2, _, _) => decode_interleaved::<T, 2, false, false>(c, scan, restart_interval),
        (3, true, false) => decode_interleaved::<T, 3, true, false>(c, scan, restart_interval),
        (3, false, _) => decode_interleaved::<T, 3, false, false>(c, scan, restart_interval),
        _ => unreachable!("validated component count"),
    }
}

fn is_byte_lossless(c: &ScanConfig) -> bool {
    c.near == 0 && c.maxval == 255 && c.preset.maxval == 255 && c.preset.reset == 64
}

fn decode_interleaved<
    T: OutputSample,
    const COMPONENTS: usize,
    const LOSSLESS: bool,
    const BYTE_LOSSLESS: bool,
>(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<T>, String> {
    if c.maxval != c.preset.maxval {
        return Err("JPEG-LS scan and preset MAXVAL differ".into());
    }
    let (range, limit, qbpp) = golomb_params(&c);
    // With no DRI, BitReader validates the entire scan as one entropy segment.
    let segments = if restart_interval == 0 {
        Vec::new()
    } else {
        split_restart_segments(
            scan,
            restart_interval,
            expected_segments(&c, restart_interval),
        )?
    };
    let gradients = GradientTable::for_scan(c.preset, c.near);
    let interval = u64::from(restart_interval);
    let mut segment_index = 0usize;
    let mut reader = BitReader::new(segments.first().copied().unwrap_or(scan));
    let mut contexts = [RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_index = 0usize;
    let stride = c.width + 2;
    let mut stack_lines = [[T::default(); COMPONENTS]; 32];
    let mut allocated_lines;
    let lines = if stride * 2 <= stack_lines.len() {
        &mut stack_lines[..stride * 2]
    } else {
        allocated_lines = vec![[T::default(); COMPONENTS]; stride * 2];
        &mut allocated_lines
    };
    let (mut previous, mut current) = lines.split_at_mut(stride);
    let mut previous_left = [T::default(); COMPONENTS];
    let mut output = Vec::with_capacity(c.width * c.height * COMPONENTS);
    for y in 0..c.height {
        // T.87 B.4/D.4: the MCU is one image line, so boundaries fall between
        // lines and the restart reset applies to the prediction neighbourhood.
        if interval != 0 && y != 0 && (y as u64).is_multiple_of(interval) {
            begin_restart_interval(&mut reader, &segments, &mut segment_index)?;
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                std::slice::from_mut(&mut run_index),
                previous.as_flattened_mut(),
                &mut previous_left,
            );
        }
        // T.87 A.2.1 defines the samples immediately outside both ends of each row.
        previous[0] = previous_left;
        previous[c.width + 1] = previous[c.width];
        current[0] = previous[1];
        let mut ra = current[0];
        let mut horizontal: [i32; COMPONENTS] = std::array::from_fn(|i| {
            gradients
                .quantize_reconstructed(previous[1][i].as_integer() - previous[0][i].as_integer())
        });
        let mut x = 0usize;
        let mut neighborhoods = previous.windows(3);
        while let Some(&[rc, rb, rd]) = neighborhoods.next() {
            let qs: [i32; COMPONENTS] = std::array::from_fn(|i| {
                let next =
                    gradients.quantize_reconstructed(rd[i].as_integer() - rb[i].as_integer());
                let context = (next * 9 + horizontal[i]) * 9
                    + gradients.quantize_reconstructed(rc[i].as_integer() - ra[i].as_integer());
                horizontal[i] = next;
                context
            });
            if qs.iter().all(|&q| q == 0) {
                let run = decode_run_length(&mut reader, c.width - x, &mut run_index)?;
                current[x + 1..x + run + 1].fill(ra);
                if run == c.width - x {
                    break;
                }
                let pos = x + run;
                let mut sample = [T::default(); COMPONENTS];
                for i in 0..COMPONENTS {
                    sample[i] = T::from_integer(decode_run_interrupt(
                        &mut reader,
                        &mut run_contexts,
                        run_index,
                        ra[i].as_integer(),
                        previous[pos + 1][i].as_integer(),
                        &c,
                        range,
                        limit,
                        qbpp,
                    )?);
                }
                current[pos + 1] = sample;
                ra = sample;
                run_index = run_index.saturating_sub(1);
                x = pos + 1;
                if run != 0 {
                    let _ = neighborhoods.nth(run - 1);
                    horizontal = std::array::from_fn(|i| {
                        gradients.quantize_reconstructed(
                            previous[x + 1][i].as_integer() - previous[x][i].as_integer(),
                        )
                    });
                }
            } else {
                // All components share the adaptive model and consume codes in
                // component order. Their predictions are independent.
                let predictions: [i32; COMPONENTS] = std::array::from_fn(|i| {
                    predict(ra[i].as_integer(), rb[i].as_integer(), rc[i].as_integer())
                });
                let mut sample = [T::default(); COMPONENTS];
                if COMPONENTS == 3 && LOSSLESS && qs[0] == qs[1] && qs[0] == qs[2] {
                    // The components update the same model in sequence. Keep
                    // its statistics local until all three have been decoded.
                    // SAFETY: Three signed base-nine digits give |qs| <= 364.
                    let stored =
                        unsafe { contexts.get_unchecked_mut(qs[0].unsigned_abs() as usize) };
                    if BYTE_LOSSLESS {
                        let rgb = decode_byte_rgb_shared(
                            &mut reader,
                            stored,
                            qs[0],
                            [predictions[0], predictions[1], predictions[2]],
                        )?;
                        sample[..3].copy_from_slice(&rgb);
                        // SAFETY: x indexes an existing three-sample window in the padded row.
                        unsafe { *current.get_unchecked_mut(x + 1) = sample };
                        ra = sample;
                        x += 1;
                        continue;
                    }
                    let mut context = *stored;
                    for i in 0..COMPONENTS {
                        sample[i] = T::from_integer(decode_regular_with_context(
                            &mut reader,
                            &mut context,
                            qs[0],
                            predictions[i],
                            &c,
                            range,
                            limit,
                            qbpp,
                        )?);
                    }
                    *stored = context;
                    // SAFETY: x indexes an existing three-sample window in the padded row.
                    unsafe { *current.get_unchecked_mut(x + 1) = sample };
                    ra = sample;
                    x += 1;
                    continue;
                }
                let mut decode_component = |i: usize| {
                    if BYTE_LOSSLESS {
                        return decode_lossless_byte_regular(
                            &mut reader,
                            &mut contexts,
                            qs[i],
                            predictions[i],
                        )
                        .map(T::from_integer);
                    }
                    decode_regular(
                        &mut reader,
                        &mut contexts,
                        qs[i],
                        predictions[i],
                        &c,
                        range,
                        limit,
                        qbpp,
                    )
                    .map(T::from_integer)
                };
                sample[0] = decode_component(0)?;
                if COMPONENTS >= 2 {
                    sample[1] = decode_component(1)?;
                }
                if COMPONENTS == 3 {
                    sample[2] = decode_component(2)?;
                }
                // SAFETY: x indexes an existing three-sample window in the padded row.
                unsafe { *current.get_unchecked_mut(x + 1) = sample };
                ra = sample;
                x += 1;
            }
        }
        output.extend_from_slice(current[1..1 + c.width].as_flattened());
        previous_left = previous[1];
        std::mem::swap(&mut previous, &mut current);
    }
    reader.finish()?;
    Ok(output)
}

/// Decodes one line of one component from the scan.
///
/// `previous` is the previous line of this component, `previous_left` the first
/// sample of the line before it, and `current` receives the decoded line; the
/// buffers are component-local so a sub-sampled scan can use planes of
/// different widths. Returns the updated run-mode index.
fn decode_component_line<T: OutputSample, const BYTE_LOSSLESS: bool>(
    reader: &mut BitReader<'_>,
    contexts: &mut [RegularContext; 365],
    run_contexts: &mut [RunContext; 2],
    run_index: usize,
    previous: &mut [T],
    previous_left: T,
    current: &mut [T],
    c: &ScanConfig,
    gradients: &GradientTable,
    range: i32,
    limit: u32,
    qbpp: u8,
) -> Result<usize, String> {
    let width = c.width;
    previous[0] = previous_left;
    previous[width + 1] = previous[width];
    let mut run_index = run_index;
    let mut x = 0usize;
    let mut ra = previous[1].as_integer();
    let mut horizontal = gradients.quantize_reconstructed(ra - previous_left.as_integer());
    let mut neighborhoods = previous.windows(3);
    while let Some(&[rc, rb, rd]) = neighborhoods.next() {
        let (rc, rb, rd) = (rc.as_integer(), rb.as_integer(), rd.as_integer());
        let next = gradients.quantize_reconstructed(rd - rb);
        let qs = (next * 9 + horizontal) * 9 + gradients.quantize_reconstructed(rc - ra);
        horizontal = next;
        if qs == 0 {
            let run = decode_run_length(reader, width - x, &mut run_index)?;
            current[x..x + run].fill(T::from_integer(ra));
            x += run;
            if x == width {
                break;
            }
            ra = decode_run_interrupt(
                reader,
                run_contexts,
                run_index,
                ra,
                previous[x + 1].as_integer(),
                c,
                range,
                limit,
                qbpp,
            )?;
            current[x] = T::from_integer(ra);
            run_index = run_index.saturating_sub(1);
            x += 1;
            if run != 0 {
                horizontal = gradients.quantize_reconstructed(
                    previous[x + 1].as_integer() - previous[x].as_integer(),
                );
                neighborhoods = previous[x..].windows(3);
            }
        } else {
            let prediction = predict(ra, rb, rc);
            ra = if BYTE_LOSSLESS {
                decode_lossless_byte_regular(reader, contexts, qs, prediction)?
            } else {
                decode_regular(reader, contexts, qs, prediction, c, range, limit, qbpp)?
            };
            // SAFETY: There are width windows in the padded row and width outputs.
            unsafe { *current.get_unchecked_mut(x) = T::from_integer(ra) };
            x += 1;
        }
    }
    Ok(run_index)
}

/// Decodes a line-interleaved scan (T.87 B.2.1) into one plane per component.
///
/// `geometry` holds `(columns, lines, vertical factor)` for every component in
/// scan order, so a sub-sampled frame decodes each plane on its own grid
/// (T.87 A.2.1: `columns = ceil(X*Hi/Hmax)`, `lines = ceil(Y*Vi/Vmax)`). One MCU
/// holds `V1` lines of C1, then `V2` lines of C2, and so on (T.87 B.4), which is
/// the unit the restart interval counts. The encoder pads the last MCU, so
/// lines past the component height are decoded and dropped.
pub(crate) fn decode_line_scan_planes(
    c: ScanConfig,
    geometry: &[(usize, usize, usize)],
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<Vec<u16>>, String> {
    if geometry.len() != c.components
        || geometry
            .iter()
            .any(|(width, height, vs)| *width == 0 || *height == 0 || *vs == 0)
    {
        return Err("invalid scan geometry".into());
    }
    let mut planes: Vec<Vec<u16>> = geometry
        .iter()
        .map(|(width, height, _)| vec![0u16; width * height])
        .collect();
    decode_line_rows(
        c,
        geometry,
        scan,
        restart_interval,
        |index, line, rows, component| {
            let row = &rows[component];
            let width = row.len();
            planes[index][line * width..(line + 1) * width].copy_from_slice(row);
        },
    )?;
    Ok(planes)
}

pub(crate) fn decode_rgb_byte_lines(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<u8>, String> {
    let geometry = [(c.width, c.height, 1); 3];
    let stride = c.width * 3;
    let mut output = vec![0; stride * c.height];
    decode_line_rows(
        c,
        &geometry,
        scan,
        restart_interval,
        |index, line, rows, _| {
            if index == 2 {
                // The last component completes this row for all three components.
                let padded = c.width + 2;
                let red = &rows[1..1 + c.width];
                let green = &rows[padded + 1..padded + 1 + c.width];
                let blue = &rows[2 * padded + 1..2 * padded + 1 + c.width];
                let pixels = output[line * stride..(line + 1) * stride]
                    .as_chunks_mut::<3>()
                    .0;
                for (((pixel, &r), &g), &b) in pixels.iter_mut().zip(red).zip(green).zip(blue) {
                    *pixel = [r, g, b];
                }
            }
        },
    )?;
    Ok(output)
}

fn decode_line_rows<T: OutputSample>(
    c: ScanConfig,
    geometry: &[(usize, usize, usize)],
    scan: &[u8],
    restart_interval: u32,
    write_row: impl FnMut(usize, usize, &[T], std::ops::Range<usize>),
) -> Result<(), String> {
    if is_byte_lossless(&c) {
        decode_line_rows_impl::<T, true>(c, geometry, scan, restart_interval, write_row)
    } else {
        decode_line_rows_impl::<T, false>(c, geometry, scan, restart_interval, write_row)
    }
}

fn decode_line_rows_impl<T: OutputSample, const BYTE_LOSSLESS: bool>(
    c: ScanConfig,
    geometry: &[(usize, usize, usize)],
    scan: &[u8],
    restart_interval: u32,
    mut write_row: impl FnMut(usize, usize, &[T], std::ops::Range<usize>),
) -> Result<(), String> {
    let (range, limit, qbpp) = golomb_params(&c);
    let gradients = GradientTable::for_scan(c.preset, c.near);
    let rounds = geometry
        .iter()
        .map(|(_, height, vs)| height.div_ceil(*vs))
        .max()
        .unwrap_or(0);
    let segments = if restart_interval == 0 {
        Vec::new()
    } else {
        let expected = expected_segments_for(rounds.max(1) as u64, restart_interval);
        split_restart_segments(scan, restart_interval, expected)?
    };
    let interval = u64::from(restart_interval);
    let mut segment_index = 0usize;
    let mut reader = BitReader::new(segments.first().copied().unwrap_or(scan));
    let mut contexts = [RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_indexes = vec![0usize; c.components];
    let mut offsets = Vec::with_capacity(c.components);
    let mut total = 0usize;
    for (width, _, _) in geometry {
        offsets.push(total);
        total += *width + 2;
    }
    let mut even = vec![T::default(); total];
    let mut odd = vec![T::default(); total];
    let mut previous_left = vec![T::default(); c.components];
    for round in 0..rounds {
        if interval != 0 && round != 0 && (round as u64).is_multiple_of(interval) {
            begin_restart_interval(&mut reader, &segments, &mut segment_index)?;
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                &mut run_indexes,
                &mut even,
                &mut previous_left,
            );
            odd.fill(T::default());
        }
        for (index, &(width, height, vs)) in geometry.iter().enumerate() {
            let offset = offsets[index];
            let component = ScanConfig {
                width,
                height,
                components: 1,
                ..c
            };
            for k in 0..vs {
                let line = round * vs + k;
                let left = previous_left[index];
                let even_row = &mut even[offset..offset + width + 2];
                let odd_row = &mut odd[offset..offset + width + 2];
                let (previous, current) = if line % 2 == 0 {
                    (even_row, odd_row)
                } else {
                    (odd_row, even_row)
                };
                let run_index = decode_component_line::<T, BYTE_LOSSLESS>(
                    &mut reader,
                    &mut contexts,
                    &mut run_contexts,
                    run_indexes[index],
                    previous,
                    left,
                    &mut current[1..=width],
                    &component,
                    &gradients,
                    range,
                    limit,
                    qbpp,
                )?;
                run_indexes[index] = run_index;
                previous_left[index] = previous[1];
                if line < height {
                    let rows = if line % 2 == 0 { &odd } else { &even };
                    write_row(index, line, rows, offset + 1..offset + width + 1);
                }
            }
        }
    }
    reader.finish()
}

fn decode_line_scan(c: ScanConfig, scan: &[u8], restart_interval: u32) -> Result<Vec<u16>, String> {
    let geometry: Vec<(usize, usize, usize)> =
        (0..c.components).map(|_| (c.width, c.height, 1)).collect();
    let planes = decode_line_scan_planes(c, &geometry, scan, restart_interval)?;
    let mut output = vec![0u16; c.width * c.height * c.components];
    for (index, plane) in planes.iter().enumerate() {
        for (sample, value) in plane.iter().enumerate() {
            output[sample * c.components + index] = *value;
        }
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_runs_stop_at_the_first_out_of_range_component() {
        let reference = [0, 127, u16::MAX];
        for near in [0, 1, 3] {
            let matched = [near, 127 + near, u16::MAX - near];
            let samples = [matched; 25];
            assert_eq!(matching_run(&samples, reference, near), samples.len());
            for index in 0..samples.len() {
                for component in 0..3 {
                    let mut interrupted = samples;
                    interrupted[index][component] = reference[component] ^ u16::MAX;
                    assert_eq!(matching_run(&interrupted, reference, near), index);
                }
            }
        }
    }

    #[test]
    fn reciprocal_quantization_matches_integer_division() {
        for near in 0u16..=127 {
            let range = Preset::defaults(255, near).unwrap().range(near) as i32;
            let quantizer = ErrorQuantizer::new(near, range);
            let near = i32::from(near);
            for error in -255i32..=255 {
                let magnitude = (error.abs() + near) / (2 * near + 1);
                let signed = magnitude * error.signum();
                let expected = (signed + range / 2).rem_euclid(range) - range / 2;
                assert_eq!(quantizer.quantize(error), expected);
            }
        }
    }

    #[test]
    fn gradient_table_matches_thresholds_across_the_entire_sample_range() {
        use crate::run::quantize;

        for (preset, near) in [
            (Preset::defaults(255, 0).unwrap(), 0),
            (Preset::defaults(65535, 3).unwrap(), 3),
            (Preset::new(63, 5, 5, 63, 64).unwrap(), 4),
        ] {
            let table = GradientTable::new(preset, near);
            for d in -i32::from(preset.maxval)..=i32::from(preset.maxval) {
                assert_eq!(
                    context_id(0, 0, 0, d, &table),
                    quantize(
                        d,
                        preset.t1.into(),
                        preset.t2.into(),
                        preset.t3.into(),
                        near.into()
                    ) * 81,
                );
            }
        }
    }

    #[test]
    fn gradient_quantization_addresses_all_regular_contexts() {
        let preset = Preset::defaults(255, 0).unwrap();
        let table = GradientTable::new(preset, 0);
        let gradients = [-30, -10, -4, -1, 0, 1, 4, 10, 30];
        let mut visited = [false; 365];
        for d1 in gradients {
            for d2 in gradients {
                for d3 in gradients {
                    let rb = 128;
                    let rc = rb - d2;
                    let ra = rc - d3;
                    let rd = rb + d1;
                    visited[context_id(ra, rb, rc, rd, &table).unsigned_abs() as usize] = true;
                }
            }
        }
        assert!(visited.iter().all(|value| *value));
    }

    #[test]
    fn round_trips_small_mono_and_rgb_images() {
        for &(components, width, height) in &[(1, 5, 3), (3, 4, 2)] {
            let maxval = 255;
            let preset = Preset::defaults(maxval, 0).unwrap();
            let values: Vec<u16> = (0..width * height * components)
                .map(|i| ((i * 37 + i / 3) % 256) as u16)
                .collect();
            let ilv = if components == 1 { 0 } else { 2 };
            let cfg = ScanConfig {
                width,
                height,
                components,
                maxval,
                near: 0,
                ilv,
                preset,
            };
            let encoded = encode_scan(cfg, &values).unwrap();
            let decoded = decode_scan(cfg, &encoded, 0).unwrap();
            assert_eq!(decoded, values);
        }
    }
    #[test]
    fn near_lossless_stays_within_bound() {
        let preset = Preset::defaults(255, 3).unwrap();
        let values: Vec<u16> = (0..48).map(|i| ((i * 19) % 256) as u16).collect();
        let cfg = ScanConfig {
            width: 8,
            height: 6,
            components: 1,
            maxval: 255,
            near: 3,
            ilv: 0,
            preset,
        };
        let decoded = decode_scan(cfg, &encode_scan(cfg, &values).unwrap(), 0).unwrap();

        let max = decoded
            .iter()
            .zip(values.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(max <= 3);
    }

    #[test]
    fn decoder_rejects_trailing_entropy_data_but_tolerates_zero_padding() {
        let preset = Preset::defaults(255, 0).unwrap();
        let cfg = ScanConfig {
            width: 1,
            height: 1,
            components: 1,
            maxval: 255,
            near: 0,
            ilv: 0,
            preset,
        };
        let values = [7u16];
        // A non-zero byte after the expected samples is undecoded entropy.
        let mut encoded = encode_scan(cfg, &values).unwrap();
        encoded.push(1);
        assert!(decode_scan(cfg, &encoded, 0).is_err());
        // A zero byte is the padding fo-dicom/CharLS leave before the marker.
        let mut padded = encode_scan(cfg, &values).unwrap();
        padded.push(0);
        assert_eq!(decode_scan(cfg, &padded, 0).unwrap(), values);
    }

    #[test]
    fn round_trips_sixteen_bit_samples() {
        let preset = Preset::defaults(u16::MAX, 0).unwrap();
        let values: Vec<u16> = (0..35)
            .map(|i| ((i * 8191 + i * i * 17) & 0xffff) as u16)
            .collect();
        let cfg = ScanConfig {
            width: 7,
            height: 5,
            components: 1,
            maxval: u16::MAX,
            near: 0,
            ilv: 0,
            preset,
        };
        assert_eq!(
            decode_scan(cfg, &encode_scan(cfg, &values).unwrap(), 0).unwrap(),
            values
        );
    }

    #[test]
    fn round_trips_line_interleaved_color_samples() {
        let preset = Preset::defaults(255, 0).unwrap();
        let values: Vec<u16> = (0..36).map(|i| ((i * 29) % 256) as u16).collect();
        let cfg = ScanConfig {
            width: 4,
            height: 3,
            components: 3,
            maxval: 255,
            near: 0,
            ilv: 1,
            preset,
        };
        assert_eq!(
            decode_scan(cfg, &encode_scan(cfg, &values).unwrap(), 0).unwrap(),
            values
        );
    }

    #[test]
    fn round_trips_run_interruptions_with_custom_reset() {
        let preset = Preset::new(255, 3, 7, 21, 8).unwrap();
        let width = 32;
        let height = 12;
        let values: Vec<u16> = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    if x + 1 == width {
                        ((y * 17) % 256) as u16
                    } else {
                        ((y * 17) % 2) as u16
                    }
                })
            })
            .collect();
        for (components, ilv) in [(1, 0), (3, 1), (3, 2)] {
            let samples: Vec<u16> = values
                .iter()
                .flat_map(|&value| std::iter::repeat_n(value, components))
                .collect();
            let cfg = ScanConfig {
                width,
                height,
                components,
                maxval: 255,
                near: 0,
                ilv,
                preset,
            };
            assert_eq!(
                decode_scan(cfg, &encode_scan(cfg, &samples).unwrap(), 0).unwrap(),
                samples,
                "components={components}, ilv={ilv}"
            );
        }
    }

    #[test]
    fn rejects_prediction_errors_outside_the_sample_range() {
        let cfg = ScanConfig {
            width: 1,
            height: 1,
            components: 1,
            maxval: 3,
            near: 0,
            ilv: 0,
            preset: Preset::defaults(3, 0).unwrap(),
        };
        let mut writer = BitWriter::new();
        writer.write_bit(false); // Zero-length run, followed by an interruption.
        encode_mapped(&mut writer, 1, 8, 19, 2).unwrap();
        assert!(decode_scan(cfg, &writer.finish(), 0).is_err());
    }

    fn restart_config(components: usize, ilv: u8, width: usize, height: usize) -> ScanConfig {
        ScanConfig {
            width,
            height,
            components,
            maxval: 255,
            near: 0,
            ilv,
            preset: Preset::defaults(255, 0).unwrap(),
        }
    }

    fn restart_markers(scan: &[u8]) -> usize {
        scan.windows(2)
            .filter(|w| w[0] == 0xff && (0xd0..=0xd7).contains(&w[1]))
            .count()
    }

    #[test]
    fn restarts_round_trip_in_every_interleave_mode() {
        for (components, ilv) in [(1usize, 0u8), (3, 1), (3, 2)] {
            let cfg = restart_config(components, ilv, 5, 5);
            let values: Vec<u16> = (0..5 * 5 * components)
                .map(|i| ((i * 53 + i / 3) % 256) as u16)
                .collect();
            // intervals 1 and 2 divide the height; 3 leaves a short final group.
            for interval in [1u32, 2, 3] {
                let encoded = encode_restart_scan(cfg, &values, interval).unwrap();
                assert_eq!(
                    restart_markers(&encoded),
                    (cfg.height - 1) / interval as usize,
                    "components={components} ilv={ilv} interval={interval}"
                );
                assert_eq!(
                    decode_scan(cfg, &encoded, interval).unwrap(),
                    values,
                    "components={components}, ilv={ilv}, interval={interval}"
                );
            }
        }
    }

    #[test]
    fn restarts_reset_the_prediction_neighbourhood() {
        // The same image with and without restarts: the codestreams differ
        // because every interval re-initialises the model and neighbours, yet
        // both must reconstruct the original samples.
        let cfg = restart_config(1, 0, 6, 4);
        let values: Vec<u16> = (0..24).map(|i| ((i * 41 + 3) % 256) as u16).collect();
        let plain = encode_scan(cfg, &values).unwrap();
        let restarted = encode_restart_scan(cfg, &values, 1).unwrap();
        assert_ne!(plain, restarted);
        assert_eq!(restart_markers(&restarted), 3);
        assert_eq!(decode_scan(cfg, &plain, 0).unwrap(), values);
        assert_eq!(decode_scan(cfg, &restarted, 1).unwrap(), values);

        // Flat rows exercise one run per line, so each interval starts a fresh
        // run from a zeroed neighbourhood.
        let flat = restart_config(1, 0, 6, 4);
        for value in [0u16, 9] {
            let values = vec![value; 24];
            for interval in [1u32, 2, 3] {
                let encoded = encode_restart_scan(flat, &values, interval).unwrap();
                assert_eq!(restart_markers(&encoded), 3 / interval as usize);
                assert_eq!(
                    decode_scan(flat, &encoded, interval).unwrap(),
                    values,
                    "value={value} interval={interval}"
                );
            }
        }
    }

    #[test]
    fn near_lossless_restarts_stay_within_the_bound() {
        let preset = Preset::defaults(255, 3).unwrap();
        let values: Vec<u16> = (0..8 * 4).map(|i| ((i * 19) % 256) as u16).collect();
        let cfg = ScanConfig {
            width: 8,
            height: 4,
            components: 1,
            maxval: 255,
            near: 3,
            ilv: 0,
            preset,
        };
        for interval in [1u32, 2, 3] {
            let encoded = encode_restart_scan(cfg, &values, interval).unwrap();
            let decoded = decode_scan(cfg, &encoded, interval).unwrap();
            for (actual, source) in decoded.iter().zip(&values) {
                assert!(
                    (i32::from(*actual) - i32::from(*source)).abs() <= 3,
                    "interval={interval}"
                );
            }
        }
    }

    #[test]
    fn rejects_bad_restart_marker_sequences_and_counts() {
        let cfg = restart_config(1, 0, 4, 2);
        let values: Vec<u16> = (0..8).map(|i| (i * 37) as u16).collect();
        let encoded = encode_restart_scan(cfg, &values, 1).unwrap();
        let marker = encoded
            .windows(2)
            .position(|w| w[0] == 0xff && w[1] == 0xd0)
            .expect("interval 1 writes RST0");

        // Out-of-order RSTm index.
        let mut wrong = encoded.clone();
        wrong[marker + 1] = 0xd3;
        assert!(decode_scan(cfg, &wrong, 1)
            .unwrap_err()
            .contains("expected ffd0"));

        // A missing restart marker must not be silently accepted.
        let mut missing = encoded.clone();
        missing.drain(marker..marker + 2);
        assert!(decode_scan(cfg, &missing, 1).is_err());

        // A marker after the last interval is unexpected.
        let mut extra = encoded.clone();
        extra.extend([0xff, 0xd1]);
        assert!(decode_scan(cfg, &extra, 1)
            .unwrap_err()
            .contains("too many restart markers for the restart interval"));

        // RSTm is illegal without a DRI restart interval.
        assert!(decode_scan(cfg, &encoded, 0).is_err());
        let mut trailing_restart = encode_scan(cfg, &values).unwrap();
        trailing_restart.extend([0xff, 0xd0]);
        assert!(decode_scan(cfg, &trailing_restart, 0).is_err());
    }

    #[test]
    fn rejects_restart_marker_inside_a_line() {
        // Restart intervals count MCUs (lines), so a marker that splits a line
        // is invalid and its segment is too short to finish that line.
        let cfg = restart_config(1, 0, 16, 4);
        let values: Vec<u16> = (0..64).map(|i| ((i * 29 + 11) % 256) as u16).collect();
        let encoded = encode_restart_scan(cfg, &values, 1).unwrap();
        let marker = encoded
            .windows(2)
            .position(|w| w[0] == 0xff && w[1] == 0xd0)
            .expect("interval 1 writes RST0");
        assert!(marker >= 3, "row 0 should code more than one byte");

        let at = 2usize;
        let mut moved = Vec::with_capacity(encoded.len());
        moved.extend_from_slice(&encoded[..at]);
        moved.extend_from_slice(&[0xff, 0xd0]);
        moved.extend_from_slice(&encoded[at..marker]);
        moved.extend_from_slice(&encoded[marker + 2..]);
        // Same marker count, still RST0..RST2, but the first boundary is now
        // inside row 0.
        assert_eq!(restart_markers(&moved), restart_markers(&encoded));
        assert!(decode_scan(cfg, &moved, 1).is_err());
    }
}
