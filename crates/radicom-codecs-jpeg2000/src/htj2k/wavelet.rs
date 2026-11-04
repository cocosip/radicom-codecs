//! OpenJPH-compatible irreversible forward transform.
//!
//! Byte-exact output against OpenJPH requires reproducing its `float`
//! arithmetic: the 9/7 lifting steps and the `K` normalisation run in IEEE
//! single precision, with the constants exactly as OpenJPH rounds them from
//! its double literals (`param_atk::init_irv97`). The plane layout is the
//! standard band layout used by [`crate::wavelet`]: per level the vertical
//! pass is applied first (packing low rows above high rows), then the
//! horizontal pass.

const ALPHA: f32 = -1.586134342059924f64 as f32;
const BETA: f32 = -0.052980118572961f64 as f32;
const GAMMA: f32 = 0.882911075530934f64 as f32;
const DELTA: f32 = 0.443506852043971f64 as f32;
const K: f32 = 1.230174104914001f64 as f32;

/// Forward 9/7 transform of one component plane, in place.
pub(crate) fn forward_97(data: &mut [f32], width: usize, height: usize, levels: u8) {
    if width == 0 || height == 0 || data.len() != width * height {
        return;
    }
    let mut current_width = width;
    let mut current_height = height;
    for _ in 0..levels {
        if current_width <= 1 && current_height <= 1 {
            break;
        }
        if current_height > 1 {
            let mut column = vec![0.0f32; current_height];
            for x in 0..current_width {
                for y in 0..current_height {
                    column[y] = data[y * width + x];
                }
                forward_1d(&mut column);
                for y in 0..current_height {
                    data[y * width + x] = column[y];
                }
            }
        }
        if current_width > 1 {
            let mut row = vec![0.0f32; current_width];
            for y in 0..current_height {
                row.copy_from_slice(&data[y * width..y * width + current_width]);
                forward_1d(&mut row);
                data[y * width..y * width + current_width].copy_from_slice(&row);
            }
        }
        current_width = (current_width + 1) >> 1;
        current_height = (current_height + 1) >> 1;
    }
}

fn forward_1d(data: &mut [f32]) {
    let width = data.len();
    if width <= 1 {
        return;
    }
    let low_count = (width + 1) >> 1;
    let high_count = width - low_count;
    step(
        data,
        0,
        2,
        high_count,
        high_count.min(low_count.saturating_sub(1)),
        ALPHA,
    );
    step(data, 1, 1, low_count, low_count.min(high_count), BETA);
    step(
        data,
        0,
        2,
        high_count,
        high_count.min(low_count.saturating_sub(1)),
        GAMMA,
    );
    step(data, 1, 1, low_count, low_count.min(high_count), DELTA);
    let k_inv = 1.0f32 / K;
    for i in 0..low_count.min(high_count) {
        data[2 * i] *= k_inv;
        data[2 * i + 1] *= K;
    }
    if low_count > high_count {
        data[(low_count - 1) * 2] *= k_inv;
    }
    let mut workspace = vec![0.0f32; width];
    for i in 0..low_count {
        workspace[i] = data[2 * i];
    }
    for i in 0..high_count {
        workspace[low_count + i] = data[2 * i + 1];
    }
    data.copy_from_slice(&workspace);
}

fn step(
    data: &mut [f32],
    fl_start: usize,
    fw_start: usize,
    end: usize,
    middle: usize,
    coefficient: f32,
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
