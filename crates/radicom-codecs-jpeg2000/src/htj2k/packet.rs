//! HTJ2K packet headers (ISO/IEC 15444-15 Annex B).
//!
//! HTJ2K replaces the classic inclusion and zero-bit-plane tag trees with
//! "missing MSB" trees and a cleanup-segment length code. The packet body is
//! still a concatenation of code-block segments.

use crate::bit::{BitReader, BitWriter};
use crate::geometry::{CodeBlock, PrecinctBand};

/// Decoded code-block contribution from one HT packet.
pub(crate) struct DecodedBlock {
    pub(crate) index: usize,
    pub(crate) data: Vec<u8>,
    pub(crate) zero_bitplanes: u8,
}

#[derive(Clone)]
struct HtTree {
    widths: Vec<usize>,
    heights: Vec<usize>,
    values: Vec<Vec<i32>>,
    levels: usize,
    padding: i32,
}

impl HtTree {
    fn new(width: usize, height: usize, initial: i32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let levels = 1 + log2_ceil(width).max(log2_ceil(height));
        let mut widths = vec![0usize; levels + 1];
        let mut heights = vec![0usize; levels + 1];
        let mut values = vec![Vec::new(); levels + 1];
        for level in 0..=levels {
            let w = ((width + (1 << level) - 1) >> level).max(1);
            let h = ((height + (1 << level) - 1) >> level).max(1);
            widths[level] = w;
            heights[level] = h;
            values[level] = vec![initial; w * h];
        }
        values[levels][0] = 0;
        Self {
            widths,
            heights,
            values,
            levels,
            padding: initial,
        }
    }

    fn get(&self, x: usize, y: usize, level: usize) -> i32 {
        if level >= self.values.len() {
            return 0;
        }
        let x = x.min(self.widths[level] - 1);
        let y = y.min(self.heights[level] - 1);
        self.values[level][y * self.widths[level] + x]
    }

    fn set(&mut self, x: usize, y: usize, level: usize, value: i32) {
        if level >= self.values.len() {
            return;
        }
        if x >= self.widths[level] || y >= self.heights[level] {
            return;
        }
        self.values[level][y * self.widths[level] + x] = value;
    }

    fn from_inclusion(band: &PrecinctBand) -> Self {
        let mut tree = Self::new(
            band.code_block_count_x.max(1) as usize,
            band.code_block_count_y.max(1) as usize,
            1,
        );
        tree.padding = 255;
        for block in &band.code_blocks {
            let empty = !(block.total_passes > 0 && !block.data.is_empty());
            tree.set(
                block.local_x.max(0) as usize,
                block.local_y.max(0) as usize,
                0,
                i32::from(empty),
            );
        }
        tree.propagate_min();
        tree
    }

    fn from_missing_msb(band: &PrecinctBand) -> Self {
        let mut tree = Self::new(
            band.code_block_count_x.max(1) as usize,
            band.code_block_count_y.max(1) as usize,
            0,
        );
        tree.padding = 255;
        for block in &band.code_blocks {
            tree.set(
                block.local_x.max(0) as usize,
                block.local_y.max(0) as usize,
                0,
                i32::from(block.zero_bitplanes),
            );
        }
        tree.propagate_min();
        tree
    }

    /// Reads a child entry the way OpenJPH's `tag_tree::get` does: the flat
    /// index uses the level stride without clamping, so a child past the right
    /// edge wraps into the next row, and entries past the tree capacity read
    /// the initialization value. Mirroring this is required for byte-identical
    /// packet headers on bands whose edge code blocks are empty.
    fn raw_get(&self, level: usize, x: usize, y: usize) -> i32 {
        let stride = self.widths[level];
        self.values[level]
            .get(x + y * stride)
            .copied()
            .unwrap_or(self.padding)
    }

    fn propagate_min(&mut self) {
        for level in 1..self.levels {
            for y in 0..self.heights[level] {
                for x in 0..self.widths[level] {
                    let a = self.raw_get(level - 1, x << 1, y << 1);
                    let b = self.raw_get(level - 1, (x << 1) + 1, y << 1);
                    let c = self.raw_get(level - 1, x << 1, (y << 1) + 1);
                    let d = self.raw_get(level - 1, (x << 1) + 1, (y << 1) + 1);
                    self.set(x, y, level, a.min(b).min(c.min(d)));
                }
            }
        }
        self.set(0, 0, self.levels, 0);
    }

