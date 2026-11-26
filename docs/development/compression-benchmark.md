# Compression-speed benchmark

`radicom-bench` reports how fast each transfer syntax encodes and decodes. It is
a development tool shipped in the internal `radicom-codecs-tool` crate; it is not
part of CI and never runs under `cargo test`.

```powershell
cargo run --release -p radicom-codecs-tool --bin radicom-bench
```

## What it measures

- **Encode**: one `Dataset::clone_with_syntax(target, params)` call from an
  uncompressed Explicit VR Little Endian dataset to the target syntax. This is
  the same path `radicom-compress` uses, so the number includes the dataset copy
  and the encapsulated Pixel Data assembly. File I/O, DICOM parsing and process
  startup are excluded.
- **Decode**: `clone_with_syntax` back to Explicit VR Little Endian of the
  encoded dataset, with the encode done once outside the timed loop.
- **MB/s**: raw pixel bytes divided by the median time (1 MB = 10^6 bytes).
- **Ratio**: encapsulated Pixel Data bytes divided by raw pixel bytes. Values
  below 1 mean the output is smaller than the uncompressed pixels.
- **Check**: lossless mono syntaxes must reproduce the source byte for byte
  (`exact`); lossy syntaxes print `lossy`; colour images print `-` because the
  decoded bytes are compared in a different colour space.

## Protocol

- One warm-up encode, excluded from the statistics, then up to five timed runs;
  the median is reported. A warm-up of 500 ms or more reduces the count to three,
  and 2 s or more to one run, so the 3328x4096 image does not turn a run into a
  coffee break.
- Single-threaded. Both the codecs and the driver run on one core.
- Release builds only. The binary refuses to run with `debug_assertions`, where
  the numbers are meaningless.
- Absolute numbers are machine-specific. The relative ordering between syntaxes
  is the useful part.

## Inputs

By default the binary generates five deterministic images with integer-only
math, so every machine and commit encodes the same bytes:

| Name | Rows x columns | Bits allocated/stored | Samples | Photometric |
|---|---|---|---|---|
| CT 512x512 16/16 | 512 x 512 | 16/16, signed | 1 | MONOCHROME2 |
| CR 2048x2048 16/12 | 2048 x 2048 | 16/12 | 1 | MONOCHROME2 |
| MG 3328x4096 16/12 | 3328 x 4096 | 16/12 | 1 | MONOCHROME2 |
| US 640x480 8/8 RGB | 480 x 640 | 8/8 | 3 | RGB |
| SC 1024x768 8/8 | 768 x 1024 | 8/8 | 1 | MONOCHROME2 |

Each image is a smooth bilinear field from a coarse random grid, integer noise
and three hard-edged rectangles, so the entropy is in the range of real medical
images rather than a checkerboard. It is still synthetic: use `--corpus` for
clinical realism.

- `--corpus <dir>` benchmarks uncompressed `.dcm` files instead. Compressed
  sources are rejected; measuring them would time decode plus encode.
- `--dump <dir>` writes the raw pixel frames so external implementations can
  encode the identical bytes.
- `--format <suffix>` limits the run to one target, exactly like
  `radicom-compress`.
- `--runs <n>` overrides the maximum timed runs (default 5).

No `.dcm` file is committed. The generator is the source of truth, and real
corpora stay in the git-ignored `.interop/bench/corpus/` directory.

## Reference run

Machine: Intel Core Ultra 9 185H (16 cores, 22 threads), Windows 11
10.0.26300, rustc 1.99.1, release build. Commit `216a565` plus the uncommitted
benchmark change, 2026-11-26. Raw output:
`.interop/bench/results/2026-11-26-216a565.txt` (git-ignored).

