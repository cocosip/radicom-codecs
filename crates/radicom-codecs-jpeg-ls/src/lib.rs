//! Pure Rust JPEG-LS DICOM codecs.
mod bit;
mod color;
mod context;
mod frame;
mod golomb;
mod marker;
mod preset;
mod run;
mod scan;

use frame::Layout;
use radicom::error::{CodecError, Error};
use radicom::model::item::{DicomFragmentSequence, DicomItem};
use radicom::model::tag::Tag;
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::model::vr::Vr;
use radicom::pixel::codec::{Codec, CodecParams, JpegLsParams};
use radicom::pixel::photometric::PhotometricInterpretation;
use radicom::pixel::{PixelData, PixelDataRef};

#[derive(Debug, Default, Clone, Copy)]
pub struct JpegLsLosslessCodec;
#[derive(Debug, Default, Clone, Copy)]
pub struct JpegLsNearLosslessCodec;

impl Codec for JpegLsLosslessCodec {
    fn name(&self) -> &str {
        "JPEG-LS Lossless"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::JPEG_LS_LOSSLESS
    }
    fn default_params(&self) -> CodecParams {
        CodecParams::JpegLs(JpegLsParams::default())
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(self.name(), self.transfer_syntax(), src, dst, params, false)
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(self.name(), self.transfer_syntax(), src, dst, params, true)
    }
}
impl Codec for JpegLsNearLosslessCodec {
    fn name(&self) -> &str {
        "JPEG-LS Near-Lossless"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::JPEG_LS_NEAR_LOSSLESS
    }
    fn default_params(&self) -> CodecParams {
        CodecParams::JpegLs(JpegLsParams {
            near_lossless: 3,
            ..Default::default()
        })
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(self.name(), self.transfer_syntax(), src, dst, params, false)
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(self.name(), self.transfer_syntax(), src, dst, params, true)
    }
}
fn error(codec: &str, op: &str, msg: impl Into<String>) -> Error {
    CodecError::Operation {
        codec: codec.to_owned(),
        message: format!("{op}: {}", msg.into()),
    }
    .into()
}

/// Hard cap on the samples of one decoded frame. 2^28 samples is 512 MiB of
/// `u16`, far above any real DICOM image, and stops a tiny codestream that
/// declares `65535 x 65535` from forcing a multi-gigabyte allocation
/// (design §10: dimensions and frame sizes are rejected before allocation).
pub(crate) const MAX_FRAME_SAMPLES: usize = 1 << 28;

/// Validates `width * height * components` before a frame buffer is allocated.
/// The dimensions come from untrusted metadata, so overflow and the sample cap
/// are both checked.
pub(crate) fn checked_sample_count(
    width: usize,
    height: usize,
    components: usize,
) -> Result<usize, String> {
    let count = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(components))
        .ok_or("image dimensions overflow")?;
    if count > MAX_FRAME_SAMPLES {
        return Err(format!(
            "image dimensions exceed the {MAX_FRAME_SAMPLES} sample limit"
        ));
    }
    Ok(count)
}

/// PS3.5 Table 7.1-1: encapsulated Pixel Data uses VR `OB` regardless of
/// `BitsAllocated`. `PixelData::add_frame` creates an `OW` element for 16-bit
/// samples, so rewrite the fragment sequence after the codec appended frames.
fn ensure_encapsulated_pixel_data_is_ob(dst: &mut PixelData<'_>) {
    let fragments = match dst.dataset().item(&Tag::PIXEL_DATA) {
        Some(DicomItem::Fragments(fragments)) if fragments.vr() != Vr::Ob => fragments.clone(),
        _ => return,
    };
    let mut rebuilt = DicomFragmentSequence::new(Tag::PIXEL_DATA, Vr::Ob);
    for fragment in fragments.fragments() {
        rebuilt.push_fragment_raw(fragment.clone());
    }
    rebuilt.set_offset_table(fragments.offset_table().to_vec());
    dst.dataset_mut()
        .insert_or_update(DicomItem::Fragments(rebuilt));
}

