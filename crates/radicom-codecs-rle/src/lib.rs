//! DICOM RLE Lossless codec.

mod frame;
mod header;
mod layout;
mod segment;

use frame::{decode_frame, encode_frame};
use layout::RleLayout;
use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams};
use radicom::pixel::{PixelData, PixelDataRef};

#[derive(Debug, Default, Clone, Copy)]
pub struct RleLosslessCodec;

impl Codec for RleLosslessCodec {
    fn name(&self) -> &str {
        "RLE Lossless"
    }

    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::RLE_LOSSLESS
    }

    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        ensure_params(params, "encode")?;
        if dst.transfer_syntax().uid() != self.transfer_syntax().uid() {
            return Err(operation_error(
                "encode",
                None,
                format!(
                    "destination transfer syntax is {}, expected {}",
                    dst.transfer_syntax().uid(),
                    self.transfer_syntax().uid()
                ),
            ));
        }
        let layout = RleLayout::from_pixel_data(src)
            .map_err(|message| operation_error("encode", None, message))?;
        let mut encoded_frames = Vec::new();
        for frame_index in 0..src.number_of_frames() {
            let buffer = src
                .get_frame(frame_index)
                .map_err(|error| operation_error("encode", Some(frame_index), error.to_string()))?;
            let bytes = buffer
                .data()
                .map_err(|error| operation_error("encode", Some(frame_index), error.to_string()))?;
            encoded_frames.push(
                encode_frame(&layout, bytes.as_ref())
                    .map_err(|message| operation_error("encode", Some(frame_index), message))?,
            );
        }
        for frame in encoded_frames {
            dst.add_frame(frame)
                .map_err(|error| operation_error("encode", None, error.to_string()))?;
        }
        Ok(())
    }

    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        ensure_params(params, "decode")?;
        if src.transfer_syntax().uid() != self.transfer_syntax().uid() {
            return Err(operation_error(
                "decode",
                None,
                format!(
                    "source transfer syntax is {}, expected {}",
                    src.transfer_syntax().uid(),
                    self.transfer_syntax().uid()
                ),
            ));
        }
        if dst.transfer_syntax().is_encapsulated() {
            return Err(operation_error(
                "decode",
                None,
                "destination transfer syntax must be native",
            ));
        }
        let layout = RleLayout::from_pixel_data_mut(dst)
            .map_err(|message| operation_error("decode", None, message))?;
        let mut decoded_frames = Vec::new();
        for frame_index in 0..src.number_of_frames() {
            let buffer = src
                .get_frame(frame_index)
                .map_err(|error| operation_error("decode", Some(frame_index), error.to_string()))?;
            let bytes = buffer
                .data()
                .map_err(|error| operation_error("decode", Some(frame_index), error.to_string()))?;
            decoded_frames.push(
                decode_frame(&layout, bytes.as_ref())
                    .map_err(|message| operation_error("decode", Some(frame_index), message))?,
            );
        }
        for frame in decoded_frames {
            dst.add_frame(frame)
                .map_err(|error| operation_error("decode", None, error.to_string()))?;
        }
        Ok(())
    }
}

fn ensure_params(params: &CodecParams, operation: &str) -> Result<(), Error> {
    if matches!(params, CodecParams::None) {
        Ok(())
    } else {
        Err(operation_error(
            operation,
            None,
            "RLE Lossless accepts only CodecParams::None",
        ))
    }
}

