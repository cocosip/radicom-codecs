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
vendored in `fo-dicom.Codecs/Native/Common/CharLS` and the
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
- `fo-dicom.Codecs/Codec/DicomJpegLsCodec.cs`.
- `fo-dicom.Codecs/Native/Dicom.Imaging.Codec.JpegLS.cpp`.
- `fo-dicom.Codecs/Tests/Acceptance/PM5644-960x540_JPEG-LS_Lossless.dcm`.
- `fo-dicom.Codecs/Tests/Acceptance/PM5644-960x540_JPEG-LS_NearLossless.dcm`.

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
- One-bit packed DICOM frames, more than three components, CMYK,
  YBR_PARTIAL_422/420, HSV, or ARGB.
- Palette colour lookup tables, palette expansion, and palette rendering.
  `PALETTE COLOR` itself is supported as a one-sample index layout (§5); only
  LUT handling stays out of the codec.
- Emitting restart intervals from the public parameter contract: the decoder and
  the internal encoder path handle DRI/`RSTm`, and the encoder emits them when
  `JpegLsParams::restart_interval` is non-zero.
- Encoding sub-sampled components: T.87 allows unequal horizontal/vertical
  sampling factors, and the decoder accepts them (the `T8SSE0`/`T8SSE3`
  conformance streams use 256x256, 256x64 and 128x128 components) by decoding
  each plane on its own grid and normalizing to three samples per pixel. DICOM
  Pixel Data cannot express per-component sampling, so the encoder only writes
  `0x11` factors.
- A new typed public parameter enum in the core `radicom` crate, and `mrfx`
  colour transforms beyond HP1/HP2/HP3 (CharLS also knows `RgbAsYuvLossy` and
  matrix transforms, ids 4 and 5, which are rejected).

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
| `interleave_mode` | Scan interleave selection | `0` = derive from DICOM storage (planar → line, interleaved → sample); `1` = plane (component) interleaving, accepted for any storage because the adapter de-interleaves the samples; `2` = line; `3` = sample |
| `color_transformation` | HP1/HP2/HP3 transform selector | `0` = none; `1..=3` apply the CharLS HP transform and write the `mrfx` APP8 segment. Requires three unsigned components and 8 or 16 bits stored, matching CharLS `UnsupportedBitDepthForTransform`. The decoder always honours the segment in the stream and ignores this field |
| `preset_coding_params` | Request custom LSE preset parameters | `0` uses T.87 defaults; non-zero is reserved until a concrete preset structure exists and is rejected with a managed error |
| `reset_contexts` | Context reset policy | `false` uses the standard reset value; `true` is rejected. It is a per-frame adapter reset request, separate from the DRI restart intervals the decoder already honors |
| `restart_interval` | DRI restart interval in MCUs | `0` = no restart markers. A non-zero value is written as a DRI segment and the encoder emits `RST0..RST7` at every MCU multiple; the decoder always accepts DRI regardless of this field |

The `interleave_mode` mapping keeps the field a preference while the adapter owns
`PlanarConfiguration`: `0` picks the codestream interleave from the source
storage, `2` and `3` force line or sample interleaving, and `1` requests plane
interleaving, which PS3.5 8.2.3 allows as component interleaving (one scan per
component). Plane interleaving is opt-in only: CharLS/fo-dicom decode an
`ILV=0` stream as if it were a single scan, so the default must stay
interop-safe. If the core crate is revised later, its field documentation should
keep describing the None/Line/Sample wire modes and leave `PlanarConfiguration`
in the adapter.

Defaults are codec-specific even though `JpegLsParams::default()` is shared:

```rust
JpegLsLosslessCodec::default_params()
    == CodecParams::JpegLs(JpegLsParams { near_lossless: 0, ..Default::default() });
JpegLsNearLosslessCodec::default_params()
    == CodecParams::JpegLs(JpegLsParams { near_lossless: 3, ..Default::default() });
```

The default (field zero) is interop-safe for every component count: monochrome
is JPEG-LS "none", interleaved three-component data becomes sample interleaved,
and planar three-component data becomes line interleaved after normalization.
Plane interleaving (one scan per component) is produced only when `1` is
requested explicitly; the decoder reads it either way, and the encoder never
emits it by default.

