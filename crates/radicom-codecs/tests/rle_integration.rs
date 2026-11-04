use radicom::model::dataset::Dataset;
use radicom::model::tag::Tag;
use radicom::model::transfer_syntax::syntaxes;
use radicom::model::vr::Vr;
use radicom::pixel::{PixelData, PixelDataRef};

/// The codec registry is process-wide; tests that install it must not run
/// concurrently with each other.
static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn install() -> std::sync::MutexGuard<'static, ()> {
    let guard = REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _ = radicom_codecs::install();
    guard
}

#[test]
fn registry_transcoder_round_trips_rle_exactly() {
    let _guard = install();
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
}

#[test]
fn a_native_source_with_a_huge_frame_count_fails_instead_of_allocating() {
    let _guard = install();
    let mut source = Dataset::new();
    source.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    source.insert_value(Tag::ROWS, 1u32);
    source.insert_value(Tag::COLUMNS, 1u32);
    source.insert_value(Tag::BITS_ALLOCATED, 8u16);
    source.insert_value(Tag::BITS_STORED, 8u16);
    source.insert_value(Tag::HIGH_BIT, 7u16);
    source.insert_value(Tag::SAMPLES_PER_PIXEL, 1u16);
    source.insert_value(Tag::PIXEL_REPRESENTATION, 0u16);
    source.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME2");
    {
        let mut pixels = PixelData::create_new(&mut source).unwrap();
        pixels.add_frame([7]).unwrap();
    }
    // NumberOfFrames is untrusted metadata: a tiny pixel buffer must not let the
    // encoder iterate/accumulate ~4 billion frames.
    source.insert_str(Tag::NUMBER_OF_FRAMES, Vr::Is, u32::MAX.to_string());

    let result = source.clone_with_syntax(syntaxes::RLE_LOSSLESS.clone(), None);
    assert!(
        result.is_err(),
        "huge NumberOfFrames must fail, not allocate"
    );
}

#[test]
fn encode_rejects_a_subsampled_photometric_interpretation() {
    use radicom::pixel::codec::{Codec, CodecParams};

    let mut source = Dataset::new();
    source.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    for (tag, value) in [
        (Tag::ROWS, 4u16),
        (Tag::COLUMNS, 4u16),
        (Tag::BITS_ALLOCATED, 8u16),
        (Tag::BITS_STORED, 8u16),
        (Tag::HIGH_BIT, 7u16),
        (Tag::SAMPLES_PER_PIXEL, 3u16),
        (Tag::PIXEL_REPRESENTATION, 0u16),
        (Tag::PLANAR_CONFIGURATION, 0u16),
    ] {
        source.insert_value(tag, value);
    }
    source.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL_422");
    {
        // 4:2:2 is stored as two bytes per pixel.
        let mut pixels = PixelData::create_new(&mut source).unwrap();
        pixels.add_frame(vec![0u8; 4 * 4 * 2]).unwrap();
    }

    let mut destination = source.clone();
    destination.set_transfer_syntax(syntaxes::RLE_LOSSLESS.clone());
    destination.remove(Tag::PIXEL_DATA);

    let src = PixelDataRef::from_dataset(&source).unwrap();
    let mut dst = PixelData::create_new(&mut destination).unwrap();
    let before = dst.number_of_frames();
    let error = radicom_codecs::RleLosslessCodec
        .encode(&src, &mut dst, &CodecParams::None)
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("subsampled"),
        "unexpected error: {error}"
    );
    assert_eq!(dst.number_of_frames(), before, "no frame may be appended");
}