    fn is_empty(&self) -> bool {
        self.get(0, 0, self.levels.saturating_sub(1)) != 0
    }
}

fn log2_ceil(value: usize) -> usize {
    let mut floor = 0usize;
    let mut probe = value;
    while probe > 1 {
        probe >>= 1;
        floor += 1;
    }
    if value.is_power_of_two() {
        floor
    } else {
        floor + 1
    }
}

fn bit_length(value: usize) -> u32 {
    (usize::BITS - value.leading_zeros()).max(1)
}

/// Encodes one HT packet from the bands of one resolution and precinct.
pub(crate) fn encode_ht_packet(bands: &[&PrecinctBand]) -> Vec<u8> {
    let mut header = BitWriter::new_stuffed();
    let mut body = Vec::new();
    let mut coded = false;
    let mut skipped_subbands = 0usize;

    for band in bands {
        let inclusion = HtTree::from_inclusion(band);
        if inclusion.is_empty() {
            if coded {
                header.write_bit(false);
            } else {
                skipped_subbands += 1;
            }
            continue;
        }
        if !coded {
            header.write_bit(true);
            for _ in 0..skipped_subbands {
                header.write_bit(false);
            }
            coded = true;
        }

        let width = band.code_block_count_x.max(1) as usize;
        let height = band.code_block_count_y.max(1) as usize;
        let mut inclusion_flags = HtTree::new(width, height, 0);
        let mmsb = HtTree::from_missing_msb(band);
        let mut mmsb_flags = HtTree::new(width, height, 0);
        let block_map = |x: usize, y: usize| -> Option<&CodeBlock> {
            band.code_blocks
                .iter()
                .find(|block| block.local_x == x as i32 && block.local_y == y as i32)
        };

        for y in 0..height {
            for x in 0..width {
                let block = block_map(x, y);
                let mut empty_block = false;
                for level_plus_one in (1..=inclusion.levels).rev() {
                    let level = level_plus_one - 1;
                    if inclusion_flags.get(x >> level, y >> level, level) == 0 {
                        let skipped = inclusion.get(x >> level, y >> level, level)
                            - inclusion.get(
                                x >> level_plus_one,
                                y >> level_plus_one,
                                level_plus_one,
                            );
                        header.write_bit(skipped == 0);
                        inclusion_flags.set(x >> level, y >> level, level, 1);
                    }
                    empty_block = inclusion.get(x >> level, y >> level, level) != 0;
                    if empty_block {
                        break;
                    }
                }
                let block = match block {
                    Some(block) if !empty_block => block,
                    _ => continue,
                };
                if block.total_passes == 0 || block.data.is_empty() {
                    continue;
                }

                let mut missing;
                for level_plus_one in (1..=mmsb.levels).rev() {
                    let level = level_plus_one - 1;
                    missing = mmsb.get(x >> level_plus_one, y >> level_plus_one, level_plus_one);
                    if mmsb_flags.get(x >> level, y >> level, level) != 0 {
                        continue;
                    }
                    let zeros = mmsb.get(x >> level, y >> level, level) - missing;
                    for _ in 0..zeros.max(0) {
                        header.write_bit(false);
                    }
                    header.write_bit(true);
                    mmsb_flags.set(x >> level, y >> level, level, 1);
                }

                header.write_bit(false);
                let length = block.data.len();
                let bits = bit_length(length).saturating_sub(3);
                for _ in 0..bits {
                    header.write_bit(true);
                }
                header.write_bit(false);
                header.write_bits(length as u32, (bits + 3) as u8);
            }
        }

        for block in &band.code_blocks {
            if block.total_passes > 0 && !block.data.is_empty() {
                body.extend_from_slice(&block.data);
            }
        }
    }

    if !coded {
        return vec![0];
    }
    let mut output = header.into_bytes();
    output.extend_from_slice(&body);
    output
}