## 5. Supported DICOM pixel layouts

The first release accepts the following source and destination layouts:

| BitsAllocated | BitsStored | Samples | PlanarConfiguration | Photometric Interpretation | Result |
| ---: | ---: | ---: | ---: | --- | --- |
| 8 | 2..=8 | 1 | absent/0 | MONOCHROME1/2 | exact or NEAR-bounded |
| 16 | 2..=16 | 1 | absent/0 | MONOCHROME1/2 | exact or NEAR-bounded |
| 8 | 2..=8 | 1 | absent/0 | PALETTE COLOR | exact or NEAR-bounded index samples |
| 16 | 2..=16 | 1 | absent/0 | PALETTE COLOR | exact or NEAR-bounded index samples |
| 8 | 2..=8 | 3 | 0 | RGB/YBR_FULL | RGB-normalized exact or NEAR-bounded |
| 8 | 2..=8 | 3 | 1 | RGB | planar-to-interleaved normalization |
| 16 | 2..=16 | 3 | 0 | RGB/YBR_FULL | RGB-normalized component preservation |
| 16 | 2..=16 | 3 | 1 | RGB | planar-to-interleaved normalization |

`BitsAllocated` must be 8 or 16, `BitsStored <= BitsAllocated`, dimensions must
be non-zero, `SamplesPerPixel` must be 1 or 3, and `PixelRepresentation` must
be 0 or 1. Samples are read in the native DICOM byte order already guaranteed by
`PixelDataRef`. Signed samples preserve their `BitsStored` two's-complement
bit patterns in the JPEG-LS stream, matching fo-dicom. No sample-value offset is
applied. On decode the adapter sign-extends to `BitsAllocated`.
For signed near-lossless encoding, each source value `s` must satisfy
`-2^(b-1)+NEAR <= s <= 2^(b-1)-1-NEAR`, where `b = BitsStored`.
Values outside that range are rejected before any frame is appended, because
the unsigned JPEG-LS NEAR bound can otherwise cross the signed discontinuity.
Error is measured in the signed sample domain, including transitions through
zero.

On encode the adapter accepts `YBR_FULL` by converting each frame to full-range
RGB before JPEG-LS coding, matching fo-dicom's `YbrFullToRgb` path. The
compressed Photometric Interpretation is set to `RGB`, and lossless/near-lossless
claims are made against the normalized RGB samples rather than the original YBR
bytes. On decode the compressed dataset's Photometric Interpretation selects the
colour policy: a `YBR_FULL`-labelled stream has `color::ybr_to_rgb` applied to the
decoded samples with the same coefficients and rounding as the encode path, and
its Photometric Interpretation becomes `RGB`; an `RGB`-labelled stream is decoded
without conversion. This follows PS3.5 Table 8.2.3-1 note 3, where converting the
samples and setting `RGB` is one of the two conforming choices, while relabelling
without converting is neither.

Sub-sampled codestreams are a decode-only capability. T.87 allows SOF55
sampling factors other than `1x1`; the decoder derives each component plane as
`ceil(columns*Hi/Hmax) x ceil(lines*Vi/Vmax)` (T.87 A.2.1), decodes every plane
on that grid, and normalizes the frame to the full grid with three samples per
pixel by nearest-neighbour replication (sample `p` of the frame reads plane
sample `p*factor/factor_max`). The Photometric Interpretation is unchanged, so a
`YBR_FULL` sub-sampled stream still follows the conversion rule above. The
encoder always writes `0x11` factors because DICOM Pixel Data has no
per-component sampling.

`PALETTE COLOR` is accepted for one-sample index images (Table 8.2.3-1, 8/16
`BitsAllocated`) and is passed through exactly, with no colour conversion and no
Photometric Interpretation rewrite; palette colour lookup tables and
`PALETTE COLOR` with three samples are outside the codec. `YBR_FULL_422` remains
rejected because its subsampled native layout needs a separate unpacking
contract. `YBR_PARTIAL_*`, YBR_ICT, YBR_RCT, CMYK, and unknown photometric values
are rejected.

