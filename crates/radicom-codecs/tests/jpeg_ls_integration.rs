use radicom::model::{
    buffer::MemoryBuffer, dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr,
};
use radicom::pixel::{
    codec::{CodecParams, JpegLsParams},
    PixelData, PixelDataRef,
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

#[test]
fn registry_transcodes_jpeg_ls_multiframe_signed_and_planar_layouts() {
    let _guard = install();
    for (allocated, stored, components, planar, signed) in [
        (8, 8, 1, 0, false),
        (8, 8, 1, 0, true),
        (16, 12, 1, 0, true),
        (16, 16, 1, 0, true),
        (8, 8, 3, 1, false),
        (16, 12, 3, 1, false),
        (16, 16, 3, 0, false),
    ] {
        let mut source = Dataset::new();
        source.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        for (tag, value) in [
            (Tag::ROWS, 3u16),
            (Tag::COLUMNS, 5),
            (Tag::BITS_ALLOCATED, allocated),
            (Tag::BITS_STORED, stored),
            (Tag::HIGH_BIT, stored - 1),
            (Tag::SAMPLES_PER_PIXEL, components),
            (Tag::PIXEL_REPRESENTATION, u16::from(signed)),
            (Tag::PLANAR_CONFIGURATION, planar),
        ] {
            source.insert_value(tag, value);
        }
        source.insert_str(
            Tag::PHOTOMETRIC_INTERPRETATION,
            Vr::Cs,
            if components == 1 {
                "MONOCHROME2"
            } else {
                "RGB"
            },
        );
        let mask = (1u32 << stored) - 1;
        let frames = (0..2)
            .map(|frame| {
                (0..15 * usize::from(components))
                    .flat_map(|i| {
                        let value = ((i as u32 * 7919 + frame * 17) & mask) as u16;
                        let value = if signed {
                            let sign = 1i32 << (stored - 1);
                            ((i32::from(value) ^ sign) - sign).clamp(-sign + 3, sign - 4) as i16
                                as u16
                        } else {
                            value
                        };
                        value.to_le_bytes()[..usize::from(allocated / 8)].to_vec()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        {
            let mut pixels = PixelData::create_new(&mut source).unwrap();
            for frame in &frames {
                pixels.add_frame(frame).unwrap();
            }
        }
        for (syntax, near) in [
            (syntaxes::JPEG_LS_LOSSLESS.clone(), 0),
            (syntaxes::JPEG_LS_NEAR_LOSSLESS.clone(), 2),
        ] {
            let mut compressed = source
                .clone_with_syntax(
                    syntax.clone(),
                    Some(CodecParams::JpegLs(JpegLsParams {
                        near_lossless: near,
                        ..Default::default()
                    })),
                )
                .unwrap();
            let encoded_frames = {
                let pixels = PixelDataRef::from_dataset(&compressed).unwrap();
                (0..2)
                    .map(|i| pixels.get_frame(i).unwrap().data().unwrap().to_vec())
                    .collect::<Vec<_>>()
            };
            if allocated == 16 {
                // PS3.5 Table 7.1-1: encapsulated Pixel Data uses OB even for
                // 16-bit samples; the codec rewrites the OW element that
                // PixelData::add_frame creates.
                assert_eq!(
                    compressed.item(&Tag::PIXEL_DATA).unwrap().vr(),
                    Vr::Ob,
                    "encapsulated PixelData must be OB"
                );
            }
            compressed.remove(Tag::PIXEL_DATA);
            let fragments = compressed.insert_fragments(Tag::PIXEL_DATA, Vr::Ob);
            let mut offsets = Vec::new();
            let mut offset = 0u32;
            for frame in encoded_frames {
                offsets.push(offset);
                for chunk in frame.chunks(16) {
                    fragments
                        .push_fragment_raw(std::sync::Arc::new(MemoryBuffer::from(chunk.to_vec())));
                    offset += 8 + chunk.len() as u32;
                }
            }
            fragments.set_offset_table(offsets);
            let decoded = compressed
                .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
                .unwrap();
            assert_eq!(compressed.transfer_syntax().uid(), syntax.uid());
            let actual = PixelDataRef::from_dataset(&decoded).unwrap();
            assert_eq!(actual.number_of_frames(), 2);
            for (index, expected) in frames.iter().enumerate() {
                let data = actual
                    .get_frame(index as u32)
                    .unwrap()
                    .data()
                    .unwrap()
                    .to_vec();
                assert_eq!(data.len(), expected.len());
                // Compressed planar RGB is normalized to interleaved storage.
                let sample = |bytes: &[u8], i: usize| {
                    let value = if allocated == 8 {
                        u16::from(bytes[i])
                    } else {
                        u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]])
                    };
                    if signed {
                        if allocated == 8 {
                            i32::from(value as i8)
                        } else {
                            i32::from(value as i16)
                        }
                    } else {
                        i32::from(value)
                    }
                };
                for i in 0..15 * usize::from(components) {
                    let source_index = if planar == 1 { (i % 3) * 15 + i / 3 } else { i };
                    assert!((sample(expected, source_index) - sample(&data, i)).unsigned_abs() <= near,
                        "allocated={allocated}, stored={stored}, components={components}, planar={planar}, signed={signed}, frame={index}, sample={i}");
                }
            }
        }
    }
}

fn marker_count(stream: &[u8], marker: u8) -> usize {
    stream
        .windows(2)
        .filter(|pair| pair[0] == 0xFF && pair[1] == marker)
        .count()
}

#[test]
fn registry_transcodes_jpeg_ls_with_restart_intervals() {
    let _guard = install();
    for (components, interleave, planar) in [(1u16, 0u32, 0u16), (3, 3, 0), (3, 1, 0)] {
        let mut source = Dataset::new();
        source.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        for (tag, value) in [
            (Tag::ROWS, 50u16),
            (Tag::COLUMNS, 12),
            (Tag::BITS_ALLOCATED, 8),
            (Tag::BITS_STORED, 8),
            (Tag::HIGH_BIT, 7),
            (Tag::SAMPLES_PER_PIXEL, components),
            (Tag::PIXEL_REPRESENTATION, 0),
            (Tag::PLANAR_CONFIGURATION, planar),
        ] {
            source.insert_value(tag, value);
        }
        source.insert_str(
            Tag::PHOTOMETRIC_INTERPRETATION,
            Vr::Cs,
            if components == 1 {
                "MONOCHROME2"
            } else {
                "RGB"
            },
        );
        let frame: Vec<u8> = (0..50 * 12 * usize::from(components))
            .map(|i| ((i * 37 + i / 5) & 0xff) as u8)
            .collect();
        {
            let mut pixels = PixelData::create_new(&mut source).unwrap();
            pixels.add_frame(&frame).unwrap();
        }
        for interval in [0u16, 1, 7] {
            let params = JpegLsParams {
                interleave_mode: interleave,
                restart_interval: interval,
                ..Default::default()
            };
            let compressed = source
                .clone_with_syntax(
                    syntaxes::JPEG_LS_LOSSLESS.clone(),
                    Some(CodecParams::JpegLs(params)),
                )
                .unwrap();
            let stream = PixelDataRef::from_dataset(&compressed)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .to_vec();
            assert_eq!(marker_count(&stream, 0xDD), usize::from(interval != 0));
            let decoded = compressed
                .clone_with_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(), None)
                .unwrap();
            let restored = PixelDataRef::from_dataset(&decoded)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .to_vec();
            // Lossless: restart intervals must not change a single sample.
            let planar_layout = planar == 1;
            let samples = usize::from(components) * 12 * 50;
            let value = |bytes: &[u8], i: usize| {
                let index = if planar_layout && components == 3 {
                    (i % 3) * 12 * 50 + i / 3
                } else {
                    i
                };
                bytes[index]
            };
            for i in 0..samples {
                assert_eq!(
                    value(&frame, i),
                    value(&restored, i),
                    "components={components} interleave={interleave} interval={interval} sample={i}"
                );
            }
            if interval != 0 {
                let expected = (50u32.div_ceil(u32::from(interval)) - 1) as usize;
                let rst = (0..8)
                    .map(|index| marker_count(&stream, 0xD0 + index))
                    .sum::<usize>();
                // ILV=2 counts one MCU per line of the interleaved set; ILV=0/1
                // count one MCU per line of each component, so the plane and line
                // interleaved streams carry one restart sequence per scan (3x).
                let scans = if components == 3 && interleave != 3 {
                    3
                } else {
                    1
                };
                assert_eq!(
                    rst,
                    expected * scans,
                    "components={components} interleave={interleave} interval={interval}"
                );
            }
        }
    }
}
