use radicom::model::{dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr};
use radicom::pixel::{
    codec::{CodecParams, CodecRegistry, HtJ2kParams, ProgressionOrder},
    PixelData, PixelDataRef,
};

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

fn ht_params(progression_order: ProgressionOrder) -> CodecParams {
    CodecParams::HtJpeg2000(HtJ2kParams {
        irreversible: false,
        progression_order,
        ..Default::default()
    })
}

#[test]
fn registry_registers_three_htj2k_syntaxes() {
    let registry = radicom_codecs::install();
    assert!(registry.has_codec(&syntaxes::HTJ2K_LOSSLESS));
    assert!(registry.has_codec(&syntaxes::HTJ2K_LOSSLESS_RPCL));
    assert!(registry.has_codec(&syntaxes::HTJ2K_LOSSY));
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
            syntaxes::HTJ2K_LOSSLESS.clone(),
            Some(ht_params(ProgressionOrder::Lrcp)),
        )
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    assert_eq!(frame_bytes(&decoded, 0), raw);
}

#[test]
fn transcodes_mono8_rpcl_lossless_exactly() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..32 * 24).map(|i| (i * 53 + 7) as u8).collect();
    let mut source = source_dataset(32, 24, 8, 1, false, "MONOCHROME2");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame(&raw)
        .unwrap();
    let compressed = source
        .clone_with_syntax(
            syntaxes::HTJ2K_LOSSLESS_RPCL.clone(),
            Some(ht_params(ProgressionOrder::Rpcl)),
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
            syntaxes::HTJ2K_LOSSLESS.clone(),
            Some(ht_params(ProgressionOrder::Lrcp)),
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
            syntaxes::HTJ2K_LOSSY.clone(),
            Some(CodecParams::HtJpeg2000(HtJ2kParams {
                irreversible: true,
                ..Default::default()
            })),
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
    assert!(mean < 3.0, "mean absolute error {mean}");
}

#[test]
fn lossless_accepts_rpcl_progression() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..24 * 16).map(|i| (i * 29 + 3) as u8).collect();
    let mut source = source_dataset(24, 16, 8, 1, false, "MONOCHROME2");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame(&raw)
        .unwrap();
    let compressed = source
        .clone_with_syntax(
            syntaxes::HTJ2K_LOSSLESS.clone(),
            Some(ht_params(ProgressionOrder::Rpcl)),
        )
        .unwrap();
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    assert_eq!(frame_bytes(&decoded, 0), raw);
}

#[test]
fn encoded_color_transform_is_labelled() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..16 * 12 * 3).map(|i| (i * 13 + 7) as u8).collect();
    for (syntax, label) in [
        (syntaxes::HTJ2K_LOSSLESS.clone(), "YBR_RCT"),
        (syntaxes::HTJ2K_LOSSLESS_RPCL.clone(), "YBR_RCT"),
        (syntaxes::HTJ2K_LOSSY.clone(), "YBR_ICT"),
    ] {
        let mut source = source_dataset(16, 12, 8, 3, false, "RGB");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let compressed = source.clone_with_syntax(syntax.clone(), None).unwrap();
        assert_eq!(
            compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            label,
            "{}",
            syntax.uid()
        );
        // Decoding converts back to RGB samples and relabels the dataset.
        let decoded = compressed
            .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
            .unwrap();
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB",
            "{}",
            syntax.uid()
        );
    }
}

#[test]
fn rgb_without_color_transform_round_trips_exactly() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..16 * 12 * 3).map(|i| (i * 13 + 7) as u8).collect();
    let mut source = source_dataset(16, 12, 8, 3, false, "RGB");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame(&raw)
        .unwrap();
    let compressed = source
        .clone_with_syntax(
            syntaxes::HTJ2K_LOSSLESS.clone(),
            Some(CodecParams::HtJpeg2000(HtJ2kParams {
                irreversible: false,
                employ_color_transform: false,
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
fn honour_decomposition_level_parameter() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..24 * 16).map(|i| (i * 17 + 9) as u8).collect();
    for decompositions in [0u32, 1, 3] {
        let mut source = source_dataset(24, 16, 8, 1, false, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let compressed = source
            .clone_with_syntax(
                syntaxes::HTJ2K_LOSSLESS.clone(),
                Some(CodecParams::HtJpeg2000(HtJ2kParams {
                    irreversible: false,
                    decompositions,
                    ..Default::default()
                })),
            )
            .unwrap();
        let decoded = compressed
            .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
            .unwrap();
        assert_eq!(
            frame_bytes(&decoded, 0),
            raw,
            "decompositions {decompositions}"
        );
    }
}
#[test]
fn honours_the_lossy_quality_parameter() {
    let _ = radicom_codecs::install();
    let raw: Vec<u8> = (0..32 * 24).map(|i| (i * 7 + i / 32) as u8).collect();
    let error_for = |quality: u32| {
        let mut source = source_dataset(32, 24, 8, 1, false, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let compressed = source
            .clone_with_syntax(
                syntaxes::HTJ2K_LOSSY.clone(),
                Some(CodecParams::HtJpeg2000(HtJ2kParams {
                    irreversible: true,
                    quality,
                    ..Default::default()
                })),
            )
            .unwrap();
        let decoded = compressed
            .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
            .unwrap();
        let actual = frame_bytes(&decoded, 0);
        let total: i64 = raw
            .iter()
            .zip(&actual)
            .map(|(a, b)| i64::from((*a as i32 - *b as i32).abs()))
            .sum();
        total as f64 / raw.len() as f64
    };
    let coarse = error_for(20);
    let fine = error_for(95);
    assert!(coarse > fine, "coarse {coarse} fine {fine}");
    assert!(fine < 2.0, "fine quality error {fine}");
}