Color policy follows fo-dicom's component handling and makes normalization
visible in DICOM metadata:

- RGB data is preserved as three components; no RGB-to-YBR transform is
  performed by the first Rust release, and an `RGB`-labelled codestream is
  decoded without colour conversion.
- The optional HP1/HP2/HP3 color transform is a coding-domain decoration inside
  the codestream, not a DICOM colour space: it is applied before entropy coding
  and undone after decoding, so samples and Photometric Interpretation are
  unchanged, and it is rejected for signed or single-component images.
- YBR_FULL is converted to RGB before encoding and after decoding. Both paths use
  fo-dicom's full-range coefficients and rounding, with the chroma center at
  `2^(bits-1)`. Codec error bounds apply to the normalized RGB samples, and the
  Photometric Interpretation is `RGB` on both paths.
- PALETTE COLOR carries one index sample per pixel, so no colour conversion is
  applied and the Photometric Interpretation is preserved on encode and decode.
- Planar RGB is normalized to interleaved bytes for the core and repacked only
  after a successful decode when the destination explicitly requires planar
  storage.
- Signed RGB components preserve the two's-complement bit pattern above. Signed
  YBR_FULL is rejected on both paths because the DICOM YBR-to-RGB conversion is
  defined for unsigned samples.
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

The reference routines are `jpeg_stream_reader`/`jpeg_stream_writer` for markers,
`encoder_strategy`/`decoder_strategy` for stuffed bit I/O and Golomb coding,
`jpegls_preset_coding_parameters::compute_default` for T.87 C.2/C.3 thresholds,
`context_regular_mode`/`context_run_mode` for statistics, and `scan.h` for the
predictor and run state machine. The low-precision threshold factor and the
C.3 fallback to the lower bound are covered explicitly; ordinary numeric
clamping is not equivalent at large NEAR.

### Marker and segment rules

Encoder output is one or more JPEG-LS scans per frame:

```text
SOI       FFD8
APP8      FFE8  (only with a requested HP color transform: "mrfx" + id)
SOF55     FFF7  (precision, height, width, component descriptors)
LSE       FFF8  (only when a non-default preset is selected)
DRI       FFDD  (only in accepted streams; interval counted in MCUs)
SOS       FFDA  (component selectors, NEAR, ILV, point transform)
scan data (bit-stuffed), split by RST0..RST7 when DRI is present
[more SOS + scan data for ILV=0, one scan per component]
EOI       FFD9
```

A sub-sampled stream (SOF55 factors other than `0x11`) keeps the same structure;
each component plane is decoded on its own grid and resampled after the scans
finish.

The decoder requires SOI, one valid SOF55, at least one compatible SOS, a
complete scan, and EOI. It validates segment lengths before slicing, rejects
duplicate or inconsistent component definitions, checks `NEAR`, `ILV`, and point
transform against the frame precision, and accepts APPn/COM only by safely
skipping their bounded payload. SOF55 component descriptors must carry sampling
factors in `1..=4` and a zero quantisation-table selector; unequal factors select
the sub-sampled plane grid of §5 on decode, while the encoder always writes
`0x11`. `ILV=2` additionally requires equal component dimensions (T.87 C.2.3). A
marker-like byte inside scan data is legal only after bit stuffing or at a
restart/scan/EOI boundary. Unknown markers, truncated segments, invalid restart
transitions, and a missing EOI are errors. Bytes after EOI are ignored instead of
rejected: fo-dicom sizes encapsulated fragments by `ArrayPool` bucket, so stale
non-zero memory can follow a valid codestream, and a decoder stops at EOI.

T.87 B.2 and DICOM PS3.5 8.2.3 let the bit stream choose component, line or
sample interleaving. With `ILV=0` every scan holds `Ns=1`, and the image is
component interleaved: one scan per component, each covering its component
exactly once. A missing, duplicate, or unknown component scan is a managed error,
as is a plane-interleaved scan with more than one component. `ILV=1` and `ILV=2`
are single-scan modes, so extra scans or a component count that differs from
SOF55 are rejected, and every scan of the image must use the same `NEAR`, `ILV`
and point transform. Each scan starts with fresh model and neighbour state, so a
component scan is decoded exactly like a single-component image. The encoder
emits plane interleaving only for an explicit `interleave_mode = 1`: `0` derives
line or sample interleaving from the storage, because deployed decoders
(CharLS/fo-dicom, DCMTK lineage) mis-read `ILV=0`.

