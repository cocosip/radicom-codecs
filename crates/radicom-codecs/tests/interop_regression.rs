//! Interoperability regressions from the 2026-11-03 contract review.
//!
//! 1. A multi-frame `YBR_FULL_422` dataset decoded with
//!    `convert_colorspace_to_rgb` must keep every frame at the RGB boundary.
//!    `PixelData::add_frame` derives the next offset from the current metadata,
//!    so the PI update has to happen before the append loop (review P0-1).
//! 2. A direct codec `encode` of 16-bit data must write encapsulated Pixel Data
//!    with VR OB, not the OW that `add_frame` picks from `BitsAllocated`
//!    (PS3.5 Table 7.1-1, review P2-3).
//!
//! The file-level fo-dicom oracle runs are manual (see the task report); this
//! test file must not depend on `.interop/` binaries.

use std::sync::Mutex;

use radicom::error::Error;
use radicom::model::item::DicomItem;
use radicom::model::transfer_syntax::syntaxes;
use radicom::model::vr::Vr;
use radicom::model::{dataset::Dataset, tag::Tag};
use radicom::pixel::codec::{
    Codec, CodecParams, J2kParams, JpegLsParams, JpegParams, SampleFactor,
};
use radicom::pixel::transcoder::Transcoder;
use radicom::pixel::{PixelData, PixelDataRef};

/// The codec registry is process-wide, so tests that install it must not run
/// concurrently with each other.
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

fn installed() -> std::sync::MutexGuard<'static, ()> {
    let guard = REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _ = radicom_codecs::install();
    guard
}

fn dataset(rows: u16, cols: u16, allocated: u16, stored: u16, components: u16) -> Dataset {
    let mut dataset = Dataset::new();
    dataset.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    for (tag, value) in [
        (Tag::ROWS, rows),
        (Tag::COLUMNS, cols),
        (Tag::BITS_ALLOCATED, allocated),
        (Tag::BITS_STORED, stored),
        (Tag::HIGH_BIT, stored - 1),
        (Tag::SAMPLES_PER_PIXEL, components),
        (Tag::PIXEL_REPRESENTATION, 0),
        (Tag::PLANAR_CONFIGURATION, 0),
    ] {
        dataset.insert_value(tag, value);
    }
    dataset.insert_str(
        Tag::PHOTOMETRIC_INTERPRETATION,
        Vr::Cs,
        if components == 1 {
            "MONOCHROME2"
        } else {
            "RGB"
        },
    );
    dataset
}

fn add_frame(dataset: &mut Dataset, frame: &[u8]) {
    PixelData::create_new(dataset)
        .unwrap()
        .add_frame(frame)
        .unwrap();
}

/// Adds the second frame without going through a second dataset.
fn rgb_two_frame_source(first: [u8; 3], second: [u8; 3]) -> (Dataset, Vec<u8>, Vec<u8>) {
    let solid = |colour: [u8; 3]| (0..16).flat_map(|_| colour).collect::<Vec<u8>>();
    let first_bytes = solid(first);
    let second_bytes = solid(second);
    let mut source = dataset(4, 4, 8, 8, 3);
    {
        let mut pixels = PixelData::create_new(&mut source).unwrap();
        pixels.add_frame(first_bytes.clone()).unwrap();
        pixels.add_frame(second_bytes.clone()).unwrap();
    }
    (source, first_bytes, second_bytes)
}

fn frames(dataset: &Dataset) -> Vec<Vec<u8>> {
    let pixels = PixelDataRef::from_dataset(dataset).unwrap();
    (0..pixels.number_of_frames())
        .map(|index| pixels.get_frame(index).unwrap().data().unwrap().to_vec())
        .collect()
}

