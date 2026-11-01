//! Pure Rust JPEG 2000 (ISO/IEC 15444-1) DICOM codecs.
//!
//! The classic reversible (`.90`) and irreversible (`.91`) transfer syntaxes
//! are implemented from the standard; HTJ2K (`.201`/`.202`/`.203`) is a
//! separate follow-up.

mod bit;
mod codestream;
mod frame;
mod geometry;
mod marker;
mod mq;
mod quantization;
mod tier1;
mod tier2;
mod wavelet;

use frame::Layout;
use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams, J2kParams};
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
            if decode { "decode" } else { "encode" },
            "encode_signed_as_unsigned is not supported",
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
        let layout = Layout::destination(dst)
            .map_err(|m| error(codec, "decode", m))?
            .rgb_output();
        let mut frames = Vec::with_capacity(src.number_of_frames() as usize);
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
        frame::update_decoded_color_metadata(dst, &layout);
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
        let mut frames = Vec::with_capacity(src.number_of_frames() as usize);
        for i in 0..src.number_of_frames() {
            let b = src
                .get_frame(i)
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            let bytes = b
                .data()
                .map_err(|e| error(codec, "encode", format!("frame {i}: {e}")))?;
            frames.push(
                frame::encode(&layout, bytes.as_ref(), reversible)
                    .map_err(|m| error(codec, "encode", format!("frame {i}: {m}")))?,
            );
        }
        for frame in frames {
            dst.add_frame(frame)
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
        let decoded = round_trip(&Jpeg2000LossyCodec, &mut source, &values);
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
}
