//! Reversible 5/3 and irreversible 9/7 discrete wavelet transforms.
//!
//! Ported from the pure C# reference implementation, which in turn follows
//! OpenJPEG's lifting and symmetric-extension conventions.

const ALPHA: f64 = -1.586_134_342;
const BETA: f64 = -0.052_980_118;
const GAMMA: f64 = 0.882_911_075;
const DELTA: f64 = 0.443_506_852;
const K: f64 = 1.230_174_105;
const INV_K: f64 = 1.0 / K;

pub(crate) fn avx2_available() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        false
    }
}

pub(crate) fn jpeg2000_avx2_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        avx2_available()
            && std::env::var_os("RADICOM_J2K_AVX2").as_deref() != Some(std::ffi::OsStr::new("0"))
    })
}

pub(crate) fn forward_53(
    data: &mut [i32],
    width: usize,
    height: usize,
    levels: u8,
    x0: i32,
    y0: i32,
    avx2: bool,
) -> Result<(), String> {
    if width == 0 || height == 0 || data.len() != width * height {
        return Err("invalid reversible wavelet plane".into());
    }
    let mut current_width = width as i32;
    let mut current_height = height as i32;
    let mut current_x0 = x0;
    let mut current_y0 = y0;
    let mut columns = Vec::new();
    let mut row = Vec::new();
    for _ in 0..levels {
        if current_width <= 1 && current_height <= 1 {
            break;
        }
        forward_53_2d(
            data,
            current_width as usize,
            current_height as usize,
            width,
            (current_x0 & 1 == 0, current_y0 & 1 == 0),
            (&mut columns, &mut row),
            avx2,
        );
        current_width = split_length(current_width, current_x0 & 1 == 0);
        current_height = split_length(current_height, current_y0 & 1 == 0);
        current_x0 = (current_x0 + 1) >> 1;
        current_y0 = (current_y0 + 1) >> 1;
    }
    Ok(())
}

pub(crate) fn inverse_53(
    data: &mut [i32],
    width: usize,
    height: usize,
    levels: u8,
    x0: i32,
    y0: i32,
    avx2: bool,
) -> Result<(), String> {
    if width == 0 || height == 0 || data.len() != width * height {
        return Err("invalid reversible wavelet plane".into());
    }
    let levels = usize::from(levels);
    let mut widths = vec![0i32; levels + 1];
    let mut heights = vec![0i32; levels + 1];
    let mut xs = vec![0i32; levels + 1];
    let mut ys = vec![0i32; levels + 1];
    widths[0] = width as i32;
    heights[0] = height as i32;
    xs[0] = x0;
    ys[0] = y0;
    for level in 1..=levels {
        widths[level] = split_length(widths[level - 1], xs[level - 1] & 1 == 0);
        heights[level] = split_length(heights[level - 1], ys[level - 1] & 1 == 0);
        xs[level] = (xs[level - 1] + 1) >> 1;
        ys[level] = (ys[level - 1] + 1) >> 1;
    }
    let mut row = Vec::new();
    let mut columns = Vec::new();
    for level in (0..levels).rev() {
        inverse_53_2d(
            data,
            widths[level] as usize,
            heights[level] as usize,
            width,
            (xs[level] & 1 == 0, ys[level] & 1 == 0),
            (&mut columns, &mut row),
            avx2,
        );
    }
    Ok(())
}

/// OpenJPEG and OpenJPH round these lifting constants and gains to the same
/// single-precision values.
pub(crate) fn forward_97_f32(
    data: &mut [f32],
    width: usize,
    height: usize,
    levels: u8,
    avx2: bool,
) -> Result<(), String> {
    forward_97_levels::<false>(data, width, height, levels, avx2)
}

pub(crate) fn forward_97_openjph(
    data: &mut [f32],
    width: usize,
    height: usize,
    levels: u8,
    avx2: bool,
) -> Result<(), String> {
    forward_97_levels::<true>(data, width, height, levels, avx2)
}

fn forward_97_levels<const STREAM_ROWS: bool>(
    data: &mut [f32],
    width: usize,
    height: usize,
    levels: u8,
    avx2: bool,
) -> Result<(), String> {
    if width == 0 || height == 0 || width * height != data.len() {
        return Err("invalid irreversible wavelet plane".into());
    }
    let avx2 = avx2 && avx2_available();
    let mut w = width;
    let mut h = height;
    let mut columns = Vec::new();
    let mut row = Vec::new();
    for _ in 0..levels {
        if w <= 1 && h <= 1 {
            break;
        }
        if h > 1 && STREAM_ROWS {
            forward_97_vertical_rows(data, w, h, width, avx2, &mut columns);
        } else if h > 1 {
            for (parity, coeff) in [(1, ALPHA), (0, BETA), (1, GAMMA), (0, DELTA)] {
                lift_97_columns(data, w, h, width, parity, coeff as f32, avx2);
            }
            for (y, line) in data.chunks_exact_mut(width).take(h).enumerate() {
                let scale = if y & 1 == 0 { INV_K as f32 } else { K as f32 };
                for sample in &mut line[..w] {
                    *sample *= scale;
                }
            }
            reorder_columns::<false, _>(data, w, h, width, 0, &mut columns, |_, dst, src| {
                dst.copy_from_slice(src)
            });
        }
        if w > 1 {
            row.resize(w, 0.0);
            let sn = w.div_ceil(2);
            for line in data.chunks_exact_mut(width).take(h) {
                let line = &mut line[..w];
                let (low, high) = row.split_at_mut(sn);
                let (pairs, tail) = line.as_chunks::<2>();
                for ((low, high), pair) in low.iter_mut().zip(high.iter_mut()).zip(pairs) {
                    *low = pair[0];
                    *high = pair[1];
                }
                if let [last] = tail {
                    low[sn - 1] = *last;
                }
                for (high_step, low_step) in [(ALPHA, BETA), (GAMMA, DELTA)] {
                    lift_97_high(high, low, high_step as f32, avx2);
                    lift_97_low(low, high, low_step as f32, avx2);
                }
                for sample in low {
                    *sample *= INV_K as f32;
                }
                for sample in high {
                    *sample *= K as f32;
                }
                line.copy_from_slice(&row);
            }
        }
        w = w.div_ceil(2);
        h = h.div_ceil(2);
    }
    Ok(())
}

