# HTJ2K codec design

## Standard and reference

- ISO/IEC 15444-15 / ITU-T T.814, High Throughput JPEG 2000.
- DICOM PS3.5, HTJ2K transfer syntaxes and encapsulated pixel data.
- Reference behavior: `fo-dicom.Codecs\Codec\DicomHtJpeg2000Codec.cs`.
- Pure managed reference: `fo-dicom.PureCodecs\docs\design\jpeg2000-codec-design.md`.

## Registrations

| Type | UID | Coding mode |
| --- | --- | --- |
| `HtJpeg2000LosslessCodec` | `1.2.840.10008.1.2.4.201` | lossless |
| `HtJpeg2000LosslessRpclCodec` | `1.2.840.10008.1.2.4.202` | lossless, RPCL |
| `HtJpeg2000LossyCodec` | `1.2.840.10008.1.2.4.203` | lossy |

The types implement `radicom::pixel::codec::Codec` and use `HtJ2kParams`.

The current `radicom` checkout exposes HTJ2K constants with `.108`, `.109`, and
`.110` UIDs. Those values must be corrected or explicitly superseded at the
`radicom` boundary before these registrations are enabled. Registering a correct
codec under an incorrect UID is not acceptable.

## Coding architecture

HTJ2K reuses JPEG 2000 geometry, marker parsing, tile handling, component layout,
and DICOM frame adaptation. It has independent high-throughput block coding:

- MEL run-length coding;
- VLC coding;
- MagSgn coding;
- cleanup and refinement passes;
- HTJ2K code-block segment assembly;
- lossless and irreversible quantization paths.

The implementation must not call OpenJPH or translate OpenJPH source. Standard
tables and bitstream rules are implemented as Rust data and code with tests tied
to published HTJ2K vectors and public reference behavior.

## Parameters and progression order

`HtJ2kParams` controls irreversible mode, decomposition levels, color transform,
progression order, and TLM marker policy. The RPCL transfer syntax always forces
RPCL regardless of a caller-provided progression value. Invalid decomposition,
precision, rate, or progression settings fail before encoding begins.

Three-component MCT handling follows the PureCodecs contract: supported YBR input
is normalized before encoding, and decoded MCT output is repacked with consistent
RGB/YBR metadata. Lossless paths require exact samples; lossy `.203` uses a fixed
fixture tolerance.

## Tests

- HT marker and CAP/MAGB parsing.
- MEL, VLC, MagSgn, cleanup, refinement, and code-block vectors.
- `.201` exact lossless round-trips.
- `.202` exact RPCL round-trips and forced-progression checks.
- `.203` fixed-tolerance lossy round-trips.
- 8/16-bit, signed, monochrome, RGB, MCT, multi-frame, and tile tests.
- Invalid codestream, bounds, precision, and parameter tests.
- Bidirectional public-API interoperability with `fo-dicom.Codecs`.

