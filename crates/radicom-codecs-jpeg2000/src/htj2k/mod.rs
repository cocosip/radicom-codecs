//! HTJ2K (ISO/IEC 15444-15) primitives.
//!
//! The module is introduced incrementally; the shared classic JPEG 2000
//! container and DICOM adapter are reused by the HT codecs.

#![allow(dead_code)]

pub(crate) mod mel;
pub(crate) mod tables;
