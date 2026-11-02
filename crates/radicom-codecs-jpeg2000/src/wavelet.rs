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
    for level in (0..levels).rev() {
        inverse_53_2d(
            data,
            widths[level] as usize,
            heights[level] as usize,
            width,
            xs[level] & 1 == 0,
            ys[level] & 1 == 0,
        );
    }
    Ok(())
}

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
    for level in (0..levels).rev() {
        inverse_97_2d(data, widths[level], heights[level], width, INV_K);
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
) {
    if height > 1 {
        let mut column = vec![0i32; height];
        for x in 0..width {
            for y in 0..height {
                column[y] = data[y * stride + x];
            }
            forward_53_1d(&mut column, even_column);
            for y in 0..height {
                data[y * stride + x] = column[y];
            }
        }
    }
    if width > 1 {
        let mut row = vec![0i32; width];
        for y in 0..height {
            row.copy_from_slice(&data[y * stride..y * stride + width]);
            forward_53_1d(&mut row, even_row);
            data[y * stride..y * stride + width].copy_from_slice(&row);
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
) {
    if width > 1 {
        let mut row = vec![0i32; width];
        for y in 0..height {
            row.copy_from_slice(&data[y * stride..y * stride + width]);
            inverse_53_1d(&mut row, even_row);
            data[y * stride..y * stride + width].copy_from_slice(&row);
        }
    }
    if height > 1 {
        let mut column = vec![0i32; height];
        for x in 0..width {
            for y in 0..height {
                column[y] = data[y * stride + x];
            }
            inverse_53_1d(&mut column, even_column);
            for y in 0..height {
                data[y * stride + x] = column[y];
            }
        }
    }
}

fn forward_53_1d(data: &mut [i32], even: bool) {
    let length = data.len();
    if even {
        if length <= 1 {
            return;
        }
        let sn = (length + 1) >> 1;
        let dn = length - sn;
        let mut output = vec![0i32; length];
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
        data.copy_from_slice(&output);
        return;
    }

    if length == 1 {
        data[0] *= 2;
        return;
    }
    let low_count = length >> 1;
    let high_count = length - low_count;
    let mut result = vec![0i32; length];
    result[low_count] = data[0] - data[1];
    let mut index = 1;
    while index < low_count {
        result[low_count + index] =
            data[2 * index] - ((data[2 * index + 1] + data[2 * (index - 1) + 1]) >> 1);
        index += 1;
    }
    if length & 1 != 0 {
        result[low_count + index] = data[2 * index] - data[2 * (index - 1) + 1];
    }
    let mut index = 0;
    while index < high_count - 1 {
        result[index] = data[2 * index + 1]
            + ((result[low_count + index] + result[low_count + index + 1] + 2) >> 2);
        index += 1;
    }
    if length & 1 == 0 {
        result[index] = data[2 * index + 1]
            + ((result[low_count + index] + result[low_count + index] + 2) >> 2);
    }
    data.copy_from_slice(&result);
}

fn inverse_53_1d(data: &mut [i32], even: bool) {
    let length = data.len();
    if even {
        if length <= 1 {
            return;
        }
        let sn = (length + 1) >> 1;
        let mut output = vec![0i32; length];
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
        data.copy_from_slice(&output);
        return;
    }

    if length == 1 {
        data[0] /= 2;
        return;
    }
    if length == 2 {
        let out1 = data[0] - ((data[1] + 1) >> 1);
        let out0 = data[1] + out1;
        data[0] = out0;
        data[1] = out1;
        return;
    }
    let low_count = length >> 1;
    let mut result = vec![0i32; length];
    let mut s1 = data[low_count + 1];
    let mut dc = data[0] - ((data[low_count] + s1 + 2) >> 2);
    result[0] = data[low_count] + dc;
    let limit = length - 2 - if length & 1 == 0 { 1 } else { 0 };
    let mut output_index = 1;
    let mut low_index = 1;
    while output_index < limit {
        let s2 = data[low_count + low_index + 1];
        let dn = data[low_index] - ((s1 + s2 + 2) >> 2);
        result[output_index] = dc;
        result[output_index + 1] = s1 + ((dn + dc) >> 1);
        dc = dn;
        s1 = s2;
        output_index += 2;
        low_index += 1;
    }
    result[output_index] = dc;
    if length & 1 == 0 {
        let dn = data[(length >> 1) - 1] - ((s1 + 1) >> 1);
        result[length - 2] = s1 + ((dn + dc) >> 1);
        result[length - 1] = dn;
    } else {
        result[length - 1] = s1 + dc;
    }
    data.copy_from_slice(&result);
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

fn inverse_97_2d(data: &mut [f64], width: usize, height: usize, stride: usize, high_scale: f64) {
    if width > 1 {
        let mut row = vec![0f64; width];
        for y in 0..height {
            row.copy_from_slice(&data[y * stride..y * stride + width]);
            inverse_97_1d(&mut row, high_scale);
            data[y * stride..y * stride + width].copy_from_slice(&row);
        }
    }
    if height > 1 {
        let mut column = vec![0f64; height];
        for x in 0..width {
            for y in 0..height {
                column[y] = data[y * stride + x];
            }
            inverse_97_1d(&mut column, high_scale);
            for y in 0..height {
                data[y * stride + x] = column[y];
            }
        }
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

fn inverse_97_1d(data: &mut [f64], high_scale: f64) {
    let width = data.len();
    if width <= 1 {
        return;
    }
    let low_count = (width + 1) >> 1;
    let high_count = width - low_count;
    let mut temp = vec![0f64; width];
    for i in 0..low_count {
        temp[2 * i] = data[i];
    }
    for i in 0..high_count {
        temp[2 * i + 1] = data[low_count + i];
    }
    data.copy_from_slice(&temp);
    for i in 0..low_count.min(high_count) {
        data[2 * i] *= K;
        data[2 * i + 1] *= high_scale;
    }
    if low_count > high_count {
        data[(low_count - 1) * 2] *= K;
    }
    encode_step_97_inverse(data, 1, 1, low_count, low_count.min(high_count), -DELTA);
    encode_step_97_inverse(
        data,
        0,
        2,
        high_count,
        high_count.min(low_count.saturating_sub(1)),
        -GAMMA,
    );
    encode_step_97_inverse(data, 1, 1, low_count, low_count.min(high_count), -BETA);
    encode_step_97_inverse(
        data,
        0,
        2,
        high_count,
        high_count.min(low_count.saturating_sub(1)),
        -ALPHA,
    );
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

fn encode_step_97_inverse(
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
            inverse_97_1d(&mut plane, INV_K);
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
