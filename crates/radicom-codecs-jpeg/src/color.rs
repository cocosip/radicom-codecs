//! JPEG colour-space policy shared by the sequential and lossless encoders.
//!
//! fo-dicom's native glue passes the input colour space to libjpeg, and
//! `jpeg_set_colorspace`/`jpeg_set_defaults` then decide the component IDs,
//! the JFIF/Adobe APP marker, the quantization-table selectors and the
//! Huffman-table selectors. This module mirrors those decisions byte for byte
//! (`jcparam.c`, `jcmarker.c`).

#![allow(clippy::chunks_exact_to_as_chunks)]

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
    const SCALEBITS: i32 = 16;
    const ONE_HALF: i32 = 1 << (SCALEBITS - 1);
    let max = (1i32 << precision) - 1;
    let center = 1i32 << (precision - 1);
    let cbcr_offset = center << SCALEBITS;
    let stride = max as usize + 1;

    #[inline]
    fn fix(value: f64) -> i32 {
        (value * 65536.0 + 0.5) as i32
    }

    // Table layout follows jccolor.c: R_Y, G_Y, B_Y, R_CB, G_CB, B_CB,
    // G_CR, B_CR; R_CR shares B_CB's table.
    let mut table = vec![0i32; 8 * stride];
    for i in 0..stride {
        let value = i as i32;
        let at = |slot: usize| slot * stride + i;
        table[at(0)] = fix(0.29900).wrapping_mul(value);
        table[at(1)] = fix(0.58700).wrapping_mul(value);
        table[at(2)] = fix(0.11400).wrapping_mul(value).wrapping_add(ONE_HALF);
        table[at(3)] = (-fix(0.16874)).wrapping_mul(value);
        table[at(4)] = (-fix(0.33126)).wrapping_mul(value);
        table[at(5)] = fix(0.50000)
            .wrapping_mul(value)
            .wrapping_add(cbcr_offset)
            .wrapping_add(ONE_HALF)
            .wrapping_sub(1);
        table[at(6)] = (-fix(0.41869)).wrapping_mul(value);
        table[at(7)] = (-fix(0.08131)).wrapping_mul(value);
    }

    // 12-bit samples arrive in a 16-bit type and are masked to the table range
    // just as `RANGE_LIMIT` does in jccolor.c.
    let mask = if precision == 12 { 0x0fff } else { max };
    for pixel in samples.chunks_exact_mut(3) {
        let r = i32::from(pixel[0]) & mask;
        let g = i32::from(pixel[1]) & mask;
        let b = i32::from(pixel[2]) & mask;
        let index = |slot: usize, value: i32| slot * stride + value as usize;
        let y = (table[index(0, r)] + table[index(1, g)] + table[index(2, b)]) >> SCALEBITS;
        let cb = (table[index(3, r)] + table[index(4, g)] + table[index(5, b)]) >> SCALEBITS;
        let cr = (table[index(5, r)] + table[index(6, g)] + table[index(7, b)]) >> SCALEBITS;
        pixel[0] = y as u16;
        pixel[1] = cb as u16;
        pixel[2] = cr as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
