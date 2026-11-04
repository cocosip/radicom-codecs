//! Pure Rust JPEG 2000 (ISO/IEC 15444-1) DICOM codecs.
//!
//! The classic reversible (`.90`) and irreversible (`.91`) transfer syntaxes
//! are implemented from the standard; HTJ2K (`.201`/`.202`/`.203`) is a
//! separate follow-up.

mod bit;
mod codestream;
mod frame;
mod geometry;
mod htj2k;
mod marker;
mod mq;
mod quantization;
mod tier1;
mod tier2;
mod wavelet;

use crate::htj2k::codestream as ht_codestream;
use frame::Layout;
use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams, HtJ2kParams, J2kParams, ProgressionOrder};
use radicom::pixel::{PixelData, PixelDataRef};

#[derive(Debug, Default, Clone, Copy)]
pub struct Jpeg2000LosslessCodec;
#[derive(Debug, Default, Clone, Copy)]
pub struct Jpeg2000LossyCodec;

impl Codec for Jpeg2000LosslessCodec {
    fn name(&self) -> &str {
        "JPEG 2000 Lossless"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::JPEG_2000_LOSSLESS
    }
    fn default_params(&self) -> CodecParams {
        CodecParams::Jpeg2000(J2kParams {
            irreversible: false,
            ..Default::default()
        })
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            false,
            true,
        )
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            true,
            true,
        )
    }
}

impl Codec for Jpeg2000LossyCodec {
    fn name(&self) -> &str {
        "JPEG 2000 Lossy"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::JPEG_2000_LOSSY
    }
    fn default_params(&self) -> CodecParams {
        CodecParams::Jpeg2000(J2kParams::default())
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            false,
            false,
        )
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            true,
            false,
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HtJpeg2000LosslessCodec;
#[derive(Debug, Default, Clone, Copy)]
pub struct HtJpeg2000LosslessRpclCodec;
#[derive(Debug, Default, Clone, Copy)]
pub struct HtJpeg2000LossyCodec;

fn progression_code(order: ProgressionOrder) -> u8 {
    match order {
        ProgressionOrder::Lrcp => 0,
        ProgressionOrder::Rlcp => 1,
        ProgressionOrder::Rpcl => 2,
        ProgressionOrder::Pcrl => 3,
        ProgressionOrder::Cprl => 4,
    }
}

fn ht_params(irreversible: bool, progression_order: ProgressionOrder) -> CodecParams {
    // fo-dicom.Codecs (OpenJPH) enables the colour transform whenever there is
    // more than one component, so the same default keeps the sizes and the
    // reconstructed samples aligned with the native reference.
    CodecParams::HtJpeg2000(HtJ2kParams {
        irreversible,
        employ_color_transform: true,
        progression_order,
        ..Default::default()
    })
}

impl Codec for HtJpeg2000LosslessCodec {
    fn name(&self) -> &str {
        "HTJ2K Lossless"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::HTJ2K_LOSSLESS
    }
    fn default_params(&self) -> CodecParams {
        ht_params(false, ProgressionOrder::Lrcp)
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate_ht(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            false,
            true,
        )
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate_ht(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            true,
            true,
        )
    }
}

impl Codec for HtJpeg2000LosslessRpclCodec {
    fn name(&self) -> &str {
        "HTJ2K Lossless RPCL"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::HTJ2K_LOSSLESS_RPCL
    }
    fn default_params(&self) -> CodecParams {
        ht_params(false, ProgressionOrder::Rpcl)
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate_ht(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            false,
            true,
        )
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate_ht(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            true,
            true,
        )
    }
}

impl Codec for HtJpeg2000LossyCodec {
    fn name(&self) -> &str {
        "HTJ2K Lossy"
    }
    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::HTJ2K_LOSSY
    }
    fn default_params(&self) -> CodecParams {
        ht_params(true, ProgressionOrder::Lrcp)
    }
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate_ht(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            false,
            false,
        )
    }
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), Error> {
        operate_ht(
            self.name(),
            self.transfer_syntax(),
            src,
            dst,
            params,
            true,
            false,
        )
    }
}
fn error(codec: &str, op: &str, message: impl Into<String>) -> Error {
    CodecError::Operation {
        codec: codec.to_owned(),
        message: format!("{op}: {}", message.into()),
    }
    .into()
}

