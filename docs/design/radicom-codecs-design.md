# radicom-codecs overall design

## 1. Purpose

`radicom-codecs` contains the concrete Rust implementations of the DICOM image
codecs that are intentionally absent from the `radicom` core library. The core
library owns the codec contract, parameter types, registry, pixel-data facade,
and transcoder. This project implements that contract and registers the concrete
codecs, mirroring the relationship between `FO-DICOM.Core` and `fo-dicom.Codecs`.

The implementation depends on the published `radicom` crate from crates.io. The
workspace is released as the `radicom-codecs` family; its version and the internal
path-dependency pins are centralized at the workspace root, and publishing is
tag-driven.

## 2. Reference boundary

The behavior and class coverage are derived from:

- `fo-dicom.Codecs DicomRleCodec.cs reference implementation`
- `fo-dicom.Codecs DicomJpegCodec.cs reference implementation`
- `fo-dicom.Codecs DicomJpegLsCodec.cs reference implementation`
- `fo-dicom.Codecs DicomJpeg2000Codec.cs reference implementation`
- `fo-dicom.Codecs DicomHtJpeg2000Codec.cs reference implementation`
- the PureCodecs RLE design
- the PureCodecs JPEG design
- the PureCodecs JPEG-LS design
- the PureCodecs JPEG 2000 design

The C/C++ implementation is a format and behavior reference only. It is never
copied, translated, compiled, linked, loaded, or called by production Rust code.
The C# implementation is used to compare public behavior and test cases; it is
not a Rust dependency.

No third-party JPEG, JPEG 2000, HTJ2K, OpenJPEG, OpenJPH, or native codec source
may be vendored into this repository. External implementations are test oracles
only; every production codec algorithm is implemented in Rust in this workspace.

The standards documents are the normative source for codestream syntax. The
separate family documents in this directory record the applicable standard,
supported profiles, algorithm stages, DICOM metadata rules, and rejection rules.

## 3. Dependency on the radicom codec interface

The implementation depends on the published `radicom` crate from crates.io:

```toml
[dependencies]
radicom = "0.1.0"
```

It must not use a local path dependency, so CI and consumers build exactly the
released package.

The implementation must use these existing types from `radicom::pixel::codec`:

- `Codec`
- `CodecRegistry`
- `CodecParams`, `JpegParams`, `JpegLsParams`, `J2kParams`, and `HtJ2kParams`
- `CodecError` through `radicom::error::Error::Codec`

It must also use the existing pixel-data boundary:

- `PixelDataRef<'_>` for reading source frames;
- `PixelData<'_>` for writing destination frames and updating DICOM metadata;
- `PixelData::get_frame`, `PixelData::add_frame`, `PixelData::dataset_mut`, and
  the metadata accessors in `radicom::pixel`.

Concrete codecs do not define a second `Codec` trait, a second parameter enum, or
a second pixel-frame model. Each concrete type implements `radicom::pixel::codec::Codec`:

```rust
pub trait Codec: Send + Sync {
    fn name(&self) -> &str;
    fn transfer_syntax(&self) -> &'static TransferSyntax;
    fn default_params(&self) -> CodecParams;
    fn encode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), radicom::error::Error>;
    fn decode(
        &self,
        src: &PixelDataRef<'_>,
        dst: &mut PixelData<'_>,
        params: &CodecParams,
    ) -> Result<(), radicom::error::Error>;
}
```

The facade registers an `Arc<dyn Codec>` for every supported transfer syntax and
injects one `Arc<dyn CodecRegistry>` with `radicom::pixel::codec::set_codec_registry`.
`radicom::pixel::transcoder::Transcoder` then selects the codec by transfer
syntax. This is the only integration path required by the core library.

## 4. Existing radicom boundary that must be preserved

The `radicom` core deliberately ships an empty codec registry. It already exposes
the following standard transfer syntax constants used by codec registration:

- `RLE_LOSSLESS`
- `JPEG_PROCESS_1`
- `JPEG_PROCESS_2_4`
- `JPEG_LOSSLESS_PROCESS_14`
- `JPEG_LOSSLESS_PROCESS_14_SV1`
- `JPEG_LS_LOSSLESS`
- `JPEG_LS_NEAR_LOSSLESS`
- `JPEG_2000_LOSSLESS`
- `JPEG_2000_LOSSY`
- `HTJ2K_LOSSLESS`
- `HTJ2K_LOSSLESS_RPCL`
- `HTJ2K_LOSSY`

