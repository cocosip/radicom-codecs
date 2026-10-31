# radicom-codecs development plan and status

**Date:** 2026-11-01
**Current milestone:** JPEG-LS implementation and interoperability hardening
**Status:** JPEG-LS local verification and interoperability audit complete

## Scope of this milestone

The workspace and registration boundary are complete. The RLE Lossless family
and the four classic JPEG transfer syntaxes are integrated through the existing
`radicom` codec trait and registry. Both JPEG-LS transfer syntaxes now have pure
Rust implementations; JPEG 2000 and HTJ2K remain placeholder registrations.

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
- `crates/radicom-codecs-jpeg2000`: classic JPEG 2000 and three HTJ2K registration skeletons.
- `tests` (facade unit tests for now): registration and transcoder-boundary checks.

## Task checklist

### Task 1: Create the workspace

- [x] Add the root Cargo workspace.
- [x] Add the four family crates and the facade crate as workspace members.
- [x] Use a local path dependency on `D:\Code\rust-code\radicom\crates\radicom`.
- [x] Do not add GitHub Actions or other CI configuration.

### Task 2: Implement family registration types

- [x] Add one `Codec` implementation for RLE Lossless.
- [x] Add four `Codec` implementations for classic JPEG.
- [x] Add two `Codec` implementations for JPEG-LS.
- [x] Add two `Codec` implementations for classic JPEG 2000.
- [x] Add three `Codec` implementations for HTJ2K.
- [x] Return the matching `radicom::syntaxes` entry from every codec.
- [x] Return the matching `CodecParams` default from every codec.
- [x] Return an explicit `CodecError::Operation` from unsupported operations.

### Task 3: Implement the facade registry

- [x] Register all twelve codec types explicitly.
- [x] Implement `has_codec`, `get_codec`, and `can_transcode` using the core contract.
- [x] Provide one installation function that calls `radicom::pixel::codec::set_codec_registry`.
- [x] Re-export the concrete codec types from the facade.

### Task 4: Verify the boundary

- [x] Test that all twelve syntaxes are registered.
- [x] Test raw-to-compressed and compressed-to-raw capability checks.
- [x] Test default parameter variants.
- [x] Test that a placeholder codec fails with a managed codec operation error.
- [x] Run full family algorithm tests after Stage 1 implementation begins.

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

## Next implementation stages

1. **JPEG 2000:** implement classic codestream, transforms, quantization, Tier-1/Tier-2,
   and `.90/.91` adapters.
2. **HTJ2K:** implement HT block coding and `.201/.202/.203` adapters.

Each stage must pass its focused tests and the previous stages before the next stage starts.

## Known integration issue

The current local `radicom` checkout defines HTJ2K transfer syntax constants with UIDs
`.108`, `.109`, and `.110`. The reference `fo-dicom.Codecs` project and current DICOM
definitions use `.201`, `.202`, and `.203`. This must be corrected in `radicom` before
HTJ2K registration can be considered compatible.

