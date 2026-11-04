//! DICOM RLE Lossless codec.

mod frame;
mod header;
mod layout;
mod segment;

use frame::{decode_frame, encode_frame};
use layout::RleLayout;
use radicom::error::{CodecError, Error};
use radicom::model::item::{DicomFragmentSequence, DicomItem};
use radicom::model::tag::Tag;
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::model::vr::Vr;
use radicom::pixel::codec::{Codec, CodecParams};
use radicom::pixel::{PixelData, PixelDataRef};

/// Upper bound for one decoded frame or one RLE segment allocation.
///
/// A 64-byte header plus a few compressed bytes must never let declared pixel
/// metadata allocate gigabytes. The limit is deliberately below common
/// container memory ceilings; a legitimate frame above it needs a larger
/// deployment-specific budget.
///
/// ponytail: fixed 1 GiB ceiling; make it configurable only when a deployment
/// has a verified large-frame need.
const MAX_DECODED_BYTES: usize = 1 << 30;

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
        if src.transfer_syntax().is_encapsulated() {
            return Err(operation_error(
                "encode",
                None,
                "source transfer syntax must be native",
            ));
        }
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
        ensure_plausible_frame_count(src, &layout)
            .map_err(|message| operation_error("encode", None, message))?;
        let mut encoded_frames = Vec::new();
        let mut accumulated_bytes = 0usize;
        for frame_index in 0..src.number_of_frames() {
            let buffer = src
                .get_frame(frame_index)
                .map_err(|error| operation_error("encode", Some(frame_index), error.to_string()))?;
            let bytes = buffer
                .data()
                .map_err(|error| operation_error("encode", Some(frame_index), error.to_string()))?;
            let encoded = encode_frame(&layout, bytes.as_ref())
                .map_err(|message| operation_error("encode", Some(frame_index), message))?;
            accumulated_bytes = accumulated_bytes
                .checked_add(encoded.len())
                .ok_or_else(|| {
                    operation_error(
                        "encode",
                        Some(frame_index),
                        "accumulated encoded output overflows usize",
                    )
                })?;
            if accumulated_bytes > MAX_DECODED_BYTES {
                return Err(operation_error(
                    "encode",
                    Some(frame_index),
                    format!(
                        "accumulated encoded output exceeds the {MAX_DECODED_BYTES}-byte limit"
                    ),
                ));
            }
            encoded_frames.push((frame_index, encoded));
        }
        for (frame_index, frame) in encoded_frames {
            dst.add_frame(frame)
                .map_err(|error| operation_error("encode", Some(frame_index), error.to_string()))?;
        }
        set_encapsulated_pixel_data_vr_to_ob(dst);
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
            decoded_frames.push((
                frame_index,
                decode_frame(&layout, bytes.as_ref())
                    .map_err(|message| operation_error("decode", Some(frame_index), message))?,
            ));
        }
        for (frame_index, frame) in decoded_frames {
            dst.add_frame(frame)
                .map_err(|error| operation_error("decode", Some(frame_index), error.to_string()))?;
        }
        Ok(())
    }
}