fn forward_97_vertical_rows(
    data: &mut [f32],
    width: usize,
    height: usize,
    stride: usize,
    avx2: bool,
    scratch: &mut Vec<f32>,
) {
    // Stagger the four lifting steps so each neighbor has completed the
    // preceding step while the rows are still in cache.
    for odd in (1..height + 4).step_by(2) {
        for (lag, coefficient) in [ALPHA, BETA, GAMMA, DELTA].into_iter().enumerate() {
            let y = odd.wrapping_sub(lag);
            if y >= height {
                continue;
            }
            let (before, rest) = data.split_at_mut(y * stride);
            let (current, after) = rest.split_at_mut(stride);
            let above = if y == 0 {
                &after[..width]
            } else {
                &before[before.len() - stride..][..width]
            };
            let below = if y + 1 == height {
                above
            } else {
                &after[..width]
            };
            lift_97_band(
                &mut current[..width],
                above,
                below,
                coefficient as f32,
                avx2,
            );
        }
    }
    // Separate the low and high rows in-place, scaling each row once as it
    // moves to its subband. One scratch row closes each permutation cycle.
    let low_rows = height.div_ceil(2);
    let mut visited = vec![false; height];
    scratch.resize(width, 0.0);
    for first in 0..height {
        if visited[first] {
            continue;
        }
        scratch.copy_from_slice(&data[first * stride..first * stride + width]);
        let mut destination = first;
        loop {
            visited[destination] = true;
            let (source, scale) = if destination < low_rows {
                (2 * destination, INV_K as f32)
            } else {
                (2 * (destination - low_rows) + 1, K as f32)
            };
            let (dst, src): (&mut [f32], &[f32]) = if source == first {
                (
                    &mut data[destination * stride..destination * stride + width],
                    scratch,
                )
            } else if source < destination {
                let (before, after) = data.split_at_mut(destination * stride);
                (
                    &mut after[..width],
                    &before[source * stride..source * stride + width],
                )
            } else {
                let (before, after) = data.split_at_mut(source * stride);
                (
                    &mut before[destination * stride..destination * stride + width],
                    &after[..width],
                )
            };
            for (out, &v) in dst.iter_mut().zip(src) {
                *out = v * scale;
            }
            if source == first {
                break;
            }
            destination = source;
        }
    }
}

#[inline]
fn lift_97_high(high: &mut [f32], low: &[f32], coefficient: f32, avx2: bool) {
    let count = high.len().min(low.len() - 1);
    lift_97_band(&mut high[..count], low, &low[1..], coefficient, avx2);
    if count != high.len() {
        high[count] += (low[count] + low[count]) * coefficient;
    }
}

#[inline]
fn lift_97_low(low: &mut [f32], high: &[f32], coefficient: f32, avx2: bool) {
    low[0] += (high[0] + high[0]) * coefficient;
    let end = high.len();
    lift_97_band(&mut low[1..end], high, &high[1..], coefficient, avx2);
    if end != low.len() {
        low[end] += (high[end - 1] + high[end - 1]) * coefficient;
    }
}

/// Double-precision reference analysis transform used by the wavelet tests.
#[allow(dead_code)]
pub(crate) fn forward_97(
    data: &mut [f64],
    width: usize,
    height: usize,
    levels: u8,
) -> Result<(), String> {
    if width == 0 || height == 0 || data.len() != width * height {
        return Err("invalid irreversible wavelet plane".into());
    }
    let mut current_width = width;
    let mut current_height = height;
    for _ in 0..levels {
        if current_width <= 1 && current_height <= 1 {
            break;
        }
        forward_97_2d(data, current_width, current_height, width);
        current_width = (current_width + 1) >> 1;
        current_height = (current_height + 1) >> 1;
    }
    Ok(())
}

/// Inverse 9/7 transform. The synthesis high-pass gain is `1/K`, the exact
/// inverse of the `K` that [`forward_97`] applies; OpenJPEG and OpenJPH agree.
#[cfg(test)]
fn inverse_97(data: &mut [f64], width: usize, height: usize, levels: u8) -> Result<(), String> {
    inverse_97_with_gains(
        data,
        width,
        height,
        levels,
        [K, INV_K],
        [-DELTA, -GAMMA, -BETA, -ALPHA],
        avx2_available(),
    )
}

pub(crate) fn inverse_97_openjph(
    data: &mut [f32],
    width: usize,
    height: usize,
    levels: u8,
) -> Result<(), String> {
    inverse_97_with_gains(
        data,
        width,
        height,
        levels,
        [K as f32, INV_K as f32],
        [-DELTA as f32, -GAMMA as f32, -BETA as f32, -ALPHA as f32],
        avx2_available(),
    )
}

pub(crate) fn inverse_97_openjpeg(
    data: &mut [f32],
    width: usize,
    height: usize,
    levels: u8,
) -> Result<(), String> {
    // OpenJPEG decodes with zero nominal subband gains and compensates here.
    inverse_97_with_gains(
        data,
        width,
        height,
        levels,
        [K as f32, 1.625_732_4],
        [-DELTA as f32, -GAMMA as f32, -BETA as f32, -ALPHA as f32],
        jpeg2000_avx2_enabled(),
    )
}

fn inverse_97_with_gains<T>(
    data: &mut [T],
    width: usize,
    height: usize,
    levels: u8,
    gains: [T; 2],
    steps: [T; 4],
    avx2: bool,
) -> Result<(), String>
where
    T: 'static
        + Copy
        + Default
        + std::ops::Add<Output = T>
        + std::ops::Mul<Output = T>
        + std::ops::AddAssign
        + std::ops::MulAssign,
{
    if width == 0 || height == 0 || data.len() != width * height {
        return Err("invalid irreversible wavelet plane".into());
    }
    let levels = usize::from(levels);
    let mut widths = vec![0usize; levels + 1];
    let mut heights = vec![0usize; levels + 1];
    widths[0] = width;
    heights[0] = height;
    for level in 1..=levels {
        widths[level] = (widths[level - 1] + 1) >> 1;
        heights[level] = (heights[level - 1] + 1) >> 1;
    }
    let mut row = Vec::new();
    let mut columns = Vec::new();
    for level in (0..levels).rev() {
        inverse_97_2d(
            data,
            widths[level],
            heights[level],
            width,
            (&mut columns, &mut row),
            (gains, steps),
            avx2,
        );
    }
    Ok(())
}