The decoder honors DRI restart intervals (`FFDD`; T.87 C.2.5 allows a 2-, 3- or
4-byte value, and `0` means no restarts). T.87 B.4 defines the MCU per interleave
mode, and the interval counts MCUs:

- `ILV=0`: one line of the single component, so a sub-sampled plane is split by
  its own line count, `ceil(plane_lines / interval)`.
- `ILV=2`: `Ns` lines, one per component; sample interleaving needs equal
  component dimensions, so this is one frame row and the count is
  `ceil(height / interval)`.
- `ILV=1`: `V1` lines of `C1`, then `V2` lines of `C2`, and so on. With `1x1`
  factors that is one frame row; with sub-sampling it is the line group, and the
  count is `ceil(rounds / interval)` with `rounds = ceil(Y/Vmax)` per component.
  The encoder pads the last MCU, so the decoder decodes the added lines and drops
  them.

Scan bytes are split at `RST0`–`RST7`, which must appear in that cyclic order
starting at `RST0`, and the segment count must equal the value implied by the MCU
count above; the final interval may be shorter than the others. A missing, extra,
or out-of-order marker is a managed error, and the count is enforced while
scanning so a marker flood cannot grow the segment list. Because a boundary never
falls inside a line, runs are never split: a marker inside a line is invalid and
its segment cannot finish that line. At each boundary T.87 D.4
`Reset_encoder`/`Reset_decoder` applies: the adaptive model returns to its
scan-start state — all 365 `A/B/C/N` contexts, both run contexts, and `RUNindex`
— and the prediction neighbourhood is re-initialized as if the first line of the
interval were the first line of the scan, so the previous line and the left edge
sample are zero. Reconstruction buffers keep their real samples; only prediction
restarts. The encoder emits DRI and `RSTm` when
`JpegLsParams::restart_interval` is non-zero.
The decoder accepts legal LSE preset segments.
Zero-valued LSE fields select defaults; thresholds must satisfy
`NEAR+1 <= T1 <= T2 <= T3 <= MAXVAL`, and RESET must be in
`3..=max(255, MAXVAL)`. Nonzero SOS mapping-table selectors are rejected
instead of being ignored.

The HP colour transforms travel in an APP8 segment that CharLS defines outside
T.87 (see `color_transform.h`): payload `m r f x` followed by one id byte, so the
segment length field is 7. The encoder writes it between SOI and SOF55 when
`color_transformation` is `1..=3`, and applies the matching forward transform to
the samples before coding. The decoder reads the id (`0` none, `1` HP1, `2` HP2,
`3` HP3), rejects other ids with `unknown JPEG-LS color transformation`, and
undoes the transform after the planes are assembled and before any
YBR-to-RGB conversion. Only five-byte APP8 payloads are candidates, matching
CharLS; every other APP8 segment (SPIFF, vendor private) is skipped. Like
CharLS, the transforms require three unsigned components and either 8 or 16 bits
stored, because a transformed sample must still fit the frame `MAXVAL`. CharLS
only applies a transform on interleaved scans (its process-line factory returns
the plain single-component path for `ILV=0`), so a transformed plane-interleaved
stream would carry a transform no implementation inverts. The decoder still
honours the segment for `ILV=0` (another tool may have written one), but the
encoder refuses that combination: `interleave_mode = 1` with
`color_transformation = 1..=3` fails with `plane interleaving (interleave_mode 1)
cannot carry a JPEG-LS color transformation`, so every stream this codec emits
has at least one reference decoder that reads it correctly (`ILV=1` line and
`ILV=2` sample interleaving are verified against CharLS, 0 differences).

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
   chunks and the run interruption error. Runs end at the line boundary, which is
   also the MCU and restart boundary.
