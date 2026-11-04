//! End-to-end tests for the compression tool's library half.
//!
//! Each test writes a small uncompressed file to a private temporary
//! directory, runs the compression loop, and reads the outputs back.

use std::path::{Path, PathBuf};

use radicom::io::file::DicomFile;
use radicom::model::dataset::Dataset;
use radicom::model::tag::Tag;
use radicom::model::transfer_syntax::syntaxes;
use radicom::model::vr::Vr;
use radicom::pixel::{PixelData, PixelDataRef};
use radicom_codecs_tool::{compress, Status};

/// Targets whose decode must reproduce the source samples byte for byte.
const LOSSLESS: &[&str] = &[
    "rle",
    "jpeg_lossless_14",
    "jpeg_lossless_sv1",
    "jpegls_lossless",
    "j2k_lossless",
    "htj2k_lossless",
    "htj2k_lossless_rpcl",
];

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("radicom-codecs-tool-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_sample(path: &Path, bits: u16, frames: &[Vec<u8>]) {
    let mut dataset = Dataset::new();
    dataset.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    dataset.insert_str(Tag::SOP_CLASS_UID, Vr::Ui, "1.2.840.10008.5.1.4.1.1.2");
    dataset.insert_str(Tag::SOP_INSTANCE_UID, Vr::Ui, "1.2.3.4.5.6.7.8");
    dataset.insert_value(Tag::ROWS, 3u16);
    dataset.insert_value(Tag::COLUMNS, 4u16);
    dataset.insert_value(Tag::BITS_ALLOCATED, bits);
    dataset.insert_value(Tag::BITS_STORED, bits);
    dataset.insert_value(Tag::HIGH_BIT, bits - 1);
    dataset.insert_value(Tag::SAMPLES_PER_PIXEL, 1u16);
    dataset.insert_value(Tag::PIXEL_REPRESENTATION, 0u16);
    dataset.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME2");
    {
        let mut pixel = PixelData::create_new(&mut dataset).expect("pixel data");
        for frame in frames {
            pixel.add_frame(frame).expect("frame");
        }
    }
    DicomFile::from_dataset(dataset)
        .save(path)
        .expect("save sample");
}

fn read(path: &Path) -> DicomFile {
    DicomFile::open(path).expect("open DICOM")
}

fn frames_of(dataset: &Dataset) -> Vec<Vec<u8>> {
    let pixel = PixelDataRef::from_dataset(dataset).expect("pixel data");
    (0..pixel.number_of_frames())
        .map(|index| {
            pixel
                .get_frame(index)
                .expect("frame")
                .data()
                .expect("frame data")
                .to_vec()
        })
        .collect()
}

fn eight_bit_frames() -> Vec<Vec<u8>> {
    vec![
        (0..12u8).map(|value| value * 10).collect(),
        (0..12u8).map(|value| 255 - value * 20).collect(),
    ]
}

fn sixteen_bit_frames() -> Vec<Vec<u8>> {
    (0..2)
        .map(|frame| {
            (0..12u16)
                .flat_map(|value| (1000 + frame * 500 + value * 100).to_le_bytes())
                .collect()
        })
        .collect()
}

#[test]
fn compresses_an_eight_bit_image_to_every_format() {
    let dir = temp_dir("all");
    let input = dir.join("study.dcm");
    let source = eight_bit_frames();
    write_sample(&input, 8, &source);
    let file = read(&input);

    let outcomes = compress(&file, &input, Some(&dir.join("out")), None).expect("compress");

    assert_eq!(outcomes.len(), 12);
    for outcome in &outcomes {
        assert_eq!(
            outcome.status,
            Status::Success,
            "{}: {}",
            outcome.suffix,
            outcome.message
        );
        assert_eq!(
            outcome.output_path.file_name().unwrap().to_string_lossy(),
            format!("study_{}.dcm", outcome.suffix)
        );
        let output = read(&outcome.output_path);
        assert_eq!(
            output.dataset.transfer_syntax().uid(),
            outcome.transfer_syntax_uid,
            "{} meta group",
            outcome.suffix
        );
        let decoded = output
            .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
            .expect("decode output");
        let frames = frames_of(&decoded.dataset);
        assert_eq!(frames.len(), source.len(), "{} frame count", outcome.suffix);
        if LOSSLESS.contains(&outcome.suffix) {
            assert_eq!(frames, source, "{} round trip", outcome.suffix);
        }
    }
}

