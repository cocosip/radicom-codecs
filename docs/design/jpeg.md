# Classic JPEG codec design

## 1. Intent and scope

This document is the implementation contract for `crates/radicom-codecs-jpeg`.
It covers the classic JPEG transfer syntaxes used by DICOM, not JPEG-LS, JPEG
2000, HTJ2K, or JPEG XL. The first implementation is pure Rust and must not
require a native library or a platform-specific build step.

The goal is a codec that can transcode DICOM `PixelData` through the existing
`radicom::pixel::codec::Codec` boundary, preserve lossless samples exactly, and
interoperate with the public behavior of `fo-dicom.Codecs`. The C/C++ code in
the fo-dicom.Codecs native JPEG reference implementation is a
behavioral reference for marker validation, color conversion, precision, and
error reporting; it is not an implementation dependency.

### Standards and references

- ISO/IEC 10918-1 / ITU-T T.81: JPEG interchange syntax and coding processes.
- DICOM PS3.3 and PS3.5: Image Pixel Module, encapsulated Pixel Data, and JPEG
  transfer syntaxes.
- `fo-dicom.Codecs DicomJpegCodec.cs reference implementation`.
- the fo-dicom.Codecs native JPEG reference implementation.
- The current Rust boundary in `radicom::pixel::codec::JpegParams` and
  `radicom::pixel::{PixelData, PixelDataRef}`.

## 2. Supported transfer syntaxes

| Codec type | UID | JPEG process | Initial support |
| --- | --- | --- | --- |
| `JpegProcess1Codec` | `1.2.840.10008.1.2.4.50` | Baseline sequential DCT, 8-bit, lossy | encode/decode |
| `JpegProcess2_4Codec` | `1.2.840.10008.1.2.4.51` | Extended sequential DCT, 8/12-bit, lossy | encode/decode |
| `JpegLossless14Codec` | `1.2.840.10008.1.2.4.57` | Non-hierarchical lossless predictive JPEG | encode/decode |
| `JpegLossless14Sv1Codec` | `1.2.840.10008.1.2.4.70` | Process 14, selection value 1 | encode/decode |

The current pure-Rust implementation covers monochrome and three-component
interleaved/planar data with 4:4:4 (1x1) and 4:2:2 sampling. Sequential RGB↔YBR
conversion and native `YBR_FULL_422` packing are handled by the frame adapter.
The available external fixture and native decoder checks are recorded in §9;
the reference checkout still does not contain a Process 2/4 fixture.

The codec selected by the transfer syntax owns the allowed process. A caller
cannot use `JpegParams` to turn Process 1 into progressive or lossless JPEG.
Process 14 accepts predictor selections 1–7; SV1 always uses predictor 1. The
point transform is accepted only where the selected process and sample depth
make it valid.

The following are explicitly outside this document and must fail with a
managed codec error rather than being silently treated as sequential JPEG:

- progressive DCT, hierarchical JPEG, arithmetic-coded JPEG, and differential
  JPEG;
- CMYK and YCCK images;
- JPEG-LS, JPEG 2000, HTJ2K, JP2 wrappers, and JPEG XL;
- unsupported component counts, sampling factors, marker combinations, or
  sample precision.

## 3. DICOM adapter contract

### Encode

`encode` requires a native, uncompressed source transfer syntax and an
encapsulated destination transfer syntax matching the codec. It validates the
source layout before reading any frame and appends one JPEG codestream as one
encapsulated frame. `PixelData::add_frame` supplies the DICOM even-length
pad; the JPEG codestream itself must end in EOI before that pad is added.

The adapter processes every source frame in order. If a frame fails, no partial
encoded frame is appended to the destination. The frame count and source
metadata remain unchanged until a complete frame is ready.

### Decode

`decode` requires an encapsulated source and a native destination. Each logical
frame returned by `PixelDataRef::get_frame` is treated as one codestream after
removing only DICOM even-length padding; the underlying fragment sequence may
contain more than one physical fragment. The decoder must accept a missing EOI
only when the reference behavior can safely repair a truncated transport
buffer; malformed marker or entropy data must still return an error. The default
Rust behavior is stricter: require EOI and report truncation, with any
compatibility repair isolated in the adapter and covered by a regression test.

Decoded frames are written in source order. The adapter checks codestream width,
height, component count, precision, and the expected uncompressed frame size
before appending a frame, preventing a crafted JPEG from causing an undersized
write or integer overflow.

### Native pixel layout

The adapter uses the same byte representation as `PixelData`:

- `BitsAllocated` is 8 or 16 for the first release; `BitsStored` must be in the
  range supported by the selected process and no sample bits may be discarded.
- Multi-byte native samples are little-endian, as required by the native DICOM
  transfer syntax. JPEG component samples are converted to and from the codec's
  unsigned sample representation at the boundary.
- `PlanarConfiguration = 0` is interleaved. `PlanarConfiguration = 1` is
  accepted for three-component, 8-bit RGB and normalized to interleaved before
  JPEG coding; decoded planar output is repacked only after the codestream has
  been validated.
