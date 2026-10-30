# Classic JPEG codec design

## Standard and reference

- ISO/IEC 10918-1 / ITU-T T.81, JPEG interchange syntax and coding processes.
- DICOM PS3.5, JPEG transfer syntaxes and encapsulated pixel data.
- Reference behavior: `fo-dicom.Codecs\Codec\DicomJpegCodec.cs`.
- Pure managed reference: `fo-dicom.PureCodecs\docs\design\jpeg-codec-design.md`.

## Registrations

The crate implements four `radicom::pixel::codec::Codec` types:

| Type | UID | Required path |
| --- | --- | --- |
| `JpegProcess1Codec` | `1.2.840.10008.1.2.4.50` | 8-bit sequential DCT, lossy |
| `JpegProcess2_4Codec` | `1.2.840.10008.1.2.4.51` | extended sequential DCT, 8/12-bit |
| `JpegLossless14Codec` | `1.2.840.10008.1.2.4.57` | lossless predictive JPEG |
| `JpegLossless14Sv1Codec` | `1.2.840.10008.1.2.4.70` | lossless predictor selection 1 |

The types use `radicom::pixel::codec::JpegParams` and reject a parameter variant
that does not match the selected transfer syntax.

## Shared codestream infrastructure

The crate is divided into marker I/O, entropy bit I/O, Huffman tables,
quantization tables, restart handling, color conversion, sequential DCT, lossless
predictors, and the DICOM frame adapter.

The decoder validates SOI, SOF0/SOF1/SOF3, DHT, DQT, DRI, SOS, restart markers,
and EOI. APPn and COM segments are skipped with length checks. Progressive,
arithmetic-coded, CMYK, and YCCK JPEG are rejected explicitly.

## Process 1 and Process 2/4

The sequential path performs level shift, 8x8 DCT, quantization, zig-zag scan,
Huffman entropy coding, byte stuffing, restart intervals, and inverse operations
on decode. Process 1 accepts 8-bit samples. Process 2/4 preserves the required
12-bit component precision and rejects unsupported sampling combinations.

`quality`, `sample_factor`, `smoothing_factor`, and color conversion are applied
through `JpegParams`. The adapter normalizes planar RGB and supported YBR input
before encoding and repacks decoded output to the target planar configuration.

## Process 14 and SV1

The lossless path uses the JPEG predictive selection values, point transform, and
Huffman-coded differences. Predictor state is reset at restart intervals. Process
14 accepts the configured predictor; SV1 enforces selection value 1. Samples are
reconstructed without DCT or quantization and must round-trip exactly.

## Tests

- Marker, Huffman, DQT, entropy bit, predictor, restart, and byte-stuffing tests.
- Process 1 8-bit lossy round-trip with fixed tolerance.
- Process 2/4 12-bit round-trip and invalid precision tests.
- Process 14 and SV1 8/12/16-bit exact round-trips.
- Planar/interleaved RGB and supported YBR adapter tests.
- Malformed marker, scan, restart, and entropy input tests.
- Bidirectional public-API interoperability with `fo-dicom.Codecs`.

