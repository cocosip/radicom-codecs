# DICOM JPEG-LS codec: technical implementation

## 1. Intent and scope

This document is the implementation contract for the two JPEG-LS transfer
syntaxes already registered by `radicom-codecs`:

```text
JPEG-LS Lossless       1.2.840.10008.1.2.4.80
JPEG-LS Near-Lossless  1.2.840.10008.1.2.4.81
Crate: radicom-codecs-jpeg-ls
Types: radicom_codecs_jpeg_ls::JpegLsLosslessCodec
       radicom_codecs_jpeg_ls::JpegLsNearLosslessCodec
```

The result must be a pure Rust encoder and decoder. The CharLS implementation
vendored in `D:\dotnet\source\fo-dicom.Codecs\Native\Common\CharLS` and the
adapter in `Codec\DicomJpegLsCodec.cs` are algorithm and compatibility
references only. The Rust crate must not compile or link that C/C++ code, use
FFI/P/Invoke, spawn a codec process, or add a native build step.

The implementation uses the existing `radicom::pixel::codec::Codec` boundary,
`PixelDataRef`/`PixelData`, and `CodecParams::JpegLs`. It does not introduce a
second codec trait, pixel buffer model, DICOM fragment implementation, or
public parameter type.

### Standards and references

- ISO/IEC 14495-1 / ITU-T T.87, JPEG-LS lossless and near-lossless coding.
- DICOM PS3.5, JPEG-LS transfer syntaxes and encapsulated Pixel Data.
- DICOM PS3.3, Image Pixel Module and Photometric Interpretation rules.
- `D:\dotnet\source\fo-dicom.Codecs\Codec\DicomJpegLsCodec.cs`.
- `D:\dotnet\source\fo-dicom.Codecs\Native\Dicom.Imaging.Codec.JpegLS.cpp`.
- `D:\dotnet\source\fo-dicom.Codecs\Tests\Acceptance\PM5644-960x540_JPEG-LS_Lossless.dcm`.
- `D:\dotnet\source\fo-dicom.Codecs\Tests\Acceptance\PM5644-960x540_JPEG-LS_NearLossless.dcm`.

CharLS is used as a behavior oracle while porting. A Rust function may be
checked against the corresponding CharLS routine, but the Rust implementation
must independently own its memory, bounds checks, and error handling.

## 2. Goals and non-goals

### Goals

- Encode and decode both DICOM JPEG-LS UIDs through the frozen `Codec` trait.
- Produce and accept valid T.87 codestreams with SOI, SOF55, SOS, scan data,
  and EOI, plus the LSE preset segment when non-default coding parameters are
  selected.
- Support exact lossless reconstruction and the JPEG-LS near-lossless bound
  `abs(source - decoded) <= NEAR` for every sample.
- Support the DICOM layouts used by the current pixel facade: 8/16
  `BitsAllocated`, `BitsStored` in 2..=16, one or three components, and
  interleaved or planar RGB input where the adapter can normalize it safely.
- Reject malformed or unsupported data with `Error::Codec` and leave the
  destination without a partial frame.
- Prove behavior with unit tests, property-style round trips, checked fixtures,
  and bidirectional fo-dicom interoperability.

### Non-goals for the first release

- JPEG-LS Part 2 extensions, hierarchical or progressive JPEG, arithmetic JPEG,
  JPEG 2000/HTJ2K, or JP2 wrappers.
- Native CharLS acceleration or a compatibility FFI layer.
- One-bit packed DICOM frames, more than three components, CMYK, palette color,
  YBR_PARTIAL_422/420, HSV, or ARGB.
- A new typed public parameter enum in the core `radicom` crate.

The implementation can add those later behind the same codec contract after a
separate design review.

## 3. Existing integration contract

The two public types implement:

```rust
impl radicom::pixel::codec::Codec for JpegLsLosslessCodec { /* ... */ }
impl radicom::pixel::codec::Codec for JpegLsNearLosslessCodec { /* ... */ }
```

Required behavior:

- `name()` returns `"JPEG-LS Lossless"` or `"JPEG-LS Near-Lossless"`.
- `transfer_syntax()` returns the matching `radicom::syntaxes` entry.
- `default_params()` returns `CodecParams::JpegLs(...)`.
- `encode` accepts native source data and a destination whose transfer syntax
  is the codec's UID.