fn operation_error(operation: &str, frame: Option<u32>, message: impl Into<String>) -> Error {
    let context = frame
        .map(|index| format!(" frame {index}"))
        .unwrap_or_default();
    CodecError::Operation {
        codec: "RLE Lossless".to_owned(),
        message: format!("{operation}{context}: {}", message.into()),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use radicom::model::dataset::Dataset;
    use radicom::model::tag::Tag;
    use radicom::model::transfer_syntax::syntaxes;
    use radicom::model::vr::Vr;
    use radicom::pixel::{PixelData, PixelDataRef};

    fn dataset(
        syntax: Arc<TransferSyntax>,
        rows: u32,
        columns: u32,
        bits: u16,
        samples: u16,
        planar: Option<u16>,
        frames: u32,
    ) -> Dataset {
        let mut ds = Dataset::new();
        ds.set_transfer_syntax(syntax);
        ds.insert_value(Tag::ROWS, rows);
        ds.insert_value(Tag::COLUMNS, columns);
        ds.insert_value(Tag::BITS_ALLOCATED, bits);
        ds.insert_value(Tag::BITS_STORED, bits);
        ds.insert_value(Tag::HIGH_BIT, bits - 1);
        ds.insert_value(Tag::SAMPLES_PER_PIXEL, samples);
        ds.insert_value(Tag::PIXEL_REPRESENTATION, 0u16);
        ds.insert_str(
            Tag::PHOTOMETRIC_INTERPRETATION,
            Vr::Cs,
            if samples == 1 { "MONOCHROME2" } else { "RGB" },
        );
        if let Some(planar) = planar {
            ds.insert_value(Tag::PLANAR_CONFIGURATION, planar);
        }
        if frames > 1 {
            ds.insert_str(Tag::NUMBER_OF_FRAMES, Vr::Is, frames.to_string());
        }
        ds
    }

    fn pack_bits(bits: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (index, &bit) in bits.iter().enumerate() {
            if bit == 1 {
                bytes[index / 8] |= 1 << (index % 8);
            }
        }
        bytes
    }

    fn round_trip(
        rows: u32,
        columns: u32,
        bits: u16,
        samples: u16,
        planar: Option<u16>,
        frames: &[Vec<u8>],
    ) -> (Vec<Vec<u8>>, Dataset) {
        let mut raw = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            rows,
            columns,
            bits,
            samples,
            planar,
            frames.len() as u32,
        );
        {
            let mut pixels = PixelData::create_new(&mut raw).unwrap();
            for frame in frames {
                pixels.add_frame(frame).unwrap();
            }
        }
        let mut compressed = raw.clone();
        compressed.set_transfer_syntax(syntaxes::RLE_LOSSLESS.clone());
        compressed.remove(Tag::PIXEL_DATA);
        compressed.remove(Tag::NUMBER_OF_FRAMES);
        {
            let source = PixelDataRef::from_dataset(&raw).unwrap();
            let mut target = PixelData::create_new(&mut compressed).unwrap();
            RleLosslessCodec
                .encode(&source, &mut target, &CodecParams::None)
                .unwrap();
        }
        let mut decoded = compressed.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        decoded.remove(Tag::NUMBER_OF_FRAMES);
        {
            let source = PixelDataRef::from_dataset(&compressed).unwrap();
            let mut target = PixelData::create_new(&mut decoded).unwrap();
            RleLosslessCodec
                .decode(&source, &mut target, &CodecParams::None)
                .unwrap();
        }
        let view = PixelDataRef::from_dataset(&decoded).unwrap();
        let actual = (0..view.number_of_frames())
            .map(|index| view.get_frame(index).unwrap().data().unwrap().to_vec())
            .collect();
        (actual, compressed)
    }

    #[test]
    fn exposes_rle_transfer_syntax_and_default_params() {
        assert_eq!(
            RleLosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.5"
        );
        assert_eq!(RleLosslessCodec.default_params(), CodecParams::None);
    }

    #[test]
    fn round_trips_8_bit_monochrome() {
        let source = vec![0, 1, 1, 1, 2, 3, 4, 5, 6, 7];
        let (actual, _) = round_trip(2, 5, 8, 1, None, std::slice::from_ref(&source));
        assert_eq!(actual, vec![source]);
    }

    #[test]
    fn round_trips_16_bit_monochrome_with_high_and_low_planes() {
        let source = vec![0x12, 0x34, 0xAB, 0xCD, 0x00, 0xFF, 0x80, 0x01];
        let (actual, _) = round_trip(1, 4, 16, 1, None, std::slice::from_ref(&source));
        assert_eq!(actual, vec![source]);
    }

    #[test]
    fn round_trips_one_bit_monochrome_with_odd_geometry() {
        // 13 columns x 7 rows = 91 bits = 12 bytes; rows are not byte-aligned.
        let bits: Vec<u8> = (0..91)
            .map(|index| u8::from(index % 3 == 0 || index % 7 == 1))
            .collect();
        let source = pack_bits(&bits);
        assert_eq!(source.len(), 12);
        assert_eq!(source[11] & 0xF8, 0);
        let (actual, _) = round_trip(7, 13, 1, 1, None, std::slice::from_ref(&source));
        assert_eq!(actual, vec![source]);
    }

    #[test]
    fn round_trips_one_bit_monochrome_multiple_frames() {
        let first = pack_bits(
            &(0..91)
                .map(|index| u8::from(index % 2 == 0))
                .collect::<Vec<_>>(),
        );
        let second = pack_bits(
            &(0..91)
                .map(|index| u8::from(index % 5 < 2))
                .collect::<Vec<_>>(),
        );
        let frames = vec![first, second];
        let (actual, compressed) = round_trip(7, 13, 1, 1, None, &frames);
        assert_eq!(actual, frames);
        assert_eq!(
            PixelDataRef::from_dataset(&compressed)
                .unwrap()
                .number_of_frames(),
            2
        );
    }

    #[test]
    fn round_trips_interleaved_and_planar_rgb() {
        let interleaved = vec![1, 2, 3, 4, 5, 6, 7, 8, 9];
        let planar = vec![1, 4, 7, 2, 5, 8, 3, 6, 9];
        let (actual_interleaved, _) =
            round_trip(1, 3, 8, 3, Some(0), std::slice::from_ref(&interleaved));
        let (actual_planar, _) = round_trip(1, 3, 8, 3, Some(1), std::slice::from_ref(&planar));
        assert_eq!(actual_interleaved, vec![interleaved]);
        assert_eq!(actual_planar, vec![planar]);
    }

    #[test]
    fn round_trips_multiple_frames() {
        let frames = vec![vec![1, 1, 2, 3], vec![4, 5, 5, 5]];
        let (actual, compressed) = round_trip(2, 2, 8, 1, None, &frames);
        assert_eq!(actual, frames);
        assert_eq!(
            PixelDataRef::from_dataset(&compressed)
                .unwrap()
                .number_of_frames(),
            2
        );
    }

    #[test]
    fn rejects_non_none_params_without_touching_destination() {
        let mut raw = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            None,
            1,
        );
        PixelData::create_new(&mut raw)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let source = PixelDataRef::from_dataset(&raw).unwrap();
        let mut target = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 1, 8, 1, None, 1);
        let mut target_pixels = PixelData::create_new(&mut target).unwrap();
        let error = RleLosslessCodec
            .encode(
                &source,
                &mut target_pixels,
                &CodecParams::Jpeg(Default::default()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("CodecParams::None"));
        assert!(!target.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn does_not_preallocate_for_an_untrusted_frame_count() {
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 3, 8, 1, None, 1);
        {
            let mut pixels = PixelData::create_new(&mut compressed).unwrap();
            pixels.add_frame([0u8; 64]).unwrap();
        }
        // NumberOfFrames is untrusted metadata: trusting it here would reserve
        // gigabytes before the first frame is even read.
        compressed.insert_str(Tag::NUMBER_OF_FRAMES, Vr::Is, u32::MAX.to_string());
        let source = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            3,
            8,
            1,
            None,
            1,
        );
        let mut target = PixelData::create_new(&mut decoded).unwrap();
        let result = RleLosslessCodec.decode(&source, &mut target, &CodecParams::None);
        assert!(result.is_err());
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_malformed_rle_without_appending_a_frame() {
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 3, 8, 1, None, 1);
        {
            let mut pixels = PixelData::create_new(&mut compressed).unwrap();
            pixels.add_frame([0; 63]).unwrap();
        }
        let source = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            3,
            8,
            1,
            None,
            1,
        );
        let mut target = PixelData::create_new(&mut decoded).unwrap();
        let error = RleLosslessCodec
            .decode(&source, &mut target, &CodecParams::None)
            .unwrap_err();
        assert!(error.to_string().contains("RLE Lossless"));
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }
}
