//! JPEG 2000 Tier-2 packet headers, tag trees and progression orders
//! (ISO/IEC 15444-1 Annex B.10 and B.12).

use std::collections::HashMap;

use crate::bit::{BitReader, BitWriter};
use crate::geometry::{CodeBlockSegment, Component};

const TAG_TREE_UNKNOWN: i32 = 999;

/// Upper bound on the number of packets enumerated for one tile. Packet count is
/// `layers * sum(component precincts over resolutions)`; without this a hostile
/// COD (many quality layers) combined with a large precinct grid would build a
/// huge `Vec<PacketKey>` before any packet is parsed. The bound matches the
/// precinct cap so a legal one-layer multi-precinct stream is never rejected
/// merely by packet enumeration.
pub(crate) const MAX_PACKETS: u64 = 1 << 22;

#[derive(Debug, Clone)]
pub(crate) struct TagTree {
    width: usize,
    height: usize,
    levels: usize,
    level_widths: Vec<usize>,
    values: Vec<Vec<i32>>,
    low: Vec<Vec<i32>>,
    known: Vec<Vec<bool>>,
}

impl TagTree {
    pub(crate) fn new(width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let mut levels = 1;
        let mut w = width;
        let mut h = height;
        while w > 1 || h > 1 {
            levels += 1;
            w = w.div_ceil(2);
            h = h.div_ceil(2);
        }
        let mut level_widths = vec![0; levels];
        let mut values = Vec::with_capacity(levels);
        let mut low = Vec::with_capacity(levels);
        let mut known = Vec::with_capacity(levels);
        w = width;
        h = height;
        for slot in level_widths.iter_mut() {
            *slot = w;
            let count = w * h;
            values.push(vec![TAG_TREE_UNKNOWN; count]);
            low.push(vec![0; count]);
            known.push(vec![false; count]);
            w = w.div_ceil(2);
            h = h.div_ceil(2);
        }
        Self {
            width,
            height,
            levels,
            level_widths,
            values,
            low,
            known,
        }
    }

    pub(crate) fn set_value(&mut self, x: usize, y: usize, value: i32) {
        let mut px = x;
        let mut py = y;
        for level in 0..self.levels {
            let index = py * self.level_widths[level] + px;
            if self.values[level][index] <= value {
                break;
            }
            self.values[level][index] = value;
            px /= 2;
            py /= 2;
        }
    }

    fn indices(&self, x: usize, y: usize) -> Vec<usize> {
        let mut indices = Vec::with_capacity(self.levels);
        let mut px = x;
        let mut py = y;
        for level in 0..self.levels {
            indices.push(py * self.level_widths[level] + px);
            px /= 2;
            py /= 2;
        }
        indices
    }

    pub(crate) fn decode(
        &mut self,
        reader: &mut BitReader<'_>,
        x: usize,
        y: usize,
        threshold: i32,
    ) -> Result<bool, String> {
        if x >= self.width || y >= self.height {
            return Err("tag-tree leaf outside the tree".into());
        }
        let indices = self.indices(x, y);
        let mut low = 0i32;
        let mut leaf_value = TAG_TREE_UNKNOWN;
        for level in (0..self.levels).rev() {
            let index = indices[level];
            if low > self.low[level][index] {
                self.low[level][index] = low;
            } else {
                low = self.low[level][index];
            }
            while low < threshold && low < self.values[level][index] {
                if reader.read_bit()? {
                    self.values[level][index] = low;
                } else {
                    low += 1;
                }
            }
            self.low[level][index] = low;
            if level == 0 {
                leaf_value = self.values[level][index];
            }
        }
        Ok(leaf_value < threshold)
    }

    pub(crate) fn decode_value(
        &mut self,
        reader: &mut BitReader<'_>,
        x: usize,
        y: usize,
    ) -> Result<i32, String> {
        for threshold in 1..64 {
            if self.decode(reader, x, y, threshold)? {
                return Ok(threshold - 1);
            }
        }
        Err("tag-tree value is too large".into())
    }

