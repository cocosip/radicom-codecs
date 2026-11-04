# DICOM RLE Lossless codec: technical implementation

## 1. Scope

This document is the implementation contract for Stage 1 of `radicom-codecs`.
It covers the DICOM RLE Lossless transfer syntax only:

```text
UID:  1.2.840.10008.1.2.5
Core: radicom::syntaxes::RLE_LOSSLESS
Crate: radicom-codecs-rle
Type: radicom_codecs_rle::RleLosslessCodec
```

The implementation must provide both encode and decode through
`radicom::pixel::codec::Codec`. It must not define a replacement codec trait,
codec parameter type, pixel-data model, or DICOM encapsulation layer.

The implementation is based on:

- DICOM PS3.5, the RLE Lossless transfer syntax and encapsulated Pixel Data rules;
- DICOM PS3.5, the RLE header and segment format;
- `fo-dicom.Codecs DicomRleCodec.cs reference implementation`;
- the PureCodecs RLE implementation;
- the PureCodecs RLE tests.

The Rust implementation must remain pure Rust. No native library, C/C++ source,
P/Invoke, or external codec process is permitted.

## 2. Radicom integration contract

The codec implements the frozen core trait:

```rust
impl radicom::pixel::codec::Codec for RleLosslessCodec {
    fn name(&self) -> &str;
    fn transfer_syntax(&self) -> &'static radicom::TransferSyntax;
    fn default_params(&self) -> radicom::pixel::codec::CodecParams;
    fn encode(
        &self,
        src: &radicom::pixel::PixelDataRef<'_>,
        dst: &mut radicom::pixel::PixelData<'_>,
        params: &radicom::pixel::codec::CodecParams,
    ) -> Result<(), radicom::error::Error>;
    fn decode(
        &self,
        src: &radicom::pixel::PixelDataRef<'_>,
        dst: &mut radicom::pixel::PixelData<'_>,
        params: &radicom::pixel::codec::CodecParams,
    ) -> Result<(), radicom::error::Error>;
}
```

The required behavior is:

- `name()` returns `"RLE Lossless"`;
- `transfer_syntax()` returns `&radicom::syntaxes::RLE_LOSSLESS`;
- `default_params()` returns `CodecParams::None`;
- any non-`None` parameter variant is rejected with `Error::Codec`;
- every failure is returned as `CodecError::Operation` with the codec name,
  operation, frame index, and a specific reason;
- no malformed frame or metadata input may panic.

The facade registers one `Arc<RleLosslessCodec>` under the RLE transfer syntax.
The codec processes all frames in `src` and appends the decoded or encoded frames
to `dst`. The core `PixelData` implementation owns fragment sequences, Basic
Offset Table handling, and DICOM even-length padding.

## 3. Supported pixel layouts

The first implementation supports the layouts already covered by PureCodecs:

| `BitsAllocated` | Samples | Planar configuration | Required result |
| ---: | ---: | --- | --- |
| 1 | 1 | absent/0 | exact round-trip (MONOCHROME1/2 only) |
| 8 | 1 | absent/0 | exact round-trip |
| 16 | 1 | absent/0 | exact round-trip |
| 8 | 3 | 0 interleaved | exact round-trip |
| 8 | 3 | 1 planar | exact round-trip |

Multi-frame input is supported for every accepted layout. `BitsAllocated` must
be 1, 8, or 16, and the segment count must be between 1 and 15.
`BitsAllocated` 1 is the BINARY (Segmentation IOD) form allowed by PS3.5
Table 8.2.2-1 Note 5. The native frame packs pixels continuously least
significant bit first, with no row-byte alignment, so its length is
`ceil(rows * columns / 8)`. It requires `BitsStored` 1, `HighBit` 0,
`SamplesPerPixel` 1, and `PhotometricInterpretation` MONOCHROME1 or
MONOCHROME2; `PixelRepresentation` 0 or 1 is accepted because the bit pattern
is used as 0/1. On the wire each 1-bit segment holds one byte per pixel
(0x00 or 0x01), so `segment_count` stays 1. The DICOM adapter in the current
`radicom` checkout rejects encapsulated output above 16 bits, so 32 and 64
bits are rejected with a managed error.

The codec must validate:

- `rows > 0` and `columns > 0`;
- `samples_per_pixel >= 1`;
- `planar_configuration` is 0 or 1 when samples are greater than one;
- `pixel_representation` is 0 or 1;
- the source frame contains at least the exact uncompressed frame size;
- `bytes_allocated * samples_per_pixel <= 15`;
- for `BitsAllocated` 1: `BitsStored` 1, `HighBit` 0, one sample, and
  MONOCHROME1/MONOCHROME2;
- checked arithmetic succeeds for every size, offset, and allocation.

