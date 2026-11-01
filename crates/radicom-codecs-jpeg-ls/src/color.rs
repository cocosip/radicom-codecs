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

/// Colour transform container width. CharLS applies the HP transforms to the
/// sample container, so 8-bit samples wrap modulo 256 and anything wider wraps
/// modulo 65536 regardless of `BitsStored`.
fn container_width(bits: u8) -> u32 {
    if bits <= 8 {
        8
    } else {
        16
    }
}

/// Forward colour transformation over interleaved `R,G,B` samples, matching
/// CharLS `color_transform.h` `transform_hp1/hp2/hp3` (a CharLS extension outside
/// the T.87 bit stream, signalled by an APP8 `mrfx` segment).
pub(crate) fn forward_transform(samples: &mut [u16], transform: u8, bits: u8) {
    let modulus = 1i32 << container_width(bits);
    let half = modulus / 2;
    let quarter = modulus / 4;
    for p in samples.chunks_exact_mut(3) {
        let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
        let (v1, v2, v3) = match transform {
            1 => (r - g + half, g, b - g + half),
            2 => (r - g + half, g, b - ((r + g) >> 1) - half),
            3 => {
                // CharLS casts v2/v3 to the container before using them in v1.
                let v2 = (b - g + half).rem_euclid(modulus);
                let v3 = (r - g + half).rem_euclid(modulus);
                (g + ((v2 + v3) >> 2) - quarter, v2, v3)
            }
            _ => (r, g, b),
        };
        p[0] = v1.rem_euclid(modulus) as u16;
        p[1] = v2.rem_euclid(modulus) as u16;
        p[2] = v3.rem_euclid(modulus) as u16;
    }
}

/// Inverse of [`forward_transform`], matching CharLS `transform_hp*.inverse`.
pub(crate) fn inverse_transform(samples: &mut [u16], transform: u8, bits: u8) {
    let modulus = 1i32 << container_width(bits);
    let half = modulus / 2;
    let quarter = modulus / 4;
    for p in samples.chunks_exact_mut(3) {
        let (v1, v2, v3) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
        let (r, g, b) = match transform {
            1 => (v1 + v2 - half, v2, v3 + v2 - half),
            2 => {
                // CharLS wraps the new red sample before averaging it with green.
                let r = (v1 + v2 - half).rem_euclid(modulus);
                let g = v2;
                (r, g, v3 + ((r + g) >> 1) - half)
            }
            3 => {
                let g = v1 - ((v3 + v2) >> 2) + quarter;
                (v3 + g - half, g, v2 + g - half)
            }
            _ => (v1, v2, v3),
        };
        p[0] = r.rem_euclid(modulus) as u16;
        p[1] = g.rem_euclid(modulus) as u16;
        p[2] = b.rem_euclid(modulus) as u16;
    }
}
pub(crate) fn is_supported(pi: PhotometricInterpretation, c: u8) -> bool {
    match c {
        // PS3.5 Table 8.2.3-1 allows PALETTE COLOR for JPEG-LS Lossless:
        // one index sample, so the codec never converts or relabels it.
        1 => matches!(
            pi,
            PhotometricInterpretation::Monochrome1
                | PhotometricInterpretation::Monochrome2
                | PhotometricInterpretation::PaletteColor
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
    use super::{forward_transform, inverse_transform, ybr_to_rgb};

    #[test]
    fn matches_fo_dicom_full_range_byte_conversion() {
        let mut pixels = [128, 128, 128, 100, 90, 200, 0, 0, 0];
        ybr_to_rgb(&mut pixels, 8);
        assert_eq!(pixels, [128, 128, 128, 201, 62, 33, 0, 135, 0]);
    }

    #[test]
    fn hp_transforms_match_the_charls_formulas() {
        // CharLS color_transform.h with range_ = 256: HP1 v1 = R-G+128,
        // v3 = B-G+128; HP2 v3 = B-((R+G)>>1)-128; HP3 v2 = B-G+128,
        // v3 = R-G+128, v1 = G+((v2+v3)>>2)-64.
        let original = [200u16, 100, 50];
        for (transform, expected) in [
            (1u8, [228u16, 100, 78]),
            (2, [228, 100, 28]),
            (3, [112, 78, 228]),
        ] {
            let mut samples = original;
            forward_transform(&mut samples, transform, 8);
            assert_eq!(samples, expected, "forward HP{transform}");
            inverse_transform(&mut samples, transform, 8);
            assert_eq!(samples, original, "inverse HP{transform}");
        }
    }

    #[test]
    fn hp_transforms_wrap_on_the_sample_container() {
        // CharLS transforms the container type, so 12-bit samples in a 16-bit
        // container use range_ = 65536 rather than 4096.
        let original = [3000u16, 1000, 200];
        for transform in 1..=3 {
            let mut samples = original;
            forward_transform(&mut samples, transform, 12);
            assert_ne!(samples, original);
            inverse_transform(&mut samples, transform, 12);
            assert_eq!(samples, original);
        }
        // Values at the container limits must wrap instead of saturating.
        for transform in 1..=3 {
            let mut samples = [65535u16, 0, 32768];
            forward_transform(&mut samples, transform, 16);
            inverse_transform(&mut samples, transform, 16);
            assert_eq!(samples, [65535, 0, 32768]);
        }
    }
}
