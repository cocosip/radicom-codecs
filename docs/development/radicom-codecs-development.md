# radicom-codecs development plan and status

**Date:** 2026-11-01
**Current milestone:** JPEG 2000 and HTJ2K self-implementation design retained
**Status:** JPEG 2000/HTJ2K production code was removed; the design is pending a
new pure-Rust implementation

## Scope of this milestone

The workspace and registration boundary are complete. The RLE Lossless family,
the four classic JPEG transfer syntaxes, and both JPEG-LS transfer syntaxes are
integrated through the existing `radicom` codec trait and registry. JPEG 2000
and HTJ2K remain design-only work until their shared pure-Rust crate is rebuilt.

The `radicom` dependency is a local path dependency. CI is intentionally deferred
until `radicom` has been published and the project can consume the released package.

## Files and responsibilities

- `Cargo.toml`: workspace members, shared package metadata, and local `radicom` path dependency.
- `crates/radicom-codecs`: public facade, registry implementation, explicit codec registration,
  and re-exports of family codec types.
- `crates/radicom-codecs-rle`: RLE Lossless implementation and registration.
- `crates/radicom-codecs-jpeg`: classic JPEG marker, entropy, DCT, sequential,
  lossless, and frame-adapter implementation.
- `crates/radicom-codecs-jpeg-ls`: JPEG-LS Lossless/Near-Lossless algorithms,
  codestream validation, and DICOM frame adapters.
- `crates/radicom-codecs-jpeg2000/` (planned): one pure-Rust crate containing
  shared DICOM/codestream layers, classic JPEG 2000, and HTJ2K Tier-1 modules.
- `docs/design/jpeg2000.md` and `docs/design/htj2k.md`: implementation contracts;
  no third-party codec source is permitted.
- `tests` (facade unit tests for now): registration and transcoder-boundary checks.

## Task checklist

### Task 1: Create the workspace

- [x] Add the root Cargo workspace.
- [x] Add the four family crates and the facade crate as workspace members.
- [x] Use a local path dependency on the sibling radicom workspace.
- [x] Do not add GitHub Actions or other CI configuration.

### Task 2: Implement family registration types

- [x] Add one `Codec` implementation for RLE Lossless.
- [x] Add four `Codec` implementations for classic JPEG.
- [x] Add two `Codec` implementations for JPEG-LS.
- [ ] Add two `Codec` implementations for classic JPEG 2000 in the shared crate.
- [ ] Add three HTJ2K codec variants in that same crate.
- [x] Return the matching `radicom::syntaxes` entry from every codec.
- [x] Return the matching `CodecParams` default from every codec.
- [x] Return an explicit `CodecError::Operation` from unsupported operations.

### Task 3: Implement the facade registry

- [x] Register the seven currently implemented codec types explicitly.
- [x] Implement `has_codec`, `get_codec`, and `can_transcode` using the core contract.
- [x] Provide one installation function that calls `radicom::pixel::codec::set_codec_registry`.
- [x] Re-export the concrete codec types from the facade.

### Task 4: Verify the boundary

- [x] Test that all seven currently implemented syntaxes are registered.
- [x] Test raw-to-compressed and compressed-to-raw capability checks.
- [x] Test default parameter variants.
- [ ] Test the shared JPEG 2000/HTJ2K boundary and managed error paths.
- [ ] Run the classic JPEG 2000 and HTJ2K frame and codestream tests.

### RLE Stage 1 implementation status

- [x] Implement checked RLE layout validation for 8/16-bit samples.
- [x] Implement 64-byte header read/write and segment offset validation.
- [x] Implement deterministic PackBits literal/repeat encoding and decoding.
- [x] Implement sample-major, high-byte-first segment mapping.
- [x] Implement interleaved and planar RGB frame mapping.
- [x] Implement multi-frame encode/decode through `PixelData`.
- [x] Reject unsupported parameters and malformed frames with managed errors.
- [x] Add unit tests for headers, segments, layout, frames, and error paths.
- [x] Add facade/transcoder integration round-trip coverage.
- [x] Decode the external `fo-dicom.Codecs` RLE acceptance fixture and verify
  an exact Rust encode/decode round trip.

### Classic JPEG implementation status

- [x] Decode the fo-dicom acceptance fixtures for JPEG Baseline YBR_FULL,
  YBR_FULL_422, and Lossless RGB with the Rust codecs.
- [x] Implement pure-Rust marker, entropy, Huffman, DCT, sequential, lossless,
  and DICOM frame-adapter layers for UIDs `.50`, `.51`, `.57`, and `.70`.
- [x] Cover 8/12/16-bit precision, predictors 1–7, point transforms, restart
  boundaries, planar RGB, YBR color conversion, and 4:2:2 native packing.
- [x] Reject malformed DHT/SOS input without appending a partial frame.
- [x] Validate Rust-encoded JPEG with fo-dicom's native decoder for all four
  implemented transfer syntaxes; Process 2/4 still has no reference fixture.
- [x] Complete final review of the RLE and classic JPEG implementations.

### JPEG-LS implementation status

- [x] Implement pure Rust Lossless/Near-Lossless through the existing codec boundary.
- [x] Support 2..=16-bit precision, mono None, RGB Line/Sample, planar normalization,
  signed two's-complement preservation and YBR_FULL-to-RGB conversion.
- [x] Validate signed near-lossless extrema, UIDs, geometry, presets, markers, and
  entropy completion before exposing frames.
- [x] Cover multiframe and fragmented Pixel Data through the real registry/Transcoder.
- [x] Decode both fo-dicom acceptance fixtures exactly against native decodes.
- [x] Verify 300 bidirectional native CharLS cases using unshifted signed wire data.
- [x] Keep external harnesses, binaries, fixtures and results under ignored `.interop/`.

Restart intervals/RST, HP transforms, mapping tables and encoder custom-preset/reset
flags are explicitly rejected. Custom LSE decoding is supported. Full details and
the distinction between portable tests and native evidence are in
[the JPEG-LS implementation contract](../design/jpeg-ls.md).

### Classic JPEG 2000 implementation status

- [ ] Implement the shared codestream, wavelet, quantization, Tier-1, and
  Tier-2 layers in Rust without a vendor or third-party codec engine.
- [ ] Route `.90` and `.91` through one common JPEG 2000 crate contract.
- [ ] Cover 5/3 lossless, 9/7 lossy, quality layers, marker profiles, signed
  samples, planar RGB, YBR 4:2:2, multi-frame data, and malformed codestreams.
- [ ] Keep fo-dicom/OpenJPEG interoperability tools outside production source.
- [ ] Run the independent fo-dicom bidirectional acceptance matrix.

## Next implementation stages

1. **JPEG 2000 crate:** implement shared codestream/adapter layers, classic
   5/3 and 9/7 coding, then the HTJ2K MEL/VLC/MagSgn block coder in the same
   crate.
2. **Interoperability:** run the independent fo-dicom/OpenJPEG bidirectional
   matrix for `.90/.91/.201/.202/.203` after the pure-Rust implementation lands.

Each stage must pass its focused tests and the previous stages before the next stage starts.

## Known integration issue

The sibling `radicom` checkout now uses its generated UID dictionary for HTJ2K, and
its values match fo-dicom and DICOM `.201`, `.202`, and `.203`. The compatibility
issue is resolved; codec work only needs a read-only boundary assertion.

