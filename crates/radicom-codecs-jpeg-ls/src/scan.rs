#![allow(clippy::too_many_arguments, clippy::if_same_then_else)]
use crate::{
    bit::{BitReader, BitWriter},
    context::{RegularContext, RunContext},
    golomb::{decode_mapped, encode_mapped},
    marker::split_restart_segments,
    preset::Preset,
    run::{map_error, predict, quantize, unmap_error, J},
};

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

fn quantized_error(value: i32, near: i32, range: i32) -> i32 {
    let quantized = if near == 0 {
        value
    } else if value >= 0 {
        (value + near) / (2 * near + 1)
    } else {
        -((near - value) / (2 * near + 1))
    };
    let mut e = quantized.rem_euclid(range);
    if e >= (range + 1) / 2 {
        e -= range;
    }
    e
}

fn reconstructed(pred: i32, e: i32, sign: i32, near: i32, maxval: i32) -> i32 {
    if near == 0 {
        (pred + sign * e).rem_euclid(maxval + 1)
    } else {
        let mut value = pred + sign * e * (2 * near + 1);
        let range = (maxval + 2 * near) / (2 * near + 1) + 1;
        if value < -near {
            value += range * (2 * near + 1);
        } else if value > maxval + near {
            value -= range * (2 * near + 1);
        }
        correct_prediction(value, maxval)
    }
}

fn correct_prediction(pred: i32, maxval: i32) -> i32 {
    pred.clamp(0, maxval)
}

