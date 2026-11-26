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

pub(crate) fn forward_53(
    data: &mut [i32],
    width: usize,
    height: usize,
    levels: u8,
    x0: i32,
    y0: i32,
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
            current_x0 & 1 == 0,
            current_y0 & 1 == 0,
            (&mut columns, &mut row),
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
            xs[level] & 1 == 0,
            ys[level] & 1 == 0,
            (&mut columns, &mut row),
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
) -> Result<(), String> {
    if width == 0 || height == 0 || width * height != data.len() {
        return Err("invalid irreversible wavelet plane".into());
    }
    let mut w = width;
    let mut h = height;
    let mut columns = Vec::new();
    let mut row = Vec::new();
    for _ in 0..levels {
        if w <= 1 && h <= 1 {
            break;
        }
        if h > 1 {
            for (parity, coeff) in [(1, ALPHA), (0, BETA), (1, GAMMA), (0, DELTA)] {
                lift_97_columns(data, w, h, width, parity, coeff as f32);
            }
            for (y, line) in data.chunks_exact_mut(width).take(h).enumerate() {
                let scale = if y & 1 == 0 { INV_K as f32 } else { K as f32 };
                for sample in &mut line[..w] {
                    *sample *= scale;
                }
            }
            reorder_columns::<false, _>(data, w, h, width, 0, &mut columns);
        }
        if w > 1 {
            row.resize(w, 0.0);
            let sn = w.div_ceil(2);
            for line in data.chunks_exact_mut(width).take(h) {
                let line = &mut line[..w];
                for (parity, coeff) in [(1, ALPHA), (0, BETA), (1, GAMMA), (0, DELTA)] {
                    lift_97_line(line, parity, coeff as f32);
                }
                let (low, high) = row.split_at_mut(sn);
                for (sample, &value) in low.iter_mut().zip(line.iter().step_by(2)) {
                    *sample = value * INV_K as f32;
                }
                for (sample, &value) in high.iter_mut().zip(line[1..].iter().step_by(2)) {
                    *sample = value * K as f32;
                }
                line.copy_from_slice(&row);
            }
        }
        w = w.div_ceil(2);
        h = h.div_ceil(2);
    }
    Ok(())
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
pub(crate) fn inverse_97(
    data: &mut [f64],
    width: usize,
    height: usize,
    levels: u8,
) -> Result<(), String> {
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
    even_row: bool,
    even_column: bool,
    (columns, row): (&mut Vec<i32>, &mut Vec<i32>),
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
            lift_columns::<true>(group, count, high_parity);
            lift_columns::<false>(group, count, low_parity);
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

fn lift_columns<const PREDICT: bool>(group: &mut [i32], count: usize, parity: usize) {
    let height = group.len() / count;
    for y in (parity..height).step_by(2) {
        let (before, current_and_after) = group.split_at_mut(y * count);
        let (current, after) = current_and_after.split_at_mut(count);
        // Symmetric extension at the first and last rows.
        let above = if before.is_empty() {
            &after[..count]
        } else {
            &before[before.len() - count..]
        };
        let below = if after.is_empty() {
            above
        } else {
            &after[..count]
        };
        if PREDICT && (before.is_empty() || after.is_empty()) {
            for (sample, &neighbor) in current.iter_mut().zip(above) {
                *sample -= neighbor;
            }
        } else if PREDICT {
            for ((sample, &above), &below) in current.iter_mut().zip(above).zip(below) {
                *sample -= (above + below) >> 1;
            }
        } else {
            for ((sample, &above), &below) in current.iter_mut().zip(above).zip(below) {
                *sample += (above + below + 2) >> 2;
            }
        }
    }
}

fn inverse_53_2d(
    data: &mut [i32],
    width: usize,
    height: usize,
    stride: usize,
    even_row: bool,
    even_column: bool,
    (columns, row): (&mut Vec<i32>, &mut Vec<i32>),
) {
    if width > 1 {
        row.resize(width, 0);
        for line in data.chunks_exact_mut(stride).take(height) {
            let line = &mut line[..width];
            inverse_53_row(line, row, even_row);
            line.copy_from_slice(row);
        }
    }
    if height > 1 {
        let low = usize::from(!even_column);
        reorder_columns::<true, _>(data, width, height, stride, low, columns);
        for_each_lifting_row(
            data,
            width,
            height,
            stride,
            low,
            |curr, above, below, edge| {
                if edge {
                    for (sample, &neighbor) in curr.iter_mut().zip(above) {
                        *sample -= (neighbor + 1) >> 1;
                    }
                } else {
                    for ((sample, &a), &b) in curr.iter_mut().zip(above).zip(below) {
                        *sample -= (a + b + 2) >> 2;
                    }
                }
            },
        );
        for_each_lifting_row(
            data,
            width,
            height,
            stride,
            1 - low,
            |curr, above, below, edge| {
                if edge {
                    for (sample, &neighbor) in curr.iter_mut().zip(above) {
                        *sample += neighbor;
                    }
                } else {
                    for ((sample, &a), &b) in curr.iter_mut().zip(above).zip(below) {
                        *sample += (a + b) >> 1;
                    }
                }
            },
        );
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
) {
    const GROUP: usize = 32;
    scratch.resize(height * width.min(GROUP), T::default());
    for x in (0..width).step_by(GROUP) {
        let count = (width - x).min(GROUP);
        let group = &mut scratch[..height * count];
        for (dst, src) in group.chunks_exact_mut(count).zip(data.chunks_exact(stride)) {
            dst.copy_from_slice(&src[x..x + count]);
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

fn inverse_97_2d(
    data: &mut [f64],
    width: usize,
    height: usize,
    stride: usize,
    (columns, row): (&mut Vec<f64>, &mut Vec<f64>),
) {
    if width > 1 {
        row.resize(width, 0.0);
        for line in data.chunks_exact_mut(stride).take(height) {
            let line = &mut line[..width];
            let (low, high) = line.split_at(width.div_ceil(2));
            for (sample, &value) in row.iter_mut().step_by(2).zip(low) {
                *sample = value * K;
            }
            for (sample, &value) in row[1..].iter_mut().step_by(2).zip(high) {
                *sample = value * INV_K;
            }
            for (parity, coeff) in [(0, -DELTA), (1, -GAMMA), (0, -BETA), (1, -ALPHA)] {
                lift_97_line(row, parity, coeff);
            }
            line.copy_from_slice(row);
        }
    }
    if height > 1 {
        let low = height.div_ceil(2);
        for (y, line) in data.chunks_exact_mut(stride).take(height).enumerate() {
            let scale = if y < low { K } else { INV_K };
            for value in &mut line[..width] {
                *value *= scale;
            }
        }
        reorder_columns::<true, _>(data, width, height, stride, 0, columns);
        for (parity, coeff) in [(0, -DELTA), (1, -GAMMA), (0, -BETA), (1, -ALPHA)] {
            lift_97_columns(data, width, height, stride, parity, coeff);
        }
    }
}

fn lift_97_columns<T>(
    data: &mut [T],
    width: usize,
    height: usize,
    stride: usize,
    parity: usize,
    coefficient: T,
) where
    T: Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T> + std::ops::AddAssign,
{
    for_each_lifting_row(
        data,
        width,
        height,
        stride,
        parity,
        |current, above, below, _| {
            for ((sample, &a), &b) in current.iter_mut().zip(above).zip(below) {
                *sample += (a + b) * coefficient;
            }
        },
    );
}

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
            forward_53(&mut plane, w, h, levels, 0, 0).unwrap();
            inverse_53(&mut plane, w, h, levels, 0, 0).unwrap();
            assert_eq!(plane, source, "{w}x{h} levels={levels}");
        }
    }

    #[test]
    fn reversible_53_preserves_odd_origins_and_partial_column_groups() {
        for (width, height) in [(33, 32), (65, 49)] {
            for (x0, y0) in [(0, 1), (1, 0), (1, 1)] {
                let source = pattern(width, height);
                let mut plane = source.clone();
                forward_53(&mut plane, width, height, 3, x0, y0).unwrap();
                inverse_53(&mut plane, width, height, 3, x0, y0).unwrap();
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
            forward_97_f32(&mut single, width, height, 3).unwrap();
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
        forward_53(&mut plane, 16, 16, 2, 0, 0).unwrap();
        assert_eq!(plane[0], 7);
        // Only the 4x4 LL region holds the DC level; all detail bands are zero.
        assert!(plane[4..8].iter().all(|&v| v == 0));
        assert!(plane[8 * 16..].iter().all(|&v| v == 0));
    }
}
