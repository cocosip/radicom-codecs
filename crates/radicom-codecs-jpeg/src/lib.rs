//! Classic JPEG codec registrations.
//!
//! Pure-Rust classic JPEG marker, entropy, DCT, sequential, and lossless
//! predictor codecs behind the frozen `radicom` codec boundary.

use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams, JpegParams};
use radicom::pixel::{PixelData, PixelDataRef};

mod bit;
mod color;
mod dct;
mod frame;
mod huffman;
mod lossless;
mod marker;
mod sequential;

/// Upper bound on the samples a single SOF may declare, shared with the other
/// pure-Rust codecs (`docs/design/radicom-codecs-design.md`). The decoders
/// check it before allocating so a crafted 65535x65535 header is a managed
/// error instead of an allocation failure.
pub(crate) const MAX_DECODED_SAMPLES: u64 = 1 << 28;

macro_rules! sequential_jpeg_codec {
    ($type:ident, $name:literal, $syntax:ident, $baseline:expr) => {
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $type;

        impl Codec for $type {
            fn name(&self) -> &str {
                $name
            }

            fn transfer_syntax(&self) -> &'static TransferSyntax {
                &syntaxes::$syntax
            }

            fn default_params(&self) -> CodecParams {
                CodecParams::Jpeg(JpegParams::default())
            }

            fn encode(
                &self,
                _src: &PixelDataRef<'_>,
                _dst: &mut PixelData<'_>,
                _params: &CodecParams,
            ) -> Result<(), Error> {
                let params = jpeg_params(_params, self.name(), "encode")?;
                ensure_syntax(
                    &_dst.transfer_syntax(),
                    self.transfer_syntax(),
                    self.name(),
                    "encode",
                )?;
                crate::frame::encode_sequential(_src, _dst, params, $name, $baseline)
            }

            fn decode(
                &self,
                _src: &PixelDataRef<'_>,
                _dst: &mut PixelData<'_>,
                _params: &CodecParams,
            ) -> Result<(), Error> {
                let params = jpeg_params(_params, self.name(), "decode")?;
                ensure_syntax(
                    &_src.transfer_syntax(),
                    self.transfer_syntax(),
                    self.name(),
                    "decode",
                )?;
                crate::frame::decode_sequential(_src, _dst, params, $name, $baseline)
            }
        }
    };
}

sequential_jpeg_codec!(
    JpegProcess1Codec,
    "JPEG Baseline Process 1",
    JPEG_PROCESS_1,
    true
);
sequential_jpeg_codec!(
    JpegProcess2_4Codec,
    "JPEG Extended Process 2/4",
    JPEG_PROCESS_2_4,
    false
);
#[derive(Debug, Default, Clone, Copy)]
pub struct JpegLossless14Codec;

