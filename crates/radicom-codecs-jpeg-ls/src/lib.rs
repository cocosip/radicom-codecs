//! JPEG-LS codec registrations.
//!
//! JPEG-LS marker, regular-mode, run-mode, and Golomb coding will be implemented
//! in Stage 3.

use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams, JpegLsParams};
use radicom::pixel::{PixelData, PixelDataRef};

macro_rules! jpeg_ls_codec {
    ($type:ident, $name:literal, $syntax:ident) => {
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $type;

        impl Codec for $type {
            fn name(&self) -> &str {
                $name
            }

            fn transfer_syntax(&self) -> &'static TransferSyntax {
                &syntaxes::$syntax
            }

            fn default_params(&self) -> CodecParams {
                CodecParams::JpegLs(JpegLsParams::default())
            }

            fn encode(
                &self,
                _src: &PixelDataRef<'_>,
                _dst: &mut PixelData<'_>,
                _params: &CodecParams,
            ) -> Result<(), Error> {
                Err(unimplemented(self.name(), "encode"))
            }

            fn decode(
                &self,
                _src: &PixelDataRef<'_>,
                _dst: &mut PixelData<'_>,
                _params: &CodecParams,
            ) -> Result<(), Error> {
                Err(unimplemented(self.name(), "decode"))
            }
        }
    };
}

jpeg_ls_codec!(JpegLsLosslessCodec, "JPEG-LS Lossless", JPEG_LS_LOSSLESS);
jpeg_ls_codec!(
    JpegLsNearLosslessCodec,
    "JPEG-LS Near-Lossless",
    JPEG_LS_NEAR_LOSSLESS
);

fn unimplemented(codec: &str, operation: &str) -> Error {
    CodecError::Operation {
        codec: codec.to_owned(),
        message: format!("{operation} is not implemented; JPEG-LS skeleton only"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_jpeg_ls_transfer_syntaxes() {
        assert_eq!(
            JpegLsLosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.4.80"
        );
        assert_eq!(
            JpegLsNearLosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.4.81"
        );
        assert!(matches!(
            JpegLsLosslessCodec.default_params(),
            CodecParams::JpegLs(_)
        ));
    }
}
