use radicom::model::dataset::Dataset;
use radicom::model::tag::Tag;
use radicom::model::transfer_syntax::syntaxes;
use radicom::model::vr::Vr;
use radicom::pixel::{PixelData, PixelDataRef};

#[test]
fn registry_transcoder_round_trips_rle_exactly() {
    let _ = radicom_codecs::install();
    let mut source = Dataset::new();
    source.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    source.insert_value(Tag::ROWS, 2u32);
    source.insert_value(Tag::COLUMNS, 4u32);
    source.insert_value(Tag::BITS_ALLOCATED, 8u16);
    source.insert_value(Tag::BITS_STORED, 8u16);
    source.insert_value(Tag::HIGH_BIT, 7u16);
    source.insert_value(Tag::SAMPLES_PER_PIXEL, 1u16);
    source.insert_value(Tag::PIXEL_REPRESENTATION, 0u16);
    source.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME2");
    {
        let mut pixels = PixelData::create_new(&mut source).unwrap();
        pixels.add_frame([0, 1, 1, 2, 3, 5, 8, 13]).unwrap();
    }

    let compressed = source
        .clone_with_syntax(syntaxes::RLE_LOSSLESS.clone(), None)
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    let expected = PixelDataRef::from_dataset(&source)
        .unwrap()
        .get_frame(0)
        .unwrap()
        .data()
        .unwrap()
        .to_vec();
    let actual = PixelDataRef::from_dataset(&decoded)
        .unwrap()
        .get_frame(0)
        .unwrap()
        .data()
        .unwrap()
        .to_vec();
    assert_eq!(actual, expected);
    assert_eq!(
        compressed.transfer_syntax().uid(),
        syntaxes::RLE_LOSSLESS.uid()
    );
    assert_eq!(
        decoded.transfer_syntax().uid(),
        syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.uid()
    );
    radicom::pixel::codec::reset_codec_registry();
}