fn context_id(ra: i32, rb: i32, rc: i32, rd: i32, p: &Preset, near: i32) -> i32 {
    let q1 = quantize(rd - rb, p.t1 as i32, p.t2 as i32, p.t3 as i32, near);
    let q2 = quantize(rb - rc, p.t1 as i32, p.t2 as i32, p.t3 as i32, near);
    let q3 = quantize(rc - ra, p.t1 as i32, p.t2 as i32, p.t3 as i32, near);
    (q1 * 9 + q2) * 9 + q3
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
fn reset_restart_state(
    range: i32,
    contexts: &mut [RegularContext],
    run_contexts: &mut [RunContext; 2],
    run_indexes: &mut [usize],
    previous: &mut [i32],
    previous_left: &mut [i32],
) {
    contexts.fill(RegularContext::new(range));
    run_contexts[0] = RunContext::new(0, range);
    run_contexts[1] = RunContext::new(1, range);
    run_indexes.fill(0);
    previous.fill(0);
    previous_left.fill(0);
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
fn end_restart_interval(output: &mut Vec<u8>, writer: BitWriter, index: &mut u8) -> BitWriter {
    output.extend(writer.finish());
    output.extend([0xff, 0xd0 + *index]);
    *index = (*index + 1) & 7;
    BitWriter::new()
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

fn encode_regular(
    w: &mut BitWriter,
    ctx: &mut RegularContext,
    x: i32,
    ra: i32,
    rb: i32,
    rc: i32,
    rd: i32,
    c: &ScanConfig,
    range: i32,
    limit: u32,
    qbpp: u8,
) -> i32 {
    let qs = context_id(ra, rb, rc, rd, &c.preset, c.near as i32);
    let sign = if qs < 0 { -1 } else { 1 };
    let corrected = correct_prediction(predict(ra, rb, rc) + sign * ctx.bias(), c.maxval as i32);
    let error = quantized_error((x - corrected) * sign, c.near as i32, range);
    let k = ctx.k();
    let correction = if c.near == 0 { ctx.correction(k) } else { 0 };
    let mapped = map_error(error ^ correction);
    encode_mapped(w, k, mapped, limit, qbpp).expect("bounded Golomb parameters");
    ctx.update(error, c.near as i32, c.preset.reset as i32);
    reconstructed(corrected, error, sign, c.near as i32, c.maxval as i32)
}

fn decode_regular(
    r: &mut BitReader<'_>,
    ctx: &mut RegularContext,
    qs: i32,
    ra: i32,
    rb: i32,
    rc: i32,
    c: &ScanConfig,
    limit: u32,
    qbpp: u8,
) -> Result<i32, String> {
    let sign = if qs < 0 { -1 } else { 1 };
    let corrected = correct_prediction(predict(ra, rb, rc) + sign * ctx.bias(), c.maxval as i32);
    let k = ctx.k();
    let mapped = decode_mapped(r, k, limit, qbpp)?;
    let range = c.preset.range(c.near);
    if mapped > range {
        return Err("mapped prediction error exceeds RANGE".into());
    }
    let mut error = unmap_error(mapped);
    if k == 0 && c.near == 0 {
        error ^= ctx.correction(k);
    }
    if error < -(range as i32 / 2) || error > (range as i32 - 1) / 2 {
        return Err("prediction error exceeds RANGE".into());
    }
    ctx.update(error, c.near as i32, c.preset.reset as i32);
    Ok(reconstructed(
        corrected,
        error,
        sign,
        c.near as i32,
        c.maxval as i32,
    ))
}

fn encode_run_interrupt(
    w: &mut BitWriter,
    contexts: &mut [RunContext; 2],
    run_index: usize,
    x: i32,
    ra: i32,
    rb: i32,
    c: &ScanConfig,
    range: i32,
    limit: u32,
    qbpp: u8,
) -> i32 {
    let near = c.near as i32;
    let (typ, error) = if (c.components == 1 || c.ilv == 1) && (ra - rb).abs() <= near {
        (1usize, quantized_error(x - ra, near, range))
    } else {
        let sign = if rb >= ra { 1 } else { -1 };
        (0usize, quantized_error((x - rb) * sign, near, range))
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
    reconstructed(reference, error, sign, near, c.maxval as i32)
}

fn decode_run_interrupt(
    r: &mut BitReader<'_>,
    contexts: &mut [RunContext; 2],
    run_index: usize,
    ra: i32,
    rb: i32,
    c: &ScanConfig,
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
    let range = c.preset.range(c.near) as i32;
    if mapped > range {
        return Err("mapped run interruption error exceeds RANGE".into());
    }
    let error = contexts[typ].decode_value(mapped + typ as u32 as i32, k, c.preset.reset as i32);
    if error < -(range / 2) || error > (range - 1) / 2 {
        return Err("run interruption error exceeds RANGE".into());
    }
    Ok(reconstructed(reference, error, sign, near, c.maxval as i32))
}

fn encode_run_length(
    w: &mut BitWriter,
    mut run: usize,
    run_index: &mut usize,
    end_of_line: bool,
) -> Result<(), String> {
    while run >= (1usize << J[*run_index]) {
        w.write_bit(true);
        run -= 1usize << J[*run_index];
        *run_index = (*run_index + 1).min(31);
    }
    if end_of_line {
        if run != 0 {
            w.write_bit(true);
        }
    } else {
        w.write_bits(run as u32, J[*run_index] + 1)?;
    }
    Ok(())
}

/// Test-only shorthand for a scan without restart markers.
#[cfg(test)]
pub(crate) fn encode_scan(c: ScanConfig, input: &[u16]) -> Result<Vec<u8>, String> {
    encode_scan_inner(c, input, 0)
}

/// Encodes one scan, emitting an `RSTm` marker every `restart_interval` MCUs
/// (image lines, T.87 B.4); `0` emits none.
pub(crate) fn encode_restart_scan(
    c: ScanConfig,
    input: &[u16],
    restart_interval: u32,
) -> Result<Vec<u8>, String> {
    encode_scan_inner(c, input, restart_interval)
}

fn encode_scan_inner(
    c: ScanConfig,
    input: &[u16],
    restart_interval: u32,
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
        return encode_line_scan(c, input, restart_interval);
    }
    let (range, limit, qbpp) = golomb_params(&c);
    let interval = u64::from(restart_interval);
    let mut restart_index = 0u8;
    let mut output = Vec::new();
    let mut writer = BitWriter::new();
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_index = 0usize;
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    for y in 0..c.height {
        // T.87 B.4/D.4: an MCU is one image line, so a restart boundary always
        // falls between lines and never inside a run.
        if interval != 0 && y != 0 && (y as u64).is_multiple_of(interval) {
            writer = end_restart_interval(&mut output, writer, &mut restart_index);
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                std::slice::from_mut(&mut run_index),
                &mut previous,
                &mut previous_left,
            );
        }
        let mut current = vec![0i32; c.width * c.components];
        let mut x = 0usize;
        while x < c.width {
            let mut ra = vec![0i32; c.components];
            let mut rb = vec![0i32; c.components];
            let mut rc = vec![0i32; c.components];
            let mut rd = vec![0i32; c.components];
            for i in 0..c.components {
                ra[i] = if x == 0 {
                    if y == 0 {
                        0
                    } else {
                        previous[i]
                    }
                } else {
                    current[(x - 1) * c.components + i]
                };
                rb[i] = previous[x * c.components + i];
                rc[i] = if x == 0 {
                    if y == 0 {
                        0
                    } else {
                        previous_left[i]
                    }
                } else {
                    previous[(x - 1) * c.components + i]
                };
                rd[i] = if x + 1 == c.width {
                    rb[i]
                } else {
                    previous[(x + 1) * c.components + i]
                };
            }
            let qs: Vec<i32> = (0..c.components)
                .map(|i| context_id(ra[i], rb[i], rc[i], rd[i], &c.preset, c.near as i32))
                .collect();
            let base = (y * c.width + x) * c.components;
            if qs.iter().all(|&q| q == 0) {
                let room = c.width - x;
                let mut run = 0usize;
                while run < room {
                    let sample = (y * c.width + x + run) * c.components;
                    if (0..c.components)
                        .any(|i| (input[sample + i] as i32 - ra[i]).abs() > c.near as i32)
                    {
                        break;
                    }
                    run += 1;
                }
                encode_run_length(&mut writer, run, &mut run_index, run == room)?;
                for j in 0..run {
                    for i in 0..c.components {
                        current[(x + j) * c.components + i] = ra[i];
                    }
                }
                if run == room {
                    x += run;
                } else {
                    let pos = x + run;
                    let sample = (y * c.width + pos) * c.components;
                    let prev_at = (0..c.components)
                        .map(|i| previous[pos * c.components + i])
                        .collect::<Vec<_>>();
                    for i in 0..c.components {
                        current[pos * c.components + i] = encode_run_interrupt(
                            &mut writer,
                            &mut run_contexts,
                            run_index,
                            input[sample + i] as i32,
                            ra[i],
                            prev_at[i],
                            &c,
                            range,
                            limit,
                            qbpp,
                        );
                    }
                    run_index = run_index.saturating_sub(1);
                    x = pos + 1;
                }
            } else {
                for i in 0..c.components {
                    let idx = qs[i].unsigned_abs() as usize;
                    let sample = input[base + i] as i32;
                    current[x * c.components + i] = encode_regular(
                        &mut writer,
                        &mut contexts[idx],
                        sample,
                        ra[i],
                        rb[i],
                        rc[i],
                        rd[i],
                        &c,
                        range,
                        limit,
                        qbpp,
                    );
                }
                x += 1;
            }
        }
        previous_left.copy_from_slice(&previous[..c.components]);
        previous = current;
    }
    output.extend(writer.finish());
    Ok(output)
}

fn encode_line_scan(
    c: ScanConfig,
    input: &[u16],
    restart_interval: u32,
) -> Result<Vec<u8>, String> {
    let (range, limit, qbpp) = golomb_params(&c);
    let interval = u64::from(restart_interval);
    let mut restart_index = 0u8;
    let mut output = Vec::new();
    let mut writer = BitWriter::new();
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_indexes = vec![0usize; c.components];
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    for y in 0..c.height {
        // V1 = V2 = 1: one image line is one MCU, even for line interleave.
        if interval != 0 && y != 0 && (y as u64).is_multiple_of(interval) {
            writer = end_restart_interval(&mut output, writer, &mut restart_index);
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                &mut run_indexes,
                &mut previous,
                &mut previous_left,
            );
        }
        let mut current = vec![0i32; c.width * c.components];
        for component in 0..c.components {
            let mut run_index = run_indexes[component];
            let mut x = 0usize;
            while x < c.width {
                let at = component * c.width + x;
                let ra = if x == 0 {
                    if y != 0 {
                        previous[component * c.width]
                    } else {
                        0
                    }
                } else {
                    current[component * c.width + x - 1]
                };
                let rb = previous[at];
                let rc = if x == 0 {
                    if y != 0 {
                        previous_left[component]
                    } else {
                        0
                    }
                } else {
                    previous[component * c.width + x - 1]
                };
                let rd = if x + 1 == c.width {
                    rb
                } else {
                    previous[at + 1]
                };
                let qs = context_id(ra, rb, rc, rd, &c.preset, c.near as i32);
                if qs == 0 {
                    let mut run = 0usize;
                    while x + run < c.width {
                        let sample = (y * c.width + x + run) * c.components + component;
                        if (input[sample] as i32 - ra).abs() > c.near as i32 {
                            break;
                        }
                        run += 1;
                    }
                    encode_run_length(&mut writer, run, &mut run_index, run == c.width - x)?;
                    for j in 0..run {
                        current[component * c.width + x + j] = ra;
                    }
                    if run < c.width - x {
                        let pos = x + run;
                        let sample = (y * c.width + pos) * c.components + component;
                        let previous_at = previous[component * c.width + pos];
                        current[component * c.width + pos] = encode_run_interrupt(
                            &mut writer,
                            &mut run_contexts,
                            run_index,
                            input[sample] as i32,
                            ra,
                            previous_at,
                            &c,
                            range,
                            limit,
                            qbpp,
                        );
                        run_index = run_index.saturating_sub(1);
                        x = pos + 1;
                    } else {
                        x += run;
                    }
                } else {
                    let idx = qs.unsigned_abs() as usize;
                    let sample = input[(y * c.width + x) * c.components + component] as i32;
                    current[at] = encode_regular(
                        &mut writer,
                        &mut contexts[idx],
                        sample,
                        ra,
                        rb,
                        rc,
                        rd,
                        &c,
                        range,
                        limit,
                        qbpp,
                    );
                    x += 1;
                }
            }
            run_indexes[component] = run_index;
        }
        for component in 0..c.components {
            previous_left[component] = previous[component * c.width];
        }
        previous = current;
    }
    output.extend(writer.finish());
    Ok(output)
}

pub(crate) fn decode_scan(
    c: ScanConfig,
    scan: &[u8],
    restart_interval: u32,
) -> Result<Vec<u16>, String> {
    if c.components == 0 || c.components > 3 || c.ilv > 2 {
        return Err("unsupported interleave mode".into());
    }
    if c.ilv == 0 && c.components != 1 {
        return Err("non-interleaved scan requires one component".into());
    }
    if c.ilv == 1 {
        return decode_line_scan(c, scan, restart_interval);
    }
    let (range, limit, qbpp) = golomb_params(&c);
    let expected = expected_segments(&c, restart_interval);
    let segments = split_restart_segments(scan, restart_interval, expected)?;
    let interval = u64::from(restart_interval);
    let mut segment_index = 0usize;
    let mut reader = BitReader::new(segments[0]);
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_index = 0usize;
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    let mut output = vec![0u16; c.width * c.height * c.components];
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
                &mut previous,
                &mut previous_left,
            );
        }
        let mut current = vec![0i32; c.width * c.components];
        let mut x = 0usize;
        while x < c.width {
            let mut ra = vec![0i32; c.components];
            let mut rb = vec![0i32; c.components];
            let mut rc = vec![0i32; c.components];
            let mut rd = vec![0i32; c.components];
            for i in 0..c.components {
                ra[i] = if x == 0 {
                    if y == 0 {
                        0
                    } else {
                        previous[i]
                    }
                } else {
                    current[(x - 1) * c.components + i]
                };
                rb[i] = previous[x * c.components + i];
                rc[i] = if x == 0 {
                    if y == 0 {
                        0
                    } else {
                        previous_left[i]
                    }
                } else {
                    previous[(x - 1) * c.components + i]
                };
                rd[i] = if x + 1 == c.width {
                    rb[i]
                } else {
                    previous[(x + 1) * c.components + i]
                };
            }
            let qs: Vec<i32> = (0..c.components)
                .map(|i| context_id(ra[i], rb[i], rc[i], rd[i], &c.preset, c.near as i32))
                .collect();
            if qs.iter().all(|&q| q == 0) {
                let mut run = 0usize;
                while x + run < c.width && reader.read_bit()? {
                    let chunk = 1usize << J[run_index];
                    let count = chunk.min(c.width - x - run);
                    run += count;
                    if count == chunk {
                        run_index = (run_index + 1).min(31);
                    }
                    if run == c.width - x {
                        break;
                    }
                }
                if run < c.width - x {
                    let remainder = if J[run_index] == 0 {
                        0
                    } else {
                        reader.read_bits(J[run_index])? as usize
                    };
                    run += remainder;
                }
                if run > c.width - x {
                    return Err("run exceeds line".into());
                }
                for j in 0..run {
                    for i in 0..c.components {
                        current[(x + j) * c.components + i] = ra[i];
                    }
                }
                if run == c.width - x {
                    x += run;
                    continue;
                }
                let pos = x + run;
                for i in 0..c.components {
                    current[pos * c.components + i] = decode_run_interrupt(
                        &mut reader,
                        &mut run_contexts,
                        run_index,
                        ra[i],
                        previous[pos * c.components + i],
                        &c,
                        limit,
                        qbpp,
                    )?;
                }
                run_index = run_index.saturating_sub(1);
                x = pos + 1;
            } else {
                for i in 0..c.components {
                    let idx = qs[i].unsigned_abs() as usize;
                    current[x * c.components + i] = decode_regular(
                        &mut reader,
                        &mut contexts[idx],
                        qs[i],
                        ra[i],
                        rb[i],
                        rc[i],
                        &c,
                        limit,
                        qbpp,
                    )?;
                }
                x += 1;
            }
        }
        for i in 0..current.len() {
            output[y * c.width * c.components + i] = current[i] as u16;
        }
        previous_left.copy_from_slice(&previous[..c.components]);
        previous = current;
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
fn decode_component_line(
    reader: &mut BitReader<'_>,
    contexts: &mut [RegularContext],
    run_contexts: &mut [RunContext; 2],
    run_index: usize,
    previous: &[i32],
    previous_left: i32,
    current: &mut [i32],
    c: &ScanConfig,
    limit: u32,
    qbpp: u8,
) -> Result<usize, String> {
    let width = c.width;
    let mut run_index = run_index;
    let mut x = 0usize;
    while x < width {
        let ra = if x == 0 { previous[0] } else { current[x - 1] };
        let rb = previous[x];
        let rc = if x == 0 {
            previous_left
        } else {
            previous[x - 1]
        };
        let rd = if x + 1 == width { rb } else { previous[x + 1] };
        let qs = context_id(ra, rb, rc, rd, &c.preset, c.near as i32);
        if qs == 0 {
            let mut run = 0usize;
            while x + run < width && reader.read_bit()? {
                let chunk = 1usize << J[run_index];
                let count = chunk.min(width - x - run);
                run += count;
                if count == chunk {
                    run_index = (run_index + 1).min(31);
                }
                if run == width - x {
                    break;
                }
            }
            if run < width - x {
                let remainder = if J[run_index] == 0 {
                    0
                } else {
                    reader.read_bits(J[run_index])? as usize
                };
                run += remainder;
            }
            if run > width - x {
                return Err("run exceeds line".into());
            }
            for value in current[x..x + run].iter_mut() {
                *value = ra;
            }
            if run == width - x {
                x += run;
            } else {
                let pos = x + run;
                current[pos] = decode_run_interrupt(
                    reader,
                    run_contexts,
                    run_index,
                    ra,
                    previous[pos],
                    c,
                    limit,
                    qbpp,
                )?;
                run_index = run_index.saturating_sub(1);
                x = pos + 1;
            }
        } else {
            let idx = qs.unsigned_abs() as usize;
            current[x] =
                decode_regular(reader, &mut contexts[idx], qs, ra, rb, rc, c, limit, qbpp)?;
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
    let (range, limit, qbpp) = golomb_params(&c);
    let rounds = geometry
        .iter()
        .map(|(_, height, vs)| height.div_ceil(*vs))
        .max()
        .unwrap_or(0);
    let expected = expected_segments_for(rounds.max(1) as u64, restart_interval);
    let segments = split_restart_segments(scan, restart_interval, expected)?;
    let interval = u64::from(restart_interval);
    let mut segment_index = 0usize;
    let mut reader = BitReader::new(segments[0]);
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_indexes = vec![0usize; c.components];
    let mut offsets = Vec::with_capacity(c.components);
    let mut total = 0usize;
    for (width, _, _) in geometry {
        offsets.push(total);
        total += *width;
    }
    let mut previous = vec![0i32; total];
    let mut current = vec![0i32; total];
    let mut previous_left = vec![0i32; c.components];
    let mut planes: Vec<Vec<u16>> = geometry
        .iter()
        .map(|(width, height, _)| vec![0u16; width * height])
        .collect();
    for round in 0..rounds {
        if interval != 0 && round != 0 && (round as u64).is_multiple_of(interval) {
            begin_restart_interval(&mut reader, &segments, &mut segment_index)?;
            reset_restart_state(
                range,
                &mut contexts,
                &mut run_contexts,
                &mut run_indexes,
                &mut previous,
                &mut previous_left,
            );
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
                let run_index = decode_component_line(
                    &mut reader,
                    &mut contexts,
                    &mut run_contexts,
                    run_indexes[index],
                    &previous[offset..offset + width],
                    left,
                    &mut current[offset..offset + width],
                    &component,
                    limit,
                    qbpp,
                )?;
                run_indexes[index] = run_index;
                if line < height {
                    for (dst, src) in planes[index][line * width..(line + 1) * width]
                        .iter_mut()
                        .zip(&current[offset..offset + width])
                    {
                        *dst = *src as u16;
                    }
                }
                previous_left[index] = previous[offset];
                previous[offset..offset + width].copy_from_slice(&current[offset..offset + width]);
            }
        }
    }
    reader.finish()?;
    Ok(planes)
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
    fn gradient_quantization_addresses_all_regular_contexts() {
        let preset = Preset::defaults(255, 0).unwrap();
        let gradients = [-30, -10, -4, -1, 0, 1, 4, 10, 30];
        let mut visited = [false; 365];
        for d1 in gradients {
            for d2 in gradients {
                for d3 in gradients {
                    let rb = 128;
                    let rc = rb - d2;
                    let ra = rc - d3;
                    let rd = rb + d1;
                    visited[context_id(ra, rb, rc, rd, &preset, 0).unsigned_abs() as usize] = true;
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
    fn decoder_rejects_entropy_bytes_after_the_expected_samples() {
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
        let mut encoded = encode_scan(cfg, &values).unwrap();
        encoded.push(0);
        assert!(decode_scan(cfg, &encoded, 0).is_err());
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
