pub(crate) const J: [u8; 32] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13,
    14, 15,
];
pub(crate) fn predict(ra: i32, rb: i32, rc: i32) -> i32 {
    if rc >= ra.max(rb) {
        ra.min(rb)
    } else if rc <= ra.min(rb) {
        ra.max(rb)
    } else {
        ra + rb - rc
    }
}

pub(crate) fn quantize(d: i32, t1: i32, t2: i32, t3: i32, near: i32) -> i32 {
    if d <= -t3 {
        -4
    } else if d <= -t2 {
        -3
    } else if d <= -t1 {
        -2
    } else if d < -near {
        -1
    } else if d <= near {
        0
    } else if d < t1 {
        1
    } else if d < t2 {
        2
    } else if d < t3 {
        3
    } else {
        4
    }
}
pub(crate) fn map_error(e: i32) -> u32 {
    if e < 0 {
        ((-e as u32) << 1) - 1
    } else {
        (e as u32) << 1
    }
}
pub(crate) fn unmap_error(m: u32) -> i32 {
    if m & 1 != 0 {
        -((m as i32 + 1) / 2)
    } else {
        (m / 2) as i32
    }
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
