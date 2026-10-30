//! Classic JPEG codec registrations.
//!
//! Marker, entropy, DCT, and lossless predictor coding will be implemented in
//! Stage 2. These types establish the frozen radicom codec boundary first.

use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams, JpegParams};
use radicom::pixel::{PixelData, PixelDataRef};

macro_rules! jpeg_codec {
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
                CodecParams::Jpeg(JpegParams::default())
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

jpeg_codec!(JpegProcess1Codec, "JPEG Baseline Process 1", JPEG_PROCESS_1);
jpeg_codec!(
    JpegProcess2_4Codec,
    "JPEG Extended Process 2/4",
    JPEG_PROCESS_2_4
);
jpeg_codec!(
    JpegLossless14Codec,
    "JPEG Lossless Process 14",
    JPEG_LOSSLESS_PROCESS_14
);
jpeg_codec!(
    JpegLossless14Sv1Codec,
    "JPEG Lossless Process 14 SV1",
    JPEG_LOSSLESS_PROCESS_14_SV1
);

fn unimplemented(codec: &str, operation: &str) -> Error {
    CodecError::Operation {
        codec: codec.to_owned(),
        message: format!("{operation} is not implemented; JPEG skeleton only"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_all_jpeg_transfer_syntaxes() {
        let codecs: [&dyn Codec; 4] = [
            &JpegProcess1Codec,
            &JpegProcess2_4Codec,
            &JpegLossless14Codec,
            &JpegLossless14Sv1Codec,
        ];
        assert_eq!(codecs[0].transfer_syntax().uid(), "1.2.840.10008.1.2.4.50");
        assert_eq!(codecs[1].transfer_syntax().uid(), "1.2.840.10008.1.2.4.51");
        assert_eq!(codecs[2].transfer_syntax().uid(), "1.2.840.10008.1.2.4.57");
        assert_eq!(codecs[3].transfer_syntax().uid(), "1.2.840.10008.1.2.4.70");
        assert!(codecs
            .iter()
            .all(|codec| matches!(codec.default_params(), CodecParams::Jpeg(_))));
    }
}