fn split_length(length: i32, even: bool) -> i32 {
    if even {
        (length + 1) / 2
    } else {
        length / 2
    }
}

fn forward_53_2d(
    data: &mut [i32],
    width: usize,
    height: usize,
    stride: usize,
    (even_row, even_column): (bool, bool),
    (columns, row): (&mut Vec<i32>, &mut Vec<i32>),
    avx2: bool,
) {
    if height > 1 {
        // Work on a contiguous group of columns so each plane access takes
        // advantage of the cache line and each lifting step spans adjacent samples.
        const COLUMN_GROUP: usize = 32;
        let low_parity = usize::from(!even_column);
        let high_parity = 1 - low_parity;
        columns.resize(height * width.min(COLUMN_GROUP), 0);
        for x in (0..width).step_by(COLUMN_GROUP) {
            let count = (width - x).min(COLUMN_GROUP);
            let group = &mut columns[..height * count];
            for (destination, source) in
                group.chunks_exact_mut(count).zip(data.chunks_exact(stride))
            {
                destination.copy_from_slice(&source[x..x + count]);
            }
            lift_53_columns::<true, false>(group, count, height, count, high_parity, avx2);
            lift_53_columns::<false, false>(group, count, height, count, low_parity, avx2);
            let band_order = (low_parity..height)
                .step_by(2)
                .chain((high_parity..height).step_by(2));
            for (destination, source_y) in data.chunks_exact_mut(stride).zip(band_order) {
                destination[x..x + count]
                    .copy_from_slice(&group[source_y * count..(source_y + 1) * count]);
            }
        }
    }
    if width > 1 {
        row.resize(width, 0);
        for line in data.chunks_exact_mut(stride).take(height) {
            let line = &mut line[..width];
            forward_53_row(line, row, even_row);
            line.copy_from_slice(row);
        }
    }
}

fn lift_53_columns<const PREDICT: bool, const INVERSE: bool>(
    data: &mut [i32],
    width: usize,
    height: usize,
    stride: usize,
    parity: usize,
    avx2: bool,
) {
    let avx2 = avx2 && avx2_available();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if width >= 8 && avx2 {
        for_each_lifting_row(data, width, height, stride, parity, |curr, a, b, edge| {
            // SAFETY: AVX2 was detected above.
            unsafe { lift_53_row_avx2::<PREDICT, INVERSE>(curr, a, b, edge) };
        });
        return;
    }
    let _ = avx2;
    for_each_lifting_row(
        data,
        width,
        height,
        stride,
        parity,
        lift_53_row::<PREDICT, INVERSE>,
    );
}

