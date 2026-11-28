//! JPEG 2000 Tier-2 packet headers, tag trees and progression orders
//! (ISO/IEC 15444-1 Annex B.10 and B.12).

use std::collections::HashMap;

use crate::bit::{BitReader, BitWriter};
use crate::geometry::Component;

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
    levels: Vec<(usize, usize)>,
    nodes: Vec<TagNode>,
}

#[derive(Debug, Clone, Copy)]
struct TagNode {
    value: i32,
    low: i32,
    known: bool,
}

impl TagTree {
    fn copy_from_tree(&mut self, source: &Self) {
        self.width = source.width;
        self.height = source.height;
        self.levels.clone_from(&source.levels);
        self.nodes.clone_from(&source.nodes);
    }

    pub(crate) fn new(width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let mut levels = Vec::new();
        let mut count = 0;
        let mut w = width;
        let mut h = height;
        loop {
            levels.push((w, count));
            count += w * h;
            if w == 1 && h == 1 {
                break;
            }
            w = w.div_ceil(2);
            h = h.div_ceil(2);
        }
        Self {
            width,
            height,
            levels,
            nodes: vec![
                TagNode {
                    value: TAG_TREE_UNKNOWN,
                    low: 0,
                    known: false
                };
                count
            ],
        }
    }

    pub(crate) fn set_value(&mut self, x: usize, y: usize, value: i32) {
        let mut px = x;
        let mut py = y;
        for &(width, offset) in &self.levels {
            let node = &mut self.nodes[offset + py * width + px];
            if node.value <= value {
                break;
            }
            node.value = value;
            px /= 2;
            py /= 2;
        }
    }

