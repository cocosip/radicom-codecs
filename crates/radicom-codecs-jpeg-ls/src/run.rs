pub(crate) const J: [u8; 32] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13,
    14, 15,
];
#[inline]
pub(crate) fn predict(ra: i32, rb: i32, rc: i32) -> i32 {
    ra + rb - rc.max(ra.min(rb)).min(ra.max(rb))
}

#[inline]
pub(crate) fn quantize(d: i32, t1: i32, t2: i32, t3: i32, near: i32) -> i32 {
    let magnitude = d.abs();
    let quantized = i32::from(magnitude > near)
        + i32::from(magnitude >= t1)
        + i32::from(magnitude >= t2)
        + i32::from(magnitude >= t3);
    if d < 0 {
        -quantized
    } else {
        quantized
    }
}

#[inline]
pub(crate) fn map_error(e: i32) -> u32 {
    ((e << 1) ^ (e >> 31)) as u32
}

#[inline]
pub(crate) fn unmap_error(m: u32) -> i32 {
    ((m >> 1) as i32) ^ -((m & 1) as i32)
}

#[cfg(test)]
mod tests {
    use super::predict;

    #[test]
    fn predictor_uses_the_median_edge_rule() {
        assert_eq!(predict(29, 0, 0), 29);
        assert_eq!(predict(0, 29, 0), 29);
        assert_eq!(predict(29, 0, 10), 19);
    }
}
