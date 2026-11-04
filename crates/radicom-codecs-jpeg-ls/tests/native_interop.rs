//! Byte-for-byte checks against the fo-dicom.Codecs (CharLS 2.4.5) reference
//! codestreams for `D:\dicom-test\1.dcm`.
//!
//! The fixtures live outside the repository, so the tests report and skip when
//! they are absent instead of failing a checkout that does not carry them.

use radicom::io::file::DicomFile;
use radicom::model::{
    dataset::Dataset,
    tag::Tag,
    transfer_syntax::{syntaxes, TransferSyntax},
};
use radicom::pixel::codec::Codec;
use radicom::pixel::{PixelData, PixelDataRef};
use radicom_codecs_jpeg_ls::{JpegLsLosslessCodec, JpegLsNearLosslessCodec};
use std::sync::Arc;

const FIXTURES: &str = r"D:\dicom-test";

/// SOI..EOI inclusive: the native reference pads odd fragments after EOI for
/// DICOM even-length encapsulation, which is not part of the codestream.
fn codestream(bytes: &[u8]) -> &[u8] {
    match bytes.windows(2).rposition(|window| window == [0xff, 0xd9]) {
        Some(index) => &bytes[..index + 2],
        None => bytes,
    }
}

fn frame_zero(dataset: &Dataset) -> Vec<u8> {
    PixelDataRef::from_dataset(dataset)
        .unwrap()
        .get_frame(0)
        .unwrap()
        .data()
        .unwrap()
        .to_vec()
}

fn encode_first_frame(source: &Dataset, syntax: Arc<TransferSyntax>, codec: &dyn Codec) -> Vec<u8> {
    let mut out = source.clone();
    out.set_transfer_syntax(syntax);
    out.remove(Tag::PIXEL_DATA);
    {
        let view = PixelDataRef::from_dataset(source).unwrap();
        let mut target = PixelData::create_new(&mut out).unwrap();
        codec
            .encode(&view, &mut target, &codec.default_params())
            .unwrap();
    }
    frame_zero(&out)
}

fn assert_same(ours: &[u8], native: &[u8], label: &str) {
    if ours == native {
        return;
    }
    let common = ours.len().min(native.len());
    let first = (0..common)
        .find(|&index| ours[index] != native[index])
        .unwrap_or(common);
    let differing = (0..common)
        .filter(|&index| ours[index] != native[index])
        .count()
        + ours.len().abs_diff(native.len());
    panic!(
        "{label}: first diff at {first}, {differing} differing bytes, ours {} native {}",
        ours.len(),
        native.len()
    );
}

#[test]
fn one_dcm_matches_the_fo_dicom_charls_codestreams() {
    let source_path = format!(r"{FIXTURES}\1.dcm");
    let native_dir = format!(r"{FIXTURES}\1-native");
    let lossless_path = format!(r"{native_dir}\1_jpegls_lossless.dcm");
    let near_path = format!(r"{native_dir}\1_jpegls_near_lossless.dcm");
    let Ok(source) = DicomFile::open(&source_path) else {
        eprintln!("skipping: {source_path} is not present");
        return;
    };
    let (Ok(lossless), Ok(near)) = (DicomFile::open(&lossless_path), DicomFile::open(&near_path))
    else {
        eprintln!("skipping: fo-dicom reference codestreams are not present");
        return;
    };
    // 1.dcm is 16-bit signed MONOCHROME2; the precision above 12 bits is what
    // makes CharLS emit the LSE preset-parameters segment.
    let ours_lossless = encode_first_frame(
        &source.dataset,
        syntaxes::JPEG_LS_LOSSLESS.clone(),
        &JpegLsLosslessCodec,
    );
    let ours_near = encode_first_frame(
        &source.dataset,
        syntaxes::JPEG_LS_NEAR_LOSSLESS.clone(),
        &JpegLsNearLosslessCodec,
    );
    assert_same(
        codestream(&ours_lossless),
        codestream(&frame_zero(&lossless.dataset)),
        "1.dcm .80",
    );
    assert_same(
        codestream(&ours_near),
        codestream(&frame_zero(&near.dataset)),
        "1.dcm .81",
    );
}