```
machine: windows x86_64, 22 logical cores, release
runs: up to 5 per case (median), warm-up excluded

image                    syntax                            enc ms    MB/s  ratio    dec ms    MB/s  check
------------------------------------------------------------------------------------------------------------
CT 512x512 16/16         RLE Lossless                         2.5   210.1   0.84       1.4   371.4  exact
CT 512x512 16/16         JPEG Baseline Process 1        unsupported: codec JPEG Baseline Process 1: sequential JPEG supports 8-bit Process 1 or 8/12-bit Process 2/4 samples
CT 512x512 16/16         JPEG Extended Process 2/4      unsupported: codec JPEG Extended Process 2/4: sequential JPEG supports 8-bit Process 1 or 8/12-bit Process 2/4 samples
CT 512x512 16/16         JPEG Lossless Process 14             9.4    56.1   0.71      11.7    45.0  exact
CT 512x512 16/16         JPEG Lossless Process 14 SV1         9.1    57.9   0.71      12.5    42.0  exact
CT 512x512 16/16         JPEG-LS Lossless                    43.1    12.2   0.64      42.2    12.4  exact
CT 512x512 16/16         JPEG-LS Near-Lossless               45.3    11.6   0.50      43.1    12.2  lossy
CT 512x512 16/16         JPEG 2000 Lossless                  56.7     9.2   0.64      60.3     8.7  exact
CT 512x512 16/16         JPEG 2000 Lossy                     62.7     8.4   0.06      11.3    46.5  lossy
CT 512x512 16/16         HTJ2K Lossless                       6.4    81.8   0.65       4.6   114.6  exact
CT 512x512 16/16         HTJ2K Lossless RPCL                  6.3    82.9   0.65       4.5   116.5  exact
CT 512x512 16/16         HTJ2K Lossy                          7.4    71.2   0.67       5.7    92.7  lossy
CR 2048x2048 16/12       RLE Lossless                        43.6   192.5   0.67      27.9   300.7  exact
CR 2048x2048 16/12       JPEG Baseline Process 1        unsupported: codec JPEG Baseline Process 1: sequential JPEG supports 8-bit Process 1 or 8/12-bit Process 2/4 samples
CR 2048x2048 16/12       JPEG Extended Process 2/4          149.3    56.2   0.28     611.2    13.7  lossy
CR 2048x2048 16/12       JPEG Lossless Process 14           149.4    56.2   0.50     178.2    47.1  exact
CR 2048x2048 16/12       JPEG Lossless Process 14 SV1       150.9    55.6   0.50     179.9    46.6  exact
CR 2048x2048 16/12       JPEG-LS Lossless                   751.6    11.2   0.42     692.4    12.1  exact
CR 2048x2048 16/12       JPEG-LS Near-Lossless              742.5    11.3   0.27     711.1    11.8  lossy
CR 2048x2048 16/12       JPEG 2000 Lossless                 662.8    12.7   0.40     761.5    11.0  exact
CR 2048x2048 16/12       JPEG 2000 Lossy                    811.4    10.3   0.06     244.4    34.3  lossy
CR 2048x2048 16/12       HTJ2K Lossless                     171.2    49.0   0.42     142.8    58.7  exact
CR 2048x2048 16/12       HTJ2K Lossless RPCL                179.2    46.8   0.42     109.7    76.4  exact
CR 2048x2048 16/12       HTJ2K Lossy                        157.8    53.2   0.43     126.6    66.3  lossy
MG 3328x4096 16/12       RLE Lossless                       151.2   180.3   0.67      98.1   277.9  exact
MG 3328x4096 16/12       JPEG Baseline Process 1        unsupported: codec JPEG Baseline Process 1: sequential JPEG supports 8-bit Process 1 or 8/12-bit Process 2/4 samples
MG 3328x4096 16/12       JPEG Extended Process 2/4          507.0    53.8   0.28    1962.6    13.9  lossy
MG 3328x4096 16/12       JPEG Lossless Process 14           483.3    56.4   0.50     567.7    48.0  exact
MG 3328x4096 16/12       JPEG Lossless Process 14 SV1       449.0    60.7   0.50     528.9    51.5  exact
MG 3328x4096 16/12       JPEG-LS Lossless                  2199.7    12.4   0.42    2381.5    11.4  exact
MG 3328x4096 16/12       JPEG-LS Near-Lossless             2363.7    11.5   0.27    2155.1    12.7  lossy
MG 3328x4096 16/12       JPEG 2000 Lossless                2435.7    11.2   0.40    2308.1    11.8  exact
MG 3328x4096 16/12       JPEG 2000 Lossy                   2715.4    10.0   0.06     668.0    40.8  lossy
MG 3328x4096 16/12       HTJ2K Lossless                     718.3    38.0   0.42     489.4    55.7  exact
MG 3328x4096 16/12       HTJ2K Lossless RPCL                846.9    32.2   0.42     502.8    54.2  exact
MG 3328x4096 16/12       HTJ2K Lossy                        949.4    28.7   0.43     618.8    44.1  lossy
US 640x480 8/8 RGB       RLE Lossless                         4.9   187.0   0.84       3.0   304.1  -
US 640x480 8/8 RGB       JPEG Baseline Process 1             19.5    47.4   0.06     102.8     9.0  -
US 640x480 8/8 RGB       JPEG Extended Process 2/4           19.9    46.2   0.06     114.8     8.0  -
US 640x480 8/8 RGB       JPEG Lossless Process 14            27.4    33.6   0.54      32.0    28.8  -
US 640x480 8/8 RGB       JPEG Lossless Process 14 SV1        25.5    36.1   0.54      29.6    31.2  -
US 640x480 8/8 RGB       JPEG-LS Lossless                    93.9     9.8   0.39      80.3    11.5  -
US 640x480 8/8 RGB       JPEG-LS Near-Lossless               84.9    10.9   0.16      70.1    13.2  -
US 640x480 8/8 RGB       JPEG 2000 Lossless                  79.0    11.7   0.25      63.0    14.6  -
US 640x480 8/8 RGB       JPEG 2000 Lossy                     59.3    15.5   0.06      32.9    28.0  -
US 640x480 8/8 RGB       HTJ2K Lossless                      22.5    41.0   0.28      17.2    53.6  -
US 640x480 8/8 RGB       HTJ2K Lossless RPCL                 21.3    43.2   0.28      16.0    57.5  -
US 640x480 8/8 RGB       HTJ2K Lossy                         19.7    46.7   0.13      20.7    44.6  -
SC 1024x768 8/8          RLE Lossless                         3.3   236.7   0.84       1.9   418.6  exact
SC 1024x768 8/8          JPEG Baseline Process 1             17.2    45.6   0.14      84.6     9.3  lossy
SC 1024x768 8/8          JPEG Extended Process 2/4           17.0    46.3   0.14      84.2     9.3  lossy
SC 1024x768 8/8          JPEG Lossless Process 14            25.1    31.4   0.57      27.8    28.2  exact
SC 1024x768 8/8          JPEG Lossless Process 14 SV1        24.7    31.8   0.57      26.2    30.0  exact
SC 1024x768 8/8          JPEG-LS Lossless                   120.1     6.5   0.42     124.6     6.3  exact
SC 1024x768 8/8          JPEG-LS Near-Lossless              112.9     7.0   0.18     104.8     7.5  lossy
SC 1024x768 8/8          JPEG 2000 Lossless                  77.8    10.1   0.38      73.7    10.7  exact
SC 1024x768 8/8          JPEG 2000 Lossy                     79.2     9.9   0.06      27.1    29.1  lossy
SC 1024x768 8/8          HTJ2K Lossless                      19.1    41.1   0.41      15.5    50.6  exact
SC 1024x768 8/8          HTJ2K Lossless RPCL                 19.1    41.2   0.41      14.8    53.2  exact
SC 1024x768 8/8          HTJ2K Lossy                         22.6    34.9   0.39      18.6    42.3  lossy
```

