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
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams, JpegLsParams};
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
        let mut frames = Vec::with_capacity(src.number_of_frames() as usize);
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
                )
                .map_err(|m| error(codec, "decode", format!("frame {i}: {m}")))?,
            )
        }
        for f in frames {
            dst.add_frame(f)
                .map_err(|e| error(codec, "decode", e.to_string()))?;
        }
        frame::update_decoded_color_metadata(dst, &layout);
        Ok(())
    } else {
        if src.transfer_syntax().is_encapsulated() || !dst.transfer_syntax().is_encapsulated() {
            return Err(error(
                codec,
                "encode",
                "source must be native and destination encapsulated",
            ));
        }
        let layout = Layout::source(src).map_err(|m| error(codec, "encode", m))?;
        let mut frames = Vec::with_capacity(src.number_of_frames() as usize);
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
    fn rgb_sample_interleave_roundtrip() {
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
    fn ybr_full_is_normalized_to_rgb_on_both_paths() {
        let ybr = vec![128, 128, 128, 100, 90, 200, 0, 0, 0];
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
        let mut compressed = source.clone();
        compressed.set_transfer_syntax(syntaxes::JPEG_LS_LOSSLESS.clone());
        compressed.remove(Tag::PIXEL_DATA);
        {
            let source_view = PixelDataRef::from_dataset(&source).unwrap();
            let mut target = PixelData::create_new(&mut compressed).unwrap();
            JpegLsLosslessCodec
                .encode(
                    &source_view,
                    &mut target,
                    &JpegLsLosslessCodec.default_params(),
                )
                .unwrap();
        }
        assert_eq!(
            compressed.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        let mut decoded = compressed.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        decoded.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL");
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
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        assert_eq!(
            PixelDataRef::from_dataset(&decoded)
                .unwrap()
                .get_frame(0)
                .unwrap()
                .data()
                .unwrap()
                .as_ref(),
            &[128, 128, 128, 201, 62, 33, 0, 135, 0]
        );
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
