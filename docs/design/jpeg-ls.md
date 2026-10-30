# JPEG-LS codec design

## Standard and reference

- ISO/IEC 14495-1 / ITU-T T.87, JPEG-LS lossless and near-lossless coding.
- DICOM PS3.5, JPEG-LS transfer syntaxes and encapsulated pixel data.
- Reference behavior: `fo-dicom.Codecs\Codec\DicomJpegLsCodec.cs`.
- Pure managed reference: `fo-dicom.PureCodecs\docs\design\jpegls-codec-design.md`.

## Registrations

| Type | UID | Parameters |
| --- | --- | --- |
| `JpegLsLosslessCodec` | `1.2.840.10008.1.2.4.80` | `near_lossless = 0` |
| `JpegLsNearLosslessCodec` | `1.2.840.10008.1.2.4.81` | `near_lossless >= 0` |

Both types implement `radicom::pixel::codec::Codec` and use
`radicom::pixel::codec::JpegLsParams`.

## Codestream components

The implementation follows the JPEG-LS model: SOI, SOF55, optional LSE preset
coding parameters, SOS, scan data, restart intervals, and EOI. The scan codec
contains regular mode prediction, run mode, context modeling, Golomb coding,
near-lossless reconstruction, and interleave handling.

The decoder supports the DICOM-relevant marker forms, validates all marker
lengths, and resets scan state at legal restart markers. APPn and COM segments
are skipped safely. Unsupported component counts, bit depths, and malformed
restart sequences return `Error::Codec`.

## Parameters and DICOM adaptation

`JpegLsParams` controls near-lossless error, interleave mode, color transformation,
preset coding parameters, and context reset behavior. The adapter validates that
`near_lossless` is zero for the lossless transfer syntax and within the sample
range for near-lossless encoding.

The implementation supports non-interleaved monochrome, sample-interleaved color,
and line-interleaved color where the DICOM layout permits it. Planar RGB is
normalized at the adapter boundary. Supported YBR input is normalized according
to the codec contract; unsupported planar subsampled layouts fail explicitly.

## Correctness rules

Lossless decoding must reproduce every sample exactly. Near-lossless decoding must
satisfy `abs(source - decoded) <= near_lossless` for every sample, including signed
and multi-byte samples after the DICOM pixel representation is applied.

## Tests

- Marker, preset parameter, Golomb, context, predictor, regular mode, and run mode tests.
- Lossless 8/16-bit monochrome and color exact round-trips.
- Near-lossless tolerance tests for 8/16-bit data.
- None, line, and sample interleave tests.
- Restart interval and malformed marker tests.
- APP8 color-transform compatibility tests where the reference API accepts them.
- Bidirectional public-API interoperability with `fo-dicom.Codecs`.