#[inline(always)]
fn lift_53_row<const PREDICT: bool, const INVERSE: bool>(
    current: &mut [i32],
    above: &[i32],
    below: &[i32],
    edge: bool,
) {
    if edge && PREDICT {
        for (sample, &neighbor) in current.iter_mut().zip(above) {
            if INVERSE {
                *sample += neighbor;
            } else {
                *sample -= neighbor;
            }
        }
    } else if edge && INVERSE {
        for (sample, &neighbor) in current.iter_mut().zip(above) {
            *sample -= (neighbor + 1) >> 1;
        }
    } else {
        for ((sample, &a), &b) in current.iter_mut().zip(above).zip(below) {
            let correction = if PREDICT {
                (a + b) >> 1
            } else {
                (a + b + 2) >> 2
            };
            if PREDICT == INVERSE {
                *sample += correction;
            } else {
                *sample -= correction;
            }
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn lift_53_row_avx2<const PREDICT: bool, const INVERSE: bool>(
    current: &mut [i32],
    above: &[i32],
    below: &[i32],
    edge: bool,
) {
    lift_53_row::<PREDICT, INVERSE>(current, above, below, edge);
}

fn inverse_53_2d(
    data: &mut [i32],
    width: usize,
    height: usize,
    stride: usize,
    (even_row, even_column): (bool, bool),
    (columns, row): (&mut Vec<i32>, &mut Vec<i32>),
    avx2: bool,
) {
    if width > 1 {
        row.resize(width, 0);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let use_avx2 = even_row && width >= 32 && avx2 && avx2_available();
        for line in data.chunks_exact_mut(stride).take(height) {
            let line = &mut line[..width];
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            if use_avx2 {
                // SAFETY: Dispatch checked AVX2 and an even-origin line with both subbands.
                unsafe { inverse_53_subbands_avx2(line, row) };
                continue;
            }
            inverse_53_row(line, row, even_row);
            line.copy_from_slice(row);
        }
    }
    if height > 1 {
        let low = usize::from(!even_column);
        reorder_columns::<true, _>(data, width, height, stride, low, columns, |_, dst, src| {
            dst.copy_from_slice(src)
        });
        lift_53_columns::<false, true>(data, width, height, stride, low, avx2);
        lift_53_columns::<true, true>(data, width, height, stride, 1 - low, avx2);
    }
}

// Gather and scatter short runs so both the band and spatial layouts are
// traversed by cache line, even on a plane with a large stride.
fn reorder_columns<const INVERSE: bool, T: Copy + Default>(
    data: &mut [T],
    width: usize,
    height: usize,
    stride: usize,
    low_parity: usize,
    scratch: &mut Vec<T>,
    mut load: impl FnMut(usize, &mut [T], &[T]),
) {
    const GROUP: usize = 32;
    scratch.resize(height * width.min(GROUP), T::default());
    for x in (0..width).step_by(GROUP) {
        let count = (width - x).min(GROUP);
        let group = &mut scratch[..height * count];
        for (y, (dst, src)) in group
            .chunks_exact_mut(count)
            .zip(data.chunks_exact(stride))
            .enumerate()
        {
            load(y, dst, &src[x..x + count]);
        }
        let spatial_rows = (low_parity..height)
            .step_by(2)
            .chain((1 - low_parity..height).step_by(2));
        for (band_row, spatial_row) in spatial_rows.enumerate() {
            let (src, dst) = if INVERSE {
                (band_row, spatial_row)
            } else {
                (spatial_row, band_row)
            };
            data[dst * stride + x..dst * stride + x + count]
                .copy_from_slice(&group[src * count..(src + 1) * count]);
        }
    }
}

fn for_each_lifting_row<T>(
    data: &mut [T],
    width: usize,
    height: usize,
    stride: usize,
    parity: usize,
    mut lift: impl FnMut(&mut [T], &[T], &[T], bool),
) {
    debug_assert!(height > 1);
    for y in (parity..height).step_by(2) {
        let (before, rest) = data.split_at_mut(y * stride);
        let (current, after) = rest.split_at_mut(stride);
        let above = if y == 0 {
            &after[..width]
        } else {
            &before[before.len() - stride..][..width]
        };
        let below = if y + 1 == height {
            above
        } else {
            &after[..width]
        };
        lift(
            &mut current[..width],
            above,
            below,
            y == 0 || y + 1 == height,
        );
    }
}

fn forward_53_row(data: &[i32], output: &mut [i32], even: bool) {
    let length = data.len();
    if even {
        let sn = (length + 1) >> 1;
        let dn = length - sn;
        let mut i = 0;
        while i < sn - 1 {
            output[sn + i] = data[2 * i + 1] - ((data[2 * i] + data[2 * (i + 1)]) >> 1);
            i += 1;
        }
        if length & 1 == 0 {
            output[sn + i] = data[2 * i + 1] - data[2 * i];
        }
        output[0] = data[0] + ((output[sn] + output[sn] + 2) >> 2);
        let mut i = 1;
        while i < dn {
            output[i] = data[2 * i] + ((output[sn + i - 1] + output[sn + i] + 2) >> 2);
            i += 1;
        }
        if length & 1 != 0 {
            output[i] = data[2 * i] + ((output[sn + i - 1] + output[sn + i - 1] + 2) >> 2);
        }
        return;
    }

    let low_count = length >> 1;
    let high_count = length - low_count;
    output[low_count] = data[0] - data[1];
    let mut index = 1;
    while index < low_count {
        output[low_count + index] =
            data[2 * index] - ((data[2 * index + 1] + data[2 * (index - 1) + 1]) >> 1);
        index += 1;
    }
    if length & 1 != 0 {
        output[low_count + index] = data[2 * index] - data[2 * (index - 1) + 1];
    }
    let mut index = 0;
    while index < high_count - 1 {
        output[index] = data[2 * index + 1]
            + ((output[low_count + index] + output[low_count + index + 1] + 2) >> 2);
        index += 1;
    }
    if length & 1 == 0 {
        output[index] = data[2 * index + 1]
            + ((output[low_count + index] + output[low_count + index] + 2) >> 2);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn inverse_53_subbands_avx2(line: &mut [i32], output: &mut [i32]) {
    let (low, high) = line.split_at_mut(line.len().div_ceil(2));
    let last_low = low.len() - 1;
    let last_high = high.len() - 1;
    let odd_length = low.len() != high.len();
    low[0] -= (high[0] + 1) >> 1;
    lift_53_row::<false, true>(&mut low[1..high.len()], high, &high[1..], false);
    if odd_length {
        low[last_low] -= (high[last_high] + 1) >> 1;
    }
    let high_interior = high.len() - usize::from(!odd_length);
    lift_53_row::<true, true>(&mut high[..high_interior], low, &low[1..], false);
    if !odd_length {
        high[last_high] += low[last_low];
    }
    let (pairs, tail) = output.as_chunks_mut::<2>();
    for (pair, (&low, &high)) in pairs.iter_mut().zip(low.iter().zip(high.iter())) {
        *pair = [low, high];
    }
    if let [last] = tail {
        *last = low[last_low];
    }
    line.copy_from_slice(output);
}

fn inverse_53_row(data: &[i32], output: &mut [i32], even: bool) {
    let length = data.len();
    debug_assert!(length > 1);
    if even {
        let sn = (length + 1) >> 1;
        let mut s1n = data[0];
        let mut d1n = data[sn];
        let mut s0n = s1n - ((d1n + 1) >> 1);
        let mut i = 0;
        let mut j = 1;
        while i + 3 < length {
            let d1c = d1n;
            let s0c = s0n;
            s1n = data[j];
            d1n = data[sn + j];
            s0n = s1n - ((d1c + d1n + 2) >> 2);
            output[i] = s0c;
            output[i + 1] = d1c + ((s0c + s0n) >> 1);
            i += 2;
            j += 1;
        }
        output[i] = s0n;
        if length & 1 != 0 {
            output[length - 1] = data[(length - 1) >> 1] - ((d1n + 1) >> 1);
            output[length - 2] = d1n + ((s0n + output[length - 1]) >> 1);
        } else {
            output[length - 1] = d1n + s0n;
        }
        return;
    }

    if length == 2 {
        let out1 = data[0] - ((data[1] + 1) >> 1);
        let out0 = data[1] + out1;
        output[0] = out0;
        output[1] = out1;
        return;
    }
    let low_count = length >> 1;
    let mut s1 = data[low_count + 1];
    let mut dc = data[0] - ((data[low_count] + s1 + 2) >> 2);
    output[0] = data[low_count] + dc;
    let limit = length - 2 - if length & 1 == 0 { 1 } else { 0 };
    let mut output_index = 1;
    let mut low_index = 1;
    while output_index < limit {
        let s2 = data[low_count + low_index + 1];
        let dn = data[low_index] - ((s1 + s2 + 2) >> 2);
        output[output_index] = dc;
        output[output_index + 1] = s1 + ((dn + dc) >> 1);
        dc = dn;
        s1 = s2;
        output_index += 2;
        low_index += 1;
    }
    output[output_index] = dc;
    if length & 1 == 0 {
        let dn = data[(length >> 1) - 1] - ((s1 + 1) >> 1);
        output[length - 2] = s1 + ((dn + dc) >> 1);
        output[length - 1] = dn;
    } else {
        output[length - 1] = s1 + dc;
    }
}

fn forward_97_2d(data: &mut [f64], width: usize, height: usize, stride: usize) {
    if height > 1 {
        let mut column = vec![0f64; height];
        for x in 0..width {
            for y in 0..height {
                column[y] = data[y * stride + x];
            }
            forward_97_1d(&mut column);
            for y in 0..height {
                data[y * stride + x] = column[y];
            }
        }
    }
    if width > 1 {
        let mut row = vec![0f64; width];
        for y in 0..height {
            row.copy_from_slice(&data[y * stride..y * stride + width]);
            forward_97_1d(&mut row);
            data[y * stride..y * stride + width].copy_from_slice(&row);
        }
    }
}

fn inverse_97_2d<T>(
    data: &mut [T],
    width: usize,
    height: usize,
    stride: usize,
    (columns, row): (&mut Vec<T>, &mut Vec<T>),
    ([low_gain, high_gain], steps): ([T; 2], [T; 4]),
    avx2: bool,
) where
    T: 'static
        + Copy
        + Default
        + std::ops::Add<Output = T>
        + std::ops::Mul<Output = T>
        + std::ops::AddAssign
        + std::ops::MulAssign,
{
    if width > 1 {
        row.resize(width, T::default());
        let avx2 = avx2 && avx2_available();
        for line in data.chunks_exact_mut(stride).take(height) {
            let line = &mut line[..width];
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            if avx2 && line.len() >= 32 / std::mem::size_of::<T>() {
                // SAFETY: Runtime dispatch checked AVX2.
                unsafe { inverse_97_line_avx2(line, row, [low_gain, high_gain], steps) };
                continue;
            }
            let _ = avx2;
            inverse_97_subbands(
                line,
                row,
                [low_gain, high_gain],
                steps,
                lift_97_row_fallback,
            );
        }
    }
    if height > 1 {
        if width >= 512 && height >= 256 {
            // Keep one group in scratch across all four lifting passes.
            const GROUP: usize = 128;
            let low = height.div_ceil(2);
            columns.resize(height * GROUP, T::default());
            for x in (0..width).step_by(GROUP) {
                let columns_in_group = (width - x).min(GROUP);
                let group = &mut columns[..height * columns_in_group];
                for (y, row) in group.chunks_exact_mut(columns_in_group).enumerate() {
                    let band_row = y / 2 + (y & 1) * low;
                    let scale = if y & 1 == 0 { low_gain } else { high_gain };
                    let start = band_row * stride + x;
                    for (out, &value) in row.iter_mut().zip(&data[start..start + columns_in_group])
                    {
                        *out = value * scale;
                    }
                }
                for (step, coeff) in steps.into_iter().enumerate() {
                    lift_97_columns(
                        group,
                        columns_in_group,
                        height,
                        columns_in_group,
                        step & 1,
                        coeff,
                        avx2,
                    );
                }
                for (source, row) in group
                    .chunks_exact(columns_in_group)
                    .zip(data.chunks_exact_mut(stride))
                {
                    row[x..x + columns_in_group].copy_from_slice(source);
                }
            }
            return;
        }
        let low = height.div_ceil(2);
        reorder_columns::<true, _>(data, width, height, stride, 0, columns, |y, dst, src| {
            let scale = if y < low { low_gain } else { high_gain };
            for (output, &input) in dst.iter_mut().zip(src) {
                *output = input * scale;
            }
        });
        for (step, coeff) in steps.into_iter().enumerate() {
            lift_97_columns(data, width, height, stride, step & 1, coeff, avx2);
        }
    }
}

#[inline(always)]
fn inverse_97_subbands<T>(
    line: &mut [T],
    output: &mut [T],
    [low_gain, high_gain]: [T; 2],
    [first, second, third, fourth]: [T; 4],
    mut lift: impl FnMut(&mut [T], &[T], &[T], T),
) where
    T: 'static
        + Copy
        + std::ops::Add<Output = T>
        + std::ops::Mul<Output = T>
        + std::ops::AddAssign
        + std::ops::MulAssign,
{
    let (low, high) = line.split_at_mut(line.len().div_ceil(2));
    for value in &mut *low {
        *value *= low_gain;
    }
    for value in &mut *high {
        *value *= high_gain;
    }

    let last_low = low.len() - 1;
    let last_high = high.len() - 1;
    let has_extra_low = last_low != last_high;
    let high_interior = high.len() - usize::from(!has_extra_low);
    // Lift the contiguous subbands and interleave only after all four steps.
    for (low_step, high_step) in [(first, second), (third, fourth)] {
        low[0] += (high[0] + high[0]) * low_step;
        lift(&mut low[1..high.len()], high, &high[1..], low_step);
        if has_extra_low {
            low[last_low] += (high[last_high] + high[last_high]) * low_step;
        }
        lift(&mut high[..high_interior], low, &low[1..], high_step);
        if !has_extra_low {
            high[last_high] += (low[last_low] + low[last_low]) * high_step;
        }
    }

    let (pairs, tail) = output.as_chunks_mut::<2>();
    for (pair, (&low, &high)) in pairs.iter_mut().zip(low.iter().zip(high.iter())) {
        pair[0] = low;
        pair[1] = high;
    }
    if let [last] = tail {
        *last = low[last_low];
    }
    line.copy_from_slice(output);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn inverse_97_line_avx2<T>(line: &mut [T], output: &mut [T], gains: [T; 2], steps: [T; 4])
where
    T: 'static
        + Copy
        + std::ops::Add<Output = T>
        + std::ops::Mul<Output = T>
        + std::ops::AddAssign
        + std::ops::MulAssign,
{
    inverse_97_subbands(line, output, gains, steps, lift_97_row);
}

#[inline]
fn lift_97_band<T>(current: &mut [T], a: &[T], b: &[T], coefficient: T, avx2: bool)
where
    T: 'static + Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if avx2 && current.len() >= 32 / std::mem::size_of::<T>() {
        // SAFETY: The transform checked runtime AVX2 support.
        unsafe { lift_97_row_avx2(current, a, b, coefficient) };
        return;
    }
    let _ = avx2;
    lift_97_row_fallback(current, a, b, coefficient);
}

fn lift_97_columns<T>(
    data: &mut [T],
    width: usize,
    height: usize,
    stride: usize,
    parity: usize,
    coefficient: T,
    avx2: bool,
) where
    T: 'static + Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    let avx2 = avx2 && avx2_available();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if width >= 32 / std::mem::size_of::<T>() && avx2 {
        for_each_lifting_row(data, width, height, stride, parity, |curr, a, b, _| {
            // SAFETY: AVX2 was detected above.
            unsafe { lift_97_row_avx2(curr, a, b, coefficient) };
        });
        return;
    }
    let _ = avx2;
    for_each_lifting_row(
        data,
        width,
        height,
        stride,
        parity,
        |current, above, below, _| {
            lift_97_row_fallback(current, above, below, coefficient);
        },
    );
}

#[inline]
fn lift_97_row_fallback<T>(current: &mut [T], a: &[T], b: &[T], coefficient: T)
where
    T: 'static + Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f32>()
        && current.len() >= 4
        && std::arch::is_x86_feature_detected!("sse2")
    {
        // SAFETY: T is exactly f32. The casts keep each slice's length and aliasing.
        unsafe {
            let coefficient = *(&coefficient as *const T).cast::<f32>();
            let current =
                std::slice::from_raw_parts_mut(current.as_mut_ptr().cast::<f32>(), current.len());
            let a = std::slice::from_raw_parts(a.as_ptr().cast::<f32>(), a.len());
            let b = std::slice::from_raw_parts(b.as_ptr().cast::<f32>(), b.len());
            lift_97_f32_row_sse2(current, a, b, coefficient);
        }
        return;
    }
    lift_97_row(current, a, b, coefficient);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
unsafe fn lift_97_f32_row_sse2(current: &mut [f32], a: &[f32], b: &[f32], coefficient: f32) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let length = current.len().min(a.len()).min(b.len());
    let (out, tail) = current[..length].as_chunks_mut::<4>();
    let src_a = a[..length].as_chunks::<4>().0;
    let src_b = b[..length].as_chunks::<4>().0;
    let scale = _mm_set1_ps(coefficient);
    for ((out, a), b) in out.iter_mut().zip(src_a).zip(src_b) {
        // SAFETY: The chunks each contain four floats. All accesses are unaligned.
        unsafe {
            let x = _mm_loadu_ps(out.as_ptr());
            let a = _mm_loadu_ps(a.as_ptr());
            let b = _mm_loadu_ps(b.as_ptr());
            _mm_storeu_ps(
                out.as_mut_ptr(),
                _mm_add_ps(x, _mm_mul_ps(_mm_add_ps(a, b), scale)),
            );
        }
    }
    let end = out.len() * 4;
    lift_97_row(tail, &a[end..], &b[end..], coefficient);
}

#[inline(always)]
fn lift_97_row<T>(current: &mut [T], above: &[T], below: &[T], coefficient: T)
where
    T: Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    for ((sample, &a), &b) in current.iter_mut().zip(above).zip(below) {
        *sample += (a + b) * coefficient;
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn lift_97_row_avx2<T>(current: &mut [T], above: &[T], below: &[T], coefficient: T)
where
    T: Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    lift_97_row(current, above, below, coefficient);
}

#[cfg(test)]
fn lift_97_line<T>(data: &mut [T], parity: usize, coefficient: T)
where
    T: Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    let length = data.len();
    if parity == 0 {
        data[0] += (data[1] + data[1]) * coefficient;
    }
    for i in (2 - parity..length - 1).step_by(2) {
        data[i] += (data[i - 1] + data[i + 1]) * coefficient;
    }
    if (length - 1) & 1 == parity {
        data[length - 1] += (data[length - 2] + data[length - 2]) * coefficient;
    }
}