- `SamplesPerPixel = 1` is monochrome. Three-component RGB/YBR input is handled
  by the color policy below. Other component counts are rejected in the first
  release.

The adapter must not use a lossy color conversion on a Process 14 frame. For
signed samples, color conversion to RGB is rejected because the reference
contract does not define a signed YCbCr/RGB conversion.

## 4. Photometric interpretation and color policy

The encoder accepts `MONOCHROME1`, `MONOCHROME2`, `RGB`, `YBR_FULL`, and
`YBR_FULL_422` for the supported process and sampling. `YBR_ICT`,
`YBR_RCT`, palette color, CMYK, and YCCK are rejected for classic JPEG.

For lossy RGB encoding, the implementation converts RGB to full-range YCbCr.
`sample_factor = Full` emits `YBR_FULL`; `sample_factor = Half` emits
`YBR_FULL_422`. Decode can convert the YCbCr components back to RGB when
`convert_colorspace_to_rgb` is true.

For lossless JPEG, the encoder keeps the component color space and forces
one-by-one component sampling. This avoids the RGB-to-YCbCr conversion that
would violate exact sample preservation. The first Rust release preserves the
decoded component color space; `convert_colorspace_to_rgb` is rejected on the
lossless path because a lossless color conversion is outside this contract.

The adapter updates photometric interpretation and planar configuration only
after the codestream reports its actual component color space. Metadata updates
must be deterministic and must agree with the bytes written to the destination.

## 5. Parameter contract

`radicom::pixel::codec::JpegParams` is the only public parameter object:

| Field | Meaning | Validation |
| --- | --- | --- |
| `quality` | Quantization quality for Process 1/2/4 | `0..=100`; ignored by Process 14 |
| `smoothing_factor` | Pre-encoding smoothing | must be `0` in the first Rust release; ignored by Process 14 |
| `convert_colorspace_to_rgb` | Decode YCbCr/RGB as RGB | reject for signed samples |
| `sample_factor` | Chroma sampling | `Full` = 4:4:4, `Half` = 4:2:2; lossless forces 4:4:4 |
| `predictor` | Process 14 predictor selection | `1..=7`; SV1 overrides to `1` |
| `point_transform` | Right shift before predictive coding | must fit the selected precision; lossless exact tests use `0` |

The wrong `CodecParams` variant is rejected before touching the destination.
Parameters that the selected transfer syntax cannot use are rejected instead
of being silently ignored, except for fields explicitly marked “ignored” above.

## 6. Codestream architecture

The crate is organized around small, testable layers. The exact file split may
follow the existing RLE crate conventions, but responsibilities are fixed:

1. **Marker reader/writer** — SOI/EOI, SOF0/SOF1/SOF3, DQT, DHT, DRI, SOS,
   APPn/COM skipping, segment lengths, and bounds checks.
2. **Entropy bit I/O** — MSB-first bit reads/writes, `0xFF` byte stuffing,
   marker detection, and restart marker boundaries.
3. **Huffman tables** — canonical table construction, encode lookup, bounded
   decode lookup, and duplicate/oversubscribed table rejection.
4. **Sequential DCT** — level shift, 8×8 forward/inverse DCT, quantization,
   zig-zag order, DC differentials, AC run-length coding, and dequantization.
5. **Lossless predictor** — predictors 1–7, point transform, signed difference
   reconstruction, and predictor reset at restart intervals.
6. **Frame adapter** — DICOM layout validation, color conversion, planar
   repacking, per-frame encapsulation, and output metadata.

### Marker rules

The decoder must validate SOI, one supported SOF marker, DQT/DHT definitions,
SOS component selectors, restart interval use, and EOI. Segment lengths include
their two length bytes and must stay inside the frame. APPn and COM are skipped
with the same bounds checks. Standalone markers are recognized only where the
standard permits them. A marker encountered inside entropy data is valid only
after byte stuffing or at a legal restart/EOI boundary.

### Process 1 and Process 2/4

The sequential path performs level shift, 8×8 DCT, quantization, zig-zag scan,
Huffman coding, restart handling, and inverse operations. Process 1 is limited
to 8-bit samples. Process 2/4 supports the DICOM-relevant 8-bit and 12-bit
precision and rejects 16-bit sequential JPEG rather than truncating it.

The first release uses baseline Huffman coding and emits one sequential scan.
It does not emit progressive or arithmetic-coded scans. Decoder support is
limited to the same sequential forms so that acceptance is explicit and testable.

### Process 14 and SV1

The lossless path emits SOF3 and one Huffman-coded scan. For each sample it
predicts from already reconstructed neighbors, applies the point transform,
and codes the signed difference with the JPEG lossless category/amplitude
representation. Category 16 is the JPEG special case for a 16-bit difference:
it carries no amplitude bits, reconstructs with modulo-2^16 arithmetic, and
must not be treated as an ordinary 16-bit amplitude. Decoding reverses these
operations exactly. Restart intervals reset the predictor state at MCU
boundaries. No DCT, quantization, smoothing, or chroma subsampling is permitted
on this path.