/// Decodes one HT packet, returning the number of consumed bytes and the
/// included code-block contributions in packet order.
pub(crate) fn decode_ht_packet(
    data: &[u8],
    bands: &[&PrecinctBand],
) -> Result<(usize, Vec<DecodedBlock>), String> {
    if data.is_empty() {
        return Err("HTJ2K packet is empty".into());
    }
    let mut reader = BitReader::new_stuffed(data);
    let present = reader.read_bit()?;
    if !present {
        reader.align_to_byte();
        return Ok((reader.position(), Vec::new()));
    }

    struct Pending {
        index: usize,
        length: usize,
        zero_bitplanes: u8,
    }
    let mut pending = Vec::new();

    for band in bands {
        let width = band.code_block_count_x.max(1) as usize;
        let height = band.code_block_count_y.max(1) as usize;
        let mut inclusion = HtTree::new(width, height, 0);
        let mut inclusion_flags = HtTree::new(width, height, 0);
        let mut mmsb = HtTree::new(width, height, 0);
        let mut mmsb_flags = HtTree::new(width, height, 0);
        let mut included_state = vec![false; width * height];
        let block_map = |x: usize, y: usize| -> Option<&CodeBlock> {
            band.code_blocks
                .iter()
                .find(|block| block.local_x == x as i32 && block.local_y == y as i32)
        };

        for y in 0..height {
            for x in 0..width {
                let empty =
                    decode_ht_empty_block(&mut reader, &mut inclusion, &mut inclusion_flags, x, y)?;
                if empty {
                    continue;
                }
                let block = match block_map(x, y) {
                    Some(block) => block,
                    None => continue,
                };
                let state_index = y * width + x;
                let zero_bitplanes = if !included_state[state_index] {
                    included_state[state_index] = true;
                    decode_ht_missing_msbs(&mut reader, &mut mmsb, &mut mmsb_flags, x, y)?
                } else {
                    0
                };
                let pass_count = decode_pass_count(&mut reader)?;
                if pass_count > 1 {
                    return Err("HTJ2K refinement passes are not supported".into());
                }
                let (_, length) = decode_ht_segments(&mut reader, pass_count)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| "HTJ2K packet is missing a cleanup segment".to_string())?;
                pending.push(Pending {
                    index: block.index,
                    length,
                    zero_bitplanes: zero_bitplanes as u8,
                });
            }
        }
    }

    reader.align_to_byte();
    let header_len = reader.position();
    let mut offset = header_len;
    let mut blocks = Vec::with_capacity(pending.len());
    for item in pending {
        if offset + item.length > data.len() {
            return Err("HTJ2K packet code-block data exceeds the packet length".into());
        }
        blocks.push(DecodedBlock {
            index: item.index,
            data: data[offset..offset + item.length].to_vec(),
            zero_bitplanes: item.zero_bitplanes,
        });
        offset += item.length;
    }
    Ok((offset, blocks))
}

