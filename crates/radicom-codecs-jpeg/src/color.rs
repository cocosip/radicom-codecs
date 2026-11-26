//! JPEG colour-space policy shared by the sequential and lossless encoders.
//!
//! fo-dicom's native glue passes the input colour space to libjpeg, and
//! `jpeg_set_colorspace`/`jpeg_set_defaults` then decide the component IDs,
//! the JFIF/Adobe APP marker, the quantization-table selectors and the
//! Huffman-table selectors. This module mirrors those decisions byte for byte
//! (`jcparam.c`, `jcmarker.c`).

#![allow(clippy::chunks_exact_to_as_chunks)]

use super::dct::DecodedSample;
use super::marker::{Marker, MarkerWriter};
use radicom::pixel::photometric::PhotometricInterpretation;

/// The `J_COLOR_SPACE` libjpeg ends up using for fo-dicom's encoder input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JpegColorSpace {
    /// `JCS_GRAYSCALE`: JFIF APP0, component ID 1, tables 0.
    Grayscale,
    /// `JCS_UNKNOWN`: no APP marker, component IDs 0.., tables 0.
    Unknown,
    /// `JCS_RGB`: Adobe APP14, component IDs 'R'/'G'/'B', tables 0.
    Rgb,
    /// `JCS_YCbCr`: JFIF APP0, component IDs 1/2/3, luma table 0 and chroma 1.
    Ycbcr,
}

impl JpegColorSpace {
    /// fo-dicom `JpegCodec.getJpegColorSpace` (non-turbo path).
    pub(crate) fn from_photometric(photometric: PhotometricInterpretation) -> Self {
        match photometric {
            PhotometricInterpretation::Rgb => Self::Rgb,
            PhotometricInterpretation::Monochrome1 | PhotometricInterpretation::Monochrome2 => {
                Self::Grayscale
            }
            PhotometricInterpretation::PaletteColor => Self::Unknown,
            PhotometricInterpretation::YbrFull
            | PhotometricInterpretation::YbrFull422
            | PhotometricInterpretation::YbrPartial422 => Self::Ycbcr,
            // The encoders reject every other photometric interpretation
            // before reaching this point; `JCS_UNKNOWN` is libjpeg's fallback.
            _ => Self::Unknown,
        }
    }

    /// Component identifier written to SOF and SOS (`jpeg_set_colorspace`).
    pub(crate) fn component_id(self, component: usize) -> u8 {
        match self {
            Self::Grayscale => 1,
            Self::Unknown => component as u8,
            Self::Rgb => b"RGB"[component],
            Self::Ycbcr => component as u8 + 1,
        }
    }

    /// Quantization- and Huffman-table selector for a component.
    pub(crate) fn table_id(self, component: usize) -> u8 {
        match self {
            Self::Ycbcr => u8::from(component != 0),
            Self::Grayscale | Self::Unknown | Self::Rgb => 0,
        }
    }

    /// SOI plus the APP marker `jpeg_set_colorspace` selects:
    /// JFIF APP0 for grayscale/YCbCr, Adobe APP14 for RGB, none for unknown.
    pub(crate) fn write_file_header(self, out: &mut MarkerWriter) -> Result<(), String> {
        out.write_marker(Marker::Soi)?;
        match self {
            Self::Grayscale | Self::Ycbcr => out.write_segment(Marker::App(0), &JFIF_APP0)?,
            Self::Rgb => out.write_segment(Marker::App(14), &ADOBE_APP14_RGB)?,
            Self::Unknown => {}
        }
        Ok(())
    }
}

/// JFIF 1.01, no density unit, 1x1 pixel density, no thumbnail.
const JFIF_APP0: [u8; 14] = [
    0x4a, 0x46, 0x49, 0x46, 0x00, // "JFIF\0"
    0x01, 0x01, // version 1.01
    0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
];

/// Adobe APP14: version 100, no flags, `transform = 0` (RGB).
const ADOBE_APP14_RGB: [u8; 12] = [
    0x41, 0x64, 0x6f, 0x62, 0x65, // "Adobe"
    0x00, 0x64, // version 100
    0x00, 0x00, // flags0
    0x00, 0x00, // flags1
    0x00, // colour transform
];

