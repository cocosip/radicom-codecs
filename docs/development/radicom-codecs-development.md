# radicom-codecs development plan and status

**Date:** 2026-11-02
**Current milestone:** All twelve DICOM codecs implemented and registered
**Status:** RLE, classic JPEG, JPEG-LS, classic JPEG 2000, and HTJ2K are
integrated through the `radicom` codec trait and registry.

## Scope of this milestone

The workspace and registration boundary are complete. The RLE Lossless family,
the four classic JPEG transfer syntaxes, both JPEG-LS transfer syntaxes, both
classic JPEG 2000 transfer syntaxes, and the three HTJ2K transfer syntaxes are
integrated through the existing `radicom` codec trait and registry.

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
- `crates/radicom-codecs-jpeg2000`: pure-Rust classic JPEG 2000 container,
  wavelet, quantization, Tier-1/Tier-2, the HTJ2K block coder (MEL/VLC/MagSgn),
  and DICOM frame layers.
- `docs/design/jpeg2000.md` and `docs/design/htj2k.md`: implementation contracts;
  no third-party codec source is permitted.
- `tests`: facade and registry/transcoder boundary checks, including
  `crates/radicom-codecs/tests/jpeg2000_integration.rs`.

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
- [x] Add two `Codec` implementations for classic JPEG 2000 in the shared crate.
- [x] Add three HTJ2K codec variants in that same crate.
- [x] Return the matching `radicom::syntaxes` entry from every codec.
- [x] Return the matching `CodecParams` default from every codec.
- [x] Return an explicit `CodecError::Operation` from unsupported operations.

### Task 3: Implement the facade registry

- [x] Register the twelve currently implemented codec types explicitly.
- [x] Implement `has_codec`, `get_codec`, and `can_transcode` using the core contract.
- [x] Provide one installation function that calls `radicom::pixel::codec::set_codec_registry`.
- [x] Re-export the concrete codec types from the facade.

### Task 4: Verify the boundary

- [x] Test that all twelve currently implemented syntaxes are registered.
- [x] Test raw-to-compressed and compressed-to-raw capability checks.
- [x] Test default parameter variants.
- [x] Test the shared JPEG 2000 boundary and managed error paths.
- [x] Run the classic JPEG 2000 frame and codestream tests.

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

- [x] Implement the shared codestream, wavelet, quantization, Tier-1, and
  Tier-2 layers in Rust without a vendor or third-party codec engine.
- [x] Route `.90` and `.91` through one common JPEG 2000 crate contract.
- [x] Cover 5/3 lossless, 9/7 lossy, multi-layer decoding, signed samples,
  planar RGB, 8/12/16-bit precision, odd dimensions, and malformed codestreams.
- [x] Keep fo-dicom/OpenJPEG interoperability tools outside production source
  (`.interop/`, git-ignored).
- [x] Run the independent fo-dicom bidirectional acceptance matrix for `.90`
  and `.91`.

Interop evidence: the bidirectional matrix that compared Rust and
fo-dicom/OpenJPEG output was run against the previous 9/7 synthesis scaling.
That scaling was wrong — the classic inverse applied `2/K` where the analysis
applies `K`, so `1/K` is the exact inverse and is what OpenJPEG and OpenJPH
use — so the lossy half of the matrix must be re-run before its numbers are
restated. The lossless `.90` results are unaffected: the 5/3 path never used
the irreversible scaling. The `.91` encoder also divided coefficients by the
subband norm times `2^gain` while the QCD declared only the norm; the gain is
now carried by the norm table alone, matching OpenJPEG.

The classic encoder emits one quality layer and does not apply MCT; `rate`,
`rate_levels`, and `allow_mct` are accepted but do not alter the codestream yet.
YBR_FULL/YBR_FULL_422 input is normalized to RGB. See
[the JPEG 2000 implementation contract](../design/jpeg2000.md).

### HTJ2K implementation status

- [x] Implement the Part 15 MEL/VLC/MagSgn block coder and the `.201`/`.202`/`.203`
  codec variants in the shared crate.
- [x] Write CAP, optional TLM, RPCL for `.202`, and one quality layer.
- [x] Apply RCT/ICT by default and label the encoded dataset `YBR_RCT`/`YBR_ICT`.
- [x] Round-trip lossless HTJ2K exactly and bound `.203` by the quality profile.
- [ ] Re-run the fo-dicom/OpenJPH interop matrix after the shared 9/7 fix.

## Next implementation stages

1. **Broader JPEG 2000/HTJ2K coverage:** optional rate control/quality layers,
   classic encoder MCT, and fuzz/property tests for malformed codestreams.
2. **Interop re-verification:** re-run the `.interop/` matrix for `.90`, `.91`,
   `.201`, `.202`, and `.203` after the 9/7 scaling correction and the
   quantization/COC/QCC fixes.

Each stage must pass its focused tests and the previous stages before the next stage starts.

## Known integration issue

The sibling `radicom` checkout now uses its generated UID dictionary for HTJ2K, and
its values match fo-dicom and DICOM `.201`, `.202`, and `.203`. The compatibility
issue is resolved; codec work only needs a read-only boundary assertion.