- `decode` accepts the codec's encapsulated source and a native destination.
- A wrong `CodecParams` variant, wrong source/destination syntax, invalid pixel
  layout, malformed marker, or failed frame decode becomes
  `CodecError::Operation { codec, message }`.
- Frames are encoded/decoded into temporary `Vec<u8>` values first. They are
  appended with `PixelData::add_frame` only after validation succeeds, so a
  failed frame cannot expose a partial compressed or native frame.
- The core `PixelData` implementation remains responsible for fragment
  sequences, Basic Offset Table handling, even-length padding, and native
  endianness normalization.

The facade registration in `crates/radicom-codecs/src/lib.rs` already includes
both types and must remain unchanged apart from tests that exercise real codec
operations.

## 4. Transfer syntax and parameter contract

The existing `radicom::pixel::codec::JpegLsParams` is carried across the frozen
boundary as numeric fields. The JPEG-LS crate owns their validation and mapping.

| Field | Codec meaning | Validation and mapping |
| --- | --- | --- |
| `near_lossless` | JPEG-LS `NEAR` value | `.80` requires `0`; `.81` accepts `0..=min(255, MAXVAL/2)`, with default `3`; `NEAR=0` is mathematically lossless even when the near-lossless UID is selected |
| `interleave_mode` | Scan interleave selection | `0` = derive from DICOM storage; `1` = planar DICOM input (encoded with line interleave); `2` = line; `3` = sample. The value `1` is an adapter/storage mode, not a T.87 codestream interleave mode |
| `color_transformation` | HP1/HP2/HP3 transform selector | `0` = none; `1..=3` are accepted only when the chosen RGB policy supports the transform; first release defaults to `0` and rejects transforms rather than silently changing samples |
| `preset_coding_params` | Request custom LSE preset parameters | `0` uses T.87 defaults; non-zero is reserved until a concrete preset structure exists and is rejected with a managed error |
| `reset_contexts` | Context reset policy | `false` uses the standard reset value; `true` is rejected until restart/context-reset signaling is implemented and tested |

The `interleave_mode` mapping keeps the current core field usable without
pretending that JPEG-LS has a separate “planar” codestream mode. If the core
crate is revised later, its field documentation should describe the actual
None/Line/Sample wire modes and keep DICOM `PlanarConfiguration` in the adapter.

Defaults are codec-specific even though `JpegLsParams::default()` is shared:

```rust
JpegLsLosslessCodec::default_params()
    == CodecParams::JpegLs(JpegLsParams { near_lossless: 0, ..Default::default() });
JpegLsNearLosslessCodec::default_params()
    == CodecParams::JpegLs(JpegLsParams { near_lossless: 3, ..Default::default() });
```

The default scan mode is derived from the source when the field is zero: None
for monochrome, Sample for interleaved three-component data, and Line for
planar three-component data after normalization. An explicit non-zero value
overrides that choice only when the source layout supports it.

## 5. Supported DICOM pixel layouts

The first release accepts the following source and destination layouts:

| BitsAllocated | BitsStored | Samples | PlanarConfiguration | Photometric Interpretation | Result |
| ---: | ---: | ---: | ---: | --- | --- |
| 8 | 2..=8 | 1 | absent/0 | MONOCHROME1/2 | exact or NEAR-bounded |
| 16 | 2..=16 | 1 | absent/0 | MONOCHROME1/2 | exact or NEAR-bounded |
| 8 | 2..=8 | 3 | 0 | RGB/YBR_FULL | RGB-normalized exact or NEAR-bounded |
| 8 | 2..=8 | 3 | 1 | RGB | planar-to-interleaved normalization |
| 16 | 2..=16 | 3 | 0 | RGB/YBR_FULL | RGB-normalized component preservation |

`BitsAllocated` must be 8 or 16, `BitsStored <= BitsAllocated`, dimensions must
be non-zero, `SamplesPerPixel` must be 1 or 3, and `PixelRepresentation` must
be 0 or 1. Samples are read in the native DICOM byte order already guaranteed by
`PixelDataRef`. Signed samples use an explicit offset mapping: for `b =
BitsStored`, decode the DICOM two's-complement value `s` and pass
`u = s + 2^(b-1)` to JPEG-LS with `MAXVAL = 2^b-1`; decoding reverses the
offset and sign-extends to `BitsAllocated`. The NEAR bound is measured after
the inverse mapping in the signed sample domain, so the offset does not change
the allowed absolute error.