## 7. Error and resource rules

All malformed input, unsupported parameters, precision mismatches, overflow,
and destination-layout conflicts return `radicom::error::Error` with codec and
frame context. Panics are not an error path for untrusted codestream data.

The decoder performs checked arithmetic for dimensions, component counts,
MCU counts, row strides, and output allocation. It rejects frames whose decoded
size cannot fit the destination or the library's supported buffer size. It does
not allocate based solely on a compressed length estimate.

The destination is unchanged when validation or decoding fails. Temporary
buffers are released on every error path, and no frame buffer is retained after
`encode` or `decode` returns.

## 8. Implementation stages

1. **Primitives:** marker parser/writer, bit reader/writer, canonical Huffman
   tables, byte stuffing, restart handling, and malformed-input tests.
2. **Lossless core:** Process 14 predictor and entropy coding, exact 8/12/16-bit
   unit tests, and SV1 predictor enforcement.
3. **Sequential core:** DCT, quantization, Process 1, then Process 2/4 with
   12-bit validation and fixed-tolerance tests.
4. **DICOM adapter:** frame iteration, encapsulation, planar/color conversion,
   metadata updates, signedness, and parameter validation.
5. **Interop and hardening:** decode fixtures produced by `fo-dicom.Codecs`,
   decode Rust output through `fo-dicom.Codecs`, malformed codestream tests,
   multi-frame tests, and full workspace verification.

Each stage must keep the previous stage's tests passing. No native dependency,
new public parameter type, progressive mode, or extra transfer syntax is added
without updating this document first.

## 9. Verification matrix

### Unit and property checks

- Marker lengths, unsupported markers, truncated segments, SOF/SOS mismatch,
  missing DHT/DQT, duplicate/oversubscribed Huffman tables, and invalid restart
  markers.
- Bit stuffing, marker boundaries, canonical Huffman round-trips, and restart
  state reset.
- DCT/IDCT numerical bounds, quantization tables, zig-zag order, and DC/AC
  category coding.
- Predictors 1–7, point transform bounds, signed differences, and exact
  reconstruction.

### Codec checks

- Process 1 8-bit monochrome and RGB/YBR 4:4:4 and 4:2:2 round-trips with a
  documented pixel-error tolerance.
- Process 2/4 12-bit round-trips and rejection of invalid precision.
- Process 14 and SV1 exact 8/12/16-bit monochrome and supported color
  round-trips, including signed samples where the DICOM layout permits them.
- Interleaved and planar RGB normalization, multi-frame ordering, odd-length
  DICOM padding, and metadata consistency.

### Interoperability checks

- Rust decodes the available `fo-dicom.Codecs` fixtures for Baseline
  `YBR_FULL`, Baseline `YBR_FULL_422`, and Lossless RGB. A Process 2/4 fixture
  is not present in the reference checkout yet.
- The built fo-dicom native decoder accepts Rust output for all four transfer
  syntaxes. Process 1 and Process 2/4 decode with fixed lossy byte deltas of
  11 and 250 in the smoke fixtures; Process 14 and SV1 are byte-exact,
  including the 16-bit SV1 fixture.
- Lossless comparisons are byte/sample exact after the DICOM byte-order and
  planar rules are applied. Lossy comparisons use fixed fixtures and a stated
  tolerance; visual similarity alone is not an acceptance criterion.
- The malformed Process 14 fixture from
  the fo-dicom.Codecs malformed JPEG unit test is
  covered by a Rust regression test that returns a managed error and leaves the
  destination without a partial frame.

## 10. Non-goals and upgrade points

The first release does not promise progressive JPEG, arithmetic coding,
hierarchical JPEG, 4:2:0 sampling, CMYK/YCCK, palette-color JPEG, or arbitrary
component counts. These are separate compatibility projects because they alter
marker validation, color semantics, or the DICOM photometric contract.

If deployment later requires native acceleration, it may be added behind the
same codec boundary after the pure Rust behavior and interoperability matrix are
stable. It must not change the accepted transfer syntax, metadata, or error
contract without a new design review.

Current interoperability evidence includes Rust decoding of the fo-dicom
acceptance fixtures for Baseline `YBR_FULL`, Baseline `YBR_FULL_422`, and
Lossless RGB, plus native fo-dicom decoding of Rust output for all four
implemented transfer syntaxes. The reference checkout still does not contain
a Process 2/4 sample, so that direction remains covered by the Rust/native
smoke fixture rather than a checked-in reference fixture.
The Lossless RGB codestream and the separately named raw RGB fixture differ in
1,212 bytes (404 RGB samples) in exact 0/255 blocks, so that cross-file check
is recorded as non-exact rather than treated as decoder proof or failure.

