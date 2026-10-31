#![allow(clippy::chunks_exact_to_as_chunks)]
use radicom::pixel::photometric::PhotometricInterpretation;
pub(crate) fn ybr_to_rgb(s: &mut [u16], bits: u8) {
    let max = ((1u32 << bits) - 1) as f64;
    let c = max / 2.0;
    for p in s.chunks_exact_mut(3) {
        let y = p[0] as f64;
        let cb = p[1] as f64 - c;
        let cr = p[2] as f64 - c;
        p[0] = (y + 1.402 * cr).round().clamp(0.0, max) as u16;
        p[1] = (y - 0.344136 * cb - 0.714136 * cr).round().clamp(0.0, max) as u16;
        p[2] = (y + 1.772 * cb).round().clamp(0.0, max) as u16;
    }
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