The adapter accepts `YBR_FULL` by converting each frame to full-range RGB before
JPEG-LS coding, matching fo-dicom's `YbrFullToRgb` path. The destination
Photometric Interpretation is set to `RGB`, and lossless/near-lossless claims
are made against the normalized RGB samples rather than the original YBR bytes.
`YBR_FULL_422` remains rejected because its subsampled native layout needs a
separate unpacking contract. `YBR_PARTIAL_*`, YBR_ICT, YBR_RCT, palette color,
CMYK, and unknown photometric values are rejected.

Color policy follows fo-dicom's component handling and makes normalization
visible in DICOM metadata:

- RGB data is preserved as three components; no RGB-to-YBR transform is
  performed by the first Rust release.
- YBR_FULL is converted to RGB before encoding. The integer conversion helper is
  shared by encode/decode tests and its maximum per-component conversion delta
  is reported separately from JPEG-LS's NEAR error.
- Planar RGB is normalized to interleaved bytes for the core and repacked only
  after a successful decode when the destination explicitly requires planar
  storage.
- Signed RGB components use the offset mapping above. Signed YBR_FULL is
  rejected because the DICOM YBR-to-RGB conversion is defined for unsigned
  samples.
- The codestream's component count and sample precision must match the DICOM
  metadata before the output frame is appended.

## 6. Codestream architecture

The crate is split into small private modules following the existing JPEG and
RLE crates. The exact implementation files are fixed here so each stage has a
testable boundary:

| File | Responsibility |
| --- | --- |
| `src/marker.rs` | SOI/EOI/SOF55/LSE/SOS parsing and writing, segment lengths, APPn/COM skipping, marker validation |
| `src/bit.rs` | MSB-first scan bit reader/writer, byte stuffing, marker boundary detection, bounded reads |
| `src/preset.rs` | T.87 default and LSE preset coding parameters, `MAXVAL`, `T1/T2/T3`, `RESET`, range checks |
| `src/context.rs` | JPEG-LS regular-mode context arrays, `A`, `B`, `C`, `N` updates, bias correction, reset behavior |
| `src/golomb.rs` | signed mapping, unary quotient, remainder coding, `k` selection, limit checks |
| `src/run.rs` | run interruption coding, run-length segments, run index updates and end-of-line handling |
| `src/scan.rs` | LOCO-I predictor, edge gradients, regular/run mode state machine, interleave traversal, restart state |
| `src/frame.rs` | DICOM layout validation, per-frame encode/decode, planar/color adaptation, checked output size |
| `src/lib.rs` | two `Codec` implementations, parameter validation, syntax checks, error mapping |

The CharLS port is behavior-oriented rather than a line-for-line C translation:
Rust slices and checked arithmetic replace raw pointers and caller-sized output
buffers; every parser returns `Result`; and the core never writes into the
destination dataset directly.

### Marker and segment rules

Encoder output is one interleaved or non-interleaved JPEG-LS scan per frame:

```text
SOI       FFD8
SOF55     FFF7  (precision, height, width, component descriptors)
LSE       FFF8  (only when a non-default preset is selected)
SOS       FFDA  (component selectors, NEAR, ILV, point transform)
scan data (byte-stuffed)
EOI       FFD9
```

The decoder requires SOI, one valid SOF55, one compatible SOS, a complete scan,
and EOI. It validates segment lengths before slicing, rejects duplicate or
inconsistent component definitions, checks `NEAR`, `ILV`, and point transform
against the frame precision, and accepts APPn/COM only by safely skipping their
bounded payload. A marker-like byte inside scan data is legal only after byte
stuffing or at a restart/EOI boundary. Unknown markers, truncated segments,
missing EOI, invalid restart transitions, and extra compressed bytes are errors.
The decoder accepts legal LSE preset segments. Restart intervals and RST
markers are rejected in this release; the encoder emits neither custom LSE
values nor restart markers because the public parameter contract has no
restart interval field.

### JPEG-LS scan algorithm

The scan implementation follows ISO/IEC 14495-1 and the corresponding CharLS
encoder/decoder routines:

1. Initialize `MAXVAL`, `RANGE`, `QBPP`, `LIMIT`, and `RESET` from precision or
   the LSE preset.
2. For each component sample, load `Ra`, `Rb`, and `Rc`; compute the three LOCO-I
   gradients and the median predictor.
3. Quantize gradients into one of the 365 regular contexts, apply the context
   bias correction, and select the Golomb parameter `k` from `A/N`.
4. Map the signed prediction error into the non-negative mapped error, code it
   with unary quotient and remainder, and update `A/B/C/N` with the JPEG-LS
   near-lossless rules.
5. Enter run mode when the quantized gradients are all zero; encode run-length
   chunks and the run interruption error, including end-of-line and restart
   boundaries.
6. Decode by reversing the same mapping and clamping reconstructed samples to
   `[0, MAXVAL]`; for `NEAR > 0`, the decoded sample must be within `NEAR` of
   the source sample used by the test oracle.

No floating point is used in the scan core. All intermediate values use a
signed type wide enough for 16-bit samples and checked conversions at the
byte/layout boundary.

## 7. DICOM frame adapter

### Encode path

1. Validate `CodecParams::JpegLs`, destination UID, source native syntax, image
   geometry, sample precision, photometric interpretation, and planar layout.
2. Read each source frame with `src.get_frame(index)` and verify its byte length
   against `src.uncompressed_frame_size()`.
3. Normalize planar RGB or explicitly supported color input into the scan layout.
4. Build a per-frame `JlsFrame` containing width, height, components, precision,
   `NEAR`, and the resolved interleave mode.
5. Encode into a temporary `Vec<u8>`, require a final EOI, and retain only the
   exact codestream length.
6. After every frame succeeds, append frames with `dst.add_frame`; the core adds
   DICOM even-length padding.

### Decode path

1. Validate `CodecParams::JpegLs`, source UID, destination native syntax, and
   destination metadata before reading compressed bytes.
2. Read each logical frame with `src.get_frame(index)`; after locating EOI, the
   adapter permits and removes the single DICOM even-length pad byte, but never
   treats bytes after that pad as scan data.
3. Parse and validate the codestream header before allocating the output. Check
   dimensions, precision, component count, `NEAR`, and interleave against the
   destination layout and a checked expected frame size.
4. Decode into a temporary native-order buffer, normalize/repack planar output
   only after successful scan completion, and verify the final byte length.
5. Append the validated frame with `dst.add_frame`; no destination frame is
   changed when parsing or scan decoding fails.

The adapter must not allocate from a compressed-length guess. It uses checked
`width * height * components * bytes_per_sample` arithmetic and rejects sizes
that do not fit `usize` or the destination's declared frame size.

## 8. Implementation stages

Each stage ends with its own focused tests and keeps earlier stages passing.

1. **Porting map and primitives**
   - Record CharLS source files/functions used as the oracle for marker parsing,
     bit I/O, presets, context updates, Golomb coding, and run mode.
   - Implement `marker.rs`, `bit.rs`, and `preset.rs` with malformed-input tests.
   - Add no public API or native dependency.
2. **Regular mode and Golomb core**
   - Implement `context.rs` and the regular-mode portion of `scan.rs`.
   - Prove signed mapping, `k` selection, bias updates, reset arithmetic, and
     exact reconstruction on small synthetic rows.
3. **Run mode and complete scan state machine**
   - Implement `run.rs`, restart handling, line boundaries, and None/Line/Sample
     traversal.
   - Add encoder/decoder byte-for-byte fixtures generated from the Rust core and
     decode fixtures produced by CharLS.
4. **DICOM adapter and registrations**
   - Replace the skeleton in `src/lib.rs`, add `frame.rs`, and implement the
     layout/color policy above.
   - Set the near-lossless codec default to `near_lossless = 3`; enforce `0` for
     the lossless codec.
   - Add multi-frame, odd-length padding, planar RGB, signed 8/16-bit offset
     mapping,
     wrong-parameter, wrong-syntax, and no-partial-frame tests.
5. **Interoperability and hardening**
   - Decode both fo-dicom acceptance DICOM files in Rust.
   - Encode Rust output and decode it with fo-dicom/CharLS for 8-bit and 16-bit
     monochrome and supported RGB layouts.
   - Fuzz or property-test marker/bit/scan parsers with bounded input and add
     regressions for every discovered malformed stream.