#[test]
fn format_option_selects_one_target_case_insensitively() {
    let dir = temp_dir("filter");
    let input = dir.join("study.dcm");
    write_sample(&input, 8, &eight_bit_frames());
    let file = read(&input);

    let outcomes = compress(&file, &input, Some(&dir), Some("RLE")).expect("compress");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].suffix, "rle");
    assert_eq!(outcomes[0].status, Status::Success);
}

#[test]
fn default_output_directory_sits_next_to_the_input() {
    let dir = temp_dir("default-dir");
    let input = dir.join("study.dcm");
    write_sample(&input, 8, &eight_bit_frames());
    let file = read(&input);

    let outcomes = compress(&file, &input, None, Some("rle")).expect("compress");

    let expected = dir.join("study_compressed").join("study_rle.dcm");
    assert_eq!(outcomes[0].output_path, expected);
    assert!(expected.exists());
}

#[test]
fn a_source_already_in_the_target_syntax_is_not_reencoded() {
    let dir = temp_dir("same-syntax");
    let input = dir.join("study.dcm");
    write_sample(&input, 8, &eight_bit_frames());
    let file = read(&input);

    let first = compress(&file, &input, Some(&dir), Some("j2k_lossless")).expect("first pass");
    assert_eq!(first[0].status, Status::Success);
    let compressed_path = first[0].output_path.clone();
    let compressed = read(&compressed_path);

    let second = compress(
        &compressed,
        &compressed_path,
        Some(&dir.join("second")),
        Some("j2k_lossless"),
    )
    .expect("second pass");

    assert_eq!(second[0].status, Status::Success);
    assert!(
        second[0].message.contains("already"),
        "{}",
        second[0].message
    );
}

#[test]
fn unsupported_formats_are_skipped_without_writing_a_file() {
    let dir = temp_dir("partial");
    let input = dir.join("study.dcm");
    write_sample(&input, 16, &sixteen_bit_frames());
    let file = read(&input);

    let outcomes = compress(&file, &input, Some(&dir), None).expect("compress");

    assert_eq!(outcomes.len(), 12);
    for suffix in ["jpeg_baseline", "jpeg_process2_4"] {
        let outcome = outcomes
            .iter()
            .find(|outcome| outcome.suffix == suffix)
            .expect("sequential JPEG row");
        assert_eq!(
            outcome.status,
            Status::Unsupported,
            "{suffix} must be classified before encoding"
        );
        assert!(
            outcome.message.contains("BitsAllocated"),
            "{}",
            outcome.message
        );
        assert!(
            !outcome.output_path.exists(),
            "{suffix} must not leave a file behind"
        );
    }
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.status == Status::Success),
        "the remaining formats still run"
    );
}

#[test]
fn an_input_without_pixel_data_is_rejected() {
    let dir = temp_dir("no-pixels");
    let path = dir.join("empty.dcm");
    let mut dataset = Dataset::new();
    dataset.insert_str(Tag::SOP_CLASS_UID, Vr::Ui, "1.2.840.10008.5.1.4.1.1.2");
    dataset.insert_str(Tag::SOP_INSTANCE_UID, Vr::Ui, "1.2.3.4.5.6.7.9");
    DicomFile::from_dataset(dataset)
        .save(&path)
        .expect("save sample");
    let file = read(&path);

    let error = compress(&file, &path, None, None).expect_err("no pixel data");

    assert!(error.to_string().contains("no pixel data"), "{error}");
}

#[test]
fn an_unknown_format_is_rejected() {
    let dir = temp_dir("unknown-format");
    let input = dir.join("study.dcm");
    write_sample(&input, 8, &eight_bit_frames());
    let file = read(&input);

    let error = compress(&file, &input, None, Some("nope")).expect_err("unknown format");

    assert!(error.to_string().contains("unknown format"), "{error}");
}
