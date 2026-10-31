use std::f64::consts::PI;

pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub(crate) const LUMA_Q: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
pub(crate) const CHROMA_Q: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

pub(crate) fn scaled_table(base: &[u8; 64], quality: u32) -> [u16; 64] {
    let q = quality.clamp(1, 100);
    let scale = if q < 50 { 5000 / q } else { 200 - q * 2 };
    std::array::from_fn(|i| (((u32::from(base[i]) * scale + 50) / 100).clamp(1, 255)) as u16)
}

pub(crate) fn forward(input: &[i32; 64]) -> [i32; 64] {
    std::array::from_fn(|v| {
        let y = v / 8;
        let x = v % 8;
        let mut sum = 0.0;
        for j in 0..8 {
            for i in 0..8 {
                let a = if x == 0 { 1.0 / 2.0_f64.sqrt() } else { 1.0 };
                let b = if y == 0 { 1.0 / 2.0_f64.sqrt() } else { 1.0 };
                sum += a
                    * b
                    * f64::from(input[j * 8 + i])
                    * (((2 * i + 1) as f64 * x as f64 * PI) / 16.0).cos()
                    * (((2 * j + 1) as f64 * y as f64 * PI) / 16.0).cos();
            }
        }
        (sum / 4.0).round() as i32
    })
}

pub(crate) fn inverse(input: &[i32; 64]) -> [i32; 64] {
    std::array::from_fn(|v| {
        let y = v / 8;
        let x = v % 8;
        let mut sum = 0.0;
        for j in 0..8 {
            for i in 0..8 {
                let a = if i == 0 { 1.0 / 2.0_f64.sqrt() } else { 1.0 };
                let b = if j == 0 { 1.0 / 2.0_f64.sqrt() } else { 1.0 };
                sum += a
                    * b
                    * f64::from(input[j * 8 + i])
                    * (((2 * x + 1) as f64 * i as f64 * PI) / 16.0).cos()
                    * (((2 * y + 1) as f64 * j as f64 * PI) / 16.0).cos();
            }
        }
        (sum / 4.0).round() as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dct_round_trip_stays_within_one_sample() {
        let input = std::array::from_fn(|i| (i as i32 * 13 % 255) - 128);
        let output = inverse(&forward(&input));
        assert!(input.iter().zip(output).all(|(a, b)| (*a - b).abs() <= 1));
    }
}
