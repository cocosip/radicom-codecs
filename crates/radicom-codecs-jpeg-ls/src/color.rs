#![allow(clippy::chunks_exact_to_as_chunks)]
use radicom::pixel::photometric::PhotometricInterpretation;
pub(crate) fn ybr_to_rgb(s: &mut [u16], bits: u8) {
    let max = ((1u32 << bits) - 1) as f64;
    let c = (1u32 << (bits - 1)) as f64;
    for p in s.chunks_exact_mut(3) {
        let y = p[0] as f64;
        let cb = p[1] as f64 - c;
        let cr = p[2] as f64 - c;
        p[0] = to_sample(y + 1.4020 * cr + 0.5, max);
        p[1] = to_sample(y - 0.3441 * cb - 0.7141 * cr + 0.5, max);
        p[2] = to_sample(y + 1.7720 * cb + 0.5, max);
    }
}

fn to_sample(value: f64, max: f64) -> u16 {
    value.clamp(0.0, max) as u16
}
pub(crate) fn is_supported(pi: PhotometricInterpretation, c: u8) -> bool {
    match c {
        1 => matches!(
            pi,
            PhotometricInterpretation::Monochrome1 | PhotometricInterpretation::Monochrome2
        ),
        3 => matches!(
            pi,
            PhotometricInterpretation::Rgb | PhotometricInterpretation::YbrFull
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::ybr_to_rgb;

    #[test]
    fn matches_fo_dicom_full_range_byte_conversion() {
        let mut pixels = [128, 128, 128, 100, 90, 200, 0, 0, 0];
        ybr_to_rgb(&mut pixels, 8);
        assert_eq!(pixels, [128, 128, 128, 201, 62, 33, 0, 135, 0]);
    }
}