    pub(crate) fn encode(&mut self, writer: &mut BitWriter, x: usize, y: usize, threshold: i32) {
        let indices = self.indices(x, y);
        let mut low = 0i32;
        for level in (0..self.levels).rev() {
            let index = indices[level];
            if low > self.low[level][index] {
                self.low[level][index] = low;
            } else {
                low = self.low[level][index];
            }
            while low < threshold {
                if low >= self.values[level][index] {
                    if !self.known[level][index] {
                        writer.write_bit(true);
                        self.known[level][index] = true;
                    }
                    break;
                }
                writer.write_bit(false);
                low += 1;
            }
            self.low[level][index] = low;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PacketKey {
    pub(crate) layer: u16,
    pub(crate) resolution: i32,
    pub(crate) component: usize,
    pub(crate) precinct: i32,
}

fn max_resolutions(components: &[Component]) -> i32 {
    components.iter().map(|c| c.levels + 1).max().unwrap_or(1)
}

fn precinct_count(component: &Component, resolution: i32) -> i32 {
    component.precincts_including_empty(resolution).len() as i32
}

/// Enumerates packets for the supported progression orders. LRCP and RLCP use
/// the standard precinct raster order; the position-based orders use the same
/// precinct index, which is exact when a resolution has a single precinct
/// (the DICOM default).
pub(crate) fn packet_order(
    components: &[Component],
    layer_count: u16,
    progression: u8,
) -> Result<Vec<PacketKey>, String> {
    let max_res = max_resolutions(components);
    let max_precincts = (0..max_res)
        .map(|r| {
            components
                .iter()
                .map(|c| precinct_count(c, r))
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0);
    // Total packets is `layers * sum_{resolution,component} precincts` for every
    // supported progression order (the loops only reorder the same key set), so
    // the count can be checked analytically before any Vec is allocated.
    let mut packets_per_layer = 0u64;
    for resolution in 0..max_res {
        for component in components {
            packets_per_layer = packets_per_layer
                .checked_add(precinct_count(component, resolution) as u64)
                .ok_or_else(|| "packet count overflow".to_string())?;
        }
    }
    let total_packets = packets_per_layer
        .checked_mul(u64::from(layer_count))
        .ok_or_else(|| "packet count overflow".to_string())?;
    if total_packets > MAX_PACKETS {
        return Err("packet count exceeds the supported limit".into());
    }
    let mut packets = Vec::new();
    match progression {
        0 => {
            for layer in 0..layer_count {
                for resolution in 0..max_res {
                    for (component_index, component) in components.iter().enumerate() {
                        for precinct in 0..precinct_count(component, resolution) {
                            packets.push(PacketKey {
                                layer,
                                resolution,
                                component: component_index,
                                precinct,
                            });
                        }
                    }
                }
            }
        }
        1 => {
            for resolution in 0..max_res {
                for layer in 0..layer_count {
                    for (component_index, component) in components.iter().enumerate() {
                        for precinct in 0..precinct_count(component, resolution) {
                            packets.push(PacketKey {
                                layer,
                                resolution,
                                component: component_index,
                                precinct,
                            });
                        }
                    }
                }
            }
        }
        2 => {
            for resolution in 0..max_res {
                for precinct in 0..max_precincts {
                    for (component_index, component) in components.iter().enumerate() {
                        if !component.has_precinct(resolution, precinct) {
                            continue;
                        }
                        for layer in 0..layer_count {
                            packets.push(PacketKey {
                                layer,
                                resolution,
                                component: component_index,
                                precinct,
                            });
                        }
                    }
                }
            }
        }
        3 => {
            for precinct in 0..max_precincts {
                for (component_index, component) in components.iter().enumerate() {
                    for resolution in 0..max_res {
                        if !component.has_precinct(resolution, precinct) {
                            continue;
                        }
                        for layer in 0..layer_count {
                            packets.push(PacketKey {
                                layer,
                                resolution,
                                component: component_index,
                                precinct,
                            });
                        }
                    }
                }
            }
        }
        4 => {
            for (component_index, component) in components.iter().enumerate() {
                for precinct in 0..max_precincts {
                    for resolution in 0..max_res {
                        if !component.has_precinct(resolution, precinct) {
                            continue;
                        }
                        for layer in 0..layer_count {
                            packets.push(PacketKey {
                                layer,
                                resolution,
                                component: component_index,
                                precinct,
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(packets)
}

fn floor_log2(value: u32) -> u32 {
    if value == 0 {
        0
    } else {
        31 - value.leading_zeros()
    }
}

fn encode_pass_count(writer: &mut BitWriter, count: u32) {
    if count == 1 {
        writer.write_bit(false);
        return;
    }
    if count == 2 {
        writer.write_bit(true);
        writer.write_bit(false);
        return;
    }
    if count <= 5 {
        writer.write_bit(true);
        writer.write_bit(true);
        writer.write_bits(count - 3, 2);
        return;
    }
    if count <= 36 {
        writer.write_bit(true);
        writer.write_bit(true);
        writer.write_bits(3, 2);
        writer.write_bits(count - 6, 5);
        return;
    }
    writer.write_bit(true);
    writer.write_bit(true);
    writer.write_bits(3, 2);
    writer.write_bits(31, 5);
    writer.write_bits(count - 37, 7);
}

fn decode_pass_count(reader: &mut BitReader<'_>) -> Result<u32, String> {
    if !reader.read_bit()? {
        return Ok(1);
    }
    if !reader.read_bit()? {
        return Ok(2);
    }
    let value = reader.read_bits(2)?;
    if value < 3 {
        return Ok(3 + value);
    }
    let value = reader.read_bits(5)?;
    if value < 31 {
        return Ok(6 + value);
    }
    Ok(37 + reader.read_bits(7)?)
}

/// How [`encode_packets`] runs: write the tile data, or only measure how many
/// bytes the packets for the requested layers would occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacketMode {
    Final,
    Measure { max_len: usize },
}

/// OpenJPEG's `opj_tcd_makelayer` state for one code block while packets are
/// being emitted: passes already placed in earlier layers and the current
/// length-indicator width.
#[derive(Debug, Clone)]
struct BlockEncState {
    numpasses: u32,
    numlenbits: u32,
}

#[derive(Debug, Clone)]
struct BandEncState {
    inclusion: TagTree,
    zero_bitplanes: TagTree,
    blocks: Vec<BlockEncState>,
}

#[derive(Debug, Clone)]
struct PrecinctEncState {
    bands: Vec<BandEncState>,
}

fn new_precinct_enc_state(
    component: &Component,
    resolution: i32,
    precinct: i32,
) -> PrecinctEncState {
    let bands = component.bands(resolution, precinct);
    PrecinctEncState {
        bands: bands
            .iter()
            .map(|band| {
                let mut zero_bitplanes = TagTree::new(
                    band.code_block_count_x.max(1) as usize,
                    band.code_block_count_y.max(1) as usize,
                );
                // OpenJPEG seeds the zero-bit-plane tag tree for every block
                // when the layer-0 packet resets it.
                for block in &band.code_blocks {
                    zero_bitplanes.set_value(
                        block.local_x as usize,
                        block.local_y as usize,
                        i32::from(block.zero_bitplanes),
                    );
                }
                BandEncState {
                    inclusion: TagTree::new(
                        band.code_block_count_x.max(1) as usize,
                        band.code_block_count_y.max(1) as usize,
                    ),
                    zero_bitplanes,
                    blocks: band
                        .code_blocks
                        .iter()
                        .map(|_| BlockEncState {
                            numpasses: 0,
                            numlenbits: 0,
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

fn layer_pass_count(block: &crate::geometry::CodeBlock, layer: u16) -> u32 {
    block.layer_passes.get(layer as usize).copied().unwrap_or(0) as u32
}

fn pass_offset(block: &crate::geometry::CodeBlock, pass: usize) -> u32 {
    if pass == 0 {
        0
    } else {
        block.passes[pass - 1].rate
    }
}

fn write_commacode(writer: &mut BitWriter, mut n: u32) {
    while n > 0 {
        writer.write_bit(true);
        n -= 1;
    }
    writer.write_bit(false);
}

/// Encodes the quality-layer packets for one tile (LRCP by default).
///
/// `block.layer_passes[layer]` holds the number of Tier-1 passes assigned to
/// that layer. `PacketMode::Measure` replicates OpenJPEG's `THRESH_CALC` pass:
/// the same header bytes are produced, but the packets are discarded as soon as
/// the running byte count exceeds `max_len`.
pub(crate) fn encode_packets(
    components: &[Component],
    layer_count: u16,
    progression: u8,
    mode: PacketMode,
) -> Result<(Vec<u8>, usize, bool), String> {
    let order = packet_order(components, layer_count, progression)?;
    let mut states: Vec<HashMap<(i32, i32), PrecinctEncState>> =
        (0..components.len()).map(|_| HashMap::new()).collect();
    let mut output = Vec::new();
    let mut written = 0usize;
    for key in order {
        let component = &components[key.component];
        if !component.has_precinct(key.resolution, key.precinct) {
            continue;
        }
        let bands_meta = component.bands(key.resolution, key.precinct);
        let state = states[key.component]
            .entry((key.resolution, key.precinct))
            .or_insert_with(|| new_precinct_enc_state(component, key.resolution, key.precinct));
        // OpenJPEG writes the "empty packet" header bit unconditionally in this
        // build (ENABLE_EMPTY_PACKET_OPTIMIZATION is not defined), so a packet
        // always starts with a one bit and always carries its header.
        let mut header = BitWriter::new_stuffed();
        header.write_bit(true);
        let mut body = Vec::new();
        let mut body_len = 0usize;
        for (band_index, band) in bands_meta.iter().enumerate() {
            if band.code_blocks.is_empty() {
                continue;
            }
            let band_state = &mut state.bands[band_index];
            let layer = key.layer;
            for (block_index, block) in band.code_blocks.iter().enumerate() {
                let passes = layer_pass_count(block, layer);
                if passes > 0 && band_state.blocks[block_index].numpasses == 0 {
                    band_state.inclusion.set_value(
                        block.local_x as usize,
                        block.local_y as usize,
                        i32::from(layer),
                    );
                }
            }
            for (block_index, block) in band.code_blocks.iter().enumerate() {
                let block_state = &mut band_state.blocks[block_index];
                let passes = layer_pass_count(block, layer);
                if block_state.numpasses == 0 {
                    band_state.inclusion.encode(
                        &mut header,
                        block.local_x as usize,
                        block.local_y as usize,
                        i32::from(layer) + 1,
                    );
                } else {
                    header.write_bit(passes != 0);
                }
                if passes == 0 {
                    continue;
                }
                if block_state.numpasses == 0 {
                    block_state.numlenbits = 3;
                    band_state.zero_bitplanes.encode(
                        &mut header,
                        block.local_x as usize,
                        block.local_y as usize,
                        999,
                    );
                }
                encode_pass_count(&mut header, passes);
                let start = block_state.numpasses as usize;
                let end = start + passes as usize;
                let mut increment = 0i32;
                let mut len = 0u32;
                let mut nump = 0u32;
                for pass in start..end {
                    nump += 1;
                    len += block.passes[pass].rate - pass_offset(block, pass);
                    if block.passes[pass].term || pass + 1 == end {
                        increment = increment.max(
                            floor_log2(len) as i32 + 1
                                - (block_state.numlenbits as i32 + floor_log2(nump) as i32),
                        );
                        len = 0;
                        nump = 0;
                    }
                }
                write_commacode(&mut header, increment as u32);
                block_state.numlenbits += increment as u32;
                let mut len = 0u32;
                let mut nump = 0u32;
                for pass in start..end {
                    nump += 1;
                    len += block.passes[pass].rate - pass_offset(block, pass);
                    if block.passes[pass].term || pass + 1 == end {
                        header.write_bits(len, (block_state.numlenbits + floor_log2(nump)) as u8);
                        len = 0;
                        nump = 0;
                    }
                }
                block_state.numpasses += passes;
                let offset = pass_offset(block, start) as usize;
                let segment_len = (pass_offset(block, end) - offset as u32) as usize;
                body_len += segment_len;
                if mode == PacketMode::Final {
                    body.extend_from_slice(&block.data[offset..offset + segment_len]);
                }
            }
        }
        let header_bytes = header.into_bytes();
        written += header_bytes.len() + body_len;
        if let PacketMode::Measure { max_len } = mode {
            if written > max_len {
                return Ok((Vec::new(), written, false));
            }
        } else {
            output.extend_from_slice(&header_bytes);
            output.extend_from_slice(&body);
        }
    }
    Ok((output, written, true))
}

#[derive(Debug, Clone)]
struct BlockState {
    included: bool,
    num_len_bits: u32,
}

#[derive(Debug, Clone)]
struct BandState {
    inclusion: TagTree,
    zero_bitplanes: TagTree,
    blocks: Vec<BlockState>,
}

#[derive(Debug, Clone)]
struct PrecinctState {
    bands: Vec<BandState>,
}

fn new_precinct_state(component: &Component, resolution: i32, precinct: i32) -> PrecinctState {
    let bands = component.bands(resolution, precinct);
    PrecinctState {
        bands: bands
            .iter()
            .map(|band| BandState {
                inclusion: TagTree::new(
                    band.code_block_count_x.max(1) as usize,
                    band.code_block_count_y.max(1) as usize,
                ),
                zero_bitplanes: TagTree::new(
                    band.code_block_count_x.max(1) as usize,
                    band.code_block_count_y.max(1) as usize,
                ),
                blocks: band
                    .code_blocks
                    .iter()
                    .map(|_| BlockState {
                        included: false,
                        num_len_bits: 3,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Decodes all packets for one tile, appending coding segments to code blocks.
pub(crate) fn decode_packets(
    data: &[u8],
    components: &mut [Component],
    layer_count: u16,
    progression: u8,
    code_block_styles: &[u8],
) -> Result<(), String> {
    let order = packet_order(components, layer_count, progression)?;
    let mut states: Vec<HashMap<(i32, i32), PrecinctState>> =
        (0..components.len()).map(|_| HashMap::new()).collect();
    let mut offset = 0usize;
    for key in order {
        let component = &mut components[key.component];
        if !component.has_precinct(key.resolution, key.precinct) {
            continue;
        }
        let style = code_block_styles.get(key.component).copied().unwrap_or(0);
        let mut reader = BitReader::new_stuffed(&data[offset..]);
        let empty = !reader.read_bit()?;
        if empty {
            reader.align_to_byte();
            offset += reader.position();
            continue;
        }
        let bands_meta = component.bands(key.resolution, key.precinct).to_vec();
        let state = states[key.component]
            .entry((key.resolution, key.precinct))
            .or_insert_with(|| new_precinct_state(component, key.resolution, key.precinct));
        let mut contributions: Vec<(usize, usize, u32, usize)> = Vec::new();
        for (band_index, band) in bands_meta.iter().enumerate() {
            let band_state = &mut state.bands[band_index];
            for (block_index, block) in band.code_blocks.iter().enumerate() {
                let block_state = &mut band_state.blocks[block_index];
                if !block_state.included {
                    let included = band_state.inclusion.decode(
                        &mut reader,
                        block.local_x as usize,
                        block.local_y as usize,
                        i32::from(key.layer) + 1,
                    )?;
                    if !included {
                        continue;
                    }
                    let zbp = band_state.zero_bitplanes.decode_value(
                        &mut reader,
                        block.local_x as usize,
                        block.local_y as usize,
                    )?;
                    if !(0..=31).contains(&zbp) {
                        return Err("invalid zero-bit-plane count".into());
                    }
                    block_state.included = true;
                    block_state.num_len_bits = 3;
                    if let Some(target) = component
                        .bands_mut(key.resolution, key.precinct)
                        .and_then(|bands| bands.get_mut(band_index))
                        .and_then(|band| band.code_blocks.get_mut(block_index))
                    {
                        target.zero_bitplanes = zbp as u8;
                    }
                } else if !reader.read_bit()? {
                    continue;
                }
                let passes = decode_pass_count(&mut reader)?;
                if passes == 0 || passes > 164 {
                    return Err("invalid coding pass count".into());
                }
                let mut remaining = passes;
                let mut first_segment = true;
                while remaining > 0 {
                    let segment_passes = segment_max_passes(style, first_segment).min(remaining);
                    while reader.read_bit()? {
                        block_state.num_len_bits += 1;
                        if block_state.num_len_bits > 32 {
                            return Err("code-block length bits exceed 32".into());
                        }
                    }
                    let length_bits = block_state.num_len_bits + floor_log2(segment_passes);
                    let length = reader.read_bits(length_bits as u8)? as usize;
                    contributions.push((band_index, block_index, segment_passes, length));
                    remaining -= segment_passes;
                    first_segment = false;
                }
            }
        }
        reader.align_to_byte();
        offset += reader.position();
        for (band_index, block_index, passes, length) in contributions {
            let end = offset
                .checked_add(length)
                .ok_or_else(|| "packet body length overflow".to_string())?;
            if end > data.len() {
                return Err("packet body exceeds tile data".into());
            }
            let bytes = data[offset..end].to_vec();
            offset = end;
            let target = component
                .bands_mut(key.resolution, key.precinct)
                .and_then(|bands| bands.get_mut(band_index))
                .and_then(|band| band.code_blocks.get_mut(block_index))
                .ok_or_else(|| "packet references a missing code block".to_string())?;
            target.total_passes += passes;
            target.segments.push(CodeBlockSegment {
                data: bytes,
                pass_count: passes,
            });
        }
    }
    if offset != data.len() {
        return Err("tile data has trailing bytes".into());
    }
    Ok(())
}

fn segment_max_passes(style: u8, first: bool) -> u32 {
    if style & 0x04 != 0 {
        1
    } else if style & 0x01 != 0 {
        if first {
            10
        } else {
            2
        }
    } else {
        109
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_tree_round_trips_values() {
        for (width, height) in [(1usize, 1usize), (4, 3), (7, 9)] {
            let values = (0..width * height)
                .map(|i| (i * 3) as i32 % 11)
                .collect::<Vec<_>>();
            let mut writer = BitWriter::new_stuffed();
            let mut encoder = TagTree::new(width, height);
            for y in 0..height {
                for x in 0..width {
                    encoder.set_value(x, y, values[y * width + x]);
                }
            }
            for y in 0..height {
                for x in 0..width {
                    encoder.encode(&mut writer, x, y, 32);
                }
            }
            let bytes = writer.into_bytes();
            let mut reader = BitReader::new_stuffed(&bytes);
            let mut decoder = TagTree::new(width, height);
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        decoder.decode_value(&mut reader, x, y).unwrap(),
                        values[y * width + x],
                        "{width}x{height} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn pass_count_encoding_round_trips() {
        for count in [1u32, 2, 3, 5, 6, 36, 37, 100, 164] {
            let mut writer = BitWriter::new_stuffed();
            encode_pass_count(&mut writer, count);
            let bytes = writer.into_bytes();
            let mut reader = BitReader::new_stuffed(&bytes);
            assert_eq!(decode_pass_count(&mut reader).unwrap(), count);
        }
    }

    #[test]
    fn packet_count_over_cap_is_rejected_before_allocation() {
        // 100 components x 6 resolutions = 600 packets per layer; 65535 layers
        // is ~39M packets, far above the cap, so the Vec must not be built.
        let planes: Vec<Vec<i32>> = (0..100).map(|_| vec![0i32; 4]).collect();
        let mut components =
            crate::codestream::build_components(&planes, 2, 2, 8, false, 5).unwrap();
        for component in &mut components {
            component.build_code_blocks(64, 64, None).unwrap();
        }
        let error = packet_order(&components, 65535, 0).unwrap_err();
        assert!(error.contains("packet count"), "{error}");
    }
}