/// PS3.5 8.2.2 requires OB for encapsulated Pixel Data. The core picks OW from
/// `BitsAllocated` when it first appends a 16-bit frame; rebuild the fragment
/// sequence with the correct VR while preserving the offset table and buffers.
fn set_encapsulated_pixel_data_vr_to_ob(dst: &mut PixelData<'_>) {
    let Some(DicomItem::Fragments(fragments)) = dst.dataset().item(&Tag::PIXEL_DATA) else {
        return;
    };
    if fragments.vr() == Vr::Ob {
        return;
    }
    let mut corrected = DicomFragmentSequence::new(Tag::PIXEL_DATA, Vr::Ob);
    for fragment in fragments.fragments() {
        corrected.push_fragment_raw(fragment.clone());
    }
    corrected.set_offset_table(fragments.offset_table().to_vec());
    dst.dataset_mut()
        .insert_or_update(DicomItem::Fragments(corrected));
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

/// Rejects a declared frame count the native source pixel data cannot hold,
/// before the encode preflight trusts it. `NumberOfFrames` is untrusted
/// metadata: a tiny pixel buffer declaring a huge count would otherwise make the
/// encoder iterate one (>=64-byte) encoded frame per pixel byte and accumulate
/// gigabytes. Returns `Ok(())` when the source buffer length is unknown, leaving
/// the accumulated-size guard in `encode` as the bound.
fn ensure_plausible_frame_count(src: &PixelDataRef<'_>, layout: &RleLayout) -> Result<(), String> {
    let Some(DicomItem::Element(element)) = src.dataset().item(&Tag::PIXEL_DATA) else {
        return Ok(());
    };
    let Some(buffer) = element.value().as_buffer() else {
        return Ok(());
    };
    let max_frames = if layout.bit_packed {
        // Single-bit frames share bytes, so the frame count is bounded by the
        // total bit capacity divided by the bits in one frame (`pixel_count`).
        buffer.size().saturating_mul(8) / (layout.pixel_count as u64)
    } else {
        buffer.size() / (layout.frame_length as u64)
    };
    let declared = u64::from(src.number_of_frames());
    if declared > max_frames {
        return Err(format!(
            "declared NumberOfFrames {declared} exceeds the {max_frames} frames the native pixel data can hold"
        ));
    }
    Ok(())
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
    use radicom::DicomItem;

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
    fn rejects_an_untrusted_frame_count_before_encoding() {
        // A tiny native source must not let a huge `NumberOfFrames` drive the
        // encode preflight into iterating/accumulating an encoded frame per
        // pixel byte.
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
        raw.insert_str(Tag::NUMBER_OF_FRAMES, Vr::Is, u32::MAX.to_string());
        let source = PixelDataRef::from_dataset(&raw).unwrap();
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 1, 8, 1, None, 1);
        let mut target = PixelData::create_new(&mut compressed).unwrap();
        let error = RleLosslessCodec
            .encode(&source, &mut target, &CodecParams::None)
            .unwrap_err();
        assert!(error.to_string().contains("NumberOfFrames"), "{error}");
        assert!(!compressed.contains(&Tag::PIXEL_DATA));
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

    #[test]
    fn rejects_an_encapsulated_source() {
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 1, 8, 1, None, 1);
        {
            let mut pixels = PixelData::create_new(&mut compressed).unwrap();
            pixels.add_frame([0u8; 64]).unwrap();
        }
        let source = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut target = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 1, 8, 1, None, 1);
        let mut target_pixels = PixelData::create_new(&mut target).unwrap();
        let error = RleLosslessCodec
            .encode(&source, &mut target_pixels, &CodecParams::None)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("source transfer syntax must be native"),
            "{error}"
        );
        assert!(!target.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_pixel_representation_outside_zero_and_one() {
        let mut raw = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            None,
            1,
        );
        raw.insert_value(Tag::PIXEL_REPRESENTATION, 2u16);
        PixelData::create_new(&mut raw)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let source = PixelDataRef::from_dataset(&raw).unwrap();
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 1, 8, 1, None, 1);
        let mut target = PixelData::create_new(&mut compressed).unwrap();
        let error = RleLosslessCodec
            .encode(&source, &mut target, &CodecParams::None)
            .unwrap_err();
        assert!(
            error.to_string().contains("PixelRepresentation 2"),
            "{error}"
        );
        assert!(!compressed.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn encodes_encapsulated_16_bit_pixel_data_as_ob() {
        let frame = vec![0x12, 0x34, 0xAB, 0xCD];
        let (actual, compressed) = round_trip(1, 2, 16, 1, None, std::slice::from_ref(&frame));
        assert_eq!(actual, vec![frame]);
        let item = compressed.item(&Tag::PIXEL_DATA).expect("pixel data");
        assert!(matches!(item, DicomItem::Fragments(_)), "{item:?}");
        assert_eq!(item.vr(), Vr::Ob);
    }

    #[test]
    fn round_trips_fifteen_segments() {
        let frame: Vec<u8> = (0..60).map(|value| value as u8).collect();
        let (actual, compressed) = round_trip(2, 2, 8, 15, None, std::slice::from_ref(&frame));
        assert_eq!(actual, vec![frame]);
        let view = PixelDataRef::from_dataset(&compressed).unwrap();
        let frame = view.get_frame(0).unwrap();
        let bytes = frame.data().unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 15);
    }

    #[test]
    fn rejects_sixteen_segments() {
        let mut raw = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            16,
            None,
            1,
        );
        PixelData::create_new(&mut raw)
            .unwrap()
            .add_frame([0u8; 16])
            .unwrap();
        let source = PixelDataRef::from_dataset(&raw).unwrap();
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 1, 8, 16, None, 1);
        let mut target = PixelData::create_new(&mut compressed).unwrap();
        let error = RleLosslessCodec
            .encode(&source, &mut target, &CodecParams::None)
            .unwrap_err();
        assert!(error.to_string().contains("segment count 16"), "{error}");
        assert!(!compressed.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_a_declared_frame_size_before_allocating_it() {
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1000, 1000, 8, 1, None, 1);
        {
            let mut frame = vec![0u8; 64];
            frame[0] = 1;
            frame[4] = 64;
            frame.extend_from_slice(&[0x00, 0x2a]);
            PixelData::create_new(&mut compressed)
                .unwrap()
                .add_frame(frame)
                .unwrap();
        }
        let source = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1000,
            1000,
            8,
            1,
            None,
            1,
        );
        let mut target = PixelData::create_new(&mut decoded).unwrap();
        let error = RleLosslessCodec
            .decode(&source, &mut target, &CodecParams::None)
            .unwrap_err();
        assert!(error.to_string().contains("expansion"), "{error}");
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn does_not_append_a_first_frame_when_a_later_frame_fails() {
        let mut first = vec![0u8; 68];
        first[0] = 1;
        first[4] = 64;
        first[64..].copy_from_slice(&[2, 1, 2, 3]);
        let mut compressed = dataset(syntaxes::RLE_LOSSLESS.clone(), 1, 3, 8, 1, None, 2);
        {
            let mut pixels = PixelData::create_new(&mut compressed).unwrap();
            pixels.add_frame(first).unwrap();
            pixels.add_frame(vec![0u8; 10]).unwrap();
        }
        let source = PixelDataRef::from_dataset(&compressed).unwrap();
        let mut decoded = dataset(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            3,
            8,
            1,
            None,
            2,
        );
        let mut target = PixelData::create_new(&mut decoded).unwrap();
        let error = RleLosslessCodec
            .decode(&source, &mut target, &CodecParams::None)
            .unwrap_err();
        assert!(error.to_string().contains("frame 1"), "{error}");
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }
}