The sibling `radicom` transfer-syntax table is already reconciled with its
generated UID dictionary, the DICOM transfer syntax definitions, and fo-dicom:
HTJ2K uses `.201`, `.202`, and `.203`. The codec project must assert those values
and must not silently register under a different UID.

## 5. Cargo workspace layout

The workspace is split by codec family, with a facade that owns registration:

```text
radicom-codecs/
├── Cargo.toml
├── crates/
│   ├── radicom-codecs/              # registry, registration, public re-exports
│   ├── radicom-codecs-rle/          # RLE implementation
│   ├── radicom-codecs-jpeg/         # classic JPEG implementation
│   ├── radicom-codecs-jpeg-ls/      # JPEG-LS implementation
│   └── radicom-codecs-jpeg2000/     # one crate: classic JPEG 2000 + HTJ2K
├── tests/                           # cross-crate and radicom integration tests
└── docs/
```

Each family crate depends on `radicom` and implements its concrete `Codec` types.
The facade depends on all family crates, constructs the registry, and provides a
single registration function for applications. No codec is discovered through
reflection or dynamic library loading.

The JPEG 2000 crate contains both classic JPEG 2000 and HTJ2K because they share
codestream geometry, DICOM frame adaptation, and marker infrastructure. HTJ2K is
an alternate Part 15 code-block algorithm inside that crate, not a separate
top-level codec family. Their wavelet transforms, quantization, packet policy,
and Tier-1 block coding remain separate modules and separate compatibility tests.

## 6. Codec registration surface

The facade exposes one explicit registration entry point, for example:

```rust
pub fn register_codecs() -> Arc<dyn CodecRegistry>;
```

The exact function name follows the existing `radicom` naming conventions, but
its behavior is fixed:

1. construct one codec object for each supported transfer syntax;
2. map the syntax UID to the codec object;
3. return a `CodecRegistry` whose `has_codec`, `can_transcode`, and `get_codec`
   behavior follows the core `CodecRegistry` contract;
4. let the application call `set_codec_registry` once during setup.

The facade does not modify the transcoder decision table, move lossy-history
ownership out of `radicom`, or require applications to register family crates
individually.

## 7. Supported codec registrations

The first release covers the twelve registrations below. The transfer syntax
constants and UIDs must match `radicom` and the DICOM standard before a row is
marked complete.

| Family | `radicom` registration | UID | Standard family | Implementation crate |
| --- | --- | --- | --- | --- |
| RLE | `RLE_LOSSLESS` | `1.2.840.10008.1.2.5` | DICOM RLE | `radicom-codecs-rle` |
| JPEG | `JPEG_PROCESS_1` | `1.2.840.10008.1.2.4.50` | JPEG Baseline | `radicom-codecs-jpeg` |
| JPEG | `JPEG_PROCESS_2_4` | `1.2.840.10008.1.2.4.51` | JPEG Extended | `radicom-codecs-jpeg` |
| JPEG | `JPEG_LOSSLESS_PROCESS_14` | `1.2.840.10008.1.2.4.57` | JPEG Lossless | `radicom-codecs-jpeg` |
| JPEG | `JPEG_LOSSLESS_PROCESS_14_SV1` | `1.2.840.10008.1.2.4.70` | JPEG Lossless SV1 | `radicom-codecs-jpeg` |
| JPEG-LS | `JPEG_LS_LOSSLESS` | `1.2.840.10008.1.2.4.80` | JPEG-LS Lossless | `radicom-codecs-jpeg-ls` |
| JPEG-LS | `JPEG_LS_NEAR_LOSSLESS` | `1.2.840.10008.1.2.4.81` | JPEG-LS Near-Lossless | `radicom-codecs-jpeg-ls` |
| JPEG 2000 | `JPEG_2000_LOSSLESS` | `1.2.840.10008.1.2.4.90` | JPEG 2000 Lossless | `radicom-codecs-jpeg2000` |
| JPEG 2000 | `JPEG_2000_LOSSY` | `1.2.840.10008.1.2.4.91` | JPEG 2000 Lossy | `radicom-codecs-jpeg2000` |
| HTJ2K | `HTJ2K_LOSSLESS` | `1.2.840.10008.1.2.4.201` | HTJ2K Lossless | `radicom-codecs-jpeg2000` |
| HTJ2K | `HTJ2K_LOSSLESS_RPCL` | `1.2.840.10008.1.2.4.202` | HTJ2K Lossless RPCL | `radicom-codecs-jpeg2000` |
| HTJ2K | `HTJ2K_LOSSY` | `1.2.840.10008.1.2.4.203` | HTJ2K Lossy | `radicom-codecs-jpeg2000` |