/// libjpeg's fixed-point RGB -> YCbCr conversion (`jccolor.c`,
/// `rgb_ycc_convert`). The input is converted in place, leaving Y, Cb and Cr in
/// the three interleaved components, exactly as the native encoder sees them.
pub(crate) fn rgb_to_ycc(samples: &mut [u16], precision: u8) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports the target feature of this loop.
        return unsafe { rgb_to_ycc_avx2(samples, precision) };
    }
    convert_rgb(samples, precision);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgb_to_ycc_avx2(samples: &mut [u16], precision: u8) {
    convert_rgb(samples, precision);
}

#[inline(always)]
fn convert_rgb(samples: &mut [u16], precision: u8) {
    let mask = (1 << precision) - 1;
    // jccolor.c rounds chroma half down so saturated primaries fit in range.
    let chroma_bias = (1i32 << (precision + 15)) + 32767;
    for [red, green, blue] in samples.as_chunks_mut::<3>().0 {
        let r = i32::from(*red & mask);
        let g = i32::from(*green & mask);
        let b = i32::from(*blue & mask);
        *red = ((19595 * r + 38470 * g + 7471 * b + 32768) >> 16) as u16;
        *green = ((32768 * b - 11059 * r - 21709 * g + chroma_bias) >> 16) as u16;
        *blue = ((32768 * r - 27439 * g - 5329 * b + chroma_bias) >> 16) as u16;
    }
}

pub(crate) fn ybr_to_rgb(samples: &mut [u16], precision: u8) {
    let max = (1 << precision) - 1;
    let center = 1 << (precision - 1);
    for pixel in samples.as_chunks_mut::<3>().0 {
        *pixel = rgb_pixel(pixel[0], pixel[1], pixel[2], center, max);
    }
}

#[inline]
fn rgb_pixel(y: u16, cb: u16, cr: u16, center: i32, max: i32) -> [u16; 3] {
    let luma = i32::from(y);
    let cb = i32::from(cb) - center;
    let cr = i32::from(cr) - center;
    // The fixed-point coefficients and rounding match libjpeg's jdcolor.c.
    [
        (luma + ((91881 * cr + 32768) >> 16)).clamp(0, max) as u16,
        (luma + ((32768 - 22554 * cb - 46802 * cr) >> 16)).clamp(0, max) as u16,
        (luma + ((116130 * cb + 32768) >> 16)).clamp(0, max) as u16,
    ]
}

pub(crate) fn ybr422_to_rgb<T: DecodedSample + Into<u16>>(
    samples: &mut [T],
    width: usize,
    precision: u8,
) {
    let max = (1 << precision) - 1;
    let center = 1 << (precision - 1);
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: The detected feature enables the eight-pixel row kernel.
        unsafe { ybr422_to_rgb_avx2(samples, width, center, max) };
        return;
    }
    for row in samples.chunks_exact_mut(width * 3) {
        convert_422_tail(row, None, center, max);
    }
}