fn max_abs_diff(left: &[u8], right: &[u8]) -> u8 {
    assert_eq!(left.len(), right.len(), "frame length differs");
    left.iter()
        .zip(right)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

fn assert_close(actual: &[u8], expected: &[u8], tolerance: u8, what: &str) {
    let diff = max_abs_diff(actual, expected);
    assert!(
        diff <= tolerance,
        "{what}: max |decoded - source| = {diff} > {tolerance}"
    );
}

/// Decode with explicit parameters. `clone_with_syntax` only forwards output
/// parameters (used when encoding); a decode must go through `in_params`.
fn decode_to_native(compressed: &Dataset, params: CodecParams) -> Result<Dataset, Error> {
    Transcoder::new(
        compressed.transfer_syntax(),
        syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
        Some(params),
        None,
    )
    .transcode_dataset(compressed)
}

#[test]
fn jpeg_422_multiframe_conversion_keeps_frames_aligned() {
    let _guard = installed();
    let (source, first, second) = rgb_two_frame_source([40, 120, 200], [220, 80, 30]);
    let encode = CodecParams::Jpeg(JpegParams {
        quality: 100,
        sample_factor: SampleFactor::Half,
        ..Default::default()
    });
    let compressed = source
        .clone_with_syntax(syntaxes::JPEG_PROCESS_1.clone(), Some(encode))
        .unwrap();
    assert_eq!(
        compressed
            .try_get_str(Tag::PHOTOMETRIC_INTERPRETATION)
            .as_deref(),
        Some("YBR_FULL_422"),
        "the 4:2:2 encoder must label the codestream YBR_FULL_422"
    );

    let decoded = decode_to_native(
        &compressed,
        CodecParams::Jpeg(JpegParams {
            convert_colorspace_to_rgb: true,
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(
        decoded
            .try_get_str(Tag::PHOTOMETRIC_INTERPRETATION)
            .as_deref(),
        Some("RGB"),
        "converted decode must end as RGB"
    );
    assert_eq!(
        decoded
            .try_get_str(Tag::NUMBER_OF_FRAMES)
            .unwrap_or_else(|| "1".to_owned()),
        "2"
    );
    let decoded_frames = frames(&decoded);
    assert_eq!(decoded_frames.len(), 2);
    for (index, frame) in decoded_frames.iter().enumerate() {
        assert_eq!(frame.len(), 48, "frame {index} must hold 3 bytes/pixel");
    }
    // `.50` is lossy, so compare within a small bound instead of byte equality;
    // the colours are far apart, so a swapped or duplicated frame cannot pass.
    assert_close(&decoded_frames[0], &first, 4, "frame 0");
    assert_close(&decoded_frames[1], &second, 4, "frame 1");
    assert_ne!(decoded_frames[0], decoded_frames[1]);
}

#[test]
fn jpeg_422_multiframe_conversion_matches_an_isolated_frame() {
    let _guard = installed();
    let (source, _, second) = rgb_two_frame_source([40, 120, 200], [220, 80, 30]);
    let decode_params = || {
        CodecParams::Jpeg(JpegParams {
            convert_colorspace_to_rgb: true,
            ..Default::default()
        })
    };
    let encode_params = JpegParams {
        quality: 100,
        sample_factor: SampleFactor::Half,
        ..Default::default()
    };
    let multi = decode_to_native(
        &source
            .clone_with_syntax(
                syntaxes::JPEG_PROCESS_1.clone(),
                Some(CodecParams::Jpeg(encode_params.clone())),
            )
            .unwrap(),
        decode_params(),
    )
    .unwrap();

    let mut single = dataset(4, 4, 8, 8, 3);
    add_frame(&mut single, &second);
    let single_decoded = decode_to_native(
        &single
            .clone_with_syntax(
                syntaxes::JPEG_PROCESS_1.clone(),
                Some(CodecParams::Jpeg(encode_params)),
            )
            .unwrap(),
        decode_params(),
    )
    .unwrap();

    let (multi_frames, single_frames) = (frames(&multi), frames(&single_decoded));
    assert_eq!(multi_frames[1], single_frames[0]);
}

#[test]
fn j2k_422_labelled_multiframe_decodes_exactly() {
    let _guard = installed();
    let (source, first, second) = rgb_two_frame_source([40, 120, 200], [220, 80, 30]);
    let mut compressed = source
        .clone_with_syntax(
            syntaxes::JPEG_2000_LOSSLESS.clone(),
            Some(CodecParams::Jpeg2000(J2kParams {
                irreversible: false,
                ..Default::default()
            })),
        )
        .unwrap();
    // J2K codestreams are not 4:2:2, but the label is the trigger for the
    // converted decode path; this is the synthetic stream from the review.
    compressed.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL_422");

    let decoded = decode_to_native(
        &compressed,
        CodecParams::Jpeg2000(J2kParams {
            irreversible: false,
            ..Default::default()
        }),
    )
    .unwrap_or_else(|error| {
        panic!("J2K two-frame YBR_FULL_422 decode must stay supported: {error}")
    });
    assert_eq!(
        decoded
            .try_get_str(Tag::PHOTOMETRIC_INTERPRETATION)
            .as_deref(),
        Some("RGB")
    );
    assert_eq!(
        decoded
            .try_get_str(Tag::NUMBER_OF_FRAMES)
            .unwrap_or_else(|| "1".to_owned()),
        "2"
    );
    let decoded_frames = frames(&decoded);
    assert_eq!(decoded_frames.len(), 2);
    assert_eq!(decoded_frames[0], first);
    assert_eq!(decoded_frames[1], second);
}

fn mono16(width: u16, height: u16) -> Dataset {
    let mut source = dataset(height, width, 16, 16, 1);
    let bytes: Vec<u8> = (0..u32::from(width) * u32::from(height))
        .flat_map(|value| ((value as u16) << 3 | 3).to_le_bytes())
        .collect();
    add_frame(&mut source, &bytes);
    source
}

fn encode_direct(codec: &dyn Codec, source: &Dataset, params: &CodecParams) -> Dataset {
    let mut encoded = source.clone();
    encoded.set_transfer_syntax(std::sync::Arc::new(codec.transfer_syntax().clone()));
    encoded.remove(Tag::PIXEL_DATA);
    encoded.remove(Tag::NUMBER_OF_FRAMES);
    let source_pixels = PixelDataRef::from_dataset(source).unwrap();
    let mut dst = PixelData::create_new(&mut encoded).unwrap();
    codec.encode(&source_pixels, &mut dst, params).unwrap();
    encoded
}

#[test]
fn direct_16_bit_encode_writes_ob_pixel_data() {
    let _guard = installed();
    let source = mono16(32, 32);
    let j2k = CodecParams::Jpeg2000(J2kParams {
        irreversible: false,
        ..Default::default()
    });
    let jls = CodecParams::JpegLs(JpegLsParams::default());
    let cases: Vec<(&dyn Codec, &str, CodecParams)> = vec![
        (&radicom_codecs::RleLosslessCodec, "RLE", CodecParams::None),
        (
            &radicom_codecs::JpegLossless14Codec,
            "JPEG lossless",
            CodecParams::Jpeg(JpegParams::default()),
        ),
        (&radicom_codecs::JpegLsLosslessCodec, "JPEG-LS", jls),
        (&radicom_codecs::Jpeg2000LosslessCodec, "JPEG 2000", j2k),
        (
            &radicom_codecs::HtJpeg2000LosslessCodec,
            "HTJ2K",
            CodecParams::HtJpeg2000(radicom::pixel::codec::HtJ2kParams {
                irreversible: false,
                ..Default::default()
            }),
        ),
    ];
    for (codec, name, params) in cases {
        let encoded = encode_direct(codec, &source, &params);
        match encoded.item(&Tag::PIXEL_DATA) {
            Some(DicomItem::Fragments(fragments)) => assert_eq!(
                fragments.vr(),
                Vr::Ob,
                "{name}: encapsulated 16-bit Pixel Data must use VR OB"
            ),
            other => panic!("{name}: expected an encapsulated fragment sequence, got {other:?}"),
        }
    }
}