fn forward_97_1d(data: &mut [f64]) {
    let width = data.len();
    if width <= 1 {
        return;
    }
    let low_count = (width + 1) >> 1;
    let high_count = width - low_count;
    encode_step_97(
        data,
        0,
        2,
        high_count,
        high_count.min(low_count.saturating_sub(1)),
        ALPHA,
    );
    encode_step_97(data, 1, 1, low_count, low_count.min(high_count), BETA);
    encode_step_97(
        data,
        0,
        2,
        high_count,
        high_count.min(low_count.saturating_sub(1)),
        GAMMA,
    );
    encode_step_97(data, 1, 1, low_count, low_count.min(high_count), DELTA);
    for i in 0..low_count.min(high_count) {
        data[2 * i] *= INV_K;
        data[2 * i + 1] *= K;
    }
    if low_count > high_count {
        data[(low_count - 1) * 2] *= INV_K;
    }
    let mut workspace = vec![0f64; width];
    for i in 0..low_count {
        workspace[i] = data[2 * i];
    }
    for i in 0..high_count {
        workspace[low_count + i] = data[2 * i + 1];
    }
    data.copy_from_slice(&workspace);
}

fn encode_step_97(
    data: &mut [f64],
    fl_start: usize,
    fw_start: usize,
    end: usize,
    middle: usize,
    coefficient: f64,
) {
    if middle > 0 {
        let mut fw = fw_start;
        let fl = fl_start;
        data[fw - 1] += (data[fl] + data[fw]) * coefficient;
        fw += 2;
        for _ in 1..middle {
            data[fw - 1] += (data[fw - 2] + data[fw]) * coefficient;
            fw += 2;
        }
    }
    if middle < end {
        let fw = fw_start + 2 * middle;
        data[fw - 1] += (2.0 * data[fw - 2]) * coefficient;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_vertical_lifting_preserves_every_coefficient() {
        for width in [1, 2, 7, 16, 31, 64] {
            for height in [1, 2, 3, 8, 17, 64] {
                let source: Vec<_> = (0..width * height)
                    .map(|i| (((i * 757 + 41) % 997) as f32 - 498.0) / 255.0)
                    .collect();
                for avx2 in [false, true] {
                    let mut expected = source.clone();
                    let mut actual = source.clone();
                    forward_97_f32(&mut expected, width, height, 5, avx2).unwrap();
                    forward_97_openjph(&mut actual, width, height, 5, avx2).unwrap();
                    assert!(
                        actual
                            .iter()
                            .zip(&expected)
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                        "{width}x{height} AVX2={avx2}"
                    );
                }
            }
        }
    }

    #[test]
    fn forward_97_subband_lifting_matches_interleaved_lifting() {
        for width in 2usize..=67 {
            let input: Vec<_> = (0..width)
                .map(|i| ((i * 73 + 17) % 251) as f32 - 117.0)
                .collect();
            let mut lifted = input.clone();
            for (parity, coefficient) in [(1, ALPHA), (0, BETA), (1, GAMMA), (0, DELTA)] {
                lift_97_line(&mut lifted, parity, coefficient as f32);
            }
            let expected: Vec<_> = lifted
                .iter()
                .step_by(2)
                .map(|&value| value * INV_K as f32)
                .chain(lifted[1..].iter().step_by(2).map(|&value| value * K as f32))
                .collect();
            for avx2 in [false, avx2_available()] {
                let mut actual = input.clone();
                forward_97_f32(&mut actual, width, 1, 1, avx2).unwrap();
                assert_eq!(actual, expected, "width={width}, avx2={avx2}");
            }
        }
    }

    #[test]
    fn inverse_97_subband_lifting_matches_interleaved_lifting() {
        let gains = [K as f32, 1.625_732_4];
        let steps = [-DELTA as f32, -GAMMA as f32, -BETA as f32, -ALPHA as f32];
        for width in 2usize..=67 {
            let input: Vec<_> = (0..width)
                .map(|i| ((i * 73 + 17) % 251) as f32 - 117.0)
                .collect();
            let (low, high) = input.split_at(width.div_ceil(2));
            let mut expected = vec![0.0; width];
            for (sample, &value) in expected.iter_mut().step_by(2).zip(low) {
                *sample = value * gains[0];
            }
            for (sample, &value) in expected[1..].iter_mut().step_by(2).zip(high) {
                *sample = value * gains[1];
            }
            for (step, coefficient) in steps.into_iter().enumerate() {
                lift_97_line(&mut expected, step & 1, coefficient);
            }
            for avx2 in [false, avx2_available()] {
                let mut actual = input.clone();
                let mut scratch = vec![0.0; width];
                #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
                if avx2 {
                    // SAFETY: Feature detected at the top of the test loop.
                    unsafe { inverse_97_line_avx2(&mut actual, &mut scratch, gains, steps) };
                } else {
                    inverse_97_subbands(
                        &mut actual,
                        &mut scratch,
                        gains,
                        steps,
                        lift_97_row_fallback,
                    );
                }
                #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
                inverse_97_subbands(
                    &mut actual,
                    &mut scratch,
                    gains,
                    steps,
                    lift_97_row_fallback,
                );
                assert_eq!(actual, expected, "width={width}, avx2={avx2}");
            }
        }
    }

    #[test]
    fn inverse_97_column_groups_match_scalar_with_odd_dimensions_and_stride() {
        let (width, height, stride) = (517usize, 259usize, 523usize);
        let gains = [K as f32, 1.625_732_4];
        let steps = [-DELTA as f32, -GAMMA as f32, -BETA as f32, -ALPHA as f32];
        let input: Vec<_> = (0..stride * height)
            .map(|i| ((i * 73 + 17) % 251) as f32 - 117.0)
            .collect();
        let mut expected = input.clone();
        let mut row = vec![0.0; width];
        for line in expected.chunks_exact_mut(stride) {
            inverse_97_subbands(
                &mut line[..width],
                &mut row,
                gains,
                steps,
                lift_97_row_fallback,
            );
        }
        let mut column = vec![0.0; height];
        for x in 0..width {
            for (y, sample) in column.iter_mut().enumerate() {
                let band_row = y / 2 + (y & 1) * height.div_ceil(2);
                *sample = expected[band_row * stride + x] * gains[y & 1];
            }
            for (step, coefficient) in steps.into_iter().enumerate() {
                lift_97_line(&mut column, step & 1, coefficient);
            }
            for (y, &sample) in column.iter().enumerate() {
                expected[y * stride + x] = sample;
            }
        }
        for avx2 in [false, avx2_available()] {
            let mut actual = input.clone();
            inverse_97_2d(
                &mut actual,
                width,
                height,
                stride,
                (&mut Vec::new(), &mut row),
                (gains, steps),
                avx2,
            );
            assert_eq!(actual, expected, "avx2={avx2}");
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn inverse_53_avx2_subbands_match_scalar_at_edges_and_vector_tails() {
        if !avx2_available() {
            return;
        }
        for width in 2..=145 {
            let mut input = pattern(width, 1);
            let mut expected = vec![0; width];
            let mut output = vec![0; width];
            inverse_53_row(&input, &mut expected, true);
            // SAFETY: AVX2 is available and the input has both subbands.
            unsafe { inverse_53_subbands_avx2(&mut input, &mut output) };
            assert_eq!(input, expected, "width={width}");
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn sse2_lifting_matches_scalar_with_partial_vectors() {
        if !std::arch::is_x86_feature_detected!("sse2") {
            return;
        }
        let a: Vec<_> = (0..35).map(|index| (index as f32 - 18.0) * 1.25).collect();
        let b: Vec<_> = a.iter().rev().copied().collect();
        for length in 0..=35 {
            let mut scalar = vec![0.625; length];
            let mut vector = scalar.clone();
            lift_97_row(&mut scalar, &a, &b, -DELTA as f32);
            // SAFETY: SSE2 was detected above.
            unsafe { lift_97_f32_row_sse2(&mut vector, &a, &b, -DELTA as f32) };
            assert_eq!(scalar, vector);
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_lifting_matches_scalar_at_edges_and_partial_vectors() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }

        fn check_53<const PREDICT: bool, const INVERSE: bool>(
            input: &[i32],
            above: &[i32],
            below: &[i32],
            edge: bool,
        ) {
            let mut scalar = input.to_vec();
            let mut vector = input.to_vec();
            lift_53_row::<PREDICT, INVERSE>(&mut scalar, above, below, edge);
            // SAFETY: the test detects AVX2 before calling this helper.
            unsafe { lift_53_row_avx2::<PREDICT, INVERSE>(&mut vector, above, below, edge) };
            assert_eq!(vector, scalar);
        }

        fn check_97<T>(input: &[T], above: &[T], below: &[T], coefficient: T)
        where
            T: Copy
                + std::fmt::Debug
                + PartialEq
                + std::ops::Add<Output = T>
                + std::ops::Mul<Output = T>
                + std::ops::AddAssign,
        {
            let mut scalar = input.to_vec();
            let mut vector = input.to_vec();
            lift_97_row(&mut scalar, above, below, coefficient);
            // SAFETY: the test detects AVX2 before calling this helper.
            unsafe { lift_97_row_avx2(&mut vector, above, below, coefficient) };
            assert_eq!(vector, scalar);
        }

        for width in [1, 4, 7, 8, 9, 17, 31, 32, 33, 129] {
            let input = pattern(width, 3);
            let (samples, neighbors) = input.split_at(width);
            let (above, below) = neighbors.split_at(width);
            for (below, edge) in [(above, true), (below, false)] {
                check_53::<true, false>(samples, above, below, edge);
                check_53::<false, false>(samples, above, below, edge);
                check_53::<false, true>(samples, above, below, edge);
                check_53::<true, true>(samples, above, below, edge);
            }

            let single: Vec<_> = input.iter().map(|&x| x as f32 * 0.625).collect();
            let double: Vec<_> = input.iter().map(|&x| f64::from(x) * 0.625).collect();
            check_97(
                &single[..width],
                &single[width..2 * width],
                &single[2 * width..],
                ALPHA as f32,
            );
            check_97(
                &double[..width],
                &double[width..2 * width],
                &double[2 * width..],
                -DELTA,
            );
        }
    }

    fn pattern(width: usize, height: usize) -> Vec<i32> {
        (0..width * height)
            .map(|i| ((i * 37 + i * i * 11) % 512) as i32 - 256)
            .collect()
    }

    #[test]
    fn reversible_53_round_trips_every_shape() {
        for (w, h, levels) in [
            (1, 1, 0),
            (1, 17, 1),
            (17, 1, 1),
            (13, 9, 2),
            (32, 32, 5),
            (65, 49, 4),
        ] {
            let source = pattern(w, h);
            let mut plane = source.clone();
            forward_53(&mut plane, w, h, levels, 0, 0, jpeg2000_avx2_enabled()).unwrap();
            inverse_53(&mut plane, w, h, levels, 0, 0, jpeg2000_avx2_enabled()).unwrap();
            assert_eq!(plane, source, "{w}x{h} levels={levels}");
        }
    }

    #[test]
    fn reversible_53_preserves_odd_origins_and_partial_column_groups() {
        for (width, height) in [(33, 32), (65, 49)] {
            for (x0, y0) in [(0, 1), (1, 0), (1, 1)] {
                let source = pattern(width, height);
                let mut plane = source.clone();
                forward_53(
                    &mut plane,
                    width,
                    height,
                    3,
                    x0,
                    y0,
                    jpeg2000_avx2_enabled(),
                )
                .unwrap();
                inverse_53(
                    &mut plane,
                    width,
                    height,
                    3,
                    x0,
                    y0,
                    jpeg2000_avx2_enabled(),
                )
                .unwrap();
                assert_eq!(plane, source, "{width}x{height} origin ({x0}, {y0})");
            }
        }
    }

    #[test]
    fn irreversible_analysis_and_synthesis_handle_short_axes_and_partial_groups() {
        for (width, height) in [(1, 17), (18, 1), (2, 3), (33, 35)] {
            let source = pattern(width, height);
            let mut single: Vec<f32> = source.iter().map(|&value| value as f32).collect();
            let mut reference: Vec<f64> = source.iter().map(|&value| f64::from(value)).collect();
            forward_97_f32(&mut single, width, height, 3, jpeg2000_avx2_enabled()).unwrap();
            forward_97(&mut reference, width, height, 3).unwrap();
            for (&single, &double) in single.iter().zip(&reference) {
                assert!((f64::from(single) - double).abs() < 0.001);
            }
            let mut reconstructed: Vec<f64> = single.into_iter().map(f64::from).collect();
            inverse_97(&mut reconstructed, width, height, 3).unwrap();
            for (&pixel, &value) in source.iter().zip(&reconstructed) {
                assert!((f64::from(pixel) - value).abs() < 0.001);
            }
        }
    }

    #[test]
    fn irreversible_97_round_trips_through_the_classic_inverse() {
        // The classic 2000 codec decodes with `inverse_97`; it must be the
        // exact inverse of `forward_97` or detail bands are mis-scaled.
        for (w, h, levels) in [(8, 8, 1), (17, 13, 2), (32, 32, 3)] {
            let source: Vec<f64> = pattern(w, h).iter().map(|&v| f64::from(v)).collect();
            let mut plane = source.clone();
            forward_97(&mut plane, w, h, levels).unwrap();
            inverse_97(&mut plane, w, h, levels).unwrap();
            for (a, b) in source.iter().zip(&plane) {
                assert!((a - b).abs() < 0.05, "{w}x{h} levels={levels}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn irreversible_97_reconstructs_with_openjpeg_scaling() {
        // OpenJPH's synthesis scales the high band by 1/K, matching the
        // analysis scale K; classic JPEG 2000 uses 2/K instead.
        for length in [2usize, 3, 8, 17, 32] {
            let source: Vec<f64> = (0..length)
                .map(|i| ((i * 37 + i * i * 11) % 512) as f64 - 256.0)
                .collect();
            let mut plane = source.clone();
            forward_97_1d(&mut plane);
            inverse_97(&mut plane, length, 1, 1).unwrap();
            for (a, b) in source.iter().zip(&plane) {
                assert!((a - b).abs() < 0.05, "length={length}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn constant_plane_stays_in_the_low_band() {
        let mut plane = vec![7i32; 16 * 16];
        forward_53(&mut plane, 16, 16, 2, 0, 0, jpeg2000_avx2_enabled()).unwrap();
        assert_eq!(plane[0], 7);
        // Only the 4x4 LL region holds the DC level; all detail bands are zero.
        assert!(plane[4..8].iter().all(|&v| v == 0));
        assert!(plane[8 * 16..].iter().all(|&v| v == 0));
    }
}