6. Decode by reversing the same mapping and clamping reconstructed samples to
   `[0, MAXVAL]`; for `NEAR > 0`, the decoded sample must be within `NEAR` of
   the source sample used by the test oracle.
7. At each restart boundary, finish the byte-aligned segment, require the next
   `RSTm` marker, apply D.4 `Reset_encoder`/`Reset_decoder` (model plus
   neighbourhood), and continue decoding with the real reconstructed samples.

The restart rule is checked item by item: T.87 B.4 makes one MCU the unit, which
is one line of the single component for `ILV=0`, `V1` lines of C1 followed by
`V2` of C2 and so on for `ILV=1`, and one line of all components for `ILV=2`
(legal only with equal component dimensions). Input values for an HP color
transform are converted before the model sees them, exactly like CharLS
`process_line.h`: the transform maps `R,G,B` to `v1,v2,v3` in the sample
container on encode, and the inverse maps them back after reconstruction on
decode. D.4 requires the
model and the first-line initialization to be re-applied, so this decoder also
zeroes the previous line and the left edge sample. CharLS agrees on both points
(`decode_lines` counts `lines_in_interval`, resets `contexts_`,
`context_run_mode_`, `run_index_` and does `std::fill(line_buffer, 0)`), and the
vendored CharLS decoder was used to confirm the result: restart streams from the
Rust test encoder decode in CharLS to the original samples (§12).

No floating point is used in the scan core. All intermediate values use a
signed type wide enough for 16-bit samples and checked conversions at the
byte/layout boundary.

## 7. DICOM frame adapter

### Encode path

1. Validate `CodecParams::JpegLs`, destination UID, source native syntax, image
   geometry, sample precision, photometric interpretation, and planar layout.
2. Read each source frame with `src.get_frame(index)` and verify its byte length
   against `src.uncompressed_frame_size()`.
3. Normalize planar RGB or explicitly supported color input into the scan layout,
   then apply the requested HP color transform to the samples when
   `color_transformation` is `1..=3`.
4. Build a per-frame `JlsFrame` containing width, height, components, precision,
   `NEAR`, and the resolved interleave mode.
5. Encode into a temporary `Vec<u8>`, require a final EOI, and retain only the
   exact codestream length.
6. After every frame succeeds, append frames with `dst.add_frame`; the core adds
   DICOM even-length padding.

### Decode path

1. Validate `CodecParams::JpegLs`, source UID, destination native syntax, and
   destination metadata before reading compressed bytes.
2. Read each logical frame with `src.get_frame(index)`; the decoder stops at the
   first EOI and ignores every remaining byte of the fragment (DICOM even-length
   padding and fo-dicom `ArrayPool` bucket slack), but never treats those bytes
   as scan data and never rejects a fragment because of them.
3. Parse and validate the codestream header before allocating the output. Check
   dimensions, precision, component count, `NEAR`, interleave, and the DRI
   restart interval against the destination layout and a checked expected frame
   size.
4. Decode every scan into component planes. Plane-interleaved (`ILV=0`) scans
   fill one plane each, line interleaving (`ILV=1`) decodes `Vi` lines per
   component and MCU, and sample interleaving (`ILV=2`) produces one frame row
   per MCU. Within a scan, split at `RSTm` markers and apply the D.4 restart
   reset (model and prediction neighbourhood) at each MCU boundary. Then map the
   planes onto the frame grid: exact for `1x1` sampling factors, nearest-neighbour
   replication of each plane sample when a component is sub-sampled, so the
   output frame always holds `SamplesPerPixel` samples per pixel. Undo an `mrfx`
   color transform on the assembled samples, then apply the colour-space policy
   (YBR_FULL to RGB), normalize planar output only after successful scan
   completion, and verify the final byte length.
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
   - Implement `run.rs`, line boundaries, and the plane/line/sample traversal.
   - Support multi-scan component interleaving: validate one scan per component
     with exactly one coverage each, and initialize the model and neighbours at
     every scan start.
   - Handle DRI restart intervals: split the scan at `RSTm`, validate the marker
     sequence and segment count against `ceil(height / interval)`, and apply the
     D.4 model plus neighbourhood reset at each line boundary.
   - Add encoder/decoder byte-for-byte fixtures generated from the Rust core and
     decode fixtures produced by CharLS.