    #[inline]
    fn node_index(&self, level: usize, x: usize, y: usize) -> usize {
        let (width, offset) = self.levels[level];
        // After usize::BITS halvings even the widest possible leaf is at the root.
        offset
            + y.checked_shr(level as u32).unwrap_or(0) * width
            + x.checked_shr(level as u32).unwrap_or(0)
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
        let mut low = 0i32;
        for level in (0..self.levels.len()).rev() {
            let index = self.node_index(level, x, y);
            let node = &mut self.nodes[index];
            low = low.max(node.low);
            while low < threshold && low < node.value {
                if reader.read_bit()? {
                    node.value = low;
                } else {
                    low += 1;
                }
            }
            node.low = low;
        }
        Ok(self.nodes[y * self.width + x].value < threshold)
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
        let mut low = 0i32;
        for level in (0..self.levels.len()).rev() {
            let index = self.node_index(level, x, y);
            let node = &mut self.nodes[index];
            low = low.max(node.low);
            while low < threshold {
                if low >= node.value {
                    if !node.known {
                        writer.write_bit(true);
                        node.known = true;
                    }
                    break;
                }
                writer.write_bit(false);
                low += 1;
            }
            node.low = low;
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
                .map(|c| c.precinct_count(r))
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
                .checked_add(component.precinct_count(resolution) as u64)
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
                        for precinct in 0..component.precinct_count(resolution) {
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
                        for precinct in 0..component.precinct_count(resolution) {
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

/// Encodes LRCP packets in layer order. Each trial starts from the
/// tag trees and pass counts of the last committed layer.
pub(crate) struct LayerPackets {
    packets: Vec<(PacketKey, PrecinctEncState)>,
    data: Vec<u8>,
    trial: PrecinctEncState,
}

impl LayerPackets {
    pub(crate) fn new(components: &[Component], layer_count: u16) -> Result<Self, String> {
        let order = packet_order(components, 1, 0)?;
        if order.len() as u64 * u64::from(layer_count) > MAX_PACKETS {
            return Err("packet count exceeds the supported limit".into());
        }
        let packets = order
            .into_iter()
            .filter_map(|key| {
                let component = &components[key.component];
                component
                    .has_precinct(key.resolution, key.precinct)
                    .then(|| {
                        let state = new_precinct_enc_state(component, key.resolution, key.precinct);
                        (key, state)
                    })
            })
            .collect();
        Ok(Self {
            packets,
            data: Vec::new(),
            trial: PrecinctEncState { bands: Vec::new() },
        })
    }

    pub(crate) fn fits(&mut self, components: &[Component], layer: u16, max_len: usize) -> bool {
        let mut written = self.data.len();
        if written > max_len {
            return false;
        }
        for (key, state) in &self.packets {
            let key = PacketKey { layer, ..*key };
            let band_count = self.trial.bands.len().min(state.bands.len());
            for (trial, source) in self.trial.bands.iter_mut().zip(&state.bands) {
                trial.inclusion.copy_from_tree(&source.inclusion);
                trial.zero_bitplanes.copy_from_tree(&source.zero_bitplanes);
                trial.blocks.clone_from(&source.blocks);
            }
            self.trial
                .bands
                .extend_from_slice(&state.bands[band_count..]);
            written += encode_packet(components, key, &mut self.trial, None);
            if written > max_len {
                return false;
            }
        }
        true
    }

    pub(crate) fn commit(&mut self, components: &[Component], layer: u16) {
        for (key, state) in &mut self.packets {
            let key = PacketKey { layer, ..*key };
            encode_packet(components, key, state, Some(&mut self.data));
        }
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

fn encode_packet(
    components: &[Component],
    key: PacketKey,
    state: &mut PrecinctEncState,
    output: Option<&mut Vec<u8>>,
) -> usize {
    let bands_meta = components[key.component].bands(key.resolution, key.precinct);
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
            // The encoder emits coding style 0, with one MQ segment
            // across all passes. A packet stores its slice of that segment.
            let offset = pass_offset(block, start);
            let segment_len = pass_offset(block, end) - offset;
            let pass_bits = floor_log2(passes);
            let increment =
                (floor_log2(segment_len) + 1).saturating_sub(block_state.numlenbits + pass_bits);
            write_commacode(&mut header, increment);
            block_state.numlenbits += increment;
            header.write_bits(segment_len, (block_state.numlenbits + pass_bits) as u8);
            block_state.numpasses += passes;
            let offset = offset as usize;
            let segment_len = segment_len as usize;
            body_len += segment_len;
            if output.is_some() {
                body.extend_from_slice(&block.data[offset..offset + segment_len]);
            }
        }
    }
    let header_bytes = header.into_bytes();
    let len = header_bytes.len() + body_len;
    if let Some(output) = output {
        output.extend_from_slice(&header_bytes);
        output.extend_from_slice(&body);
    }
    len
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
    let mut contributions = Vec::new();
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
        let state = states[key.component]
            .entry((key.resolution, key.precinct))
            .or_insert_with(|| new_precinct_state(component, key.resolution, key.precinct));
        let bands = component
            .bands_mut(key.resolution, key.precinct)
            .ok_or_else(|| "packet references a missing precinct".to_string())?;
        for (band_index, band) in bands.iter_mut().enumerate() {
            let band_state = &mut state.bands[band_index];
            for (block_index, block) in band.code_blocks.iter_mut().enumerate() {
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
                    block.zero_bitplanes = zbp as u8;
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
        for (band_index, block_index, passes, length) in contributions.drain(..) {
            let end = offset
                .checked_add(length)
                .ok_or_else(|| "packet body length overflow".to_string())?;
            if end > data.len() {
                return Err("packet body exceeds tile data".into());
            }
            let target = &mut bands[band_index].code_blocks[block_index];
            target.total_passes += passes;
            target.data.extend_from_slice(&data[offset..end]);
            offset = end;
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
            crate::codestream::build_components(planes, 2, 2, 8, false, 5).unwrap();
        for component in &mut components {
            component.build_code_blocks(64, 64, None).unwrap();
        }
        let error = packet_order(&components, 65535, 0).unwrap_err();
        assert!(error.contains("packet count"), "{error}");
    }
}