## 9. Verification matrix

### Unit and property checks

- Marker lengths, SOF55/SOS consistency, LSE validation, APPn/COM skipping,
  byte stuffing, EOI detection, and truncated input.
- `MAXVAL/RANGE/QBPP/LIMIT/RESET` calculations for 2..=16 bits and custom LSE
  parameters.
- All regular contexts, predictor edge cases, signed mapping around zero,
  Golomb `k=0` and large `k`, bias correction, and context reset.
- Run lengths at zero, one, full-row, end-of-line, and restart boundaries.
- Exact lossless 8/16-bit round trips and near-lossless checks for NEAR values
  `1`, `3`, and `MAXVAL` on deterministic synthetic images.

### DICOM adapter checks

- One and three components, interleaved and planar RGB, 8/16 `BitsAllocated`,
  non-byte-aligned `BitsStored`, signed offset mapping, and multi-frame order.
- Native-to-encapsulated and encapsulated-to-native paths through
  `radicom::pixel::transcoder::Transcoder`.
- DICOM padding, fragment sequence handling, wrong transfer syntax, wrong
  parameter variant, zero dimensions, short source frames, and malformed
  codestreams without partial destination frames.
- Explicit YBR_FULL-to-RGB normalization, rejection of YBR_FULL_422, one-bit
  packed input, signed YBR color conversion, and unsupported color,
  custom preset/reset flags, and component counts other than 1 or 3.

### Interoperability evidence

- Rust decodes the small native CharLS mono and RGB line-interleave fixtures
  exactly, including byte stuffing.
- The fo-dicom 960x540 RGB lossless and near-lossless acceptance fixtures are
  decoded successfully by the Rust implementation, including line interleave.
- fo-dicom/CharLS decodes Rust output for 8-bit lossless and near-lossless
  line-interleaved RGB fixtures; the decoded near-lossless samples stay within
  the requested `NEAR` bound.
- Lossless comparisons are byte/sample exact after DICOM native byte order and
  planar rules. Near-lossless comparisons report maximum absolute sample error,
  not only visual similarity.
- The acceptance matrix records real database/file fixtures separately from
  local unit and property checks; a successful Rust round trip alone is not
  claimed as cross-implementation proof.

## 10. Error, resource, and security rules

- No parser path may panic on untrusted codestream bytes.
- Every marker length, bit count, unary run, dimension product, offset, and
  allocation uses checked arithmetic or a bounded cursor.
- The decoder rejects dimensions or frame sizes that exceed the destination or
  `usize` before allocation.
- Temporary buffers are dropped on all error paths; compressed output is copied
  to an owned frame before `add_frame`.
- Error messages include codec name, operation, frame index when known, and the
  first failing structural condition.
- Failure is frame-atomic: if frame `N` fails, that frame is not appended; frames
  `0..N-1` already appended by the existing JPEG/RLE contract may remain in the
  destination.
- A known shortcut is intentionally deferred: `preset_coding_params != 0` and
  `reset_contexts = true` are rejected until their marker/state semantics have
  dedicated tests; they must not be silently ignored.

## 11. Files and ownership

Implementation changes are confined to:

- Create/modify `crates/radicom-codecs-jpeg-ls/src/{marker,bit,preset,context,golomb,run,scan,frame,color,lib}.rs`.
- Modify `crates/radicom-codecs/src/lib.rs` only if facade integration tests need
  an additional public re-export; registration already exists.
- Add focused tests beside each private module and a facade/transcoder test under
  `crates/radicom-codecs/tests/` for public behavior.
- Keep fo-dicom DICOM files outside the repository unless their redistribution
  license is confirmed; otherwise store small generated codestream fixtures and
  document the external fixture paths used for interoperability.

No Cargo dependency is required for the core implementation. Any future pure
Rust helper dependency requires a separate license, audit, and benchmark review;
CharLS remains a reference source, never a runtime dependency.

## 12. Acceptance criteria

JPEG-LS support is complete only when both registered codecs perform native ↔
encapsulated transcoding, all focused tests and workspace tests pass, malformed
input returns managed errors without partial frames, the lossless fixture is
sample-exact, the near-lossless fixture meets its stated bound, and fo-dicom
successfully decodes Rust output in both directions for the supported layouts.