4. **DICOM adapter and registrations**
   - Replace the skeleton in `src/lib.rs`, add `frame.rs`, and implement the
     layout/color policy above.
   - Set the near-lossless codec default to `near_lossless = 3`; enforce `0` for
     the lossless codec.
   - Add multi-frame, odd-length padding, planar RGB, signed 8/16-bit wire
     preservation and near-lossless boundary checks,
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
- Run lengths at zero, one, full-row and end-of-line. A line is an MCU, so a run
  never crosses a restart boundary and a marker inside a line is rejected.
- Restart intervals: DRI parsing (2-, 3-, and 4-byte values), `RST0`..`RST7`
  sequence and count validation against `ceil(height / interval)` including a
  short final group, the D.4 model and neighbourhood reset, a restarted stream
  that differs from the plain stream byte for byte but decodes identically, and
  mono/line/sample-interleaved round trips at intervals 1, 2 and 3.
- Multi-scan `ILV=0` images: three plane-interleaved scans round-trip exactly,
  streams with a missing, duplicate, or unknown component scan are rejected, a
  plane-interleaved scan with several components and an interleaved image with
  several scans are rejected, `NEAR` must match across scans,
  `interleave_mode = 1` produces a three-scan stream from planar and from
  interleaved storage alike, and plane-interleaved scans combine with DRI
  restart intervals.
- Interleave selection: mode `1` streams differ byte for byte from modes `2` and
  `3` but decode identically; mode `0` with interleaved storage produces exactly
  the mode `3` bytes, and mode `0` with planar storage exactly the mode `2`
  bytes, so the default stays interop-safe.
- Plane-interleaved scans are decoded from any component order: each scan is
  mapped to its component by the SOS id, not by scan position.
- Sub-sampled codestreams (SOF55 factors other than `1x1`): the official
  `T8SSE0`/`T8SSE3` conformance streams decode each component plane on its own
  grid, invalid sampling factors (`0`, `>4`) and a non-zero table selector are
  rejected, sample interleaving of unequal components is rejected, and the frame
  is normalized to three samples per pixel by nearest-neighbour replication.
- HP1/HP2/HP3 color transforms: `1..=3` round-trip sample-exactly for 8/16-bit
  line-interleaved and sample-interleaved streams and combine with DRI restarts;
  the encoder writes exactly one `mrfx` segment with the matching id, a foreign
  five-byte segment is inverted with the CharLS formulas, id `4` is rejected,
  duplicate ids must agree, and a single-component image, 12-bit precision, or a
  plane-interleaved (`interleave_mode = 1`) request is rejected. The decoder still
  honours an `mrfx` segment on a plane-interleaved stream from another tool.
- Exact lossless 8/16-bit round trips and near-lossless checks for NEAR values
  `1`, `min(3, MAXVAL/2)`, and `min(255, MAXVAL/2)` on deterministic synthetic images.

### DICOM adapter checks

- One and three components, interleaved and planar RGB, 8/16 `BitsAllocated`,
  non-byte-aligned `BitsStored`, signed bit preservation/sign extension, and multi-frame order.
- PALETTE COLOR index images with 8- and 16-bit allocation and `BitsStored` in
  `2..=16` round-trip exactly with an unchanged Photometric Interpretation;
  three-sample PALETTE COLOR is rejected on encode and decode.
- A `YBR_FULL`-labelled stream decodes to converted RGB samples labelled `RGB`,
  and an `RGB`-labelled stream decodes without conversion.
- A sub-sampled codestream decodes to a full-resolution frame with three samples
  per pixel: the adapter normalizes the planes instead of unpacking a native
  `4:2:2` layout, so the destination keeps its declared dimensions and
  Photometric Interpretation.
- Fragments carrying non-zero bytes after EOI decode normally; a stream without
  EOI still fails.
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
- The acceptance matrix records external file fixtures separately from
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
- Keep all fo-dicom interoperability harnesses, samples, generated streams and
  raw comparison outputs under the ignored `.interop/` directory. They must not
  be staged or committed. Portable Rust-only unit and integration tests remain
  in their owning crates.

