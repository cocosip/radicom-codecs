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
Process 14 accepts predictor selections 1–7 and a point transform smaller than
the sample precision. SV1 is Process 14 with selection value 1: it always codes
with predictor 1 and no point transform, rejects a parameter set carrying
anything else, and its decoder rejects a codestream whose SOS names another
predictor or a non-zero point transform, so a lossless UID cannot carry
transformed (lossy) data.

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

The encoder accepts `MONOCHROME1`, `MONOCHROME2`, `RGB`, `YBR_FULL`,
`YBR_FULL_422`, and one-component `PALETTE COLOR` for every supported process.
A palette image is coded as a plain one-component plane: the LUT index samples
are neither converted nor rescaled, and the Photometric Interpretation is left
untouched. Signed samples are coded as their two's-complement bit patterns, which
is what libjpeg sees; the level shift cancels on decode, so signed input
round-trips through the same path as unsigned input. `YBR_ICT`, `YBR_RCT`, CMYK,
and YCCK are rejected for classic JPEG.

For lossy RGB encoding, the implementation converts RGB to full-range YCbCr.
`sample_factor = Full` emits `YBR_FULL`; `sample_factor = Half` emits
`YBR_FULL_422`. Decoding accepts every sampling factor set that T.81 allows
(`H_i`,`V_i` in `1..=4`, each dividing the frame maximum) because such streams
exist in the wild even though Table 8.2.1-1 defines no `YBR_FULL_420`
photometric interpretation: the block geometry comes from one normalized
`(Hmax, Vmax)` implementation, and chroma reconstruction replicates integer
factors (libjpeg's `int_upsample`) while 2×1, 1×2 and 2×2 use the triangular
filters from `jdsample.c`. A frame decoded from a non-DICOM sampling
configuration cannot be re-encoded as such — the encoder only offers 4:4:4 and
4:2:2. Decode converts the components back to RGB when
`convert_colorspace_to_rgb` is true and the compressed dataset is labelled
`YBR_FULL` or `YBR_FULL_422`; an `RGB`-labelled codestream is never converted
again. A `YBR_FULL_422` destination declares two bytes per pixel, so a converted
decode rebuilds the destination layout as interleaved RGB before writing the
three samples per pixel and then sets the photometric interpretation to `RGB`.
Note that decoding 4:2:0 into a `YBR_FULL_422` destination keeps only the
horizontal chroma resolution (2 bytes/pixel is the DICOM lossy contract); use
`YBR_FULL` or `convert_colorspace_to_rgb` when vertical chroma matters.

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
| `smoothing_factor` | Pre-encoding input smoothing | `0..=100`; ignored by Process 14. Ported bit for bit from libjpeg-turbo `jcsample.c`: `fullsize_smooth_downsample` for undownsampled components and `h2v2_smooth_downsample` for 2×2 chroma. Those are the only two smoothing kernels libjpeg has — for 2×1 and other integer ratios it sets `smoothok = FALSE` and logs `JTRC_SMOOTH_NOTIMPL`, so a 4:2:2 encode smooths luma only, exactly like libjpeg/fo-dicom |
| `convert_colorspace_to_rgb` | Decode YCbCr/RGB as RGB | reject for signed samples |
| `sample_factor` | Chroma sampling on encode | `Full` = 4:4:4, `Half` = 4:2:2; lossless forces 4:4:4. Decode additionally accepts any T.81 factor set (4:2:0, 4:1:1, 4:4:0, …) |
| `predictor` | Process 14 predictor selection | `1..=7`; SV1 requires `1` and rejects anything else |
| `point_transform` | Right shift before predictive coding | must be smaller than the sample precision; SV1 (`.70`) requires `0` |
| `restart_interval` | DRI restart interval in MCUs (`0` = none) | `0`..=`u16::MAX`; emitted as one DRI segment plus `RST0`–`RST7`; Process 14 counts one MCU per pixel, so a lossless interval must be `0` or an integer multiple of the image width (libijg-based decoders reject an off-row interval); Process 1/2/4 count one MCU per 8×8 block set |

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

The decoder validates SOI, one supported SOF marker, DQT/DHT definitions (one
DHT segment may carry several tables; the lossless path accepts DC tables 0–3),
SOS component selectors, DRI restart intervals, the restart-marker sequence, and
EOI. Before SOS the table and restart segments may appear in any order: T.81 does
not fix their order, libjpeg writes DQT and DHT ahead of the frame header, and
the lossless path accepts APP, COM, DQT, DHT, and DRI before or after SOF3 (SOF3
is still required before the first SOS; a lossless DQT is legal and unused). A
restart interval of `0` means “no restarts”. At each restart boundary the
decoder requires the next `RST0`–`RST7` marker in sequence, resets the DC
predictors (for Process 14 the predictor state restarts, so the whole first row
of the new interval uses the one-dimensional predictor of T.81 G.1.2, as in
libjpeg-turbo's first-row undifferencer), and aligns to the byte boundary; a
missing, unexpected, or mid-entropy restart marker is a managed error. Both
sequential and lossless decoding accept any number of scans as long as every
component is covered exactly once: an interleaved scan (Ns > 1) uses the frame
MCU grid with `h_i*v_i` blocks per component and a non-interleaved scan (Ns = 1)
uses the component plane grid from T.81 A.2.3, each scan resetting its DC
predictors. When `restart_interval` is non-zero the encoders write one DRI
segment (after the frame header) and an `RST0`–`RST7` marker at every interval
boundary; the default `0` keeps the single interleaved scan with no DRI. The
lossless encoder additionally rejects a restart interval that is not a multiple
of the image width, because one lossless MCU row is the whole image row and
libijg-based decoders refuse an interval that does not align to it.

Segment lengths include their two length bytes and must stay inside the frame.
APPn and COM are skipped with the same bounds checks. Standalone markers are
recognized only where the standard permits them. A marker encountered inside
entropy data is valid only after byte stuffing or at a legal restart/EOI
boundary. After the final EOI the decoder accepts at most one trailing `0x00`
byte — the DICOM even-length pad — and any other trailing byte is a managed
error (see `radicom-codecs-design.md` §9.2); this rejects spliced or
concatenated codestreams that a single-frame fragment cannot represent.

### Process 1 and Process 2/4

The sequential path performs level shift, 8×8 DCT, quantization, zig-zag scan,
Huffman coding, restart handling, and inverse operations. Process 1 is limited
to 8-bit samples. Process 2/4 supports the DICOM-relevant 8-bit and 12-bit
precision and rejects 16-bit sequential JPEG rather than truncating it.

Process 1 keeps its quantization tables in the baseline 8-bit range (1–255).
Process 2/4 follows libjpeg's `jpeg_set_quality(..., force_baseline = FALSE)`,
which is what fo-dicom's IJG path always calls: entries are clamped to 1–32767
instead of 1–255, and a table whose largest entry exceeds 255 is written with
`Pq = 1` (16-bit entries; the precision is per DQT table). The standard tables
cross that threshold only at quality ≤ 23, so a 12-bit encode at the default
quality keeps the byte-identical `Pq = 0` form. The decoder accepts `Pq = 0` and
`Pq = 1`, so 12-bit fo-dicom streams carrying 16-bit tables decode.

The sequential path emits one scan per frame and does not emit progressive or
arithmetic-coded scans, and it does not accept them: PS3.5 A.4 specifies DICOM
JPEG transfer syntaxes for Process 1 (`.50`), Process 2/4 (`.51`), and Process 14
(`.57`/`.70`); progressive and hierarchical JPEG are not part of that family. The
encoder walks the scan twice: the first pass collects DC/AC symbol frequencies,
`HuffmanTable::optimal` builds the four tables (T.81 Annex K.2), and the second
pass codes with them, which is libjpeg's `optimize_coding = TRUE` behavior. The
decoder accepts sequential streams with any number of interleaved or
non-interleaved scans and baseline streams that carry restart intervals from
other encoders, so acceptance stays explicit and testable.

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

The encoder sizes its DC table from the image itself: it walks the prediction
differences first, builds the optimal canonical table with the T.81 K.2 merge
algorithm (the same procedure libjpeg runs under `optimize_coding = TRUE`), and
writes that table in DHT before the scan. The fixed Annex K table costs about 50%
more on real images, which is why the encoder always optimizes; the decoder is
unchanged because it already accepts any DHT. Sequential DCT still writes the
Annex K default tables.

## 7. Error and resource rules

All malformed input, unsupported parameters, precision mismatches, overflow,
and destination-layout conflicts return `radicom::error::Error` with codec and
frame context. Panics are not an error path for untrusted codestream data.

The decoder performs checked arithmetic for dimensions, component counts,
MCU counts, row strides, and output allocation. It rejects frames whose decoded
size cannot fit the destination or the library's supported buffer size. Sequential
decoding checks the SOF header against the destination layout *before* allocating
the frame buffer, and both decoders reserve fallibly (`try_reserve_exact`), so a
crafted header such as `65535×65535` returns an error instead of aborting. Frame
counts are untrusted metadata: the adapters never reserve per-frame containers
sized by `NumberOfFrames`, they grow them as frames are produced. It does not
allocate based solely on a compressed length estimate.

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
- Restart intervals: decode of DRI/RST streams, `RST0`–`RST7` sequencing, DC and
  predictor reset, and rejection of missing or mismatched restart markers.
- Lossless table and scan structure: several tables in one DHT segment, DC
  selectors `0..=3`, non-interleaved multi-scan streams, and rejection of
  repeated, unknown, or missing components.
- Sequential multi-scan structure: 4:4:4 and 4:2:2 streams split into
  per-component and mixed scans decode to the same samples as the equivalent
  single interleaved scan, multi-scan plus DRI preserves the reconstructed
  samples with per-scan restart sequencing, and repeated, unknown, or uncovered
  components are managed errors.
- Per-component sampling-factor and quantization-table validation, including
  rejection of unreferenced or missing quantization tables.
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
- `PALETTE COLOR` round-trips at 8 and 16 bits through Process 14 and SV1, and
  the sequential syntaxes code the 8-bit index plane with the Photometric
  Interpretation preserved. A `YBR_FULL_422` destination with
  `convert_colorspace_to_rgb` yields an RGB frame of three bytes per pixel.
- Signed 8-bit RGB codes and decodes without sign-extension surprises: the
  decoded byte patterns equal the encoded ones, and `PixelRepresentation`
  survives.

### Interoperability checks

- Rust decodes the available `fo-dicom.Codecs` fixtures for Baseline
  `YBR_FULL`, Baseline `YBR_FULL_422`, and Lossless RGB. A Process 2/4 fixture
  is not present in the reference checkout yet.
- The built fo-dicom native decoder accepts Rust output for all four transfer
  syntaxes. Process 1 and Process 2/4 decode with lossy sample deltas bounded by
  the fixtures, and Process 14/SV1 are byte-exact, including 16-bit SV1.
- A Baseline fixture carrying DRI/RST restart markers decodes to the same
  samples as the reference decoder; the residual differences are `±1` IDCT
  rounding on 48 of 3,072 samples, not restart-state errors.
- `PALETTE COLOR` round-trips byte-exactly through `.57`, `.70`, and `.80` in
  both directions, and the sequential syntaxes code the index plane with the
  Photometric Interpretation preserved.
- Lossless comparisons are byte/sample exact after the DICOM byte-order and
  planar rules are applied. Lossy comparisons use fixed fixtures and a stated
  tolerance; visual similarity alone is not an acceptance criterion. For
  `YBR_FULL_422` output the reference decoder performs triangular chroma
  upsampling (`h2v1`) during its RGB conversion; the decoder follows libjpeg-turbo
  `jdsample.c` (even columns `(3*c[j] + c[j-1] + 1) >> 2`, odd columns
  `(3*c[j] + c[j+1] + 2) >> 2`, edges clamped), which brings the RGB comparison to
  `max=4` on the `PM5644` fixture — the same residual the 4:4:4 control shows
  without any upsampling, i.e. remaining deltas come from the IDCT and the
  fixed-point colour conversion, not from the filter. Before the filter the same
  comparison was `max=78`. The packed `YBR_FULL_422` decode path (no RGB
  conversion) is unchanged and byte-exact.
- The DICOM-level fo-dicom.Codecs matrix (`.interop/run_matrix.sh` driving
  `.interop/dicomref`) agrees in both directions: `.57` (16-bit) and `.70`
  (12 bits stored in 16) are sample-exact, and `.50` (8-bit) and `.51`
  (12 bits stored in 16) stay within one sample of fo-dicom's own decode of the
  same codestream.
- `smoothing_factor` parity with libjpeg is sample-exact: at `quality=100` a
  monochrome encode with `smoothing=50` decodes (DCMTK) within `max=1` of the
  unsmoothed stream — the same bound as `smoothing=0` — while the codestream
  shrinks (PM5644, 4:2:2, q90: −0.1 % / −1.0 % / −2.8 % for 1 / 25 / 50).
  Decoding a fo-dicom `SmoothingFactor=50` stream and comparing with fo-dicom's
  own decode gives `max=3` (64×48) and `max=4` (640×480), the same bound as
  without smoothing. `smoothing=0` output is byte-identical to the previous
  release (8 A/B probes across mono/4:4:4/4:2:2, 8/12 bit, with and without DRI).
- Encoder-written restart intervals are readable by independent toolkits: DCMTK
  3.7.1 decodes a `.70` stream with `restart_interval = 960` (one MCU row —
  libijg rejects intervals that are not an integer multiple of the MCU row) to
  the source samples with 0 differences, and a `.50` stream with an interval of 2
  to RGB within `max=4`, the same bound as the same stream without restarts.
  The `.70`/SV1 path forces predictor 1, so this evidence does not cover the
  `.57` predictors 2–7: that rule is pinned by the hand-computed predictor-4
  fixture in the `lossless.rs` unit tests, which DCMTK also decodes to the same
  samples.
  The lossless encoder enforces the same rule: a Process 14 interval that is not
  a multiple of the image width is refused with a codec error, and the
  integration test uses whole-MCU-row intervals only.
- 16-bit quantization tables: a 12-bit Process 2/4 encode at quality 10 writes
  DQT `Pq = 1` and re-decodes within tolerance, quality 90 keeps `Pq = 0` and a
  65-byte-per-table payload (the unchanged default path), and a stream whose
  `Pq = 0` tables are widened to `Pq = 1` decodes to the same samples.
- SV1 rejects a non-zero point transform and a predictor other than 1 on both
  encode and decode; a `.57` stream carrying `Pt != 0` that is relabelled `.70`
  is a managed error rather than silently lossy data.
- Sample comparisons come from the C# interop harness (`dicomref cmp`), which
  compares decoded frames sample by sample at 8 or 16 bits instead of byte by
  byte. The earlier byte metric reported a spurious 255-byte difference whenever
  a 16-bit sample crossed a 256 boundary; the sample metric reports the real
  deltas (14 and 13 against the source, 6 between the two decoders).
- The malformed Process 14 fixture from
  the fo-dicom.Codecs malformed JPEG unit test is
  covered by a Rust regression test that returns a managed error and leaves the
  destination without a partial frame.

## 10. Non-goals and upgrade points

The first release does not promise progressive JPEG, arithmetic coding,
hierarchical JPEG, CMYK/YCCK, or arbitrary component counts. These are separate
compatibility projects because they alter marker validation, color semantics, or
the DICOM photometric contract. Within DICOM two of them are not merely
deferred but excluded by the standard: PS3.5 A.4 defines JPEG transfer syntaxes
for Process 1, Process 2/4 and Process 14 only (progressive and hierarchical JPEG
are not among them), and Table 8.2.1-1 admits only `YBR_FULL_422` or `RGB` for
three-sample lossy JPEG. Multi-scan
sequential JPEG, by contrast, is inside the standard (`A.4` only requires the
JPEG interchange format) and is implemented, and the decoder also accepts the
non-DICOM 4:2:0/4:1:1 sampling layouts described in §4. Encoding remains limited
to the two DICOM sampling configurations, and progressive/arithmetic coding stays
out of scope: those require a different entropy decoder and coefficient
progression model, not a parameter.

A known decoder limit sits in the tail blocks: samples of a block that falls
outside the image are clamped onto the last real column/row instead of being
discarded. Reconstructing them differently would change the single-scan output
byte stream, so the clamp is kept; T.81 leaves the content of those padded
samples undefined, and both this encoder and libjpeg's replicate edges, so the
difference is not observable for streams written by either (one 960×540 fo-dicom
stream shows it only on its last row when a smoothing parameter changes that
row's content).

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
1,212 bytes (404 RGB samples) in exact 0/255 blocks. Rust and the reference
decoder reproduce the codestream byte-identically, so the difference is a
mismatch between the two acceptance fixtures; the cross-file check is recorded
as non-exact rather than treated as decoder proof or failure.

