use radicom::model::{dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr};

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
    codec::{CodecParams, JpegParams},
    PixelData, PixelDataRef,
};

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