impl Codec for JpegLossless14Codec {
    fn name(&self) -> &str {
        "JPEG Lossless Process 14"
    }

    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::JPEG_LOSSLESS_PROCESS_14
    }

    fn default_params(&self) -> CodecParams {
        CodecParams::Jpeg(JpegParams::default())
    }

    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        let params = jpeg_params(params, self.name(), "encode")?;
        ensure_syntax(
            &dst.transfer_syntax(),
            self.transfer_syntax(),
            self.name(),
            "encode",
        )?;
        let predictor = u8::try_from(params.predictor).map_err(|_| CodecError::Operation {
            codec: self.name().to_owned(),
            message: "predictor must fit the JPEG 1..=7 range".to_owned(),
        })?;
        frame::encode_lossless(src, dst, params, predictor, self.name())
    }

    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        let params = jpeg_params(params, self.name(), "decode")?;
        ensure_syntax(
            &src.transfer_syntax(),
            self.transfer_syntax(),
            self.name(),
            "decode",
        )?;
        frame::decode_lossless(
            src,
            dst,
            params,
            lossless::Selection::Process14,
            self.name(),
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JpegLossless14Sv1Codec;

impl Codec for JpegLossless14Sv1Codec {
    fn name(&self) -> &str {
        "JPEG Lossless Process 14 SV1"
    }

    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1
    }

    fn default_params(&self) -> CodecParams {
        CodecParams::Jpeg(JpegParams::default())
    }

    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        let params = jpeg_params(params, self.name(), "encode")?;
        ensure_syntax(
            &dst.transfer_syntax(),
            self.transfer_syntax(),
            self.name(),
            "encode",
        )?;
        // SV1 is Process 14 with selection value 1 and no point transform; a
        // lossless UID must not carry a lossy or higher-order stream.
        for (invalid, message) in [
            (params.predictor != 1, "SV1 requires predictor 1"),
            (
                params.point_transform != 0,
                "SV1 requires a zero point transform",
            ),
        ] {
            if invalid {
                return Err(CodecError::Operation {
                    codec: self.name().to_owned(),
                    message: message.to_owned(),
                }
                .into());
            }
        }
        frame::encode_lossless(src, dst, params, 1, self.name())
    }

    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        let params = jpeg_params(params, self.name(), "decode")?;
        ensure_syntax(
            &src.transfer_syntax(),
            self.transfer_syntax(),
            self.name(),
            "decode",
        )?;
        frame::decode_lossless(src, dst, params, lossless::Selection::Sv1, self.name())
    }
}

fn jpeg_params<'a>(
    params: &'a CodecParams,
    codec: &str,
    operation: &str,
) -> Result<&'a JpegParams, Error> {
    match params {
        CodecParams::Jpeg(params) => Ok(params),
        other => Err(CodecError::Operation {
            codec: codec.to_owned(),
            message: format!("{operation} requires CodecParams::Jpeg, got {other:?}"),
        }
        .into()),
    }
}