#[allow(clippy::too_many_arguments)]
fn operate(
    codec: &str,
    syntax: &TransferSyntax,
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &CodecParams,
    decode: bool,
    reversible: bool,
) -> Result<(), Error> {
    let p = match params {
        CodecParams::Jpeg2000(p) => p,
        _ => {
            return Err(error(
                codec,
                if decode { "decode" } else { "encode" },
                "requires CodecParams::Jpeg2000",
            ))
        }
    };
    if !decode {
        if reversible && p.irreversible {
            return Err(error(
                codec,
                "encode",
                "lossless JPEG 2000 requires reversible coding",
            ));
        }
        if p.encode_signed_as_unsigned {
            return Err(error(
                codec,
                "encode",
                "encode_signed_as_unsigned is not implemented",
            ));
        }
        let will_be_lossy = !reversible && p.irreversible;
        if will_be_lossy && !p.rate_levels.is_empty() {
            return Err(error(
                codec,
                "encode",
                "rate_levels (multiple quality layers) are not implemented by the classic JPEG 2000 encoder",
            ));
        }
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
        let layout = Layout::destination(dst)
            .map_err(|m| error(codec, "decode", m))?
            .rgb_output()
            .map_err(|m| error(codec, "decode", m))?;
        frame::update_decoded_color_metadata(dst, &layout);
        let mut frames = Vec::new();
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "decode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "decode", format!("frame {i}: {e}")))?;
            frames.push(
                frame::decode(&layout, bytes.as_ref())
                    .map_err(|m| error(codec, "decode", format!("frame {i}: {m}")))?,
            );
        }
        for frame in frames {
            dst.add_frame(frame)
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
        let encode_reversible = if reversible { true } else { !p.irreversible };
        let apply_mct =
            p.allow_mct && p.update_photometric_interpretation && layout.components == 3;
        let mut frames = Vec::new();
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            // `rate` is the target compression ratio for the irreversible path:
            // the payload may occupy `1 / rate` of the stored pixel data. The
            // baseline files land 10% over that budget because OpenJPEG's rate
            // allocator works in whole truncation points.
            let target = if encode_reversible || p.rate == 0 {
                None
            } else {
                Some((bytes.as_ref().len() as f64 / f64::from(p.rate)).max(1.0))
            };
            let mut encoded =
                frame::encode(&layout, bytes.as_ref(), encode_reversible, apply_mct, 1.0)
                    .map_err(|m| error(codec, "encode", format!("frame {i}: {m}")))?;
            if let Some(target) = target {
                if encoded.len() as f64 > target {
                    // The quantization step is continuous (`QCD` carries an
                    // 11-bit mantissa), so bisect on log2(scale) instead of
                    // dropping whole bit-planes: dropping a bit-plane halves the
                    // payload and overshoots the requested ratio by up to 2x.
                    let mut floor = 0.0f64;
                    let mut ceiling: Option<f64> = None;
                    let mut log = (encoded.len() as f64 / target).log2().max(0.5);
                    let mut best: Option<Vec<u8>> = None;
                    let mut smallest = Vec::new();
                    for _ in 0..14 {
                        let data = frame::encode(
                            &layout,
                            bytes.as_ref(),
                            encode_reversible,
                            apply_mct,
                            2f64.powf(log),
                        )
                        .map_err(|m| error(codec, "encode", format!("frame {i}: {m}")))?;
                        let size = data.len() as f64;
                        if size <= target {
                            let close = size >= target * 0.98;
                            best = Some(data);
                            ceiling = Some(log);
                            if close {
                                break;
                            }
                        } else {
                            floor = log;
                            smallest = data;
                        }
                        log = match ceiling {
                            Some(ceiling) => (floor + ceiling) / 2.0,
                            None => floor + (size / target).log2().max(0.5),
                        };
                    }
                    // When the target is below the codestream floor (tiny frames)
                    // no scale fits, so keep the coarsest attempt instead of the
                    // unquantized one.
                    if let Some(best) = best {
                        encoded = best;
                    } else if !smallest.is_empty() {
                        encoded = smallest;
                    }
                }
            }
            frames.push(encoded);
        }
        for frame in frames {
            dst.add_frame(frame)
                .map_err(|e| error(codec, "encode", e.to_string()))?;
        }
        frame::use_ob_vr_for_16_bit(dst);
        let mct_label = if apply_mct {
            Some(if encode_reversible {
                "YBR_RCT"
            } else {
                "YBR_ICT"
            })
        } else {
            None
        };
        frame::update_color_metadata(dst, &layout, mct_label, p.update_photometric_interpretation);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn operate_ht(
    codec: &str,
    syntax: &TransferSyntax,
    src: &PixelDataRef<'_>,
    dst: &mut PixelData<'_>,
    params: &CodecParams,
    decode: bool,
    reversible: bool,
) -> Result<(), Error> {
    let p = match params {
        CodecParams::HtJpeg2000(p) => p,
        _ => {
            return Err(error(
                codec,
                if decode { "decode" } else { "encode" },
                "requires CodecParams::HtJpeg2000",
            ))
        }
    };
    if !decode && p.irreversible == reversible {
        return Err(error(
            codec,
            if decode { "decode" } else { "encode" },
            if reversible {
                "lossless HTJ2K requires reversible coding"
            } else {
                "lossy HTJ2K requires irreversible coding"
            },
        ));
    }
    if !decode
        && matches!(
            p.progression_order,
            ProgressionOrder::Pcrl | ProgressionOrder::Cprl
        )
    {
        return Err(error(
            codec,
            "encode",
            "PCRL and CPRL progression orders are not implemented by the HTJ2K codec",
        ));
    }
    // Decoding does not use the encoder's decomposition/quality/progression
    // settings (the codestream header is authoritative), so only encode parses
    // and validates them; a caller-supplied encode-only value must not make a
    // decodable stream fail.
    let levels = if decode {
        0
    } else {
        i32::try_from(p.decompositions)
            .map_err(|_| error(codec, "encode", "invalid decomposition count"))?
    };
    if !decode && !(0..=32).contains(&levels) {
        return Err(error(codec, "encode", "invalid decomposition count"));
    }
    let rpcl = syntax.uid() == syntaxes::HTJ2K_LOSSLESS_RPCL.uid();
    let expected_progression = if rpcl { Some(2) } else { None };
    let encode_progression = if rpcl {
        2
    } else {
        progression_code(p.progression_order)
    };
    let options = ht_codestream::EncodeOptions {
        progression: encode_progression,
        reversible,
        levels,
        mct: p.employ_color_transform,
        quality: p.quality,
        insert_tlm: p.insert_tlm_markers,
    };
    if !decode {
        options.validate().map_err(|m| error(codec, "encode", m))?;
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
        let layout = Layout::destination(dst)
            .map_err(|m| error(codec, "decode", m))?
            .rgb_output()
            .map_err(|m| error(codec, "decode", m))?;
        frame::update_decoded_color_metadata(dst, &layout);
        let mut frames = Vec::new();
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "decode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "decode", format!("frame {i}: {e}")))?;
            frames.push(
                frame::decode_ht(&layout, bytes.as_ref(), expected_progression)
                    .map_err(|m| error(codec, "decode", format!("frame {i}: {m}")))?,
            );
        }
        for frame in frames {
            dst.add_frame(frame)
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
        let mut frames = Vec::new();
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            frames.push(
                frame::encode_ht(&layout, bytes.as_ref(), &options)
                    .map_err(|m| error(codec, "encode", format!("frame {i}: {m}")))?,
            );
        }
        for frame in frames {
            dst.add_frame(frame)
                .map_err(|e| error(codec, "encode", e.to_string()))?;
        }
        frame::use_ob_vr_for_16_bit(dst);
        // A multiple component transform requires the matching label; fo-dicom's
        // JPEG 2000 codec writes YBR_RCT/YBR_ICT for the same reason.
        let mct_label = if options.mct && layout.components == 3 {
            Some(if reversible { "YBR_RCT" } else { "YBR_ICT" })
        } else {
            None
        };
        frame::update_color_metadata(dst, &layout, mct_label, true);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_transfer_syntaxes() {
        assert_eq!(
            Jpeg2000LosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.4.90"
        );
        assert_eq!(
            Jpeg2000LossyCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.4.91"
        );
        match Jpeg2000LosslessCodec.default_params() {
            CodecParams::Jpeg2000(p) => assert!(!p.irreversible),
            _ => panic!("wrong default parameter family"),
        }
        match Jpeg2000LossyCodec.default_params() {
            CodecParams::Jpeg2000(p) => assert!(p.irreversible),
            _ => panic!("wrong default parameter family"),
        }
    }
}

#[cfg(test)]
mod facade_tests {
    use super::*;
    use radicom::model::{dataset::Dataset, tag::Tag, vr::Vr};

    fn ds(w: u32, h: u32, bits: u16, comp: u16, rep: u16, pi: &str) -> Dataset {
        let mut d = Dataset::new();
        d.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
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

    fn round_trip(codec: &dyn Codec, source: &mut Dataset, raw: &[u8]) -> Dataset {
        PixelData::create_new(source)
            .unwrap()
            .add_frame(raw)
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(std::sync::Arc::new(codec.transfer_syntax().clone()));
        encoded.remove(Tag::PIXEL_DATA);
        {
            let src = PixelDataRef::from_dataset(source).unwrap();
            let mut dst = PixelData::create_new(&mut encoded).unwrap();
            codec
                .encode(&src, &mut dst, &codec.default_params())
                .unwrap();
        }
        let mut decoded = encoded.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        {
            let src = PixelDataRef::from_dataset(&encoded).unwrap();
            let mut dst = PixelData::create_new(&mut decoded).unwrap();
            codec
                .decode(&src, &mut dst, &codec.default_params())
                .unwrap();
        }
        decoded
    }

    fn samples(dataset: &Dataset) -> Vec<u8> {
        PixelDataRef::from_dataset(dataset)
            .unwrap()
            .get_frame(0)
            .unwrap()
            .data()
            .unwrap()
            .to_vec()
    }

    #[test]
    fn palette_color_lossless_round_trips_indices() {
        // The LUT index plane is a plain one-component image: no colour
        // transform, and the destination keeps PALETTE COLOR.
        let raw: Vec<u8> = (0..48).map(|i| ((i * 7) % 251) as u8).collect();
        let mut source = ds(8, 6, 8, 1, 0, "PALETTE COLOR");
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "PALETTE COLOR"
        );
    }

    #[test]
    fn palette_color_htj2k_lossless_round_trips_indices() {
        let raw: Vec<u8> = (0..48).map(|i| ((i * 11) % 251) as u8).collect();
        let mut source = ds(8, 6, 8, 1, 0, "PALETTE COLOR");
        let decoded = round_trip(&HtJpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "PALETTE COLOR"
        );
    }

    #[test]
    fn mono8_lossless_round_trips_exactly() {
        let raw: Vec<u8> = (0..48).map(|i| (i * 37 + 11) as u8).collect();
        let mut source = ds(8, 6, 8, 1, 0, "MONOCHROME2");
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn rgb_lossless_round_trips_exactly() {
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 19 + 3) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn planar_rgb_lossless_round_trips_exactly() {
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 29 + 7) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        source.insert_value(Tag::PLANAR_CONFIGURATION, 1u16);
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(raw.clone())
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(std::sync::Arc::new(
            Jpeg2000LosslessCodec.transfer_syntax().clone(),
        ));
        encoded.remove(Tag::PIXEL_DATA);
        {
            let src = PixelDataRef::from_dataset(&source).unwrap();
            let mut dst = PixelData::create_new(&mut encoded).unwrap();
            Jpeg2000LosslessCodec
                .encode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params())
                .unwrap();
        }
        let mut decoded = encoded.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        decoded.insert_value(Tag::PLANAR_CONFIGURATION, 1u16);
        {
            let src = PixelDataRef::from_dataset(&encoded).unwrap();
            let mut dst = PixelData::create_new(&mut decoded).unwrap();
            Jpeg2000LosslessCodec
                .decode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params())
                .unwrap();
        }
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn decoding_into_a_ybr_destination_exposes_rgb() {
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 23 + 5) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(raw.clone())
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(std::sync::Arc::new(
            Jpeg2000LosslessCodec.transfer_syntax().clone(),
        ));
        encoded.remove(Tag::PIXEL_DATA);
        {
            let src = PixelDataRef::from_dataset(&source).unwrap();
            let mut dst = PixelData::create_new(&mut encoded).unwrap();
            Jpeg2000LosslessCodec
                .encode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params())
                .unwrap();
        }
        let mut decoded = encoded.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        decoded.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL");
        {
            let src = PixelDataRef::from_dataset(&encoded).unwrap();
            let mut dst = PixelData::create_new(&mut decoded).unwrap();
            Jpeg2000LosslessCodec
                .decode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params())
                .unwrap();
        }
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn decoding_into_a_ybr_full_422_destination_does_not_panic() {
        // A 4:2:2-labelled native destination sizes frames at two bytes per
        // pixel; the adapter still emits full RGB, so the frame length has to
        // be recomputed instead of reusing the subsampled size.
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 23 + 5) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(raw.clone())
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(std::sync::Arc::new(
            Jpeg2000LosslessCodec.transfer_syntax().clone(),
        ));
        encoded.remove(Tag::PIXEL_DATA);
        {
            let src = PixelDataRef::from_dataset(&source).unwrap();
            let mut dst = PixelData::create_new(&mut encoded).unwrap();
            Jpeg2000LosslessCodec
                .encode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params())
                .unwrap();
        }
        let mut decoded = encoded.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        decoded.insert_str(Tag::PHOTOMETRIC_INTERPRETATION, Vr::Cs, "YBR_FULL_422");
        {
            let src = PixelDataRef::from_dataset(&encoded).unwrap();
            let mut dst = PixelData::create_new(&mut decoded).unwrap();
            Jpeg2000LosslessCodec
                .decode(&src, &mut dst, &Jpeg2000LosslessCodec.default_params())
                .unwrap();
        }
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn signed_8_bit_lossless_round_trips_exactly() {
        let raw: Vec<u8> = (0..8 * 6).map(|i| ((i * 37) % 256) as u8).collect();
        let mut source = ds(8, 6, 8, 1, 1, "MONOCHROME2");
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn signed_16_bit_lossless_round_trips_exactly() {
        let values: Vec<i16> = (0..8 * 6)
            .map(|i| ((i * 997) % 4000) as i16 - 2000)
            .collect();
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut source = ds(8, 6, 16, 1, 1, "MONOCHROME2");
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn twelve_bit_samples_round_trip_exactly() {
        let values: Vec<u16> = (0..8 * 6).map(|i| ((i * 37) % 4096) as u16).collect();
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut source = ds(8, 6, 16, 1, 0, "MONOCHROME2");
        source.insert_value(Tag::BITS_STORED, 12u16);
        source.insert_value(Tag::HIGH_BIT, 11u16);
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn signed_twelve_bit_samples_round_trip_exactly() {
        let values: Vec<i16> = (0..8 * 6)
            .map(|i| ((i * 53) % 4096) as i16 - 2048)
            .collect();
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut source = ds(8, 6, 16, 1, 1, "MONOCHROME2");
        source.insert_value(Tag::BITS_STORED, 12u16);
        source.insert_value(Tag::HIGH_BIT, 11u16);
        let decoded = round_trip(&Jpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn lossy_round_trips_within_a_small_tolerance() {
        let values: Vec<u8> = (0..16 * 16).map(|i| (i * 7 + i / 16) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&values)
            .unwrap();
        // `rate = 0` keeps every irreversible bit-plane, so the 9/7 path is
        // near-lossless; a non-zero `rate` deliberately trades accuracy for size.
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: true,
            rate: 0,
            ..Default::default()
        });
        let encoded = encode_with(&Jpeg2000LossyCodec, &mut source, &params).unwrap();
        let decoded = decode_with(&Jpeg2000LossyCodec, &encoded, &params).unwrap();
        let actual = samples(&decoded);
        let mut total = 0i64;
        for (a, b) in values.iter().zip(&actual) {
            total += i64::from((*a as i32 - *b as i32).abs());
        }
        let mean = total as f64 / values.len() as f64;
        assert!(mean < 2.0, "mean absolute error {mean}");
    }

    #[test]
    fn lossless_rejects_the_irreversible_parameter() {
        let mut source = ds(4, 4, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([0u8; 16])
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(syntaxes::JPEG_2000_LOSSLESS.clone());
        encoded.remove(Tag::PIXEL_DATA);
        let src = PixelDataRef::from_dataset(&source).unwrap();
        let mut dst = PixelData::create_new(&mut encoded).unwrap();
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: true,
            ..Default::default()
        });
        assert!(Jpeg2000LosslessCodec
            .encode(&src, &mut dst, &params)
            .is_err());
    }

    #[test]
    fn wrong_parameter_family_is_rejected() {
        let mut source = ds(4, 4, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame([0u8; 16])
            .unwrap();
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(syntaxes::JPEG_2000_LOSSLESS.clone());
        encoded.remove(Tag::PIXEL_DATA);
        let src = PixelDataRef::from_dataset(&source).unwrap();
        let mut dst = PixelData::create_new(&mut encoded).unwrap();
        assert!(Jpeg2000LosslessCodec
            .encode(&src, &mut dst, &CodecParams::None)
            .is_err());
    }

    fn encode_with(
        codec: &dyn Codec,
        source: &mut Dataset,
        params: &CodecParams,
    ) -> Result<Dataset, String> {
        let mut encoded = source.clone();
        encoded.set_transfer_syntax(std::sync::Arc::new(codec.transfer_syntax().clone()));
        encoded.remove(Tag::PIXEL_DATA);
        let src = PixelDataRef::from_dataset(source).unwrap();
        let mut dst = PixelData::create_new(&mut encoded).unwrap();
        codec
            .encode(&src, &mut dst, params)
            .map_err(|e| e.to_string())?;
        Ok(encoded)
    }

    fn decode_with(
        codec: &dyn Codec,
        encoded: &Dataset,
        params: &CodecParams,
    ) -> Result<Dataset, String> {
        let mut decoded = encoded.clone();
        decoded.set_transfer_syntax(syntaxes::EXPLICIT_VR_LITTLE_ENDIAN.clone());
        decoded.remove(Tag::PIXEL_DATA);
        let src = PixelDataRef::from_dataset(encoded).unwrap();
        let mut dst = PixelData::create_new(&mut decoded).unwrap();
        codec
            .decode(&src, &mut dst, params)
            .map_err(|e| e.to_string())?;
        Ok(decoded)
    }

    #[test]
    fn lossy_syntax_honours_irreversible_false() {
        let raw: Vec<u8> = (0..16 * 16).map(|i| (i * 31 + 7) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: false,
            ..Default::default()
        });
        let encoded = encode_with(&Jpeg2000LossyCodec, &mut source, &params).unwrap();
        let decoded = decode_with(&Jpeg2000LossyCodec, &encoded, &params).unwrap();
        assert_eq!(samples(&decoded), raw, "5/3 coding must be lossless");
    }

    #[test]
    fn lossless_decode_ignores_the_irreversible_parameter() {
        let raw: Vec<u8> = (0..16 * 16).map(|i| (i * 11 + 3) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let encoded = encode_with(
            &Jpeg2000LosslessCodec,
            &mut source,
            &Jpeg2000LosslessCodec.default_params(),
        )
        .unwrap();
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: true,
            rate: 50,
            rate_levels: vec![10],
            allow_mct: true,
            update_photometric_interpretation: true,
            encode_signed_as_unsigned: true,
        });
        let decoded = decode_with(&Jpeg2000LosslessCodec, &encoded, &params).unwrap();
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn ht_lossless_decode_ignores_the_irreversible_parameter() {
        let raw: Vec<u8> = (0..16 * 16).map(|i| (i * 7 + 5) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let codec = HtJpeg2000LosslessCodec;
        let encoded = encode_with(&codec, &mut source, &codec.default_params()).unwrap();
        let decoded =
            decode_with(&codec, &encoded, &ht_params(true, ProgressionOrder::Lrcp)).unwrap();
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn ht_decode_ignores_encode_only_parameters() {
        let raw: Vec<u8> = (0..16 * 16).map(|i| (i * 11 + 7) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let codec = HtJpeg2000LosslessCodec;
        let encoded = encode_with(&codec, &mut source, &codec.default_params()).unwrap();
        // These fields only affect encoding; decoding must use the codestream
        // header and still succeed.
        let params = CodecParams::HtJpeg2000(HtJ2kParams {
            irreversible: true,
            quality: 95,
            decompositions: 40,
            progression_order: ProgressionOrder::Pcrl,
            employ_color_transform: false,
            ..Default::default()
        });
        let decoded = decode_with(&codec, &encoded, &params).unwrap();
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn lossy_decode_ignores_rate_parameters() {
        let raw: Vec<u8> = (0..16 * 16).map(|i| (i * 13 + 3) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let encoded = encode_with(
            &Jpeg2000LossyCodec,
            &mut source,
            &Jpeg2000LossyCodec.default_params(),
        )
        .unwrap();
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: true,
            rate: 50,
            rate_levels: vec![10],
            ..Default::default()
        });
        let decoded = decode_with(&Jpeg2000LossyCodec, &encoded, &params).unwrap();
        assert_eq!(samples(&decoded).len(), raw.len());
    }

    #[test]
    fn ht_defaults_enable_the_color_transform() {
        for params in [
            HtJpeg2000LosslessCodec.default_params(),
            HtJpeg2000LosslessRpclCodec.default_params(),
            HtJpeg2000LossyCodec.default_params(),
        ] {
            match params {
                CodecParams::HtJpeg2000(p) => assert!(p.employ_color_transform),
                _ => panic!("wrong default parameter family"),
            }
        }
    }

    #[test]
    fn ht_rgb_lossless_round_trips_high_chroma_exactly() {
        // Chroma extremes push the reversible transform's U/V past the nominal
        // component range, which is what the two reserved bit-planes cover.
        let raw: Vec<u8> = (0..64 * 48 * 3)
            .map(|index| match index % 3 {
                0 => (index * 37 % 256) as u8,
                1 => (index * 91 % 256) as u8,
                _ => (index * 131 % 256) as u8,
            })
            .collect();
        let mut source = ds(64, 48, 8, 3, 0, "RGB");
        let decoded = round_trip(&HtJpeg2000LosslessCodec, &mut source, &raw);
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn unimplemented_encoder_parameters_are_reported() {
        let raw: Vec<u8> = (0..16 * 16).map(|i| (i * 5) as u8).collect();
        let mut source = ds(16, 16, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        for params in [
            J2kParams {
                rate_levels: vec![32],
                ..Default::default()
            },
            J2kParams {
                encode_signed_as_unsigned: true,
                ..Default::default()
            },
        ] {
            let params = CodecParams::Jpeg2000(params);
            let error = encode_with(&Jpeg2000LossyCodec, &mut source, &params).unwrap_err();
            assert!(
                error.contains("not implemented"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn a_higher_lossy_rate_writes_a_smaller_codestream() {
        fn codestream_len(source: &mut Dataset, rate: u32) -> usize {
            let params = CodecParams::Jpeg2000(J2kParams {
                irreversible: true,
                rate,
                ..Default::default()
            });
            let encoded = encode_with(&Jpeg2000LossyCodec, source, &params).unwrap();
            let frame = PixelDataRef::from_dataset(&encoded)
                .unwrap()
                .get_frame(0)
                .unwrap();
            frame.data().unwrap().len()
        }

        let raw: Vec<u8> = (0..64 * 64)
            .map(|i| ((i * 7 + (i / 64) * 5) % 256) as u8)
            .collect();
        let mut source = ds(64, 64, 8, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let near_lossless = codestream_len(&mut source, 0);
        let lossy = codestream_len(&mut source, 20);
        assert!(
            lossy * 2 < near_lossless,
            "rate 20 must shrink the codestream: {lossy} vs {near_lossless}"
        );
    }

    #[test]
    fn default_parameters_do_not_apply_a_component_transform() {
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 19 + 3) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let encoded = encode_with(
            &Jpeg2000LosslessCodec,
            &mut source,
            &Jpeg2000LosslessCodec.default_params(),
        )
        .unwrap();
        assert_eq!(
            encoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        let decoded = decode_with(
            &Jpeg2000LosslessCodec,
            &encoded,
            &Jpeg2000LosslessCodec.default_params(),
        )
        .unwrap();
        assert_eq!(samples(&decoded), raw);
    }

    #[test]
    fn component_transform_is_labelled_and_lossless() {
        let raw: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 23 + 5) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: false,
            allow_mct: true,
            update_photometric_interpretation: true,
            ..Default::default()
        });
        let encoded = encode_with(&Jpeg2000LosslessCodec, &mut source, &params).unwrap();
        assert_eq!(
            encoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "YBR_RCT"
        );
        let decoded = decode_with(&Jpeg2000LosslessCodec, &encoded, &params).unwrap();
        assert_eq!(samples(&decoded), raw);
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );

        let lossy_params = CodecParams::Jpeg2000(J2kParams {
            irreversible: true,
            allow_mct: true,
            update_photometric_interpretation: true,
            ..Default::default()
        });
        let encoded = encode_with(&Jpeg2000LossyCodec, &mut source, &lossy_params).unwrap();
        assert_eq!(
            encoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "YBR_ICT"
        );
    }

    #[test]
    fn multiple_frames_receive_photometric_interpretation_before_frames() {
        let first: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 17 + 1) as u8).collect();
        let second: Vec<u8> = (0..8 * 6 * 3).map(|i| (i * 13 + 9) as u8).collect();
        let mut source = ds(8, 6, 8, 3, 0, "RGB");
        {
            let mut pixel = PixelData::create_new(&mut source).unwrap();
            pixel.add_frame(&first).unwrap();
            pixel.add_frame(&second).unwrap();
        }
        let params = CodecParams::Jpeg2000(J2kParams {
            irreversible: false,
            allow_mct: true,
            update_photometric_interpretation: true,
            ..Default::default()
        });
        let encoded = encode_with(&Jpeg2000LosslessCodec, &mut source, &params).unwrap();
        let decoded = decode_with(&Jpeg2000LosslessCodec, &encoded, &params).unwrap();
        assert_eq!(
            decoded.get_str(Tag::PHOTOMETRIC_INTERPRETATION).unwrap(),
            "RGB"
        );
        let pixel = PixelDataRef::from_dataset(&decoded).unwrap();
        assert_eq!(pixel.number_of_frames(), 2);
        for (index, expected) in [first, second].iter().enumerate() {
            assert_eq!(
                pixel.get_frame(index as u32).unwrap().data().unwrap(),
                expected.as_slice()
            );
        }
    }

    #[test]
    fn encapsulated_16_bit_pixel_data_uses_ob() {
        let raw: Vec<u8> = (0..8 * 6).flat_map(|v: u16| v.to_le_bytes()).collect();
        let mut source = ds(8, 6, 16, 1, 0, "MONOCHROME2");
        PixelData::create_new(&mut source)
            .unwrap()
            .add_frame(&raw)
            .unwrap();
        let encoded = encode_with(
            &Jpeg2000LosslessCodec,
            &mut source,
            &Jpeg2000LosslessCodec.default_params(),
        )
        .unwrap();
        let item = encoded.item(&Tag::PIXEL_DATA).unwrap();
        match item {
            radicom::model::item::DicomItem::Fragments(sequence) => {
                assert_eq!(sequence.vr(), Vr::Ob)
            }
            other => panic!("expected a fragment sequence, got {other:?}"),
        }
    }
}
