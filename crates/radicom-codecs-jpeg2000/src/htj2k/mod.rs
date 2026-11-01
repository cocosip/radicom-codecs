//! HTJ2K (ISO/IEC 15444-15) primitives.
//!
//! The module is introduced incrementally; the shared classic JPEG 2000
//! container and DICOM adapter are reused by the HT codecs.

pub(crate) mod cleanup;
pub(crate) mod codestream;
pub(crate) mod packet;
pub(crate) mod quant;
pub(crate) mod tables;