fn ensure_syntax(
    actual: &TransferSyntax,
    expected: &TransferSyntax,
    codec: &str,
    operation: &str,
) -> Result<(), Error> {
    if actual.uid() == expected.uid() {
        Ok(())
    } else {
        Err(CodecError::Operation {
            codec: codec.to_owned(),
            message: format!(
                "{operation} transfer syntax is {}, expected {}",
                actual.uid(),
                expected.uid()
            ),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_all_jpeg_transfer_syntaxes() {
        let codecs: [&dyn Codec; 4] = [
            &JpegProcess1Codec,
            &JpegProcess2_4Codec,
            &JpegLossless14Codec,
            &JpegLossless14Sv1Codec,
        ];
        assert_eq!(codecs[0].transfer_syntax().uid(), "1.2.840.10008.1.2.4.50");
        assert_eq!(codecs[1].transfer_syntax().uid(), "1.2.840.10008.1.2.4.51");
        assert_eq!(codecs[2].transfer_syntax().uid(), "1.2.840.10008.1.2.4.57");
        assert_eq!(codecs[3].transfer_syntax().uid(), "1.2.840.10008.1.2.4.70");
        assert!(codecs
            .iter()
            .all(|codec| matches!(codec.default_params(), CodecParams::Jpeg(_))));
    }

    fn dataset(
        syntax: std::sync::Arc<TransferSyntax>,
        rows: u32,
        columns: u32,
        bits: u16,
        samples: u16,
        planar: Option<u16>,
    ) -> radicom::model::dataset::Dataset {
        use radicom::model::dataset::Dataset;
        use radicom::model::tag::Tag;
        use radicom::model::vr::Vr;

        let mut dataset = Dataset::new();
        dataset.set_transfer_syntax(syntax);
        dataset.insert_value(Tag::ROWS, rows);
        dataset.insert_value(Tag::COLUMNS, columns);
        dataset.insert_value(Tag::BITS_ALLOCATED, bits);
        dataset.insert_value(Tag::BITS_STORED, bits);
        dataset.insert_value(Tag::HIGH_BIT, bits - 1);
        dataset.insert_value(Tag::SAMPLES_PER_PIXEL, samples);
        dataset.insert_value(Tag::PIXEL_REPRESENTATION, 0u16);
        dataset.insert_str(
            Tag::PHOTOMETRIC_INTERPRETATION,
            Vr::Cs,
            if samples == 1 { "MONOCHROME2" } else { "RGB" },
        );
        if let Some(planar) = planar {
            dataset.insert_value(Tag::PLANAR_CONFIGURATION, planar);
        }
        dataset
    }

    #[test]
    fn process14_round_trips_native_8_bit_pixel_data() {
        let source_bytes: Vec<u8> = (0..20).map(|value| value * 7).collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            4,
            5,
            8,
            1,
            None,
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&source_bytes)
            .unwrap();

        let mut compressed = dataset(syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(), 4, 5, 8, 1, None);
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        let mut compressed_pixels = PixelData::create_new(&mut compressed).unwrap();
        JpegLossless14Codec
            .encode(
                &source_view,
                &mut compressed_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();

        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            4,
            5,
            8,
            1,
            None,
        );
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded_pixels = PixelData::create_new(&mut decoded).unwrap();
        JpegLossless14Codec
            .decode(
                &compressed_view,
                &mut decoded_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let actual = PixelDataRef::from_dataset(&decoded)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        assert_eq!(actual, source_bytes);
    }

    #[test]
    fn process14_sv1_round_trips_native_16_bit_pixel_data() {
        let samples = [0u16, 1024, 32768, 65535, 7, 4095];
        let source_bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            3,
            16,
            1,
            None,
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&source_bytes)
            .unwrap();
        let mut compressed = dataset(
            syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone(),
            2,
            3,
            16,
            1,
            None,
        );
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        let mut compressed_pixels = PixelData::create_new(&mut compressed).unwrap();
        JpegLossless14Sv1Codec
            .encode(
                &source_view,
                &mut compressed_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            3,
            16,
            1,
            None,
        );
        let mut decoded_pixels = PixelData::create_new(&mut decoded).unwrap();
        JpegLossless14Sv1Codec
            .decode(
                &compressed_view,
                &mut decoded_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let actual = PixelDataRef::from_dataset(&decoded)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        assert_eq!(actual, source_bytes);
    }

    #[test]
    fn process14_round_trips_planar_rgb_multiple_frames() {
        let frames = [vec![1, 2, 3, 4, 5, 6], vec![7, 8, 9, 10, 11, 12]];
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            2,
            8,
            3,
            Some(1),
        );
        {
            let mut pixels = PixelData::create_new(&mut source).unwrap();
            for frame in &frames {
                pixels.add_frame(frame).unwrap();
            }
        }
        let mut compressed = dataset(
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            1,
            2,
            8,
            3,
            Some(1),
        );
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        let mut compressed_pixels = PixelData::create_new(&mut compressed).unwrap();
        JpegLossless14Codec
            .encode(
                &source_view,
                &mut compressed_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            2,
            8,
            3,
            Some(1),
        );
        let mut decoded_pixels = PixelData::create_new(&mut decoded).unwrap();
        JpegLossless14Codec
            .decode(
                &compressed_view,
                &mut decoded_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let decoded_view = PixelDataRef::from_dataset(&decoded).unwrap();
        for (index, expected) in frames.iter().enumerate() {
            assert_eq!(
                decoded_view
                    .get_frame(index as u32)
                    .unwrap()
                    .data()
                    .unwrap()
                    .as_ref(),
                expected
            );
        }
    }

    #[test]
    fn rejects_wrong_process_parameters_and_destination_syntax() {
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            None,
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let source_view = PixelDataRef::from_dataset(&source).unwrap();

        let mut wrong_syntax = dataset(
            syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone(),
            1,
            1,
            8,
            1,
            None,
        );
        let mut wrong_syntax_pixels = PixelData::create_new(&mut wrong_syntax).unwrap();
        assert!(JpegLossless14Codec
            .encode(
                &source_view,
                &mut wrong_syntax_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .is_err());
        assert!(!wrong_syntax.contains(&radicom::model::tag::Tag::PIXEL_DATA));

        let mut invalid_params =
            dataset(syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(), 1, 1, 8, 1, None);
        let mut invalid_pixels = PixelData::create_new(&mut invalid_params).unwrap();
        let params = JpegParams {
            predictor: 257,
            ..Default::default()
        };
        assert!(JpegLossless14Codec
            .encode(
                &source_view,
                &mut invalid_pixels,
                &CodecParams::Jpeg(params),
            )
            .is_err());
        assert!(!invalid_params.contains(&radicom::model::tag::Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_external_malformed_missing_dht_without_partial_frame() {
        let header = [
            0xff, 0xd8, // SOI
            0xff, 0xc3, 0x00, 0x0b, 0x08, 0x00, 0x40, 0x00, 0x40, 0x01, 0x01, 0x11, 0x00, 0xff,
            0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x01, 0x00, 0x00,
        ];
        let mut codestream = header.to_vec();
        codestream.extend([0x55; 128]);
        codestream.extend([0xff, 0xd9]);

        let mut compressed = dataset(
            syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone(),
            64,
            64,
            8,
            1,
            None,
        );
        PixelData::create_new(&mut compressed)
            .unwrap()
            .add_frame(codestream)
            .unwrap();
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();

        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            64,
            64,
            8,
            1,
            None,
        );
        let result = JpegLossless14Sv1Codec.decode(
            &compressed_view,
            &mut PixelData::create_new(&mut decoded).unwrap(),
            &CodecParams::Jpeg(JpegParams::default()),
        );
        assert!(result.is_err());
        assert!(!decoded.contains(&radicom::model::tag::Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_hostile_sof_dimensions_before_allocating() {
        use radicom::model::tag::Tag;

        // SOF0 declaring 65535x65535: a frame-sized buffer would be ~8.6 GB.
        // The destination is 1x1, so the frame must be rejected before any
        // allocation is attempted.
        let codestream = [
            0xff, 0xd8, // SOI
            0xff, 0xc0, 0x00, 0x0b, 0x08, 0xff, 0xff, 0xff, 0xff, 0x01, 0x01, 0x11,
            0x00, // SOF0: 8-bit, 65535x65535, one component
            0xff, 0xd9, // EOI
        ];
        let mut compressed = dataset(syntaxes::JPEG_PROCESS_1.clone(), 1, 1, 8, 1, None);
        PixelData::create_new(&mut compressed)
            .unwrap()
            .add_frame(codestream)
            .unwrap();
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            None,
        );
        let error = JpegProcess1Codec
            .decode(
                &compressed_view,
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("layout does not match destination"),
            "{error}"
        );
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn does_not_preallocate_for_an_untrusted_frame_count() {
        use radicom::model::tag::Tag;
        use radicom::model::vr::Vr;

        let mut compressed = dataset(syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(), 1, 1, 8, 1, None);
        PixelData::create_new(&mut compressed)
            .unwrap()
            .add_frame([0xff, 0xd8, 0xff, 0xd9])
            .unwrap();
        // NumberOfFrames is untrusted metadata: trusting it here would reserve
        // gigabytes before the first frame is even read.
        compressed.insert_str(Tag::NUMBER_OF_FRAMES, Vr::Is, u32::MAX.to_string());
        let view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            None,
        );
        let result = JpegLossless14Codec.decode(
            &view,
            &mut PixelData::create_new(&mut decoded).unwrap(),
            &CodecParams::Jpeg(JpegParams::default()),
        );
        assert!(result.is_err());
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_signed_subword_samples_instead_of_dropping_sign_bits() {
        use radicom::model::tag::Tag;

        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            16,
            1,
            None,
        );
        source.insert_value(Tag::BITS_STORED, 8u16);
        source.insert_value(Tag::HIGH_BIT, 7u16);
        source.insert_value(Tag::PIXEL_REPRESENTATION, 1u16);
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([0x80, 0xff])
            .unwrap();
        let mut compressed = dataset(
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            1,
            1,
            16,
            1,
            None,
        );
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        let mut compressed_pixels = PixelData::create_new(&mut compressed).unwrap();
        assert!(JpegLossless14Codec
            .encode(
                &source_view,
                &mut compressed_pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .is_err());
    }

    #[test]
    fn process1_round_trips_native_monochrome() {
        let source_bytes: Vec<u8> = (0..64).map(|v| v * 3).collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            8,
            8,
            8,
            1,
            None,
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&source_bytes)
            .unwrap();
        let mut compressed = dataset(syntaxes::JPEG_PROCESS_1.clone(), 8, 8, 8, 1, None);
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        JpegProcess1Codec
            .encode(
                &source_view,
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            8,
            8,
            8,
            1,
            None,
        );
        JpegProcess1Codec
            .decode(
                &compressed_view,
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let actual = PixelDataRef::from_dataset(&decoded)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        assert!(actual
            .iter()
            .zip(source_bytes)
            .all(|(a, b)| (*a as i16 - b as i16).abs() < 32));
    }

    #[test]
    fn accepts_nonzero_smoothing_and_rejects_out_of_range() {
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            None,
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        let mut compressed = dataset(syntaxes::JPEG_PROCESS_1.clone(), 1, 1, 8, 1, None);
        let params = JpegParams {
            smoothing_factor: 50,
            ..Default::default()
        };
        JpegProcess1Codec
            .encode(
                &source_view,
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &CodecParams::Jpeg(params),
            )
            .unwrap();
        assert!(compressed.contains(&radicom::model::tag::Tag::PIXEL_DATA));
        // Out-of-range smoothing stays a parameter error with no partial frame.
        let mut rejected = dataset(syntaxes::JPEG_PROCESS_1.clone(), 1, 1, 8, 1, None);
        let params = JpegParams {
            smoothing_factor: 101,
            ..Default::default()
        };
        assert!(JpegProcess1Codec
            .encode(
                &source_view,
                &mut PixelData::create_new(&mut rejected).unwrap(),
                &CodecParams::Jpeg(params),
            )
            .is_err());
        assert!(!rejected.contains(&radicom::model::tag::Tag::PIXEL_DATA));
    }

    #[test]
    fn process2_4_round_trips_12_bit_samples() {
        let samples: Vec<u16> = (0..64).map(|v| (v * 61) & 0x0fff).collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            8,
            8,
            16,
            1,
            None,
        );
        source.insert_value(radicom::model::tag::Tag::BITS_STORED, 12u16);
        source.insert_value(radicom::model::tag::Tag::HIGH_BIT, 11u16);
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&bytes)
            .unwrap();
        let mut compressed = dataset(syntaxes::JPEG_PROCESS_2_4.clone(), 8, 8, 16, 1, None);
        compressed.insert_value(radicom::model::tag::Tag::BITS_STORED, 12u16);
        compressed.insert_value(radicom::model::tag::Tag::HIGH_BIT, 11u16);
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        JpegProcess2_4Codec
            .encode(
                &source_view,
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = source.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(radicom::model::tag::Tag::PIXEL_DATA);
        JpegProcess2_4Codec
            .decode(
                &compressed_view,
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        let actual = PixelDataRef::from_dataset(&decoded)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        let actual: Vec<u16> = actual
            .chunks(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();
        assert!(actual
            .iter()
            .zip(samples)
            .all(|(a, b)| (*a as i32 - b as i32).abs() < 256));
    }

    #[test]
    fn process1_encodes_rgb_as_ybr_422_and_decodes_to_rgb() {
        let source_bytes: Vec<u8> = (0..17 * 9 * 3).map(|v| ((v * 23) % 256) as u8).collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            9,
            17,
            8,
            3,
            Some(0),
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&source_bytes)
            .unwrap();
        let mut compressed = dataset(syntaxes::JPEG_PROCESS_1.clone(), 9, 17, 8, 3, Some(0));
        let source_view = PixelDataRef::from_dataset(&source).unwrap();
        let params = JpegParams {
            sample_factor: radicom::pixel::codec::SampleFactor::Half,
            ..Default::default()
        };
        JpegProcess1Codec
            .encode(
                &source_view,
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &CodecParams::Jpeg(params),
            )
            .unwrap();
        assert_eq!(
            compressed
                .get_str(radicom::model::tag::Tag::PHOTOMETRIC_INTERPRETATION)
                .unwrap(),
            "YBR_FULL_422"
        );
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            9,
            17,
            8,
            3,
            Some(0),
        );
        let decode_params = JpegParams {
            convert_colorspace_to_rgb: true,
            ..Default::default()
        };
        JpegProcess1Codec
            .decode(
                &compressed_view,
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &CodecParams::Jpeg(decode_params),
            )
            .unwrap();
        let actual = PixelDataRef::from_dataset(&decoded)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        let max = actual
            .iter()
            .zip(source_bytes.iter())
            .map(|(a, b)| (*a as i16 - *b as i16).abs())
            .max()
            .unwrap();
        assert!(max < 180, "max={max}");
    }

    #[test]
    fn process1_422_conversion_normalizes_a_ybr422_destination() {
        use radicom::model::tag::Tag;
        use radicom::model::vr::Vr;
        let source_bytes: Vec<u8> = (0..17 * 9 * 3).map(|v| ((v * 29) % 256) as u8).collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            9,
            17,
            8,
            3,
            Some(0),
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&source_bytes)
            .unwrap();
        let mut compressed = dataset(syntaxes::JPEG_PROCESS_1.clone(), 9, 17, 8, 3, Some(0));
        let params = JpegParams {
            sample_factor: radicom::pixel::codec::SampleFactor::Half,
            ..Default::default()
        };
        JpegProcess1Codec
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &CodecParams::Jpeg(params),
            )
            .unwrap();
        assert_eq!(
            compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "YBR_FULL_422"
        );
        let compressed_view = PixelDataRef::from_dataset(&compressed).unwrap();

        let mut packed = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            9,
            17,
            8,
            3,
            Some(0),
        );
        packed.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL_422");
        JpegProcess1Codec
            .decode(
                &compressed_view,
                &mut PixelData::create_new(&mut packed).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .unwrap();
        assert_eq!(
            packed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "YBR_FULL_422"
        );
        assert_eq!(
            PixelDataRef::from_dataset(&packed)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .size(),
            324
        );

        let mut converted = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            9,
            17,
            8,
            3,
            Some(0),
        );
        converted.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL_422");
        JpegProcess1Codec
            .decode(
                &compressed_view,
                &mut PixelData::create_new(&mut converted).unwrap(),
                &CodecParams::Jpeg(JpegParams {
                    convert_colorspace_to_rgb: true,
                    ..Default::default()
                }),
            )
            .unwrap();
        assert_eq!(
            converted.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        let actual = PixelDataRef::from_dataset(&converted)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        assert_eq!(actual.len(), 17 * 9 * 3);
        let max = actual
            .iter()
            .zip(source_bytes.iter())
            .map(|(a, b)| (*a as i16 - *b as i16).abs())
            .max()
            .unwrap();
        assert!(max < 180, "max={max}");
    }

    #[test]
    fn sequential_jpeg_codes_signed_rgb_bit_patterns() {
        use radicom::model::tag::Tag;
        use radicom::model::vr::Vr;
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 53 + 71) as u8).collect();
        let mut source = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            8,
            6,
            8,
            3,
            None,
        );
        source.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "RGB");
        source.insert_value(Tag::PIXEL_REPRESENTATION, 1u16);
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let mut compressed = source.clone();
        compressed.set_transfer_syntax(syntaxes::JPEG_PROCESS_1.clone());
        compressed.remove(Tag::PIXEL_DATA);
        let params = CodecParams::Jpeg(JpegParams::default());
        JpegProcess1Codec
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &params,
            )
            .unwrap();
        assert_eq!(
            compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "YBR_FULL"
        );
        // The signed destination keeps its Pixel Representation and receives the
        // decoded bit patterns back.
        let mut decoded = source.clone();
        decoded.remove(Tag::PIXEL_DATA);
        let decode_params = CodecParams::Jpeg(JpegParams {
            convert_colorspace_to_rgb: false,
            ..JpegParams::default()
        });
        JpegProcess1Codec
            .decode(
                &PixelDataRef::from_dataset(&compressed).unwrap(),
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &decode_params,
            )
            .unwrap();
        let frame: Vec<u8> = PixelDataRef::from_dataset(&decoded)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec();
        assert_eq!(frame.len(), raw.len());
    }

    #[test]
    fn lossless_preserves_palette_color_without_conversion() {
        use radicom::model::tag::Tag;
        use radicom::model::vr::Vr;
        for bits in [8u16, 16] {
            let index: Vec<u8> = (0..20u32)
                .flat_map(|value| {
                    let sample = (value * 7) % 251;
                    if bits == 8 {
                        vec![sample as u8]
                    } else {
                        vec![(sample & 0xff) as u8, (sample >> 8) as u8]
                    }
                })
                .collect();
            let mut source = dataset(
                syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
                4,
                5,
                bits,
                1,
                None,
            );
            source.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "PALETTE COLOR");
            PixelData::create_new(&mut source)
                .unwrap()
                .add_frame(&index)
                .unwrap();
            let mut compressed = dataset(
                syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
                4,
                5,
                bits,
                1,
                None,
            );
            compressed.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "PALETTE COLOR");
            JpegLossless14Codec
                .encode(
                    &PixelDataRef::from_dataset(&source).unwrap(),
                    &mut PixelData::create_new(&mut compressed).unwrap(),
                    &CodecParams::Jpeg(JpegParams::default()),
                )
                .unwrap();
            let mut decoded = dataset(
                syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
                4,
                5,
                bits,
                1,
                None,
            );
            decoded.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "PALETTE COLOR");
            JpegLossless14Codec
                .decode(
                    &PixelDataRef::from_dataset(&compressed).unwrap(),
                    &mut PixelData::create_new(&mut decoded).unwrap(),
                    &CodecParams::Jpeg(JpegParams::default()),
                )
                .unwrap();
            assert_eq!(
                decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
                "PALETTE COLOR"
            );
            assert_eq!(
                PixelDataRef::from_dataset(&decoded)
                    .unwrap()
                    .get_frame(0)
                    .unwrap()
                    .data()
                    .unwrap()
                    .to_vec(),
                index,
                "bits={bits}"
            );

            // Sequential JPEG codes the LUT index plane as a one-component image
            // and leaves PALETTE COLOR in place, like the native reference codec.
            // It stays limited to 8-bit samples.
            let mut lossy = dataset(syntaxes::JPEG_PROCESS_1.clone(), 4, 5, bits, 1, None);
            lossy.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "PALETTE COLOR");
            let encoded = JpegProcess1Codec.encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut lossy).unwrap(),
                &CodecParams::Jpeg(JpegParams::default()),
            );
            if bits == 8 {
                encoded.unwrap();
                assert_eq!(
                    lossy.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
                    "PALETTE COLOR"
                );
            } else {
                assert!(encoded.is_err());
            }
        }
    }
}