Reading the table:

- **RLE is the fastest encoder** (180-237 MB/s) but it is not a compression win on
  noisy data: the ratio stays at 0.67-0.84, i.e. the output is only 16-33% smaller
  than raw, and it can grow on noisier content. It is a fallback, not a space saver.
- **HTJ2K is roughly 3-9x faster than classic JPEG 2000** at a comparable ratio
  (81.8 vs 9.2 MB/s on the 512x512 CT at 0.65/0.64, 38.0 vs 11.2 MB/s on the
  3328x4096 image at 0.42/0.40).
- **JPEG-LS and classic JPEG 2000** encode 16-bit data at about 9-13 MB/s.
- **JPEG lossless P14/SV1** encodes at ~56-60 MB/s on 16-bit and is the fastest
  lossless compressor in the table, at a higher ratio than JPEG-LS or J2K.
- **Lossy J2K (rate 16)** decodes far faster than it encodes (11.3 vs 62.7 ms on
  the CT image) because the decoder stops at the requested quality.
- Sequential JPEG only accepts 8-bit Process 1 or 8/12-bit Process 2/4, so the
  16-bit rows report `unsupported`; that is the support matrix, not a failure.

## Comparing with other implementations

Give every implementation the same input and the same boundaries:

1. `--dump <dir>` the raw frames once, or use your own real `.dcm` corpus.
2. Feed those bytes to the reference tool's dataset-to-codestream path (for
   fo-dicom, `TranscodeDataset`; for OpenJPEG, `opj_compress` on a PGM/PPM built
   from the same frame).
3. Measure encode only, not process startup, file parsing or I/O. If a reference
   tool only offers a CLI, time it end to end and subtract a baseline run on an
   empty input of the same size, or compare throughput per pixel rather than
   wall-clock milliseconds.

The `.interop` interop matrices already drive the reference implementations for
correctness; reuse their harnesses when speed and output must be compared on the
same codestream.

## Updating these numbers

Rerun before a release, save the console output under
`.interop/bench/results/<date>-<commit>.txt`, and update the table above and the
summary in the root README. Do not add timing assertions to CI: shared runners
and debug test builds make those either flaky or meaningless.
