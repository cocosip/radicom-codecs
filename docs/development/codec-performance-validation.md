# DICOM Codec Performance and Validation

## Order of Work

Proceed in this order. Complete performance, correctness, interoperability,
and regression checks for all transfer syntaxes in each family before moving
to the next family.

| Order | Family | Transfer syntaxes | fo-dicom.Codecs reference |
| --- | --- | --- | --- |
| 1 | RLE | RLE Lossless | Managed C# implementation in `DicomRleCodec.cs` |
| 2 | JPEG | Process 1, Process 2/4, Process 14, Process 14 SV1 | libjpeg-turbo |
| 3 | JPEG-LS | Lossless, Near-Lossless | CharLS |
| 4 | JPEG 2000 | Lossless, Lossy | OpenJPEG |
| 5 | HTJ2K | Lossless, Lossless RPCL, Lossy | OpenJPH |

Use the [standard validation workflow](#standard-validation-workflow) for RLE,
JPEG, JPEG-LS, and JPEG 2000. Use the [HTJ2K workflow](#htj2k-workflow) for its
three transfer syntaxes. Run JPEG 2000 and HTJ2K from extraction through
interoperability under these workflows to establish fresh results.

## JPEG 2000 AVX2

JPEG 2000 Lossless and Lossy enable AVX2 by default on supported x86 and
x86-64 CPUs. The default requires no environment variable or codec option.
The codec reads `RADICOM_J2K_AVX2` on its first JPEG 2000 SIMD dispatch and
caches the setting for the process. Set it to `0` before launching the process
to disable AVX2. The fallback can still use SSE2 wavelet lifting and SSE4.1
sample rounding and RGB packing. HTJ2K selects its kernels through runtime
CPU detection.

For SIMD comparisons, run the same Release binary in separate processes with
AVX2 enabled and disabled for the two JPEG 2000 transfer syntaxes. Use
identical inputs and codec settings and report both timings against the same
pinned OpenJPEG build. Apply the performance gate to each case and direction
for both Lossless and Lossy. With AVX2 enabled, Rust must take less time than
OpenJPEG (Rust/reference < `1.00`). With AVX2 disabled, Rust may take up to
5% longer (Rust/reference ≤ `1.05`). Benchmark HTJ2K with automatic dispatch.

## Shared Input and Timing Rules

- Extract Pixel Data once from the project's reference DICOM corpora and any
  additional DICOM test directories supplied for the run. Save one raw buffer
  per frame, including every frame in multi-frame files. Keep frames with
  different content even when their dimensions match. Decode an already
  compressed source once and record its source transfer syntax and the decoder
  revision.
- Record the sample ID, corpus-relative name, frame index, Rows, Columns,
  Samples per Pixel, Bits Allocated, Bits Stored, High Bit, Pixel
  Representation, Photometric Interpretation, Planar Configuration, byte
  order, pixel byte count, and SHA-256. Include any palette or other metadata
  required to reproduce the frame's layout.
- Apply byte-order conversion, plane reordering, and unused-bit handling once
  during extraction. Preserve the sample values, signedness, and color
  interpretation. Check the input length, SHA-256, and metadata before each
  implementation runs. Rebuild both baselines when the corpus or extraction
  rules change.
- List each frame, transfer syntax, and parameter configuration as a separate
  case. Cover small and large frames, bit depths, grayscale and color,
  signedness, and multi-frame data as supported. Record the effective codec
  options, including quality, subsampling, Huffman strategy, predictor, point
  transform, restart interval, NEAR, decomposition levels, code blocks, layers,
  and progression order where applicable. Give default and custom settings
  separate rows.
- Build both implementations in Release mode on the same machine. Record
  architecture, toolchains, source revisions, native-library build options,
  and active SIMD paths. Pin both to the same logical CPU and verify that each
  codec, including the native library, uses one worker.
- Time one implementation per process. Finish its entire series and wait for
  the last process to exit before starting the other implementation. Keep the
  case order, warm-up, and iteration counts equal. Run builds, tests, data
  preparation, and other benchmarks outside the timing window. Record process
  start and end times, the power mode, and the order in which each pair runs.
  Alternate which implementation runs first across the three round pairs.
- Time operations on preloaded buffers. Do DICOM parsing, disk I/O, process
  startup, hashing, and comparisons outside the timed interval. Align what
  counts as color conversion, packing, allocation, copying, and release.
  Document differences in buffer ownership or reuse when comparing existing
  entry points.
- Benchmark JPEG 2000 and HTJ2K against the native OpenJPEG and OpenJPH
  entry points from the pinned fo-dicom.Codecs revision. Exercise the public
  DICOM transcoders during interoperability checks.
- Start with three independent process rounds per implementation and at least
  ten measured operations per case per round after warm-up. Batch short
  operations equally on both sides. Save each round's elapsed time and
  operation count.

## Standard Validation Workflow

1. **Benchmark encoding.** Time Rust and the reference on the same raw
   Pixel Data and settings. Compare arithmetic mean time per operation across
   rounds. Weight round means by operation count when counts differ.
2. **Check the encoded bytes.** Outside the timer, compare the complete
   encoded frame payloads directly and record their lengths, SHA-256 values,
   and first differing byte, if any. Apply the same frame boundaries to both
   payloads. Inspect DICOM fragment headers and container alignment separately.
   Byte equality is required before saving one shared codestream for the
   decode benchmark. Save both artifacts when this check fails and resolve
   the difference before recording a formal decode result for that case.
3. **Check the decoded bytes.** Both implementations load the shared
   codestream and verify its length and SHA-256. Decode to the same bit depth,
   signedness, color space, planar layout, and byte order. Compare all bytes
   and check the output dimensions and length. For lossless syntaxes, also
   compare with the extracted source Pixel Data.
4. **Benchmark decoding.** Time both implementations on the same preloaded
   shared codestream. Divide each round's elapsed time by its operation count.
   Take the median of the resulting round times separately for Rust and the
   reference.
5. **Apply the performance gate.** For each case and direction, divide the
   Rust statistic by the corresponding reference statistic. Use the
   [JPEG 2000 AVX2 limits](#jpeg-2000-avx2) for its two benchmark runs. For
   the other codecs in this workflow a ratio of `1.05` or less passes. Follow the
   [retest procedure](#machine-variation-and-retests) if the result is unstable.
   After all applicable cases pass, run the [final checks](#retests-and-final-checks).

## HTJ2K Workflow

Older fo-dicom.Codecs HTJ2K output can contain substantial trailing data after
the end-of-codestream (EOC) marker, including nonzero bytes. Record the DICOM
file, encapsulated Pixel Data, and codestream sizes independently of codec
correctness and performance.

1. **Benchmark encoding with the shared timing rules.** Give Rust and OpenJPH
   the same raw frames and effective settings for each HTJ2K transfer syntax.
   Time their normal encoder outputs, including any work the implementation
   performs to produce its output. Compare the arithmetic mean using the
   same `1.05` limit.
2. **Inspect both outputs.** Preserve the original outputs. Record the file
   size when testing a DICOM file, the encapsulated frame size, the validated
   codestream length and SHA-256, and the location and length of any trailing
   bytes. Use DICOM frame boundaries and the HTJ2K tile-part lengths to locate
   EOC. Record fragment alignment and excess trailing bytes separately.
   Investigate malformed boundaries, unexpected trailing data, and
   incompatible codestream settings. Retain the complete DICOM output for
   interoperability checks.
3. **Freeze one input for decoding time.** Use the validated reference
   codestream payload as the shared input. Both decoders receive the same
   bytes and verified SHA-256. Keep DICOM parsing and padding handling
   outside the timer. Compare median decode time across independent rounds
   using the same `1.05` limit.
4. **Cross-decode the codestreams.** Feed each validated encoder output to
   both decoders. Require byte-identical Pixel Data from the two decoders
   for each codestream, with matching output metadata. Both lossless
   syntaxes must also restore the original pixels exactly. For the lossy
   syntax, record each encoder's reconstruction error against the source
   under the agreed settings. HTJ2K interoperability passes on valid structure,
   cross-decoding, and the applicable pixel checks. Encoded bytes may differ.
5. **Run final interoperability on the original DICOM outputs.** Exercise
   both directions with the padding and fragment layout that each encoder
   actually emits. Compare Pixel Data for each file and verify that both
   readers resolve the correct frame boundaries, including multi-frame
   inputs. Apply the regression checks below.

## Machine Variation and Retests

1. **Check the individual rounds.** Compare the absolute time per operation
   for both implementations and their ratio in each round pair. Investigate
   when ratios fall on both sides of the applicable limit, when the result changes with run
   order, or when the same reference build changes materially between runs.
   Check the recorded times against concurrent workloads, CPU frequency,
   thermal throttling, and power-mode changes when those observations are
   available.
2. **Replace a round pair when there is evidence of interference.** Keep both
   originals and record the affected case or time window, the observed event,
   and the replacement round. Once the machine is idle and stable, repeat the
   affected pair for both implementations with the same inputs and settings.
   Keep a slow round in the calculation if its only evidence of interference
   is the timing itself.
3. **Retest borderline cases on the same machine.** Use the same binaries,
   input hashes, logical CPU, and power mode. Run three fresh round pairs for
   the affected cases, with the first pair in reverse order and the remaining
   pairs alternating. Increase the operation count equally on both sides if
   the individual operations are short. If drift affects the entire series,
   repeat the full matrix in the same way.
4. **Report the original and retest results together.** Record absolute
   timings and ratios for each batch. Across valid rounds, use the
   operation-weighted mean for encoding and the median of per-operation
   times for decoding. If valid batches still disagree on whether a case meets
   its limit, report it as inconclusive and verify it in a stable measurement
   window before marking it as passed. A reproducible result that misses the
   limit remains a performance failure.

## Retests and Final Checks

- Report pass, fail, inconclusive, missing, and incompatible reference cases.
  Use aggregate performance figures to show trends. The acceptance gate
  applies to each frame, syntax, configuration, and direction.
- Optimize cases that remain above the tolerance. Rerun the family's full
  matrix and byte checks after changing codec behavior or performance.
- Test both public DICOM paths. Decode Rust-encoded files with fo-dicom and
  fo-dicom-encoded files with Rust. Check the transfer syntax, frame
  boundaries, Pixel Data, dimensions, bit depth, signedness, Photometric
  Interpretation, and Planar Configuration. Include existing streams and
  boundary parameters. Run formatting, Clippy, workspace tests, and rustdoc
  after the interoperability checks. Record which platforms were checked.
- Archive the extraction manifest, build and codec settings, input and
  codestream hashes, per-round data, summaries, byte differences, and
  interoperability results in the ignored `.interop/` directory. Exclude
  source DICOM files, extracted pixels, and patient metadata from shared
  replay packages.

## Notes from Earlier Tests

- Earlier RLE and JPEG-LS scripts alternated implementations in one process
  or timed each encoder's separate output. Classic JPEG's independent runner
  uses a shared codestream. Its earlier summaries used a mean for decoding,
  which can be recomputed as a median from saved per-round data. Verify
  the runner against this protocol before accepting its results.
- Very short operations on 3x3 and 32x32 images showed large single-round
  swings. Retain the round data and increase the batch size equally for
  both sides. Base product optimizations on codec rules and layouts that
  apply beyond a single test frame.
- Earlier classic JPEG runs used 4:4:4, optimized Huffman tables, and YCbCr
  output. The fo-dicom public defaults use 4:2:2 and RGB output for lossy
  color and standard Huffman tables for 8-bit sequential JPEG. Exact
  comparisons must align these settings. Matching libjpeg's integer inverse
  transform and edge handling changed some P1 and P2/4 output pixels
  compared with older Rust builds.
- For classic JPEG and JPEG-LS, the fo-dicom managed interface rejects some lossy PALETTE COLOR
  combinations. Rust supports them. Record index-plane comparisons
  separately from public-entry-point support and cross-decode the Rust
  codestream where the reference decoder accepts it.
- For JPEG 2000 and HTJ2K, classify inputs using the DICOM attribute tables.
  Monochrome permits signed samples. Color requires unsigned samples, and
  PALETTE COLOR requires a lossless-only syntax. Exercise signed RGB as an
  invalid encoder input. Compare signed 8-bit monochrome with the original
  signed samples, including negative values. Record any unsigned conversion
  by the reference bridge for that specific direction and build.
  [PS3.5 Table 8.2.4-1](https://dicom.nema.org/medical/dicom/current/output/chtml/part05/sect_8.2.4.html)
  and [Table 8.2.14-1](https://dicom.nema.org/medical/dicom/current/output/chtml/part05/sect_8.2.14.html).
- Check compressed YBR_RCT and YBR_ICT labels against the codestream MCT.
  After the inverse transform, native RGB samples require an RGB label.
  Report retained compressed labels separately from pixel differences.
  With MCT disabled, use the compressed Photometric Interpretation to
  distinguish RGB from YBR_FULL. For HTJ2K encoding timings also compare
  the per-component SIZ precision and quantization settings, especially
  for 12-bit samples stored in 16-bit words.
- For near-lossless JPEG-LS, check both byte equality on the shared
  codestream and the NEAR bound against the source. When signed samples
  cross the sign boundary, error measured on unsigned code values can
  differ from error measured on signed pixel values.

## Running the Checks

Build the reference libraries from a pinned fo-dicom.Codecs revision. Pass the
reference source directory and the paths of any additional DICOM test
directories to the local validation harness. The caller chooses the number,
names, and locations of the additional directories for each run. Record each
input path and its corpus identifier in the local run configuration. Prepare
the frame manifest and buffers described above and share them between both
implementations. External-reference harnesses and private corpora are maintained
locally and require setup on each machine.

The repository provides portable regression checks for every codec family.
Run them from the repository root.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

For whole-dataset transcode timing, see the
[Compression-speed benchmark](compression-benchmark.md).