JPEG XL, JPEG 2000 Part 2 multi-component syntaxes, JPIP/JPT, and unsupported
subsampling or photometric combinations are explicit out-of-scope or failure
cases, not silently accepted aliases.

## 8. Shared DICOM frame adapter

The family-specific bitstream code must not read arbitrary Dataset tags directly.
The adapter layer at each codec boundary uses `PixelDataRef` and `PixelData` to:

- validate rows, columns, frame count, and frame length;
- read `BitsAllocated`, `BitsStored`, `HighBit`, `PixelRepresentation`,
  `SamplesPerPixel`, `PlanarConfiguration`, and `PhotometricInterpretation`;
- normalize planar/interleaved component layout before encoding;
- convert supported YBR/RGB layouts according to the family design;
- write decoded frames with `PixelData::add_frame` or `add_frame_buffer`;
- update only the metadata required by the codec contract through `dataset_mut`.

Each codec call processes all frames in `src` and appends the same number of
frames to `dst`. A frame failure returns `Error::Codec` with the transfer syntax,
operation, and frame index; it must not leave a successful-looking partial result.
The core `PixelData` implementation owns encapsulation fragments and DICOM
even-length padding, so codec implementations must keep logical codestream length
separate from DICOM padding.

`HighBit` is read but never shifts samples. PS3.3 requires
`HighBit = BitsStored - 1`, so conforming frames are right-justified inside
`BitsStored` and the adapter masks to `BitsStored`, exactly like libjpeg, CharLS,
OpenJPEG, and fo-dicom's codecs. A non-conforming left-aligned frame must be
normalized by the caller before transcoding (the `radicom` renderer's
`BitDepth::normalize` does this); the codecs do not reinterpret the bits, and
decoded frames keep the dataset's `HighBit` because the output is right-justified.

## 9. Error and safety rules

All public codec methods return `Result<(), radicom::error::Error>`. External
codestreams, DICOM metadata, lengths, offsets, dimensions, tile bounds, packet
bounds, and parameter values are untrusted input. Implementations must validate
before allocation and before advancing a parser or decoder.

Errors must identify:

- codec family and transfer syntax;
- encode or decode operation;
- frame index when processing a Dataset;
- marker, segment, tile, packet, component, or bitstream location when known.

No codec may panic on malformed input. Native libraries, P/Invoke, build scripts
that compile C/C++, runtime native fallback, and vendored third-party codec
engines are forbidden.

## 10. Verification and compatibility gates

Every family has four test layers:

1. standards-level primitive tests for marker, header, bitstream, transform, and
   entropy behavior;
2. family round-trip tests for 8/16-bit, monochrome/RGB, planar/interleaved, and
   multi-frame data where the transfer syntax permits it;
3. invalid-input tests for truncation, bounds, unsupported precision, metadata
   mismatch, and invalid parameters;
4. integration tests through `radicom::pixel::transcoder::Transcoder` using the
   injected registry.

Reference interoperability tests call the public `fo-dicom.Codecs` API. They do
not link the local C# source, replace assemblies, inspect package identity, or
call native libraries directly.

Acceptance rules are:

- lossless codecs: exact decoded sample/byte equality;
- lossy codecs: fixed per-fixture sample tolerances;
- all codecs: frame count, dimensions, pixel metadata, transfer syntax, and
  managed error behavior;
- all registry rows: both `can_transcode(raw, compressed)` and
  `can_transcode(compressed, raw)` are true after registration.

## 11. Implementation order and release boundary

The implementation order is part of the design, not a suggestion:

1. RLE and its registration/integration tests;
2. four classic JPEG registrations;
3. two JPEG-LS registrations;
4. two classic JPEG 2000 registrations;
5. three HTJ2K registrations;
6. full cross-family compatibility matrix and package validation.

The development document records the checklist and current status for these
stages. A stage is complete only when its family tests, registry tests, and
reference interoperability tests pass without weakening an earlier stage.

The JPEG 2000 and HTJ2K rows remain design targets until the single Rust crate,
its marker/profile matrix, and the complete compatibility matrix are implemented
and verified. Publishing is not a substitute for those gates.

