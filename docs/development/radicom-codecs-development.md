# radicom-codecs development plan and status

**Date:** 2026-10-30  
**Current milestone:** workspace and registration skeleton  
**Status:** in progress

## Scope of this milestone

This milestone creates the Cargo workspace and the registration boundary only.
It does not implement RLE, JPEG, JPEG-LS, JPEG 2000, or HTJ2K bitstream coding.
Every placeholder codec returns a managed `radicom::error::Error::Codec` operation
error instead of silently passing data through.

The `radicom` dependency is a local path dependency. CI is intentionally deferred
until `radicom` has been published and the project can consume the released package.

## Files and responsibilities

- `Cargo.toml`: workspace members, shared package metadata, and local `radicom` path dependency.
- `crates/radicom-codecs`: public facade, registry implementation, explicit codec registration,
  and re-exports of family codec types.
- `crates/radicom-codecs-rle`: RLE codec registration skeleton.
- `crates/radicom-codecs-jpeg`: four classic JPEG registration skeletons.
- `crates/radicom-codecs-jpeg-ls`: two JPEG-LS registration skeletons.
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
- [x] Return an explicit `CodecError::Operation` from unimplemented encode/decode methods.

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
- [ ] Run full family algorithm tests after Stage 1 implementation begins.

## Next implementation stages

1. **RLE:** implement DICOM RLE header, segment layout, PackBits runs, frame mapping,
   and exact round-trip tests.
2. **JPEG:** implement marker/entropy primitives, sequential DCT, Process 14 predictors,
   and four transfer syntax adapters.
3. **JPEG-LS:** implement regular/run modes, context modeling, Golomb coding, interleave,
   and near-lossless validation.
4. **JPEG 2000:** implement classic codestream, transforms, quantization, Tier-1/Tier-2,
   and `.90/.91` adapters.
5. **HTJ2K:** implement HT block coding and `.201/.202/.203` adapters.

Each stage must pass its focused tests and the previous stages before the next stage starts.

## Known integration issue

The current local `radicom` checkout defines HTJ2K transfer syntax constants with UIDs
`.108`, `.109`, and `.110`. The reference `fo-dicom.Codecs` project and current DICOM
definitions use `.201`, `.202`, and `.203`. This must be corrected in `radicom` before
HTJ2K registration can be considered compatible.

