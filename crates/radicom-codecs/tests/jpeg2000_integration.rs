use radicom::model::{
    buffer::MemoryBuffer, dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr,
};
use radicom::pixel::{
    codec::{Codec, CodecParams, CodecRegistry, J2kParams},
    PixelData, PixelDataRef,
};
use radicom_codecs::Jpeg2000LosslessCodec;

fn source_dataset(w: u32, h: u32, bits: u16, components: u16, signed: bool, pi: &str) -> Dataset {
    let mut d = Dataset::new();
    d.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    for (tag, value) in [
        (Tag::ROWS, u16::try_from(h).unwrap()),
        (Tag::COLUMNS, u16::try_from(w).unwrap()),
        (Tag::BITS_ALLOCATED, bits),
        (Tag::BITS_STORED, bits),
        (Tag::HIGH_BIT, bits - 1),
        (Tag::SAMPLES_PER_PIXEL, components),
        (Tag::PIXEL_REPRESENTATION, u16::from(signed)),
    ] {
        d.insert_value(tag, value);
    }
    d.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, pi);
    if components > 1 {
        d.insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
    }
    d
}

fn frame_bytes(dataset: &Dataset, index: u32) -> Vec<u8> {
    PixelDataRef::from_dataset(dataset)
        .unwrap()
        .get_frame(index)
        .unwrap()
        .data()
        .unwrap()
        .to_vec()
}

#[test]
fn registry_registers_both_jpeg2000_syntaxes() {
    let registry = radicom_codecs::install();
    assert!(registry.has_codec(&syntaxes::JPEG_2000_LOSSLESS));
    assert!(registry.has_codec(&syntaxes::JPEG_2000_LOSSY));
    assert!(registry.can_transcode(
        &syntaxes::EXPLICIT_VR_LITTLE_ENDIAN,
        &syntaxes::JPEG_2000_LOSSLESS
    ));
    assert!(registry.can_transcode(
        &syntaxes::JPEG_2000_LOSSY,
        &syntaxes::EXPLICIT_VR_LITTLE_ENDIAN
    ));
}

#[test]
fn transcodes_mono8_lossless_exactly() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..32 * 24).map(|i| (i * 37 + 11) as u8).collect();
    let mut source = source_dataset(32, 24, 8, 1, false, "MONOCHROME2");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame(&raw)
        .unwrap();
    let compressed = source
        .clone_with_syntax(
            syntaxes::JPEG_2000_LOSSLESS.clone(),
            Some(CodecParams::Jpeg2000(J2kParams {
                irreversible: false,
                ..Default::default()
            })),
        )
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    assert_eq!(frame_bytes(&decoded, 0), raw);
}

#[test]
fn transcodes_rgb_lossless_exactly() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..32 * 24 * 3).map(|i| (i * 19 + 5) as u8).collect();
    let mut source = source_dataset(32, 24, 8, 3, false, "RGB");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame(&raw)
        .unwrap();
    let compressed = source
        .clone_with_syntax(
            syntaxes::JPEG_2000_LOSSLESS.clone(),
            Some(CodecParams::Jpeg2000(J2kParams {
                irreversible: false,
                ..Default::default()
            })),
        )
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    assert_eq!(frame_bytes(&decoded, 0), raw);
}

#[test]
fn transcodes_lossy_within_tolerance() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..32 * 24).map(|i| (i * 7 + i / 32) as u8).collect();
    let mut source = source_dataset(32, 24, 8, 1, false, "MONOCHROME2");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame(&raw)
        .unwrap();
    let compressed = source
        .clone_with_syntax(
            syntaxes::JPEG_2000_LOSSY.clone(),
            Some(CodecParams::Jpeg2000(J2kParams::default())),
        )
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    let actual = frame_bytes(&decoded, 0);
    let mut total = 0i64;
    for (a, b) in raw.iter().zip(&actual) {
        total += i64::from((*a as i32 - *b as i32).abs());
    }
    let mean = total as f64 / raw.len() as f64;
    assert!(mean < 2.0, "mean absolute error {mean}");
}

#[test]
fn transcodes_multiple_frames() {
    let _ = radicom_codecs::install();
    let frames: Vec<Vec<u8>> = (0..2)
        .map(|frame| (0..32 * 24).map(|i| (i * 37 + frame * 53) as u8).collect())
        .collect();
    let mut source = source_dataset(32, 24, 8, 1, false, "MONOCHROME2");
    {
        let mut pixels = PixelData::create_new(&mut source).unwrap();
        for frame in &frames {
            pixels.add_frame(frame).unwrap();
        }
    }
    let compressed = source
        .clone_with_syntax(
            syntaxes::JPEG_2000_LOSSLESS.clone(),
            Some(CodecParams::Jpeg2000(J2kParams {
                irreversible: false,
                ..Default::default()
            })),
        )
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    for (index, expected) in frames.iter().enumerate() {
        assert_eq!(&frame_bytes(&decoded, index as u32), expected);
    }
}

#[test]
fn malformed_codestream_is_rejected_without_appending_a_frame() {
    let _ = radicom_codecs::install();
    let mut source = source_dataset(8, 8, 8, 1, false, "MONOCHROME2");
    source.set_transfer_syntax(syntaxes::JPEG_2000_LOSSLESS.clone());
    let fragments = source.insert_fragments(Tag::PIXEL_DATA, Vr::Ob);
    fragments.push_fragment_raw(std::sync::Arc::new(MemoryBuffer::from(vec![0u8; 12])));
    fragments.set_offset_table(vec![0]);
    let src = PixelDataRef::from_dataset(&source).unwrap();
    let mut destination = source.clone();
    destination.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    destination.remove(Tag::PIXEL_DATA);
    let mut dst = PixelData::create_new(&mut destination).unwrap();
    let before = dst.number_of_frames();
    let result =
        Jpeg2000LosslessCodec.decode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params());
    assert!(result.is_err());
    assert_eq!(dst.number_of_frames(), before);
}
