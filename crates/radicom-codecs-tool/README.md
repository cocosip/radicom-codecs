# radicom-codecs-tool

Command-line DICOM image compression tool built on `radicom-codecs`. It is not
part of the `radicom-codecs` library crate; it exists to produce compressed
DICOM copies from one input file.

## What it does

The tool reads one DICOM file with pixel data and writes compressed DICOM copies
for the twelve transfer syntaxes implemented by `radicom-codecs`. By default it
writes all twelve; `--format` selects a single target.

## Command

Run from the workspace root:

```powershell
cargo run -p radicom-codecs-tool --bin radicom-compress -- <input-file> [--output-dir <directory>] [--format <format>]
```

Or run the built executable after `cargo build -p radicom-codecs-tool`:

```powershell
.\target\debug\radicom-compress.exe <input-file> [--output-dir <directory>] [--format <format>]
```

## Parameters

| Parameter | Required | Description |
| --- | --- | --- |
| `<input-file>` | Yes | Path to the DICOM file to compress. Only one input file is accepted per run. |
| `--output-dir <directory>` | No | Directory where compressed files are written. |
| `-o <directory>` | No | Short form of `--output-dir`. |
| `--format <format>` | No | Compress only the target with this suffix. Case-insensitive. |
| `--help`, `-h`, `/?` | No | Prints command usage. |

## Default Output Directory

When `--output-dir` is omitted, output files are written next to the input file
in a directory named:

```text
<input-file-name-without-extension>_compressed
```

Example:

```text
D:\dicom\study1.dcm
D:\dicom\study1_compressed\
```

## Examples

Compress one file to all formats using the default output directory:

```powershell
cargo run -p radicom-codecs-tool --bin radicom-compress -- "D:\dicom\study1.dcm"
```

Compress one file into an explicit output directory:

```powershell
cargo run -p radicom-codecs-tool --bin radicom-compress -- "D:\dicom\study1.dcm" --output-dir "D:\dicom\study1-compressed"
```

Compress only to JPEG-LS Lossless:

```powershell
cargo run -p radicom-codecs-tool --bin radicom-compress -- "D:\dicom\study1.dcm" -o "D:\dicom\out" --format jpegls_lossless
```

Print help:

```powershell
.\target\debug\radicom-compress.exe --help
```

## Output File Names

The tool writes one output file per target format using this pattern:

```text
<input-file-name-without-extension>_<format-suffix>.dcm
```

For input file `study1.dcm`, output names include:

| Format | Output suffix |
| --- | --- |
| RLE Lossless | `rle` |
| JPEG Baseline Process 1 | `jpeg_baseline` |
| JPEG Extended Process 2/4 | `jpeg_process2_4` |
| JPEG Lossless Process 14 | `jpeg_lossless_14` |
| JPEG Lossless Process 14 SV1 | `jpeg_lossless_sv1` |
| JPEG-LS Lossless | `jpegls_lossless` |
| JPEG-LS Near-Lossless | `jpegls_near_lossless` |
| JPEG 2000 Lossless | `j2k_lossless` |
| JPEG 2000 Lossy | `j2k_lossy` |
| HTJ2K Lossless | `htj2k_lossless` |
| HTJ2K Lossless RPCL | `htj2k_lossless_rpcl` |
| HTJ2K Lossy | `htj2k_lossy` |

## Target Transfer Syntaxes

| Format | Transfer Syntax UID |
| --- | --- |
| RLE Lossless | `1.2.840.10008.1.2.5` |
| JPEG Baseline Process 1 | `1.2.840.10008.1.2.4.50` |
| JPEG Extended Process 2/4 | `1.2.840.10008.1.2.4.51` |
| JPEG Lossless Process 14 | `1.2.840.10008.1.2.4.57` |
| JPEG Lossless Process 14 SV1 | `1.2.840.10008.1.2.4.70` |
| JPEG-LS Lossless | `1.2.840.10008.1.2.4.80` |
| JPEG-LS Near-Lossless | `1.2.840.10008.1.2.4.81` |
| JPEG 2000 Lossless | `1.2.840.10008.1.2.4.90` |
| JPEG 2000 Lossy | `1.2.840.10008.1.2.4.91` |
| HTJ2K Lossless | `1.2.840.10008.1.2.4.201` |
| HTJ2K Lossless RPCL | `1.2.840.10008.1.2.4.202` |
| HTJ2K Lossy | `1.2.840.10008.1.2.4.203` |

## Behavior Notes

- The input may be uncompressed or already compressed. A compressed source is
  decoded once per target format; a source already in the target format is
  saved as-is instead of being decoded and re-encoded, so lossy input is never
  compressed twice.
- Each target starts from the codec's default parameters, with two adjustments
  that align the tool with the native `fo-dicom.Codecs` reference: JPEG 2000
  uses the reversible or irreversible component transform, and colour sequential
  JPEG uses 4:2:2 chroma sampling. The reference tool instead leaves the codec
  default of 4:4:4, which costs roughly 15% more payload for the same quality
  target, so 4:2:2 is kept here. JPEG 2000 Lossy (`.91`) is coded at `rate = 16`
  and the encoder bisects a continuous quantization scale until the frame
  payload reaches that ratio, so the achieved size lands within 2% of the
  reference instead of overshooting it. `rate = 0` means "keep every bit-plane".
- The JPEG lossless targets (`.57`/`.70`) derive a per-image Huffman table from
  the actual prediction differences, matching libjpeg with
  `optimize_coding = TRUE` instead of the larger Annex K default table. The
  sequential targets (`.50`/`.51`) do the same for their DC and AC tables.
- Targets whose syntax cannot represent the image are classified before any file
  is written and reported as unsupported, mirroring
  `fo-dicom.PureCodecs.Tools/CompressionPlan.cs`: JPEG sequential DCT is limited
  to 8-bit monochrome/palette/colour images, plus 12-bit monochrome for Process
  2/4. Unsupported targets do not create output files. Every other shape failure
  still surfaces as a failed target and the remaining formats keep running.
- Lossy outputs are marked through `(0028,2110)`, `(0028,2112)` and
  `(0028,2114)` by the `radicom` transcoder.
- The SOP Class UID and SOP Instance UID are preserved from the source, like
  the `fo-dicom.PureCodecs.Tools` reference tool.

## Exit Codes

| Exit code | Meaning |
| --- | --- |
| `0` | The command ran and at least one target was not a failure (successes and unsupported targets both count). |
| `1` | The input was invalid, command-line arguments were invalid, or every target failed. |

The summary line reports all three outcomes, for example
`Summary: 10 succeeded, 2 unsupported, 0 failed.`
