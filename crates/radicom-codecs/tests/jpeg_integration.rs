use radicom::model::{
    dataset::Dataset,
    tag::Tag,
    transfer_syntax::{syntaxes, TransferSyntax},
    vr::Vr,
};

/// The codec registry is process-wide, so tests that install or reset it must
/// not run concurrently.
static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn install() -> std::sync::MutexGuard<'static, ()> {
    let guard = REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _ = radicom_codecs::install();
    guard
}
use radicom::pixel::{
    codec::{Codec, CodecParams, JpegParams},
    transcoder::Transcoder,
    PixelData, PixelDataRef,
};
use radicom_codecs::{JpegProcess1Codec, JpegProcess2_4Codec};

/// 8-bit RGB or 16-bit mono source with a deterministic gradient.
fn source(rows: u16, cols: u16, allocated: u16, stored: u16, components: u16) -> Dataset {
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
    let mask = (1u32 << stored) - 1;
    let bytes_per = usize::from(allocated / 8);
    let samples = usize::from(rows) * usize::from(cols) * usize::from(components);
    let frame: Vec<u8> = (0..samples)
        .flat_map(|i| {
            (((i as u32).wrapping_mul(2654435761) & mask) as u16).to_le_bytes()[..bytes_per]
                .to_vec()
        })
        .collect();
    {
        let mut pixels = PixelData::create_new(&mut dataset).unwrap();
        pixels.add_frame(&frame).unwrap();
    }
    dataset
}

fn frame_bytes(dataset: &Dataset) -> Vec<u8> {
    PixelDataRef::from_dataset(dataset)
        .unwrap()
        .get_frame(0)
        .unwrap()
        .data()
        .unwrap()
        .to_vec()
}

fn count_marker(stream: &[u8], marker: u8) -> usize {
    stream
        .windows(2)
        .filter(|pair| pair[0] == 0xFF && pair[1] == marker)
        .count()
}

fn restart_stream(mut dataset: Dataset, quality: u32, restart: u16) -> Vec<u8> {
    let params = JpegParams {
        quality,
        restart_interval: restart,
        ..Default::default()
    };
    let compressed = dataset
        .clone_with_syntax(
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            Some(CodecParams::Jpeg(params)),
        )
        .unwrap();
    // Decoding back must reproduce the source samples exactly (Process 14).
    let decoded = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap();
    let original = frame_bytes(&dataset);
    let restored = frame_bytes(&decoded);
    assert_eq!(original, restored, "lossless round trip, restart={restart}");
    dataset.remove(Tag::PIXEL_DATA);
    frame_bytes(&compressed)
}

#[test]
fn jpeg_lossless_encoder_emits_restart_intervals_and_stays_exact() {
    let _guard = install();
    for (rows, cols, allocated, stored, components) in
        [(17u16, 32u16, 8u16, 8u16, 3u16), (9, 17, 16, 12, 1)]
    {
        let dataset = source(rows, cols, allocated, stored, components);
        let plain = restart_stream(dataset.clone(), 0, 0);
        assert_eq!(count_marker(&plain, 0xDD), 0, "no DRI without an interval");
        assert_eq!(count_marker(&plain, 0xD0), 0);
        for interval in [cols, cols * 2] {
            // Process 14 MCU rows are image-wide, so a legal DRI is a multiple
            // of the width (libijg-based decoders reject anything else).
            let stream = restart_stream(dataset.clone(), 0, interval);
            assert_eq!(count_marker(&stream, 0xDD), 1, "one DRI segment");
            // T.81 A.2.3: a lossless interleaved scan codes one pixel per MCU.
            let mcus = u32::from(rows) * u32::from(cols);
            let expected = mcus.div_ceil(u32::from(interval)) as usize - 1;
            let rst = (0..8)
                .map(|i| count_marker(&stream, 0xD0 + i))
                .sum::<usize>();
            assert_eq!(rst, expected, "restart markers for interval {interval}");
        }
    }
}

#[test]
fn jpeg_lossless_encoder_rejects_restart_interval_off_the_mcu_row() {
    let _guard = install();
    let dataset = source(9, 17, 8, 8, 1);
    let params = JpegParams {
        restart_interval: 5,
        ..Default::default()
    };
    let error = dataset
        .clone_with_syntax(
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            Some(CodecParams::Jpeg(params)),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("multiple of the image width"),
        "{error}"
    );
}

#[test]
fn jpeg_sv1_encode_rejects_a_point_transform_and_a_foreign_predictor() {
    let _guard = install();
    let dataset = source(4, 4, 8, 8, 1);
    for params in [
        JpegParams {
            point_transform: 4,
            ..Default::default()
        },
        JpegParams {
            predictor: 2,
            ..Default::default()
        },
    ] {
        let error = dataset
            .clone_with_syntax(
                syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone(),
                Some(CodecParams::Jpeg(params)),
            )
            .unwrap_err();
        assert!(error.to_string().contains("SV1"), "{error}");
    }
}

