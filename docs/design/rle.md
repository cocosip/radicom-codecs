# RLE Lossless codec design

## Standard and reference

- DICOM PS3.5, Native or Encapsulated Pixel Data Encoding, RLE transfer syntax.
- DICOM PS3.5, RLE header and segment rules (the 64-byte header and PackBits
  segment encoding).
- Reference behavior: `fo-dicom.Codecs\Codec\DicomRleCodec.cs`.
- Pure managed reference: `fo-dicom.PureCodecs\docs\design\rle-codec-design.md`.

## Registration

The public type implements `radicom::pixel::codec::Codec` and returns
`radicom::model::transfer_syntax::syntaxes::RLE_LOSSLESS`:

```text
RLE Lossless
UID 1.2.840.10008.1.2.5
Crate radicom-codecs-rle
```

## Frame format

Each encoded frame contains a 64-byte little-endian header:

```text
uint32 segment_count
uint32 segment_offset[15]
encoded segment bytes
```

The segment count is `BytesAllocated * SamplesPerPixel` and is limited to 15.
Each segment represents one byte plane. Bytes within a multi-byte sample are
stored most-significant byte first, as required by the DICOM RLE transfer syntax.

Each segment uses PackBits-style controls:

- `0..127`: copy the next `control + 1` literal bytes;
- `-1..-127`: repeat the next byte `1 - control` times;
- `-128`: no-op, accepted by the decoder but never emitted by the encoder.

Segments are padded to even length where required by the DICOM frame rules. The
logical segment offsets are validated independently of the padding byte.

## Encoding

For every source frame:

1. Validate dimensions, frame length, `BitsAllocated`, samples per pixel, and
   planar configuration through `PixelDataRef`.
2. Compute the segment count and reject values above 15.
3. Write a placeholder header.
4. Traverse each sample byte plane in DICOM order, respecting interleaved or
   planar input, and emit literal/repeat runs.
5. Pad segment boundaries, rewrite the header, and call `dst.add_frame`.

The encoder must not modify the source Dataset. Encapsulation and final frame
padding are owned by `PixelData::add_frame`.

## Decoding

For every compressed frame:

1. Require at least 64 bytes.
2. Parse the segment count and offsets as little-endian `u32` values.
3. Require one to fifteen segments and require the expected segment count.
4. Require offsets to be inside the frame and monotonically increasing.
5. Decode each segment with output bounds checks.
6. Repack bytes into the target `PixelData` layout and append one frame.

Truncated literals, repeat runs that exceed the segment, and decoded output that
cannot fit the target frame are managed codec errors.

## Tests

- Header read/write and offset validation.
- Literal, repeat, mixed, and no-op PackBits controls.
- 8-bit and 16-bit monochrome exact round-trips.
- RGB interleaved and planar exact round-trips.
- Multi-frame integration through `radicom::pixel::transcoder::Transcoder`.
- Malformed segment count, offset, run, and frame-length errors.
- Bidirectional comparison with the public `fo-dicom.Codecs` API.