fn operate(
    codec: &str,
    syntax: &TransferSyntax,
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &CodecParams,
    decode: bool,
) -> Result<(), Error> {
    let p = match params {
        CodecParams::JpegLs(p) => p,
        _ => {
            return Err(error(
                codec,
                if decode { "decode" } else { "encode" },
                "requires CodecParams::JpegLs",
            ))
        }
    };
    if p.preset_coding_params != 0 || p.reset_contexts {
        return Err(error(
            codec,
            if decode { "decode" } else { "encode" },
            "requested JPEG-LS option is unsupported",
        ));
    }
    if syntax.uid() == syntaxes::JPEG_LS_LOSSLESS.uid() && p.near_lossless != 0 {
        return Err(error(
            codec,
            if decode { "decode" } else { "encode" },
            "lossless JPEG-LS requires NEAR=0",
        ));
    }
    if decode {
        if src.transfer_syntax().uid() != syntax.uid() {
            return Err(error(
                codec,
                "decode",
                format!(
                    "source transfer syntax is {}, expected {}",
                    src.transfer_syntax().uid(),
                    syntax.uid()
                ),
            ));
        }
        if dst.transfer_syntax().is_encapsulated() {
            return Err(error(
                codec,
                "decode",
                "destination must use native transfer syntax",
            ));
        }
        let layout = Layout::destination(dst).map_err(|m| error(codec, "decode", m))?;
        // jpeg-ls.md §5: the near-lossless syntax rejects PALETTE COLOR even when
        // NEAR is 0, because NEAR would quantize the LUT indices.
        if syntax.uid() == syntaxes::JPEG_LS_NEAR_LOSSLESS.uid()
            && layout.pi == PhotometricInterpretation::PaletteColor
        {
            return Err(error(
                codec,
                "decode",
                "PALETTE COLOR requires lossless JPEG-LS (.80)",
            ));
        }
        // The compressed dataset's PI describes its samples. A YBR_FULL stream is
        // converted to RGB exactly like the encode path converts YBR input, so the
        // output samples and the rewritten PI agree (PS3.5 Table 8.2.3-1 note 3).
        let source_ybr =
            src.photometric_interpretation() == Some(PhotometricInterpretation::YbrFull);
        let mut frames = Vec::new();
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "decode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "decode", format!("frame {i}: {e}")))?;
            frames.push(
                frame::decode(
                    &layout,
                    bytes.as_ref(),
                    p,
                    (syntax.uid() == syntaxes::JPEG_LS_LOSSLESS.uid()).then_some(0),
                    source_ybr,
                )
                .map_err(|m| error(codec, "decode", format!("frame {i}: {m}")))?,
            )
        }
        // Design §8.1: rewrite metadata that affects the frame-size calculation
        // before the first `add_frame`, never after it.
        frame::update_decoded_color_metadata(dst, &layout);
        for f in frames {
            dst.add_frame(f)
                .map_err(|e| error(codec, "decode", e.to_string()))?;
        }
        Ok(())
    } else {
        if dst.transfer_syntax().uid() != syntax.uid() {
            return Err(error(
                codec,
                "encode",
                format!(
                    "destination transfer syntax is {}, expected {}",
                    dst.transfer_syntax().uid(),
                    syntax.uid()
                ),
            ));
        }
        if src.transfer_syntax().is_encapsulated() || !dst.transfer_syntax().is_encapsulated() {
            return Err(error(
                codec,
                "encode",
                "source must be native and destination encapsulated",
            ));
        }
        let layout = Layout::source(src).map_err(|m| error(codec, "encode", m))?;
        let destination = Layout::destination(dst).map_err(|m| error(codec, "encode", m))?;
        if layout.width != destination.width
            || layout.height != destination.height
            || layout.bits_allocated != destination.bits_allocated
            || layout.bits_stored != destination.bits_stored
            || layout.components != destination.components
            || layout.signed != destination.signed
        {
            return Err(error(
                codec,
                "encode",
                "source and destination pixel layouts differ",
            ));
        }
        // jpeg-ls.md §5: the near-lossless syntax rejects PALETTE COLOR even when
        // NEAR is 0, because NEAR would quantize the LUT indices.
        if syntax.uid() == syntaxes::JPEG_LS_NEAR_LOSSLESS.uid()
            && layout.pi == PhotometricInterpretation::PaletteColor
        {
            return Err(error(
                codec,
                "encode",
                "PALETTE COLOR requires lossless JPEG-LS (.80)",
            ));
        }
        let mut frames = Vec::new();
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            frames.push(
                frame::encode(&layout, bytes.as_ref(), p)
                    .map_err(|m| error(codec, "encode", format!("frame {i}: {m}")))?,
            )
        }
        for f in frames {
            dst.add_frame(f)
                .map_err(|e| error(codec, "encode", e.to_string()))?;
        }
        frame::update_color_metadata(dst, &layout);
        ensure_encapsulated_pixel_data_is_ob(dst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_match_transfer_syntaxes() {
        assert_eq!(
            JpegLsLosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.4.80"
        );
        assert_eq!(
            JpegLsNearLosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.4.81"
        );
        assert_eq!(
            JpegLsNearLosslessCodec.default_params(),
            CodecParams::JpegLs(JpegLsParams {
                near_lossless: 3,
                ..Default::default()
            })
        );
    }
}
#[cfg(test)]
mod facade_tests {
    use super::*;
    use radicom::{
        model::{dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr},
        pixel::{PixelData, PixelDataRef},
    };
    fn ds(
        syntax: std::sync::Arc<TransferSyntax>,
        w: u32,
        h: u32,
        bits: u16,
        comp: u16,
        rep: u16,
        pi: &str,
    ) -> Dataset {
        let mut d = Dataset::new();
        d.set_transfer_syntax(syntax);
        d.insert_value(Tag::ROWS, h);
        d.insert_value(Tag::COLUMNS, w);
        d.insert_value(Tag::BITS_ALLOCATED, bits);
        d.insert_value(Tag::BITS_STORED, bits);
        d.insert_value(Tag::HIGH_BIT, bits - 1);
        d.insert_value(Tag::SAMPLES_PER_PIXEL, comp);
        d.insert_value(Tag::PIXEL_REPRESENTATION, rep);
        d.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, pi);
        if comp > 1 {
            d.insert_value(Tag::PLANAR_CONFIGURATION, 0u16);
        }
        d
    }
    fn encode_stream(source: &Dataset) -> Dataset {
        let mut compressed = source.clone();
        compressed.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        compressed.remove(Tag::PIXEL_DATA);
        JpegLsLosslessCodec
            .encode(
                &PixelDataRef::from_dataset(source).unwrap(),
                &mut PixelData::create_new(&mut compressed).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap();
        compressed
    }
    fn decode_stream(compressed: &Dataset) -> Dataset {
        let mut decoded = compressed.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        JpegLsLosslessCodec
            .decode(
                &PixelDataRef::from_dataset(compressed).unwrap(),
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap();
        decoded
    }
    fn frame_bytes(ds: &Dataset) -> Vec<u8> {
        PixelDataRef::from_dataset(ds)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec()
    }
    fn native_bytes(ba: u16, samples: &[u16]) -> Vec<u8> {
        if ba == 8 {
            samples.iter().map(|v| *v as u8).collect()
        } else {
            samples.iter().flat_map(|v| v.to_le_bytes()).collect()
        }
    }
    #[test]
    fn mono8_lossless_codec_roundtrip() {
        let raw = vec![0, 37, 74, 112, 149, 186, 224, 5, 42, 80, 117, 154];
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            4,
            3,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let mut enc = source.clone();
        enc.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        enc.remove(Tag::PIXEL_DATA);
        let c = JpegLsLosslessCodec;
        {
            let s = PixelDataRef::from_dataset(&source).unwrap();
            let mut d = PixelData::create_new(&mut enc).unwrap();
            c.encode(&s, &mut d, &c.default_params()).unwrap();
        }
        let mut out = enc.clone();
        out.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        out.remove(Tag::PIXEL_DATA);
        {
            let s = PixelDataRef::from_dataset(&enc).unwrap();
            let mut d = PixelData::create_new(&mut out).unwrap();
            c.decode(&s, &mut d, &c.default_params()).unwrap();
        }
        assert_eq!(
            PixelDataRef::from_dataset(&out)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .as_ref(),
            raw.as_slice()
        );
    }
    #[test]
    fn encapsulated_16_bit_frames_are_written_with_ob_pixel_data() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            2,
            16,
            1,
            0,
            "MONOCHROME2",
        );
        let raw = [0u8, 1, 0, 2, 0, 3, 0, 4];
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(raw)
            .unwrap();
        let enc = encode_stream(&source);
        // `add_frame` creates OW for 16-bit samples; encapsulated Pixel Data
        // must use OB (PS3.5 Table 7.1-1), so the codec rewrites the sequence.
        match enc.item(&Tag::PIXEL_DATA) {
            Some(DicomItem::Fragments(fragments)) => {
                assert_eq!(fragments.vr(), Vr::Ob);
                assert_eq!(fragments.fragments().len(), 1);
            }
            other => panic!("expected encapsulated fragments, got {other:?}"),
        }
        assert_eq!(frame_bytes(&decode_stream(&enc)), raw);
    }

    #[test]
    fn rejects_a_declared_frame_above_the_sample_limit() {
        // 65535 x 65535 x 3 samples would be 12.8 GiB, declared by a 40-byte
        // codestream; the codec must fail before allocating anything.
        let mut compressed = ds(
            syntaxes::JPEG_LS_LOSSLESS.clone(),
            65535,
            65535,
            8,
            3,
            0,
            "RGB",
        );
        let mut codestream = Vec::new();
        crate::marker::write_soi(&mut codestream);
        crate::marker::write_sof55(
            &mut codestream,
            &crate::marker::FrameHeader {
                precision: 8,
                width: 65535,
                height: 65535,
                components: 3,
            },
        )
        .unwrap();
        crate::marker::write_sos(&mut codestream, 3, 0, 2).unwrap();
        crate::marker::write_eoi(&mut codestream);
        PixelData::create_new(&mut compressed)
            .unwrap()
            .add_frame(codestream)
            .unwrap();
        let mut decoded = compressed.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        let error = JpegLsLosslessCodec
            .decode(
                &PixelDataRef::from_dataset(&compressed).unwrap(),
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap_err();
        assert!(format!("{error}").contains("sample limit"), "{error}");
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rgb_color_transform_roundtrip() {
        // The HP transforms are an encoding preference: the stream carries the
        // APP8 segment and the decoder undoes it without adapter involvement.
        let raw = (0..24).map(|i| (i * 11) as u8).collect::<Vec<_>>();
        for color_transformation in 1..=3 {
            let mut source = ds(
                syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
                2,
                4,
                8,
                3,
                0,
                "RGB",
            );
            PixelData::create_new(&mut source)
                .unwrap()
                .add_frame(&raw)
                .unwrap();
            let mut enc = source.clone();
            enc.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
            enc.remove(Tag::PIXEL_DATA);
            let c = JpegLsLosslessCodec;
            let params = CodecParams::JpegLs(JpegLsParams {
                color_transformation,
                ..Default::default()
            });
            {
                let s = PixelDataRef::from_dataset(&source).unwrap();
                let mut d = PixelData::create_new(&mut enc).unwrap();
                c.encode(&s, &mut d, &params).unwrap();
            }
            let codestream = PixelDataRef::from_dataset(&enc)
                .unwrap()
                .get_frame(0)
                .unwrap();
            let bytes = codestream.data().unwrap();
            assert_eq!(
                bytes
                    .as_ref()
                    .windows(4)
                    .filter(|window| *window == b"mrfx")
                    .count(),
                1,
                "HP{color_transformation} writes one mrfx segment"
            );
            let mut out = enc.clone();
            out.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
            out.remove(Tag::PIXEL_DATA);
            {
                let s = PixelDataRef::from_dataset(&enc).unwrap();
                let mut d = PixelData::create_new(&mut out).unwrap();
                // The decode-side params are ignored: the stream decides.
                c.decode(&s, &mut d, &c.default_params()).unwrap();
            }
            assert_eq!(
                PixelDataRef::from_dataset(&out)
                    .unwrap()
                    .get_frame(0)
                    .unwrap()
                    .data()
                    .unwrap()
                    .as_ref(),
                raw.as_slice(),
                "HP{color_transformation}"
            );
        }
    }

    #[test]
    fn rejects_color_transformation_for_monochrome() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            2,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([3u8, 4, 5, 6])
            .unwrap();
        let mut enc = source.clone();
        enc.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        enc.remove(Tag::PIXEL_DATA);
        let c = JpegLsLosslessCodec;
        let err = c
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut enc).unwrap(),
                &CodecParams::JpegLs(JpegLsParams {
                    color_transformation: 1,
                    ..Default::default()
                }),
            )
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("three unsigned components"),
            "{err:?}"
        );
    }

    #[test]
    fn rgb_sample_interleave_roundtrip() {
        // The default interleave mode derives from storage: an interleaved
        // three-component frame is written as one sample-interleaved scan.
        let raw = (0..24).map(|i| (i * 11) as u8).collect::<Vec<_>>();
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            4,
            8,
            3,
            0,
            "RGB",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let mut enc = source.clone();
        enc.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        enc.remove(Tag::PIXEL_DATA);
        let c = JpegLsLosslessCodec;
        {
            let s = PixelDataRef::from_dataset(&source).unwrap();
            let mut d = PixelData::create_new(&mut enc).unwrap();
            c.encode(&s, &mut d, &c.default_params()).unwrap();
        }
        let mut out = enc.clone();
        out.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        out.remove(Tag::PIXEL_DATA);
        {
            let s = PixelDataRef::from_dataset(&enc).unwrap();
            let mut d = PixelData::create_new(&mut out).unwrap();
            c.decode(&s, &mut d, &c.default_params()).unwrap();
        }
        assert_eq!(
            PixelDataRef::from_dataset(&out)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .as_ref(),
            raw.as_slice()
        );
    }

    #[test]
    fn rgb_line_interleave_roundtrip_with_explicit_parameter() {
        let raw = (0..24).map(|i| (i * 17) as u8).collect::<Vec<_>>();
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            4,
            8,
            3,
            0,
            "RGB",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let mut compressed = source.clone();
        compressed.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        compressed.remove(Tag::PIXEL_DATA);
        let params = CodecParams::JpegLs(JpegLsParams {
            interleave_mode: 2,
            ..Default::default()
        });
        {
            let source_view = PixelDataRef::from_dataset(&source).unwrap();
            let mut target = PixelData::create_new(&mut compressed).unwrap();
            JpegLsLosslessCodec
                .encode(&source_view, &mut target, &params)
                .unwrap();
        }
        let mut decoded = compressed.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        {
            let source_view = PixelDataRef::from_dataset(&compressed).unwrap();
            let mut target = PixelData::create_new(&mut decoded).unwrap();
            JpegLsLosslessCodec
                .decode(
                    &source_view,
                    &mut target,
                    &JpegLsLosslessCodec.default_params(),
                )
                .unwrap();
        }
        assert_eq!(
            PixelDataRef::from_dataset(&decoded)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .as_ref(),
            raw.as_slice()
        );
    }

    #[test]
    fn rgb_plane_interleave_roundtrip_with_explicit_parameter() {
        // Mode one asks for plane interleaving (one scan per component) even
        // though the source stores interleaved samples.
        let raw = (0..24).map(|i| (i * 19) as u8).collect::<Vec<_>>();
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            4,
            8,
            3,
            0,
            "RGB",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let mut compressed = source.clone();
        compressed.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        compressed.remove(Tag::PIXEL_DATA);
        let params = CodecParams::JpegLs(JpegLsParams {
            interleave_mode: 1,
            ..Default::default()
        });
        {
            let source_view = PixelDataRef::from_dataset(&source).unwrap();
            let mut target = PixelData::create_new(&mut compressed).unwrap();
            JpegLsLosslessCodec
                .encode(&source_view, &mut target, &params)
                .unwrap();
        }
        let mut decoded = compressed.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        {
            let source_view = PixelDataRef::from_dataset(&compressed).unwrap();
            let mut target = PixelData::create_new(&mut decoded).unwrap();
            JpegLsLosslessCodec
                .decode(
                    &source_view,
                    &mut target,
                    &JpegLsLosslessCodec.default_params(),
                )
                .unwrap();
        }
        assert_eq!(
            PixelDataRef::from_dataset(&decoded)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .as_ref(),
            raw.as_slice()
        );
    }

    #[test]
    fn ybr_full_is_normalized_to_rgb_on_both_paths() {
        let ybr = vec![128u8, 128, 128, 100, 90, 200, 0, 0, 0];
        let rgb = [128u8, 128, 128, 201, 62, 33, 0, 135, 0];

        // Encode path: a YBR_FULL source is converted to RGB samples and relabelled.
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            3,
            8,
            3,
            0,
            "YBR_FULL",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&ybr)
            .unwrap();
        let compressed = encode_stream(&source);
        assert_eq!(
            compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        assert_eq!(frame_bytes(&decode_stream(&compressed)), rgb);

        // Decode path: the compressed dataset's PI says YBR_FULL, so its samples
        // are YBR and must be converted before they are relabelled RGB. Encoding an
        // RGB-labelled copy of the same samples keeps them unconverted.
        let mut foreign = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            3,
            8,
            3,
            0,
            "RGB",
        );
        PixelData::create_new(&mut foreign)
            .unwrap()
            .add_frame(&ybr)
            .unwrap();
        let mut stream = encode_stream(&foreign);
        stream.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL");
        let decoded = decode_stream(&stream);
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        assert_eq!(frame_bytes(&decoded), rgb);
    }

    #[test]
    fn rgb_labelled_stream_is_not_colour_converted() {
        let rgb = vec![0u8, 10, 200, 30, 128, 255, 3, 99, 17];
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            3,
            8,
            3,
            0,
            "RGB",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&rgb)
            .unwrap();
        let decoded = decode_stream(&encode_stream(&source));
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        assert_eq!(frame_bytes(&decoded), rgb, "RGB samples stay untouched");
    }

    #[test]
    fn palette_color_round_trips_with_photometric_interpretation_preserved() {
        const PAL8: &[u16] = &[0, 1, 2, 3, 4, 5, 6, 255, 128, 64, 32, 7];
        const PAL2: &[u16] = &[0, 1, 2, 3, 3, 2, 1, 0, 1, 3, 2, 0];
        const PAL8B: &[u16] = &[0, 1, 255, 128, 7, 42, 200, 3, 99, 17, 240, 1];
        const PAL12: &[u16] = &[0, 1, 4095, 2048, 7, 42, 3000, 3, 999, 17, 2400, 1];
        const PAL16: &[u16] = &[0, 1, 65535, 32768, 7, 42, 60000, 3, 999, 17, 2400, 1];
        // PALETTE COLOR is a one-sample index image: no colour conversion happens
        // and the PI must survive the encode/decode pair unchanged.
        for (ba, bs, samples) in [
            (8u16, 8u16, PAL8),
            (16, 2, PAL2),
            (16, 8, PAL8B),
            (16, 12, PAL12),
            (16, 16, PAL16),
        ] {
            let mut source = ds(
                syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
                4,
                3,
                ba,
                1,
                0,
                "PALETTE COLOR",
            );
            source.insert_value(Tag::BITS_STORED, bs);
            source.insert_value(Tag::HIGH_BIT, bs - 1);
            PixelData::create_new(&mut source)
                .unwrap()
                .add_frame(native_bytes(ba, samples))
                .unwrap();
            let compressed = encode_stream(&source);
            assert_eq!(
                compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
                "PALETTE COLOR",
                "encode keeps the palette PI (BA={ba} BS={bs})"
            );
            let decoded = decode_stream(&compressed);
            assert_eq!(
                decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
                "PALETTE COLOR",
                "decode keeps the palette PI (BA={ba} BS={bs})"
            );
            assert_eq!(
                frame_bytes(&decoded),
                native_bytes(ba, samples),
                "BA={ba} BS={bs}"
            );
        }
    }

    #[test]
    fn palette_color_with_three_components_is_rejected_on_both_paths() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            3,
            0,
            "PALETTE COLOR",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([1u8, 2, 3])
            .unwrap();
        let mut target = source.clone();
        target.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        target.remove(Tag::PIXEL_DATA);
        let error = JpegLsLosslessCodec
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut target).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("PhotometricInterpretation"));
        assert!(!target.contains(&Tag::PIXEL_DATA));

        let mut mono = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut mono)
            .unwrap()
            .add_frame([7u8])
            .unwrap();
        let stream = encode_stream(&mono);
        let mut decoded = stream.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        decoded.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "PALETTE COLOR");
        decoded.insert_value(Tag::SAMPLES_PER_PIXEL, 3u16);
        let error = JpegLsLosslessCodec
            .decode(
                &PixelDataRef::from_dataset(&stream).unwrap(),
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("PhotometricInterpretation"));
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn near_lossless_palette_color_is_rejected_but_lossless_palette_works() {
        const PAL: &[u8] = &[0, 1, 2, 3];
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            2,
            2,
            8,
            1,
            0,
            "PALETTE COLOR",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(PAL)
            .unwrap();

        // `.80` lossless palette colour round-trips and keeps its Photometric
        // Interpretation unchanged.
        let compressed = encode_stream(&source);
        assert_eq!(
            compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "PALETTE COLOR"
        );
        assert_eq!(
            frame_bytes(&decode_stream(&compressed)),
            PAL.to_vec(),
            "lossless (.80) palette still round-trips"
        );

        // `.81` must reject PALETTE COLOR even when NEAR is zero: the syntax is
        // near-lossless, so LUT indices may not be quantized (jpeg-ls.md §5).
        let params = CodecParams::JpegLs(JpegLsParams {
            near_lossless: 0,
            ..Default::default()
        });
        let mut target = source.clone();
        target.set_transfer_syntax(syntaxes::JPEG_LS_NEAR_LOSSLESS.clone());
        target.remove(Tag::PIXEL_DATA);
        let error = JpegLsNearLosslessCodec
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut target).unwrap(),
                &params,
            )
            .unwrap_err();
        assert!(error.to_string().contains("PALETTE COLOR"), "{error}");
        assert!(!target.contains(&Tag::PIXEL_DATA));

        // The decode side enforces the same rule: a `.81`-labelled palette
        // stream is rejected before any frame is appended.
        let mut relabeled = compressed.clone();
        relabeled.set_transfer_syntax(syntaxes::JPEG_LS_NEAR_LOSSLESS.clone());
        let mut decoded = relabeled.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        let error = JpegLsNearLosslessCodec
            .decode(
                &PixelDataRef::from_dataset(&relabeled).unwrap(),
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &JpegLsNearLosslessCodec.default_params(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("PALETTE COLOR"), "{error}");
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_wrong_destination_uid_without_appending_a_frame() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let mut target = source.clone();
        target.set_transfer_syntax(syntaxes::JPEG_LS_NEAR_LOSSLESS.clone());
        target.remove(Tag::PIXEL_DATA);
        let result = JpegLsLosslessCodec.encode(
            &PixelDataRef::from_dataset(&source).unwrap(),
            &mut PixelData::create_new(&mut target).unwrap(),
            &JpegLsLosslessCodec.default_params(),
        );
        assert!(result.is_err());
        assert!(!target.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_incompatible_destination_metadata_before_encoding() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let mut target = source.clone();
        target.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        target.remove(Tag::PIXEL_DATA);
        target.insert_value(Tag::COLUMNS, 2u16);
        assert!(JpegLsLosslessCodec
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut target).unwrap(),
                &JpegLsLosslessCodec.default_params()
            )
            .is_err());
        assert!(!target.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn rejects_unsupported_decode_options_without_appending_frames() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([7])
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        encoded.remove(Tag::PIXEL_DATA);
        JpegLsLosslessCodec
            .encode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut encoded).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap();
        for params in [
            CodecParams::None,
            CodecParams::JpegLs(JpegLsParams {
                preset_coding_params: 1,
                ..Default::default()
            }),
            CodecParams::JpegLs(JpegLsParams {
                reset_contexts: true,
                ..Default::default()
            }),
        ] {
            let mut target = source.clone();
            target.remove(Tag::PIXEL_DATA);
            assert!(JpegLsLosslessCodec
                .decode(
                    &PixelDataRef::from_dataset(&encoded).unwrap(),
                    &mut PixelData::create_new(&mut target).unwrap(),
                    &params
                )
                .is_err());
            assert!(!target.contains(&Tag::PIXEL_DATA));
        }
    }

    #[test]
    fn untrusted_frame_count_does_not_preallocate_the_frame_list() {
        let mut source = ds(
            syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone(),
            1,
            1,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([7u8])
            .unwrap();
        let mut encoded = encode_stream(&source);
        // An untrusted NumberOfFrames must fail at the first missing frame
        // instead of sizing a huge Vec up front.
        encoded.insert_str(Tag::NUMBER_OF_FRAMES, Vr::Is, u32::MAX.to_string());
        let mut decoded = encoded.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        let error = JpegLsLosslessCodec
            .decode(
                &PixelDataRef::from_dataset(&encoded).unwrap(),
                &mut PixelData::create_new(&mut decoded).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("frame 1"), "{error}");
        assert!(!decoded.contains(&Tag::PIXEL_DATA));
    }

    #[test]
    fn malformed_later_frame_leaves_destination_without_partial_output() {
        let mut source = ds(
            syntaxes::JPEG_LS_LOSSLESS.clone(),
            1,
            1,
            8,
            1,
            0,
            "MONOCHROME2",
        );
        let layout = Layout::destination(&PixelData::create_new(&mut source).unwrap()).unwrap();
        let valid = frame::encode(&layout, &[7], &JpegLsParams::default()).unwrap();
        {
            let mut pixels = PixelData::create_new(&mut source).unwrap();
            pixels.add_frame(valid).unwrap();
            pixels.add_frame([0xff, 0xd8, 0xff, 0xd9]).unwrap();
        }
        let mut target = source.clone();
        target.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        target.remove(Tag::PIXEL_DATA);
        let error = JpegLsLosslessCodec
            .decode(
                &PixelDataRef::from_dataset(&source).unwrap(),
                &mut PixelData::create_new(&mut target).unwrap(),
                &JpegLsLosslessCodec.default_params(),
            )
            .unwrap_err();
        assert!(matches!(error, Error::Codec(_)));
        assert!(error.to_string().contains("frame 1"));
        assert!(!target.contains(&Tag::PIXEL_DATA));
    }
}
#[test]
fn signed8_lossless_roundtrip() {
    use radicom::{
        model::{dataset::Dataset, tag::Tag, transfer_syntax::syntaxes, vr::Vr},
        pixel::{PixelData, PixelDataRef},
    };
    let mut source = Dataset::new();
    source.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    for (tag, val) in [
        (Tag::ROWS, 1u16),
        (Tag::COLUMNS, 4),
        (Tag::BITS_ALLOCATED, 8),
        (Tag::BITS_STORED, 8),
        (Tag::HIGH_BIT, 7),
        (Tag::SAMPLES_PER_PIXEL, 1),
        (Tag::PIXEL_REPRESENTATION, 1),
    ] {
        source.insert_value(tag, val);
    }
    source.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "MONOCHROME2");
    PixelData::create_new(&mut source)
        .unwrap()
        .add_frame([128, 255, 0, 127])
        .unwrap();
    let mut enc = source.clone();
    enc.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
    enc.remove(Tag::PIXEL_DATA);
    let c = JpegLsLosslessCodec;
    {
        let s = PixelDataRef::from_dataset(&source).unwrap();
        let mut d = PixelData::create_new(&mut enc).unwrap();
        c.encode(&s, &mut d, &c.default_params()).unwrap();
    }
    let mut out = enc.clone();
    out.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
    out.remove(Tag::PIXEL_DATA);
    {
        let s = PixelDataRef::from_dataset(&enc).unwrap();
        let mut d = PixelData::create_new(&mut out).unwrap();
        c.decode(&s, &mut d, &c.default_params()).unwrap();
    }
    assert_eq!(
        PixelDataRef::from_dataset(&out)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .as_ref(),
        &[128, 255, 0, 127]
    );
}