#[test]
fn jpeg_sv1_decode_rejects_a_process14_stream_with_a_point_transform() {
    let _guard = install();
    let dataset = source(4, 4, 8, 8, 1);
    let params = JpegParams {
        point_transform: 4,
        ..Default::default()
    };
    // The .57 codec still allows a point transform; relabel its stream as SV1.
    let mut compressed = dataset
        .clone_with_syntax(
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            Some(CodecParams::Jpeg(params)),
        )
        .unwrap();
    compressed.set_transfer_syntax(syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone());
    let error = compressed
        .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
        .unwrap_err();
    assert!(error.to_string().contains("point transform"), "{error}");
}

#[test]
fn jpeg_baseline_restart_intervals_do_not_change_reconstruction() {
    let _guard = install();
    let dataset = source(24, 40, 8, 8, 3);
    let mut decoded = Vec::new();
    for restart in [0u16, 2, 7] {
        let params = JpegParams {
            quality: 90,
            restart_interval: restart,
            ..Default::default()
        };
        let compressed = dataset
            .clone_with_syntax(
                syntaxes::JPEG_PROCESS_1.clone(),
                Some(CodecParams::Jpeg(params)),
            )
            .unwrap();
        let stream = frame_bytes(&compressed);
        assert_eq!(
            count_marker(&stream, 0xDD),
            usize::from(restart != 0).min(1)
        );
        if restart != 0 {
            // Baseline 4:4:4: one MCU is one 8x8 block of all components.
            let mcus = u32::from(24u16.div_ceil(8)) * u32::from(40u16.div_ceil(8));
            let rst = (0..8)
                .map(|i| count_marker(&stream, 0xD0 + i))
                .sum::<usize>();
            assert_eq!(rst, mcus.div_ceil(u32::from(restart)) as usize - 1);
        }
        let restored = compressed
            .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
            .unwrap();
        decoded.push(frame_bytes(&restored));
    }
    // Restart intervals only relocate the DC predictor; the DCT coefficients, and
    // therefore the reconstructed samples, must be identical.
    assert_eq!(decoded[0], decoded[1]);
    assert_eq!(decoded[0], decoded[2]);
}

/// Two-frame 8-bit RGB source; the second frame is a different gradient.
fn two_frame_source(rows: u16, cols: u16) -> Dataset {
    let mut dataset = source(rows, cols, 8, 8, 3);
    let frame: Vec<u8> = (0..usize::from(rows) * usize::from(cols) * 3)
        .map(|i| ((i / 7) % 256) as u8)
        .collect();
    PixelData::from_dataset(&mut dataset)
        .unwrap()
        .add_frame(&frame)
        .unwrap();
    dataset
}

#[test]
fn jpeg_422_conversion_keeps_every_frame_aligned() {
    let _guard = install();
    let (rows, cols) = (8u16, 16u16);
    let source = two_frame_source(rows, cols);
    // A 4:2:2 encode labels the compressed dataset YBR_FULL_422, so the decode
    // destination is 2 B/px while conversion writes 3 B/px.
    let params = JpegParams {
        sample_factor: radicom::pixel::codec::SampleFactor::Half,
        ..Default::default()
    };
    let compressed = source
        .clone_with_syntax(
            syntaxes::JPEG_PROCESS_1.clone(),
            Some(CodecParams::Jpeg(params)),
        )
        .unwrap();
    assert_eq!(
        compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
        "YBR_FULL_422"
    );
    let decoded = Transcoder::new(
        compressed.transfer_syntax(),
        syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
        Some(CodecParams::Jpeg(JpegParams {
            convert_colorspace_to_rgb: true,
            ..Default::default()
        })),
        None,
    )
    .transcode_dataset(&compressed)
    .unwrap();
    assert_eq!(
        decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
        "RGB"
    );
    let frames = PixelDataRef::from_dataset(&decoded).unwrap();
    let first = frames.get_frame(0).unwrap().data().unwrap().to_vec();
    let second = frames.get_frame(1).unwrap().data().unwrap().to_vec();
    assert_eq!(first.len(), usize::from(rows) * usize::from(cols) * 3);
    assert_eq!(second.len(), first.len());
    assert_ne!(first, second, "frames must not alias after conversion");

    // Frame 1 decoded on its own is the reference for its content.
    let mut single = compressed.clone();
    single.remove(Tag::PIXEL_DATA);
    single.remove(Tag::NUMBER_OF_FRAMES);
    let codestream = PixelDataRef::from_dataset(&compressed)
        .unwrap()
        .get_frame(1)
        .unwrap()
        .data()
        .unwrap()
        .to_vec();
    PixelData::create_new(&mut single)
        .unwrap()
        .add_frame(codestream)
        .unwrap();
    let alone = Transcoder::new(
        single.transfer_syntax(),
        syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
        Some(CodecParams::Jpeg(JpegParams {
            convert_colorspace_to_rgb: true,
            ..Default::default()
        })),
        None,
    )
    .transcode_dataset(&single)
    .unwrap();
    assert_eq!(frame_bytes(&alone), second);
}