For one sample, planar configuration has no effect. For multiple samples, value
0 is interleaved and value 1 is planar. Unsupported photometric interpretations
are not rejected by the RLE bitstream itself; the adapter preserves the DICOM
metadata and leaves photometric conversion to the upper pixel pipeline.

## 4. DICOM RLE frame format

Each compressed frame is a byte stream containing a 64-byte header followed by
one to fifteen encoded segments.

### 4.1 Header

The header is little-endian:

```text
offset 0   uint32 segment_count
offset 4   uint32 segment_offset[0]
offset 8   uint32 segment_offset[1]
...
offset 60  uint32 segment_offset[14]
```

Only the first `segment_count` offsets are meaningful. The remaining header bytes
must be zero in encoder output. The encoder always writes the first segment at
offset 64. The decoder accepts offsets at or after byte 64, requires every used
offset to be within the frame, and requires strict monotonic increase. It may
ignore reserved bytes between the header and the first segment for compatibility,
but it must never read before the first offset.

The segment count must equal:

```text
segment_count = bytes_allocated * samples_per_pixel
```

For an 8-bit RGB frame this is three. For a 16-bit RGB frame this is six.

### 4.2 Segment boundaries

Segment `i` starts at `segment_offset[i]` and ends at the next used offset, or
at the compressed frame length for the final segment. A segment length of zero is
invalid. The bytes after the final logical segment may contain one DICOM padding
byte; the decoder must handle that padding without adding a decoded sample.

Every encoded segment is padded to an even byte length. The encoder inserts one
zero byte after an odd-length segment. Segment offsets include that padding.

A frame that is itself odd length is non-conforming: the core/reader applies
DICOM even-length padding before this codec sees the frame. The RLE layer does
not try to distinguish that frame-level padding from a logical payload byte,
because the original length is no longer available here; handling it is the
core/file layer's responsibility.

## 5. PackBits segment coding

The segment stream is a sequence of signed 8-bit control values.

### 5.1 Literal run

For a control value `n` in `0..=127`, copy the following `n + 1` bytes literally.
The maximum literal run is 128 bytes.

```text
control = literal_count - 1
payload = literal_count bytes
```

### 5.2 Repeat run

For a signed control value `n` in `-1..=-127`, copy the following byte `1 - n`
times. The maximum repeat run is 128 bytes.

```text
control = 1 - repeat_count
payload = one repeated byte
```

For example, three copies of `0x07` are encoded as `0xFE 0x07`.

### 5.3 No-op

`-128` (`0x80`) is a no-op. The decoder accepts it without producing output. The
encoder never emits it.

### 5.4 Encoder packetization

The encoder uses the same deterministic packetization as
`fo-dicom.Codecs`/PureCodecs, including its `Flush` behavior at the end of a
segment: a run that is still open when the segment ends is a repeat run even
when it is only two bytes. That keeps the encoded bytes identical to the
reference, including for images whose last sample run is a pair:

1. Delay a repeat run until the next byte is known.
2. A run of one or two equal bytes followed by a different byte remains in the
   literal buffer.
3. A run of three or more equal bytes flushes pending literals, then emits repeat
   packets of at most 128 bytes.
4. Literal packets are emitted in chunks of at most 128 bytes.
5. A literal buffer may temporarily hold at most 132 bytes while a run boundary
   is being decided; it must never grow without a bound.