fn convert_422_tail<T: DecodedSample + Into<u16>>(
    row: &mut [T],
    mut previous: Option<(u16, u16)>,
    center: i32,
    max: i32,
) {
    let interpolate = |here: u16, neighbor: u16, bias: u32| {
        ((3 * u32::from(here) + u32::from(neighbor) + bias) >> 2) as u16
    };
    let mut pixels = row.as_chunks_mut::<3>().0.chunks_mut(2).peekable();
    while let Some(pair) = pixels.next() {
        let [y, cb, cr] = pair[0].map(Into::into);
        let (left_cb, left_cr) = previous.unwrap_or((cb, cr));
        let (right_cb, right_cr) = pixels
            .peek()
            .map_or((cb, cr), |next| (next[0][1].into(), next[0][2].into()));
        pair[0] = rgb_pixel(
            y,
            interpolate(cb, left_cb, 1),
            interpolate(cr, left_cr, 1),
            center,
            max,
        )
        .map(T::from_word);
        if let Some(odd) = pair.get_mut(1) {
            *odd = rgb_pixel(
                odd[0].into(),
                interpolate(cb, right_cb, 2),
                interpolate(cr, right_cr, 2),
                center,
                max,
            )
            .map(T::from_word);
        }
        previous = Some((cb, cr));
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ybr422_to_rgb_avx2<T: DecodedSample + Into<u16>>(
    samples: &mut [T],
    width: usize,
    center: i32,
    max: i32,
) {
    use std::arch::x86_64::*;
    type ShuffleMasks = [[[u8; 16]; 3]; 3];
    const MASKS: (ShuffleMasks, ShuffleMasks) = {
        let mut unpack = [[[0x80; 16]; 3]; 3];
        let mut pack = [[[0x80; 16]; 3]; 3];
        let mut word = 0;
        while word < 24 {
            let channel = word % 3;
            let lane = word / 3 * 2;
            let register = word / 8;
            let byte = (word % 8) * 2;
            unpack[channel][register][lane] = byte as u8;
            unpack[channel][register][lane + 1] = byte as u8 + 1;
            pack[register][channel][byte] = lane as u8;
            pack[register][channel][byte + 1] = lane as u8 + 1;
            word += 1;
        }
        (unpack, pack)
    };
    let convert = |input: [__m128i; 3], masks: &[[u8; 16]; 3]| {
        // SAFETY: Each mask contains exactly 16 bytes.
        unsafe {
            let shuffled = std::array::from_fn::<_, 3, _>(|i| {
                _mm_shuffle_epi8(input[i], _mm_loadu_si128(masks[i].as_ptr().cast()))
            });
            _mm_or_si128(_mm_or_si128(shuffled[0], shuffled[1]), shuffled[2])
        }
    };
    let interpolate = |values, left: u16, right: u16| {
        let prev = _mm_or_si128(
            _mm_slli_si128::<2>(values),
            _mm_cvtsi32_si128(i32::from(left)),
        );
        let next = _mm_or_si128(
            _mm_srli_si128::<2>(values),
            _mm_slli_si128::<14>(_mm_cvtsi32_si128(i32::from(right))),
        );
        let adjacent = _mm_blend_epi16::<0xaa>(prev, next);
        let bias = _mm_set1_epi32(0x0002_0001);
        let three_times = _mm_add_epi16(values, _mm_slli_epi16::<1>(values));
        _mm_srli_epi16::<2>(_mm_add_epi16(_mm_add_epi16(three_times, adjacent), bias))
    };
    let center = _mm256_set1_epi32(center);
    let max_vec = _mm256_set1_epi32(max);
    let add = _mm256_add_epi32;
    let sub = _mm256_sub_epi32;
    let mul = |values, factor| _mm256_mullo_epi32(values, _mm256_set1_epi32(factor));
    let bias = _mm256_set1_epi32(32768);
    for row in samples.chunks_exact_mut(width * 3) {
        let mut previous = (row[1].into(), row[2].into());
        let mut offset = 0;
        while offset + 24 <= row.len() {
            let next = if offset + 24 < row.len() {
                offset + 24
            } else {
                offset + 18
            };
            let cb_next = row[next + 1].into();
            let cr_next = row[next + 2].into();
            let last = (row[offset + 19].into(), row[offset + 20].into());
            // SAFETY: The loop bounds leave room for three unaligned registers.
            unsafe {
                let block = row.as_mut_ptr().add(offset);
                let input = std::array::from_fn(|i| T::load_row(block.add(i * 8)));
                let y = _mm256_cvtepu16_epi32(convert(input, &MASKS.0[0]));
                let cb = sub(
                    _mm256_cvtepu16_epi32(interpolate(
                        convert(input, &MASKS.0[1]),
                        previous.0,
                        cb_next,
                    )),
                    center,
                );
                let cr = sub(
                    _mm256_cvtepu16_epi32(interpolate(
                        convert(input, &MASKS.0[2]),
                        previous.1,
                        cr_next,
                    )),
                    center,
                );
                let deltas = [
                    add(mul(cr, 91881), bias),
                    sub(sub(bias, mul(cb, 22554)), mul(cr, 46802)),
                    add(mul(cb, 116130), bias),
                ];
                let channels = deltas.map(|delta| {
                    let channel = _mm256_min_epi32(add(y, _mm256_srai_epi32::<16>(delta)), max_vec);
                    _mm_packus_epi32(
                        _mm256_castsi256_si128(channel),
                        _mm256_extracti128_si256::<1>(channel),
                    )
                });
                for (i, masks) in MASKS.1.iter().enumerate() {
                    T::store_row(block.add(i * 8), convert(channels, masks));
                }
            }
            previous = last;
            offset += 24;
        }
        convert_422_tail(
            &mut row[offset..],
            Some(previous),
            _mm256_extract_epi32::<0>(center),
            max,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ybr422_conversion_matches_the_scalar_filter_at_row_boundaries() {
        for width in 5..35 {
            for precision in [8, 12] {
                let max = (1 << precision) - 1;
                let center = 1 << (precision - 1);
                let mut actual: Vec<u16> = (0..width * 3 * 2)
                    .map(|i| {
                        let x = (i / 3) % width;
                        let index = if i % 3 == 0 { i } else { i - 3 * (x % 2) };
                        ((index * 379 + (index / 7) * 11) & max as usize) as u16
                    })
                    .collect();
                let mut expected = actual.clone();
                for row in expected.chunks_exact_mut(width * 3) {
                    convert_422_tail(row, None, center, max);
                }
                ybr422_to_rgb(&mut actual, width, precision);
                assert_eq!(actual, expected, "{width} columns at {precision} bits");
            }
        }
    }

    #[test]
    fn colour_space_component_and_table_ids_follow_libjpeg() {
        assert_eq!(JpegColorSpace::Grayscale.component_id(0), 1);
        assert_eq!(JpegColorSpace::Unknown.component_id(0), 0);
        assert_eq!(
            [
                JpegColorSpace::Rgb.component_id(0),
                JpegColorSpace::Rgb.component_id(1),
                JpegColorSpace::Rgb.component_id(2)
            ],
            *b"RGB"
        );
        assert_eq!(
            [
                JpegColorSpace::Ycbcr.component_id(0),
                JpegColorSpace::Ycbcr.component_id(1),
                JpegColorSpace::Ycbcr.component_id(2)
            ],
            [1, 2, 3]
        );
        assert_eq!(JpegColorSpace::Ycbcr.table_id(0), 0);
        assert_eq!(JpegColorSpace::Ycbcr.table_id(1), 1);
        assert_eq!(JpegColorSpace::Ycbcr.table_id(2), 1);
        assert_eq!(JpegColorSpace::Grayscale.table_id(0), 0);
    }

    #[test]
    fn writes_the_exact_jfif_and_adobe_prefixes() {
        let mut natural = MarkerWriter::default();
        JpegColorSpace::Grayscale
            .write_file_header(&mut natural)
            .unwrap();
        let mut expected = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        expected.extend(JFIF_APP0);
        assert_eq!(natural.into_bytes(), expected);

        let mut rgb = MarkerWriter::default();
        JpegColorSpace::Rgb.write_file_header(&mut rgb).unwrap();
        let mut expected = vec![0xff, 0xd8, 0xff, 0xee, 0x00, 0x0e];
        expected.extend(ADOBE_APP14_RGB);
        assert_eq!(rgb.into_bytes(), expected);

        let mut unknown = MarkerWriter::default();
        JpegColorSpace::Unknown
            .write_file_header(&mut unknown)
            .unwrap();
        assert_eq!(unknown.into_bytes(), vec![0xff, 0xd8]);
    }

    #[test]
    fn rgb_to_ycc_matches_the_fixed_point_reference() {
        // Hand-checked against jccolor.c's table values for the primaries and
        // two mixed colours.
        let mut samples = vec![
            0u16, 0, 0, 255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 17, 200, 240,
        ];
        rgb_to_ycc(&mut samples, 8);
        assert_eq!(
            &samples[..],
            &[
                0, 128, 128, // black
                255, 128, 128, // white
                76, 85, 255, // red
                150, 44, 21, // green
                29, 255, 107, // blue
                150, 179, 33, // mixed
            ]
        );
    }
}