No Cargo dependency is required for the core implementation. Any future pure
Rust helper dependency requires a separate license, audit, and benchmark review;
CharLS remains a reference source, never a runtime dependency.

## 12. Acceptance criteria

JPEG-LS support is complete only when both registered codecs perform native ↔
encapsulated transcoding, all focused tests and workspace tests pass, malformed
input returns managed errors without partial frames, the lossless fixture is
sample-exact, the near-lossless fixture meets its stated bound, and fo-dicom
successfully decodes Rust output in both directions for the supported layouts.

### Verified evidence (2026-11-01)

- The portable deterministic matrix exercises all precisions 2..=16, mono,
  RGB Line/Sample, planar normalization, one-pixel edges, runs, and NEAR bounds.
  Separate regressions cover all 365 context addresses, stuffed terminal FF,
  malformed unary codes, LSE resolution, HP/mapping rejection, signed extrema,
  wrong UID/geometry, and failure of a later frame without partial output.
- The facade integration test uses the real registry/Transcoder for both UIDs,
  two frames, a Basic Offset Table with multiple fragments per frame, signed
  8/12/16-bit samples and planar 8/16-bit RGB.
- The ignored `.interop/matrix.py` and `.interop/matrix-rust/` harnesses run
  300 bidirectional CharLS cases. They pass the original signed two's-complement
  bytes directly to the native encoder; comparisons apply only DICOM planar
  ordering and sign extension, never a sample offset. Lossless samples are
  exact; each near-lossless error is <= the requested NEAR. Rust and native
  decoders also reconstruct each native-generated stream identically.
- Rust decodes both 960x540 fo-dicom acceptance files. Each 1,555,200-byte
  reconstruction matches a fresh native decode of the same stream exactly.
- Custom RESET=8 is independently checked with native None/Line streams.
  Sample plus custom RESET has Rust scan coverage; the bundled native oracle
  does not supply a usable independent check for this combination.
- Fragments with 64 non-zero bytes after EOI decode to the original samples,
  while the same stream truncated before EOI still fails. `PALETTE COLOR`
  index images round-trip exactly for 8-bit and for 16-bit allocation with
  `BitsStored` 2/8/12/16 with an unchanged Photometric Interpretation, and
  `PALETTE COLOR` with three samples is rejected on both paths. A
  `YBR_FULL`-labelled stream decodes to the same converted RGB samples as the
  encode path, and an `RGB`-labelled stream is not converted.
- Multi-scan `ILV=0` is verified in both directions. The official T.87 encoder
  conformance stream `T8C0E0.JLS` (256x256 RGB, three plane-interleaved scans,
  102,248 bytes) decodes to `TEST8.PPM` exactly: 196,608 of 196,608 samples with
  zero differences. Re-encoding `TEST8R/G/B.PGM` with the T.87 reference encoder
  reproduces `T8C0E0.JLS` byte for byte, and the reference decoder `locod`
  reproduces its three planes as three PGM files with zero differences per plane.
  In the other direction an 8x6 RGB image from the Rust encoder with an explicit
  `interleave_mode = 1` decodes in `locod` to the original three planes (48
  samples each, zero differences).
- Plane interleaving stays opt-in because the deployed decoders cannot read it.
  CharLS via fo-dicom decodes `T8C0E0.JLS` into 178,587 of 196,608 wrong
  samples, and it decodes a 960x540 RGB (PM5644) stream from the Rust encoder
  with `interleave_mode = 0` (plane, during the experiment) into 1,107,898 of
  1,555,200 wrong samples, while the same image encoded with `interleave_mode`
  `2` and `3` matches sample for sample (0 differences). Mode `0` therefore
  derives line/sample interleaving from the storage and never emits `ILV=0`;
  only an explicit `1` asks for plane interleaving. The decode direction still
  follows T.87 strictly, including `T8C0E0.JLS`.