fn pixel_data_vr(dataset: &Dataset) -> Vr {
    match dataset.item(&Tag::PIXEL_DATA) {
        Some(radicom::model::item::DicomItem::Fragments(fragments)) => fragments.vr(),
        other => panic!("expected encapsulated pixel data, got {other:?}"),
    }
}

#[test]
fn encapsulated_pixel_data_uses_ob_for_wide_samples() {
    let _guard = install();
    for (allocated, stored, syntax) in [
        (16u16, 16u16, syntaxes::JPEG_LOSSLESS_PROCESS_14.clone()),
        (16, 12, syntaxes::JPEG_PROCESS_2_4.clone()),
        (8, 8, syntaxes::JPEG_PROCESS_1.clone()),
    ] {
        let compressed = source(8, 8, allocated, stored, 1)
            .clone_with_syntax(syntax, None)
            .unwrap();
        assert_eq!(
            pixel_data_vr(&compressed),
            Vr::Ob,
            "encapsulated BA={allocated} BS={stored} must use OB"
        );
    }
}

#[test]
fn converted_decode_into_a_planar_destination_is_interleaved() {
    let _guard = install();
    // Baseline-encode an RGB source; the 4:4:4 encoder labels the stream
    // YBR_FULL, so a converted decode applies the colour transform.
    let compressed = source(4, 4, 8, 8, 3)
        .clone_with_syntax(
            syntaxes::JPEG_PROCESS_1.clone(),
            Some(CodecParams::Jpeg(JpegParams {
                quality: 100,
                ..Default::default()
            })),
        )
        .unwrap();
    let decode_params = || {
        CodecParams::Jpeg(JpegParams {
            convert_colorspace_to_rgb: true,
            ..Default::default()
        })
    };
    let decode = |dataset: &Dataset| {
        Transcoder::new(
            dataset.transfer_syntax(),
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            Some(decode_params()),
            None,
        )
        .transcode_dataset(dataset)
        .unwrap()
    };
    let interleaved = decode(&compressed);
    // Only the declared destination planar layout differs.
    let mut planar_source = compressed.clone();
    planar_source.insert_value(Tag::PLANAR_CONFIGURATION, 1u16);
    let planar = decode(&planar_source);
    assert_eq!(
        frame_bytes(&planar),
        frame_bytes(&interleaved),
        "converted decode must emit the same interleaved RGB bytes regardless of the declared destination layout"
    );
    assert_eq!(
        planar
            .try_get::<u16>(Tag::PLANAR_CONFIGURATION)
            .unwrap_or(0),
        0,
        "converted decode must declare interleaved output"
    );
}

/// Encode `source` with one JPEG process, then relabel the encapsulated frame as
/// `decoded_as` so a different codec decodes the same codestream.
fn codestream_encoded_as(
    encode_as: std::sync::Arc<TransferSyntax>,
    decoded_as: std::sync::Arc<TransferSyntax>,
    allocated: u16,
    stored: u16,
) -> Dataset {
    let mut compressed = source(8, 8, allocated, stored, 1)
        .clone_with_syntax(encode_as, Some(CodecParams::Jpeg(JpegParams::default())))
        .unwrap();
    compressed.set_transfer_syntax(decoded_as);
    compressed
}

#[test]
fn jpeg_process1_rejects_an_extended_sof1_codestream() {
    let _guard = install();
    // A 12-bit Process 2/4 stream uses SOF1; the Process 1 decoder must refuse it.
    // (8-bit Process 2/4 streams use SOF0, matching libjpeg, and are accepted.)
    let compressed = codestream_encoded_as(
        syntaxes::JPEG_PROCESS_2_4.clone(),
        syntaxes::JPEG_PROCESS_1.clone(),
        16,
        12,
    );
    let mut dst = source(8, 8, 16, 12, 1);
    dst.remove(Tag::PIXEL_DATA);
    dst.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    let error = JpegProcess1Codec
        .decode(
            &PixelDataRef::from_dataset(&compressed).unwrap(),
            &mut PixelData::create_new(&mut dst).unwrap(),
            &CodecParams::Jpeg(JpegParams::default()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("SOF0"), "{error}");
}

#[test]
fn jpeg_process2_4_accepts_an_eight_bit_baseline_sof0_codestream() {
    let _guard = install();
    // fo-dicom/libjpeg encode 8-bit Process 2/4 streams with the baseline SOF0
    // marker, so the .51 decoder must accept a SOF0 8-bit codestream.
    let compressed = codestream_encoded_as(
        syntaxes::JPEG_PROCESS_1.clone(),
        syntaxes::JPEG_PROCESS_2_4.clone(),
        8,
        8,
    );
    let mut dst = source(8, 8, 8, 8, 1);
    dst.remove(Tag::PIXEL_DATA);
    dst.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    {
        let mut pixels = PixelData::create_new(&mut dst).unwrap();
        JpegProcess2_4Codec
            .decode(
                &PixelDataRef::from_dataset(&compressed).unwrap(),
                &mut pixels,
                &CodecParams::Jpeg(JpegParams::default()),
            )
            .expect("8-bit SOF0 under .51 must decode");
    }
    assert!(PixelDataRef::from_dataset(&dst).unwrap().number_of_frames() >= 1);
}
