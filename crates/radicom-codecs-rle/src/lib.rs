//! DICOM RLE Lossless codec.
//!
//! The codec registration is present in the skeleton. Bitstream coding will be
//! implemented in Stage 1.

use radicom::error::{CodecError, Error};
use radicom::model::transfer_syntax::{syntaxes, TransferSyntax};
use radicom::pixel::codec::{Codec, CodecParams};
use radicom::pixel::{PixelData, PixelDataRef};

#[derive(Debug, Default, Clone, Copy)]
pub struct RleLosslessCodec;

impl Codec for RleLosslessCodec {
    fn name(&self) -> &str {
        "RLE Lossless"
    }

    fn transfer_syntax(&self) -> &'static TransferSyntax {
        &syntaxes::RLE_LOSSLESS
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

fn unimplemented(codec: &str, operation: &str) -> Error {
    CodecError::Operation {
        codec: codec.to_owned(),
        message: format!("{operation} is not implemented; RLE skeleton only"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_rle_transfer_syntax() {
        assert_eq!(
            RleLosslessCodec.transfer_syntax().uid(),
            "1.2.840.10008.1.2.5"
        );
        assert_eq!(RleLosslessCodec.default_params(), CodecParams::None);
    }
}
