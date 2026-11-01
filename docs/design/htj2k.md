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

`decompositions` selects the DWT level count (0..=32, default 5).
`employ_color_transform` applies RCT for reversible RGB and ICT for irreversible
RGB when there are exactly three components; the codec defaults enable it, while
`HtJ2kParams::default()` in the sibling crate leaves it false. `.202` forces
RPCL and validates it on decode; `.201` and `.203` honour the requested
progression order, with LRCP as the default. `.203` uses OpenJPH's default
scalar-expounded steps (`baseDelta = 1 / 2^precision`) when `quality` is 0 and
the fo-dicom quality table for `quality` 1..=100, which is pinned to the
reference `CreateQualityScalarExpoundedSteps` output. `insert_tlm_markers` emits
a one-entry TLM before SOT and is accepted and parsed on decode.

The reference encoder is the native
`Native/Common/OpenJPH/interface/ojph_interface.cpp`
(`HTJpeg2000EncodeStream`), whose hard-coded defaults are `decompositions_ = 5`,
64×64 code blocks, `quantizationStep_ = -1.0f` (derive the irreversible steps,
the same table `quality = 0` selects) and `request_tlm_marker_ = true`. It also
splits one tile-part per resolution, so its TLM carries several entries; a
single tile-part with a single TLM entry is equally valid and is what fo-dicom
accepts from this encoder.


HTJ2K quality layers are fixed at 1: the cleanup pass codes every bit plane, so
a second layer would need SigProp/MagRef refinement passes. OpenJPH's decoder
rejects such codestreams too (`0x00030053`, "supports 1 quality layer only") and
its encoder asserts `num_passes == 1`, so a codestream declaring more layers is
rejected with a managed error instead of being partially decoded.

Lossless `.201/.202` comparisons are sample-exact, while lossy `.203` tests use
a mean-absolute-error tolerance and require a decodable codestream.

## Stages and checks

1. Bounded HT marker/bit structures and capability-marker parsing are
   implemented; the cleanup pass is pinned to a reference byte vector.

2. The HT cleanup pass and exact `.201` round trips are implemented; blocks
   declaring refinement passes are rejected rather than mis-decoded, and a
   codestream asking for more than one quality layer is rejected at COD parse.
3. Forced RPCL packet iteration and `.202` checks, including TLM parsing, are
   implemented, and TLM emission is covered by a marker-layout test.
4. Irreversible quantization and `.203` tolerance tests are implemented,
   including OpenJPH-compatible 9/7 analysis and synthesis scaling and the
   explicit quality table.
5. Bidirectional fo-dicom fixtures pass for mono and RGB `.201/.202` (exact),
   `.202` with TLM (exact), and `.203` at the default and at `quality = 40`
   (within one to two samples). Malformed-stream and profile-matrix coverage
   remains a follow-up.

The public registry keeps one JPEG 2000 family entry point. The classic and
HTJ2K codec types remain inside the same crate. The sibling `radicom` boundary
is aligned with the generated dictionary and the DICOM `.201/.202/.203`
assignments.

