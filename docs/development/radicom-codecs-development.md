# radicom-codecs development plan and status

**Date:** 2026-11-09
**Current milestone:** All twelve DICOM codecs implemented and registered
**Status:** RLE, classic JPEG, JPEG-LS, classic JPEG 2000, and HTJ2K are
integrated through the `radicom` codec trait and registry.

## Scope of this milestone

The workspace and registration boundary are complete. The RLE Lossless family,
the four classic JPEG transfer syntaxes, both JPEG-LS transfer syntaxes, both
classic JPEG 2000 transfer syntaxes, and the three HTJ2K transfer syntaxes are
integrated through the existing `radicom` codec trait and registry.

The `radicom` dependency is the released `0.2.0` crate from crates.io, with
`radicom-dict 0.2.0` in the lockfile. Rust 1.89 remains the MSRV. The workspace
package version and all internal dependency pins are `0.2.0`, ready for the
`v0.2.0` release workflow. CI builds and validates `master`/`main`, and the
tag-driven release workflow publishes the
workspace to crates.io. See [the release runbook](releasing.md) for version-bump,
tagging, and recovery steps.

## Files and responsibilities

- `Cargo.toml`: workspace members, the single workspace version, shared package metadata,
  internal path-dependency pins, and the published `radicom` dependency.
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
- [x] Consume `radicom` from crates.io; no local path dependency.
- [x] Add CI for `master`/`main` and a tag-driven release workflow.

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

Interop evidence: the OpenJPEG 2.5.4 matrix
(`pwsh .interop/run-j2k-interop.ps1`, which drives `j2k_tool` and
`.interop/j2k-oracle`) passes after two scaling corrections. The classic inverse
applied `2/K` where the
analysis applies `K`, so `1/K` is the exact inverse; and the irreversible
coefficient domain carries each subband's gain (`2^gain`, Equation E-4) in both
the encoder's quantization step and the decoder's dequantization step, which is
how OpenJPEG (`two_invK`, compensated by `R_b = prec` in its `tcd.c`) and
OpenJPH express the same transform. Results: `.90` is byte-exact in both
directions at 8 and 12 bits; an OpenJPEG `.91` decodes within `mean 0.002 /
max 1` of OpenJPEG's own output at 8 bits (`0.036 / 1` at 12 bits); and an
OpenJPEG decode of a Rust `.91` stays within `mean 0.172 / max 2` of the Rust
decode of the same codestream. An OpenJPEG-written irreversible codestream and
its reference samples are embedded in a unit test so the convention cannot
regress silently. A second, DICOM-level check drives the real
`fo-dicom.Codecs` 5.16.7 transcoder (`.interop/dicomref`) over the same
syntaxes and agrees: `.90` is exact in both directions at 8 and 12 bits, a
fo-dicom `.91` decodes within `max 1`, and fo-dicom decodes a Rust `.91` within
`max 2`.

The classic encoder implements multi-layer rate control through `rate` and
`rate_levels`; an empty ladder selects the reference rate levels, and the
default lossless encode writes eight layers including a final lossless layer.
`allow_mct` together with `update_photometric_interpretation` selects RCT/ICT
with the matching `YBR_RCT`/`YBR_ICT` label. The default leaves RGB without MCT.
YBR_FULL/YBR_FULL_422 input is
normalized to RGB. See
[the JPEG 2000 implementation contract](../design/jpeg2000.md).

### HTJ2K implementation status

- [x] Implement the Part 15 MEL/VLC/MagSgn block coder and the `.201`/`.202`/`.203`
  codec variants in the shared crate.
- [x] Write CAP, optional TLM, RPCL for `.202`, and one quality layer.
- [x] Apply RCT/ICT by default and label the encoded dataset `YBR_RCT`/`YBR_ICT`.
- [x] Round-trip lossless HTJ2K exactly and bound `.203` by the quality profile.
- [x] Run the HTJ2K interop matrix against a real HTJ2K encoder/decoder. No
  OpenJPH Windows binary is published and this machine has no CMake, so the
  oracle is fo-dicom.Codecs 5.16.7 (OpenJPH via P/Invoke) driven from a small C#
  harness (`.interop/dicomref`, git-ignored). It covers mono `.201`/`.202`/`.203`
  at 8 bits and `.201` at 12 bits in both directions, all sample-exact.

### Codec interop matrix against fo-dicom.Codecs

The cross-codec interop matrix (`.interop/rustharness` + `.interop/dicomref`,
run with `bash .interop/run_matrix.sh` and `bash .interop/run_extra.sh`)
encodes a raw frame with one codec, decodes the codestream with both, and
compares the two decodes; it then repeats the other direction. Lossless
syntaxes must also reproduce the source exactly. Results
(mono, 64×48, 8-bit unless noted):
`.5` (8 and 16 bits), `.57` (16), `.70` (12 stored in 16), `.80` (8 and 12),
`.81`, `.90` (8 and 12), `.201` (8 and 12), `.202`, and `.203` are sample-exact
in both directions. `.50` and `.51` (12 stored in 16) agree within one sample,
and `.91` (8 and 12) within two.

## Next implementation stages

1. **Broader JPEG 2000/HTJ2K coverage:** HTJ2K refinement passes/quality layers
   and fuzz/property tests for malformed codestreams. Classic encoder rate
   control, quality layers, and MCT are already implemented.
2. **Interop re-verification:** both matrices are done — OpenJPEG 2.5.4 for
   `.90`/`.91` and fo-dicom.Codecs 5.16.7 for every syntax. Re-run them after any
   wavelet, quantization, or COC/QCC change. RGB and profile-matrix coverage for
   HTJ2K is the remaining gap.

Each stage must pass its focused tests and the previous stages before the next stage starts.

## Core integration and upgrade validation

The released `radicom 0.2.0` uses its generated UID dictionary for HTJ2K, with
the DICOM `.201`, `.202`, and `.203` assignments checked by codec tests.

The dependency upgrade passes workspace formatting, Clippy, all 416 tests,
documentation generation, and the Rust 1.89 all-features check on Windows.
No codec API adaptation was needed. The external interop results above describe
earlier runs; native reference matrices have not been re-run for this upgrade.