6. At segment end, pending literals are emitted first, then an open run of one
   or more bytes is emitted: a one-byte run as a literal, a two-byte run as
   `0xFF xx` (`Flush`'s repeat form), and longer runs as repeat packets. The
   frame then pads the segment to an even length with `0x00`.

The compressed bytes do not define correctness by themselves, but deterministic
packetization is required for the reference packetization regression test and
for byte-identical output.

## 6. Pixel-plane mapping

Let:

```text
pixel_count    = rows * columns
bytes_allocated = bits_allocated / 8
segment_index  = sample_index * bytes_allocated + byte_plane_index
```

RLE segment order is sample-major, with the most significant byte plane first:

```text
sample_index    = segment_index / bytes_allocated
plane_in_sample = segment_index % bytes_allocated
source_byte     = bytes_allocated - 1 - plane_in_sample
```

For each pixel index `p`, the raw frame offset is:

```text
if planar_configuration == 1 && samples_per_pixel > 1:
    sample_index * pixel_count * bytes_allocated
        + p * bytes_allocated
        + source_byte
else:
    p * samples_per_pixel * bytes_allocated
        + sample_index * bytes_allocated
        + source_byte
```

Encoding reads those offsets in increasing pixel order to form one segment of
`pixel_count` bytes. Decoding writes each decoded segment back to the same offsets.
This mapping preserves the native little-endian DICOM sample representation while
honoring the RLE requirement that the high byte plane is segment zero.

For `BitsAllocated` 1 the plane mapping does not apply: the native frame is
LSB-first packed with no row alignment, and the single segment is formed by
expanding each bit to a 0x00/0x01 byte. Decoding packs those bytes back and
rejects any value above 1 as malformed input.

## 7. Rust module structure

The implementation should stay small and split by responsibility:

```text
crates/radicom-codecs-rle/src/
├── lib.rs       # public Codec type and frame-loop error context
├── layout.rs    # checked pixel metadata and raw offset mapping
├── header.rs    # 64-byte header parse/write and offset validation
├── segment.rs   # PackBits encode/decode for one segment
└── frame.rs     # segment extraction, frame assembly, and padding handling
```

Only `RleLosslessCodec` is public initially. `layout`, `header`, `segment`, and
`frame` types are `pub(crate)` or private. The family crate has no dependencies
beyond the `radicom` crate for this stage.

Recommended internal contracts:

```text
RleLayout::from_pixel_data(&PixelDataRef) -> Result<RleLayout, Error>
RleHeader::parse(frame: &[u8]) -> Result<RleHeader, Error>
RleHeader::write(segment_offsets: &[u32]) -> Result<[u8; 64], Error>
encode_segment(samples: &[u8]) -> Result<Vec<u8>, Error>
decode_segment(encoded: &[u8], expected_len: usize) -> Result<Vec<u8>, Error>
encode_frame(layout: &RleLayout, raw: &[u8]) -> Result<Vec<u8>, Error>
decode_frame(layout: &RleLayout, rle: &[u8]) -> Result<Vec<u8>, Error>
```

The exact Rust error helper may vary, but all failures must become
`Error::Codec(CodecError::Operation { codec: "RLE Lossless", ... })` at the
public boundary.

## 8. Encode algorithm

`RleLosslessCodec::encode` performs these steps:

1. Require `CodecParams::None`.
2. Build and validate `RleLayout` from `src` metadata.
3. Preflight every source frame and copy its bytes into a temporary encoded-frame
   vector. This prevents appending a partial successful result to `dst` if a later
   frame fails.
4. For each frame, require `raw.len() >= layout.frame_length`; use only the exact
   frame bytes described by the layout.
5. Extract each segment using the pixel-plane mapping.
6. Encode each segment with PackBits packetization.
7. Add one zero byte after every odd-length segment and record the padded offset.
8. Write the header and append all segments.
9. After every frame succeeds, append the encoded frames to `dst` with
   `dst.add_frame` so the core owns encapsulated-fragment padding, then set the
   encapsulated Pixel Data VR to OB as required by PS3.5 8.2.2 (the core
   initially chooses OW from `BitsAllocated`).

The encoder must not update source metadata. The destination transfer syntax is
already selected by the caller; if it is not the RLE syntax, return a managed
metadata error instead of silently writing an RLE frame into another syntax.
An encapsulated source is rejected as well: the encoder only accepts native
samples, so the caller must decode a compressed source first.

## 9. Decode algorithm

`RleLosslessCodec::decode` performs these steps:

1. Require `CodecParams::None`.
2. Build and validate `RleLayout` from the destination metadata.
3. Preflight and decode every source frame into temporary raw buffers.
4. Parse the 64-byte header and validate the count and every used offset.
5. For each segment, determine its byte range from adjacent offsets.
6. Decode the full range and require exactly `pixel_count` output bytes.
7. If the full range fails and its final byte is zero, retry once without that
   final byte to handle the required even-length segment padding.
8. Copy the decoded segment into the target raw frame using the pixel-plane mapping.
9. Append all decoded frames to `dst` only after every source frame succeeds.

The output frame is exactly `rows * columns * samples_per_pixel * bytes_allocated`
bytes, except for `BitsAllocated` 1 where it is
`ceil(rows * columns * samples_per_pixel / 8)`. `dst.add_frame` handles any final
DICOM padding and frame storage details.

Before allocating a decoded frame or segment, the decoder rejects a declared
size above `1 << 30` bytes or above 128 times the compressed payload size. One
PackBits input byte cannot expand beyond that bound, so every legal frame passes
while a tiny frame cannot request a multi-GB allocation.

## 10. Error conditions

The decoder must reject:

- frame length below 64 bytes;
- segment count zero or greater than 15;
- segment count different from the expected layout count;
- a used offset below 64 or at/after the frame length;
- non-increasing offsets;
- an empty segment;
- literal control without enough payload bytes;
- repeat control without its repeated byte;
- a run that produces more than `pixel_count` bytes;
- a segment that produces fewer than `pixel_count` bytes;
- a declared decoded frame or segment size above the 1 GiB limit or the 128x
  expansion bound of the compressed input;
- arithmetic overflow while calculating frame or output sizes;
- a destination layout incompatible with the source frame.

The encoder must reject:

- non-`None` parameters;
- zero dimensions;
- unsupported `BitsAllocated`;
- segment counts above 15;
- source frames shorter than the required raw frame size;
- invalid planar configuration;
- arithmetic overflow in offsets, output sizes, or allocations.

Every error includes `RLE Lossless`, `encode` or `decode`, and the frame index at
the public boundary. Primitive tests may use a more specific internal error type,
but the public method always returns `radicom::error::Error`.

## 11. Test plan

### 11.1 Header tests

- write a one-segment header and verify all 64 bytes;
- write multiple offsets in little-endian order;
- parse a valid 64-byte header;
- reject frames shorter than 64 bytes;
- reject segment counts 0 and 16;
- reject offsets below 64, outside the frame, and non-increasing;
- reject an empty segment range.

### 11.2 Segment tests

- literal run of one byte;
- literal run of 128 bytes;
- literal run packetization above 128 bytes;
- repeat runs of 3, 128, and 129 bytes;
- mixed literal and repeat input;
- delayed one- and two-byte repeats encoded as literals;
- no-op decode;
- truncated literal and repeat input;
- output underflow and overflow;
- exact packetization for the 133-byte PureCodecs regression vector.

### 11.3 Frame mapping tests

- 8-bit monochrome, one row and multiple rows;
- 16-bit monochrome with distinct high and low byte planes;
- 8-bit RGB interleaved;
- 8-bit RGB planar;
- multi-frame monochrome;
- odd-length segments and a padded final segment;
- a frame with all 15 legal segments;
- rejection of 16 segments and unsupported bit allocations.

### 11.4 Radicom integration tests

- `RleLosslessCodec` returns the RLE transfer syntax and `CodecParams::None`;
- registry lookup returns the RLE codec;
- `can_transcode` is true in both directions with explicit VR little endian;
- `Transcoder` converts raw → RLE → raw with exact frame equality;
- frame count and required pixel metadata are preserved;
- a malformed RLE frame returns `Error::Codec` and never panics;
- a later failing frame does not append a partial successful result.

### 11.5 Reference compatibility tests

Once the local RLE implementation passes its own tests:

- decode RLE frames produced by the public `fo-dicom.Codecs` API;
- encode the PureCodecs random 256x256 sample and compare decoded pixels;
- compare deterministic packetization with the public reference where the input
  is known to produce stable bytes;
- run the low-entropy signed 16-bit regression from `RLEissue.cs` for widths 1..1023.

Lossless acceptance is exact byte equality after decode. Compressed byte equality
is an additional diagnostic, not a substitute for decoded-pixel correctness.

#### 11.5.1 `BitsAllocated` 1 has no third-party reader

PS3.5 Annex G.2 defines the 1-bit case as a *Padded Composite Pixel Code*: the
1-bit code is padded with zero most-significant bits to one byte, so the frame is
a single byte segment holding one byte per pixel. No deployed decoder implements
that case, which was verified rather than assumed:

| Tool | Result |
| --- | --- |
| fo-dicom (CharLS/its own RLE) | rejects `BitsAllocated` 1 in both directions |
| DCMTK 3.7.1 `dcmcrle` | `F: No conversion to transfer syntax RLE Lossless possible!` |
| DCMTK 3.7.1 `dcmdrle` | `E: The RLE decoder only supports images where BitsAllocated is a multiple of 8.` |

The oracle is therefore an independent decoder written from Annex G.2/G.3 inside
the C# interop harness (`dicomref rle-dec`, ~40 lines: 64-byte header, PackBits
segments, byte-plane reassembly, bit repacking). It decodes the encoder's
`BitsAllocated` 1 output to the source samples with 0 differences over all 384
bytes. DCMTK 3.7.1 does read and write 8-bit and 16-bit RLE in both directions
with byte-exact results, and its compressed bytes differ from ours only in how
literal and repeat runs are split.

## 12. Code review checklist

Before committing the RLE implementation, review the complete diff against this
document and verify:

- all frame sizes and offsets use checked arithmetic;
- no `unwrap`, `expect`, indexing, or slice conversion can panic on external input;
- segment padding is handled separately from logical samples;
- high-byte-first segment order is correct for 16-bit samples;
- planar and interleaved formulas are tested independently;
- `CodecParams` variants are validated;
- errors contain codec, operation, and frame context;
- source metadata is not mutated;
- destination frames are appended only after successful preflight;
- no native dependency or CI configuration was introduced;
- `cargo fmt`, `cargo test`, `cargo clippy`, and `git diff --check` pass.