- Sub-sampling is verified against the official T.87 conformance streams.
  `T8SSE0.JLS` (T.87 E.1.2 test 7, lossless, line interleaved, red `H2V4`, green
  `H2V1`, blue `H1V2`) decodes to exactly the reference component planes:
  `TEST8R.PGM` 256x256 (65,536 samples), `TEST8GR4.PGM` 256x64 (16,384) and
  `TEST8BS2.PGM` 128x128 (16,384), 0 differing samples in all three planes
  (98,304 in total). The frame normalized to the full grid equals the
  nearest-neighbour replication of those planes sample for sample (196,608
  samples). `T8SSE3.JLS` (test 8, `NEAR=3`) reconstructs every plane sample
  within the bound: worst absolute error 3 of 98,304 samples.
- Sub-sampled restart counting follows T.87 B.4's line-interleaved MCU, not the
  row count: inserting a DRI segment into `T8SSE0.JLS`, which has 64 MCUs
  (256/4 lines), leaves the stream valid at interval 64 (one segment, no markers
  needed) and is rejected at interval 63 (two segments expected); with the
  unsampled 256-row count both would fail.
- Restart intervals are verified against CharLS. Four streams from the Rust test
  encoder — mono `ILV=0` interval 3 (16x8, with runs), mono flat rows interval 3
  (60-byte codestream), RGB `ILV=2` interval 2 (8x6), RGB `ILV=1` interval 1
  (8x4), each 60–261 bytes with DRI and `RSTm` beats — were wrapped as JPEG-LS
  DICOM objects and decoded by fo-dicom's native CharLS decoder, which
  reconstructed every sample exactly (128, 128, 144 and 96 samples, 0
  differences). That confirms the line-based interval and the D.4 reset (model
  plus neighbourhood) against an independent decoder.
- Remaining restart limitations: the vendored CharLS encoder has no restart
  support (only `scan.h`'s decoder reads `RSTm`) and none of the 27 JPEG-LS
  codestreams under `.interop/` carries a DRI segment, so the direction
  "CharLS-encoded restart stream decoded by Rust" cannot be produced locally.
  The T.87 reference decoder (locod V.1.00) accepts the DRI segment but does not
  implement restart intervals: it decodes the first interval of an interval-3
  stream exactly (the first 48 samples behind the 12-byte PGM header match) and
  then ends the scan at `RST0`, reporting extra bytes. Wrong, extra, missing and
  in-line markers are covered by Rust tests only.
- HP colour transforms: fo-dicom's DICOM encoder never writes them. Encoding
  960x540 RGB with `DicomJpegLsParams.ColorTransform` set to `None`, `HP1`, `HP2`
  and `HP3` produced four byte-identical codestreams (same SHA-256) and no `mrfx`
  segment, because `DicomJpegLsCodec` hard-codes
  `colorTransformation = CharlsColorTransformationType.None` before calling
  CharLS, so the property is inert for DICOM output. CharLS itself does implement
  the segment (APP8 `mrfx` + id, written between SOI and SOF55). Rust streams
  therefore carry the transform, and CharLS reads them: an 8-bit 32x24 RGB image
  from the Rust encoder with HP1, HP2 and HP3 (2,304 samples each) decodes through
  fo-dicom's native CharLS decoder back to the original samples with 0
  differences, which pins the forward transform and the segment format to
  CharLS. Line interleaving (`ILV=1`) agrees as well: the same three transforms on
  the same 2,304-sample image also decode with 0 differences. Plane interleaving
  (`ILV=0`) does not: CharLS ignores the segment for non-interleaved scans
  (2,293, 2,288 and 2,297 of 2,304 wrong samples for HP1/HP2/HP3), so that
  combination is a Rust-only round trip. The reverse direction uses the CharLS
  formulas written out in the test
  as expected values for a stream that carries an `mrfx` segment but untransformed
  samples. Both restrictions CharLS enforces are mirrored: three unsigned
  components and 8/16 bits stored
  (`UnsupportedBitDepthForTransform` for 12-bit, on both the encode and the
  decode path), and `mrfx` ids 4/5
  (`RgbAsYuvLossy`, matrix) are rejected.
- Interoperability tools, native binaries, sample files, manifests, and JSON
  results remain ignored and are excluded from commits.
