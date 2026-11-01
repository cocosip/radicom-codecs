# HTJ2K codec design

This document is the implementation contract for the three HTJ2K DICOM
transfer syntaxes.

## Standards and reference

- ISO/IEC 15444-15 / ITU-T T.814, High Throughput JPEG 2000.
- DICOM PS3.3 and PS3.5, HTJ2K transfer syntaxes and encapsulated Pixel Data.
- `fo-dicom.Codecs DicomHtJpeg2000Codec.cs reference implementation`.
- OpenJPH/OpenJPEG output and fo-dicom acceptance tests may be used as external
  oracles only; their source is not copied, vendored, linked, or called.

## Registrations

| Type | UID | Coding mode | Status target |
| --- | --- | --- | --- |
| `HtJpeg2000LosslessCodec` | `1.2.840.10008.1.2.4.201` | HT blocks, lossless | encode/decode |
| `HtJpeg2000LosslessRpclCodec` | `1.2.840.10008.1.2.4.202` | HT blocks, lossless, RPCL | encode/decode |
| `HtJpeg2000LossyCodec` | `1.2.840.10008.1.2.4.203` | HT blocks, lossy | encode/decode |

The sibling `radicom` transfer-syntax table now resolves HTJ2K through its
generated UID dictionary. The dictionary, fo-dicom, and DICOM use `.201/.202/.203`;
`.108` and `.110` remain other transfer syntaxes. The codec implementation
performs a read-only assertion before registration.

All three types implement the existing `Codec` trait and accept only
`CodecParams::HtJpeg2000(HtJ2kParams)`. `.201/.202` force reversible 5/3,
`.203` forces irreversible 9/7, and `.202` forces RPCL regardless of a
caller-supplied progression value.

## DICOM and shared codestream contract

HTJ2K is a module in the JPEG 2000 crate. It reuses classic JPEG 2000 marker
parsing, SIZ geometry, tile/component layout, wavelet/quantization selection,
Tier-2 packet assembly, and the DICOM frame adapter. It accepts the same
8/16-bit mono/RGB, signedness, planar RGB, YBR_FULL/YBR_FULL_422 normalization,
checked dimensions, and encapsulation rules described in the shared design.
JP2/JPT wrappers, partial YBR, palette/CMYK, unknown photometric
interpretations, and unsupported Part 2 extensions fail with managed errors.

SIZ width/height, component count, precision, signedness, and decoded byte size
are checked against the destination before HT decoding. The check uses checked
arithmetic and the actual destination capacity, covering the overrun regression
in fo-dicom's `HtJpeg2000GeometryUnitTest`.

## HT block pipeline

The HT path writes the normal J2K geometry and packet markers, emits the Part 15
capability marker when required by the selected profile, and sets the HT
code-block style in COD/COC. Each code block is encoded or decoded independently:

1. cleanup-pass significance state and stripe traversal;
2. MEL run-length coding for zero/significance runs;
3. VLC coding for significance and magnitude metadata;
4. MagSgn coding for sign and magnitude bits;
5. legal HT termination and refinement segments;
6. Tier-2 inclusion/zero-bit-plane tag trees and progression packets.

Classic MQ and HT block coders share no state. The declared code-block style
selects the coder; the DICOM HTJ2K profiles require HT blocks and reject a
legacy block style when it is encountered. TLM markers are optional on encode
and accepted on decode when their bounded lengths and tile-part offsets are
consistent.

## Parameters and progression

`decompositions` is the number of DWT levels, `employ_color_transform` selects
RCT/ICT for RGB, and `insert_tlm_markers` controls encoder TLM emission. `.201`
and `.203` support all five progression values with LRCP as the default. `.202`
emits and validates RPCL and rejects a conflicting progression. Lossy quality
must be an explicit project parameter shared with the classic encoder; a fixed
`rate = 20` and fixed sample-error thresholds are not ISO or DICOM rules.
Lossless `.201/.202` comparisons are sample-exact, while lossy `.203` tests
must use the selected quality profile and require a decodable codestream.

## Stages and checks

1. Implement bounded HT marker/bit structures, capability-marker parsing, and
   MEL/VLC/MagSgn unit vectors.
2. Implement HT cleanup/refinement and exact `.201` round trips.
3. Add forced RPCL packet iteration and `.202` checks, including TLM parsing.
4. Add irreversible quantization and `.203` tolerance tests.
5. Run fo-dicom/OpenJPH bidirectional fixtures, malformed-stream and profile
   matrix tests, geometry-overrun regressions, and bounded fuzz/property checks.

The public registry keeps one JPEG 2000 family entry point. The classic and
HTJ2K codec types remain inside the same crate, while the parameter boundary may
be extended with an explicit lossy quality field. The sibling `radicom`
boundary is aligned with the generated dictionary and the DICOM
`.201/.202/.203` assignments.