fn decode_ht_empty_block(
    reader: &mut BitReader<'_>,
    tree: &mut HtTree,
    flags: &mut HtTree,
    x: usize,
    y: usize,
) -> Result<bool, String> {
    for level_plus_one in (1..=tree.levels).rev() {
        let level = level_plus_one - 1;
        if tree.get(x >> level, y >> level, level) == 1 {
            return Ok(true);
        }
        if flags.get(x >> level, y >> level, level) == 0 {
            let bit = reader.read_bit()?;
            let empty = !bit;
            tree.set(x >> level, y >> level, level, i32::from(empty));
            flags.set(x >> level, y >> level, level, 1);
            if empty {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn decode_ht_missing_msbs(
    reader: &mut BitReader<'_>,
    tree: &mut HtTree,
    flags: &mut HtTree,
    x: usize,
    y: usize,
) -> Result<i32, String> {
    let mut missing = 0i32;
    for level_plus_one in (1..=tree.levels).rev() {
        let level = level_plus_one - 1;
        missing = tree.get(x >> level_plus_one, y >> level_plus_one, level_plus_one);
        if flags.get(x >> level, y >> level, level) == 0 {
            let mut bit = 0u32;
            while bit == 0 {
                bit = u32::from(reader.read_bit()?);
                missing += 1 - bit as i32;
            }
            tree.set(x >> level, y >> level, level, missing);
            flags.set(x >> level, y >> level, level, 1);
        }
    }
    Ok(missing)
}

fn decode_pass_count(reader: &mut BitReader<'_>) -> Result<u32, String> {
    if !reader.read_bit()? {
        return Ok(1);
    }
    if !reader.read_bit()? {
        return Ok(2);
    }
    let value = reader.read_bits(2)?;
    if value != 3 {
        return Ok(3 + value);
    }
    let value = reader.read_bits(5)?;
    if value != 31 {
        return Ok(6 + value);
    }
    Ok(37 + reader.read_bits(7)?)
}

fn decode_ht_segments(
    reader: &mut BitReader<'_>,
    pass_count: u32,
) -> Result<Vec<(u32, usize)>, String> {
    let mut bits1 = 3u8;
    while reader.read_bit()? {
        bits1 = bits1.saturating_add(1);
    }
    let cleanup_length = reader.read_bits(bits1)? as usize;
    if !(2..65535).contains(&cleanup_length) {
        return Err("HTJ2K cleanup segment length is invalid".into());
    }
    if pass_count <= 1 {
        return Ok(vec![(1, cleanup_length)]);
    }
    let refinement_bits = bits1 + u8::from(pass_count > 2);
    let refinement_length = reader.read_bits(refinement_bits)? as usize;
    if refinement_length >= 2047 {
        return Err("HTJ2K refinement segment length is invalid".into());
    }
    Ok(vec![
        (1, cleanup_length),
        (pass_count - 1, refinement_length),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(blocks: Vec<CodeBlock>) -> PrecinctBand {
        PrecinctBand {
            orientation: 0,
            code_block_count_x: 1,
            code_block_count_y: 1,
            code_blocks: blocks,
        }
    }

    fn block(data: Vec<u8>, zero_bitplanes: u8) -> CodeBlock {
        CodeBlock {
            index: 0,
            orientation: 0,
            local_x: 0,
            local_y: 0,
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            total_passes: 1,
            zero_bitplanes,
            data,
            segments: Vec::new(),
            passes: Vec::new(),
            layer_passes: Vec::new(),
        }
    }

    #[test]
    fn tag_tree_propagation_wraps_like_openjph() {
        // OpenJPH's tag_tree::get indexes each level with its own stride but
        // does not clamp, so propagating node (1,0) of a 3x3 band reads the
        // leaf that follows the row (here the empty block at (0,2)) and the
        // node resolves to 0. Real 6.dcm streams depend on this.
        let mut blocks = Vec::new();
        for y in 0..3i32 {
            for x in 0..3i32 {
                let mut coded = block(vec![1, 2, 3], 16);
                coded.local_x = x;
                coded.local_y = y;
                if x == 0 && y == 2 {
                    coded.total_passes = 0;
                    coded.zero_bitplanes = 0;
                    coded.data.clear();
                }
                blocks.push(coded);
            }
        }
        let band = PrecinctBand {
            orientation: 1,
            code_block_count_x: 3,
            code_block_count_y: 3,
            code_blocks: blocks,
        };
        let tree = HtTree::from_missing_msb(&band);
        assert_eq!(tree.get(1, 0, 1), 0);
        assert_eq!(tree.get(0, 0, 2), 0);
    }

    #[test]
    fn single_block_packet_round_trips() {
        let band = band(vec![block(vec![1, 2, 3, 4, 5], 7)]);
        let packet = encode_ht_packet(&[&band]);
        let (consumed, blocks) = decode_ht_packet(&packet, &[&band]).unwrap();
        assert_eq!(consumed, packet.len());
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].data, vec![1, 2, 3, 4, 5]);
        assert_eq!(blocks[0].zero_bitplanes, 7);
    }

    #[test]
    fn empty_packet_is_one_zero_byte() {
        let band = band(vec![block(Vec::new(), 0)]);
        let packet = encode_ht_packet(&[&band]);
        assert_eq!(packet, vec![0]);
        let (consumed, blocks) = decode_ht_packet(&packet, &[&band]).unwrap();
        assert_eq!(consumed, 1);
        assert!(blocks.is_empty());
    }
}
