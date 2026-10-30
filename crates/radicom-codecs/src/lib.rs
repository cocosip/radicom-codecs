//! Concrete DICOM codecs for `radicom`.
//!
//! This crate owns explicit codec registration. The codec contract, pixel-data
//! facade, parameters, and transcoder remain in the `radicom` crate.

use std::collections::HashMap;
use std::sync::Arc;

use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::TransferSyntax;
use radicom::pixel::codec::{set_codec_registry, Codec, CodecRegistry};

pub use radicom_codecs_jpeg::{
    JpegLossless14Codec, JpegLossless14Sv1Codec, JpegProcess1Codec, JpegProcess2_4Codec,
};
pub use radicom_codecs_jpeg_ls::{JpegLsLosslessCodec, JpegLsNearLosslessCodec};
pub use radicom_codecs_rle::RleLosslessCodec;

/// Registry containing every codec supplied by this crate.
pub struct RadicomCodecRegistry {
    codecs: HashMap<String, Arc<dyn Codec>>,
}

impl RadicomCodecRegistry {
    /// Creates an empty registry. Use [`Default`] for the complete registration set.
    pub fn new() -> Self {
        Self {
            codecs: HashMap::new(),
        }
    }

    /// Adds or replaces a codec using its transfer syntax UID as the key.
    pub fn register(&mut self, codec: impl Codec + 'static) {
        let codec: Arc<dyn Codec> = Arc::new(codec);
        self.codecs
            .insert(codec.transfer_syntax().uid().to_owned(), codec);
    }

    /// Number of registered codec transfer syntaxes.
    pub fn len(&self) -> usize {
        self.codecs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codecs.is_empty()
    }
}

impl Default for RadicomCodecRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        registry.register(RleLosslessCodec);
        registry.register(JpegProcess1Codec);
        registry.register(JpegProcess2_4Codec);
        registry.register(JpegLossless14Codec);
        registry.register(JpegLossless14Sv1Codec);
        registry.register(JpegLsLosslessCodec);
        registry.register(JpegLsNearLosslessCodec);
        registry
    }
}

impl CodecRegistry for RadicomCodecRegistry {
    fn has_codec(&self, syntax: &TransferSyntax) -> bool {
        self.codecs.contains_key(syntax.uid())
    }

    fn can_transcode(&self, from: &TransferSyntax, to: &TransferSyntax) -> bool {
        (!from.is_encapsulated() || self.has_codec(from))
            && (!to.is_encapsulated() || self.has_codec(to))
    }

    fn get_codec(&self, syntax: &TransferSyntax) -> Result<Arc<dyn Codec>, Error> {
        self.codecs
            .get(syntax.uid())
            .cloned()
            .ok_or_else(|| CodecError::NotRegistered(syntax.uid().to_owned()).into())
    }
}

/// Builds the complete registry without changing the global radicom registry.
pub fn codec_registry() -> Arc<RadicomCodecRegistry> {
    Arc::new(RadicomCodecRegistry::default())
}

/// Installs the complete registry into the process-wide radicom codec boundary.
pub fn install() -> Arc<RadicomCodecRegistry> {
    let registry = codec_registry();
    set_codec_registry(registry.clone());
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use radicom::model::transfer_syntax::syntaxes;
    use radicom::pixel::codec::{reset_codec_registry, CodecParams};

    fn all_syntaxes() -> Vec<std::sync::Arc<TransferSyntax>> {
        vec![
            syntaxes::RLE_LOSSLESS.clone(),
            syntaxes::JPEG_PROCESS_1.clone(),
            syntaxes::JPEG_PROCESS_2_4.clone(),
            syntaxes::JPEG_LOSSLESS_PROCESS_14.clone(),
            syntaxes::JPEG_LOSSLESS_PROCESS_14_SV1.clone(),
            syntaxes::JPEG_LS_LOSSLESS.clone(),
            syntaxes::JPEG_LS_NEAR_LOSSLESS.clone(),
        ]
    }

    #[test]
    fn registers_all_supported_transfer_syntaxes() {
        let registry = codec_registry();
        let syntaxes = all_syntaxes();
        assert_eq!(registry.len(), syntaxes.len());
        for syntax in syntaxes {
            assert!(registry.has_codec(&syntax));
            assert!(registry.get_codec(&syntax).is_ok());
        }
    }

    #[test]
    fn supports_both_transcode_directions() {
        let registry = codec_registry();
        let raw = &syntaxes::EXPLICIT_VR_LITTLE_ENDIAN;
        for syntax in all_syntaxes() {
            assert!(registry.can_transcode(raw, &syntax));
            assert!(registry.can_transcode(&syntax, raw));
        }
    }

    #[test]
    fn defaults_are_family_specific() {
        let registry = codec_registry();
        assert!(matches!(
            registry
                .get_codec(&syntaxes::RLE_LOSSLESS)
                .unwrap()
                .default_params(),
            CodecParams::None
        ));
        assert!(matches!(
            registry
                .get_codec(&syntaxes::JPEG_PROCESS_1)
                .unwrap()
                .default_params(),
            CodecParams::Jpeg(_)
        ));
        assert!(matches!(
            registry
                .get_codec(&syntaxes::JPEG_LS_LOSSLESS)
                .unwrap()
                .default_params(),
            CodecParams::JpegLs(_)
        ));
    }

    #[test]
    fn install_injects_the_registry_into_radicom() {
        let registry = install();
        assert!(radicom::pixel::codec::codec_registry().has_codec(&syntaxes::RLE_LOSSLESS));
        assert_eq!(registry.len(), 7);
        reset_codec_registry();
    }
}
