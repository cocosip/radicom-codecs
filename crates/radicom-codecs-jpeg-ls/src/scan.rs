#![allow(clippy::too_many_arguments, clippy::if_same_then_else)]
use crate::{
    bit::{BitReader, BitWriter},
    context::{RegularContext, RunContext},
    golomb::{decode_mapped, encode_mapped},
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

pub(crate) fn encode_scan(c: ScanConfig, input: &[u16]) -> Result<Vec<u8>, String> {
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
        return encode_line_scan(c, input);
    }
    let (range, limit, qbpp) = golomb_params(&c);
    let mut writer = BitWriter::new();
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_index = 0usize;
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    for y in 0..c.height {
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
                let mut run = 0usize;
                while x + run < c.width {
                    let sample = (y * c.width + x + run) * c.components;
                    if (0..c.components)
                        .any(|i| (input[sample + i] as i32 - ra[i]).abs() > c.near as i32)
                    {
                        break;
                    }
                    run += 1;
                }
                encode_run_length(&mut writer, run, &mut run_index, run == c.width - x)?;
                for j in 0..run {
                    for i in 0..c.components {
                        current[(x + j) * c.components + i] = ra[i];
                    }
                }
                if run < c.width - x {
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
                } else {
                    x += run;
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
    Ok(writer.finish())
}

fn encode_line_scan(c: ScanConfig, input: &[u16]) -> Result<Vec<u8>, String> {
    let (range, limit, qbpp) = golomb_params(&c);
    let mut writer = BitWriter::new();
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_indexes = vec![0usize; c.components];
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    for y in 0..c.height {
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
    Ok(writer.finish())
}

pub(crate) fn decode_scan(c: ScanConfig, entropy: &[u8]) -> Result<Vec<u16>, String> {
    if c.components == 0 || c.components > 3 || c.ilv > 2 {
        return Err("unsupported interleave mode".into());
    }
    if c.ilv == 0 && c.components != 1 {
        return Err("non-interleaved scan requires one component".into());
    }
    if c.ilv == 1 {
        return decode_line_scan(c, entropy);
    }
    let (range, limit, qbpp) = golomb_params(&c);
    let mut reader = BitReader::new(entropy);
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_index = 0usize;
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    let mut output = vec![0u16; c.width * c.height * c.components];
    for y in 0..c.height {
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

fn decode_line_scan(c: ScanConfig, entropy: &[u8]) -> Result<Vec<u16>, String> {
    let (range, limit, qbpp) = golomb_params(&c);
    let mut reader = BitReader::new(entropy);
    let mut contexts = vec![RegularContext::new(range); 365];
    let mut run_contexts = [RunContext::new(0, range), RunContext::new(1, range)];
    let mut run_indexes = vec![0usize; c.components];
    let mut previous = vec![0i32; c.width * c.components];
    let mut previous_left = vec![0i32; c.components];
    let mut output = vec![0u16; c.width * c.height * c.components];
    for y in 0..c.height {
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
                        current[component * c.width + x + j] = ra;
                    }
                    if run == c.width - x {
                        x += run;
                    } else {
                        let pos = x + run;
                        current[component * c.width + pos] = decode_run_interrupt(
                            &mut reader,
                            &mut run_contexts,
                            run_index,
                            ra,
                            previous[component * c.width + pos],
                            &c,
                            limit,
                            qbpp,
                        )?;
                        run_index = run_index.saturating_sub(1);
                        x = pos + 1;
                    }
                } else {
                    let idx = qs.unsigned_abs() as usize;
                    current[at] = decode_regular(
                        &mut reader,
                        &mut contexts[idx],
                        qs,
                        ra,
                        rb,
                        rc,
                        &c,
                        limit,
                        qbpp,
                    )?;
                    x += 1;
                }
            }
            run_indexes[component] = run_index;
        }
        for x in 0..c.width {
            for component in 0..c.components {
                output[(y * c.width + x) * c.components + component] =
                    current[component * c.width + x] as u16;
            }
        }
        for component in 0..c.components {
            previous_left[component] = previous[component * c.width];
        }
        previous = current;
    }
    reader.finish()?;
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
            let decoded = decode_scan(cfg, &encoded).unwrap();
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
        let decoded = decode_scan(cfg, &encode_scan(cfg, &values).unwrap()).unwrap();

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
        assert!(decode_scan(cfg, &encoded).is_err());
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
            decode_scan(cfg, &encode_scan(cfg, &values).unwrap()).unwrap(),
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
            decode_scan(cfg, &encode_scan(cfg, &values).unwrap()).unwrap(),
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
                decode_scan(cfg, &encode_scan(cfg, &samples).unwrap()).unwrap(),
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
        assert!(decode_scan(cfg, &writer.finish()).is_err());
    }
}
