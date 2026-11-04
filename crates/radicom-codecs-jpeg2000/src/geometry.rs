//! Tile, component, resolution, precinct and code-block geometry, ported from
//! the pure C# reference implementation in `fo-dicom.PureCodecs`
//! (`Jpeg2000StandardGeometry` / `Jpeg2000StandardModel`) and ISO/IEC
//! 15444-1 Annex B.

use std::collections::BTreeMap;

pub(crate) fn floor_log2(value: i32) -> i32 {
    let mut value = value;
    let mut result = 0;
    while value > 1 {
        value >>= 1;
        result += 1;
    }
    result
}

pub(crate) fn ceil_div_pow2(value: i32, power: i32) -> i32 {
    ((i64::from(value) + (1i64 << power) - 1) >> power) as i32
}

pub(crate) fn floor_div_pow2(value: i32, power: i32) -> i32 {
    value >> power
}

/// i64 variant of [`ceil_div_pow2`] for callers whose numerator is a
/// difference against `2^level`; a legal codestream may declare up to 32
/// decomposition levels, which overflows `i32` before the shift.
fn ceil_div_pow2_i64(value: i64, power: i32) -> i32 {
    ((value + (1i64 << power) - 1) >> power) as i32
}

pub(crate) fn split_length(length: i32, even: bool) -> i32 {
    if even {
        (length + 1) / 2
    } else {
        length / 2
    }
}

pub(crate) fn is_even(value: i32) -> bool {
    value & 1 == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolutionGeometry {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) x0: i32,
    pub(crate) y0: i32,
}

pub(crate) fn resolution_geometry(
    width: i32,
    height: i32,
    x0: i32,
    y0: i32,
    levels: i32,
    resolution: i32,
) -> ResolutionGeometry {
    let level_no = levels - resolution;
    let mut res_width = width;
    let mut res_height = height;
    let mut res_x0 = x0;
    let mut res_y0 = y0;
    for _ in 0..level_no {
        res_width = split_length(res_width, is_even(res_x0));
        res_height = split_length(res_height, is_even(res_y0));
        res_x0 = (res_x0 + 1) >> 1;
        res_y0 = (res_y0 + 1) >> 1;
    }
    ResolutionGeometry {
        width: res_width,
        height: res_height,
        x0: res_x0,
        y0: res_y0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubbandGeometry {
    pub(crate) orientation: u8,
    pub(crate) offset_x: i32,
    pub(crate) offset_y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) band_x0: i32,
    pub(crate) band_y0: i32,
    pub(crate) band_x1: i32,
    pub(crate) band_y1: i32,
}

pub(crate) fn subband_geometries(
    width: i32,
    height: i32,
    x0: i32,
    y0: i32,
    levels: i32,
    resolution: i32,
) -> Vec<SubbandGeometry> {
    let current = resolution_geometry(width, height, x0, y0, levels, resolution);
    let level_no = levels - resolution;
    if resolution == 0 {
        return vec![SubbandGeometry {
            orientation: 0,
            offset_x: 0,
            offset_y: 0,
            width: current.width,
            height: current.height,
            band_x0: ceil_div_pow2(x0, level_no),
            band_y0: ceil_div_pow2(y0, level_no),
            band_x1: ceil_div_pow2(x0 + width, level_no),
            band_y1: ceil_div_pow2(y0 + height, level_no),
        }];
    }
    let previous = resolution_geometry(width, height, x0, y0, levels, resolution - 1);
    let high_width = current.width - previous.width;
    let high_height = current.height - previous.height;
    vec![
        high_subband(
            1,
            previous.width,
            0,
            high_width,
            previous.height,
            x0,
            y0,
            width,
            height,
            level_no,
        ),
        high_subband(
            2,
            0,
            previous.height,
            previous.width,
            high_height,
            x0,
            y0,
            width,
            height,
            level_no,
        ),
        high_subband(
            3,
            previous.width,
            previous.height,
            high_width,
            high_height,
            x0,
            y0,
            width,
            height,
            level_no,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn high_subband(
    orientation: u8,
    offset_x: i32,
    offset_y: i32,
    width: i32,
    height: i32,
    tile_x0: i32,
    tile_y0: i32,
    tile_width: i32,
    tile_height: i32,
    level_no: i32,
) -> SubbandGeometry {
    let x0b = i32::from(orientation & 1);
    let y0b = i32::from(orientation >> 1);
    SubbandGeometry {
        orientation,
        offset_x,
        offset_y,
        width: width.max(0),
        height: height.max(0),
        // Compute the level offset in i64: with the codestream-legal 32 levels,
        // `x0b << level_no` reaches 2^31 and the subsequent `i32` subtraction
        // overflows for a small tile.
        band_x0: ceil_div_pow2_i64(
            i64::from(tile_x0) - (i64::from(x0b) << level_no),
            level_no + 1,
        ),
        band_y0: ceil_div_pow2_i64(
            i64::from(tile_y0) - (i64::from(y0b) << level_no),
            level_no + 1,
        ),
        band_x1: ceil_div_pow2_i64(
            i64::from(tile_x0) + i64::from(tile_width) - (i64::from(x0b) << level_no),
            level_no + 1,
        ),
        band_y1: ceil_div_pow2_i64(
            i64::from(tile_y0) + i64::from(tile_height) - (i64::from(y0b) << level_no),
            level_no + 1,
        ),
    }
}

/// One code block inside a precinct band.
#[derive(Debug, Clone)]
pub(crate) struct CodeBlock {
    #[allow(dead_code)]
    pub(crate) index: usize,
    #[allow(dead_code)]
    pub(crate) orientation: u8,
    pub(crate) local_x: i32,
    pub(crate) local_y: i32,
    pub(crate) x0: i32,
    pub(crate) y0: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) total_passes: u32,
    pub(crate) zero_bitplanes: u8,
    pub(crate) data: Vec<u8>,
    pub(crate) segments: Vec<CodeBlockSegment>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodeBlockSegment {
    pub(crate) data: Vec<u8>,
    pub(crate) pass_count: u32,
}

/// A band inside one precinct.
#[derive(Debug, Clone)]
pub(crate) struct PrecinctBand {
    pub(crate) orientation: u8,
    pub(crate) code_block_count_x: i32,
    pub(crate) code_block_count_y: i32,
    pub(crate) code_blocks: Vec<CodeBlock>,
}

/// Upper bound for the number of samples of one image (all components
/// together), checked before any per-component allocation.
pub(crate) const MAX_SAMPLES: u64 = 1 << 28;

/// Upper bound on the number of precincts whose state is materialised for one
/// tile. A hostile SIZ/COD pair (tiny precinct exponent over a large image) can
/// otherwise turn a few dozen bytes into millions of `BTreeMap` entries before
/// any packet is validated. Realistic DICOM streams use the default 15/15
/// precincts (one precinct per resolution) or modest multi-precinct grids that
/// stay far below this; the bound keeps worst-case precinct state around a few
/// hundred MB instead of gigabytes.
pub(crate) const MAX_PRECINCTS: u64 = 1 << 22;

/// Validates an image area and returns it as a sample count.
pub(crate) fn checked_area(width: u64, height: u64) -> Result<u64, String> {
    let area = width
        .checked_mul(height)
        .ok_or_else(|| "image area overflows".to_string())?;
    if area > MAX_SAMPLES {
        return Err("image exceeds the supported sample count".into());
    }
    Ok(area)
}

/// Validates the total sample count of a multi-component image. Every
/// component has the full image area because subsampling is rejected.
pub(crate) fn checked_total(width: u64, height: u64, components: usize) -> Result<u64, String> {
    let area = checked_area(width, height)?;
    let total = area
        .checked_mul(components as u64)
        .ok_or_else(|| "image sample count overflows".to_string())?;
    if total > MAX_SAMPLES {
        return Err("image exceeds the supported sample count".into());
    }
    Ok(area)
}

/// One component of one tile.
#[derive(Debug, Clone)]
pub(crate) struct Component {
    #[allow(dead_code)]
    pub(crate) index: usize,
    pub(crate) x0: i32,
    pub(crate) y0: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) levels: i32,
    pub(crate) precision: u8,
    pub(crate) signed: bool,
    pub(crate) coefficients: Vec<i32>,
    pub(crate) irreversible: Option<Vec<f64>>,
    pub(crate) samples: Vec<i32>,
    pub(crate) float_samples: Option<Vec<f64>>,
    bands: BTreeMap<i32, BTreeMap<i32, Vec<PrecinctBand>>>,
}

impl Component {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        index: usize,
        x0: i32,
        y0: i32,
        width: i32,
        height: i32,
        levels: i32,
        precision: u8,
        signed: bool,
    ) -> Result<Self, String> {
        let area = checked_area(width.max(0) as u64, height.max(0) as u64)?;
        Ok(Self {
            index,
            x0,
            y0,
            width,
            height,
            levels,
            precision,
            signed,
            coefficients: vec![0; area as usize],
            irreversible: None,
            samples: Vec::new(),
            float_samples: None,
            bands: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn precincts(&self, resolution: i32) -> Vec<i32> {
        self.bands
            .get(&resolution)
            .map(|map| {
                map.iter()
                    .filter(|(_, bands)| !bands.is_empty())
                    .map(|(precinct, _)| *precinct)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn precincts_including_empty(&self, resolution: i32) -> Vec<i32> {
        self.bands
            .get(&resolution)
            .map(|map| map.keys().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) fn has_precinct(&self, resolution: i32, precinct: i32) -> bool {
        self.bands
            .get(&resolution)
            .is_some_and(|map| map.contains_key(&precinct))
    }

    pub(crate) fn bands(&self, resolution: i32, precinct: i32) -> &[PrecinctBand] {
        self.bands
            .get(&resolution)
            .and_then(|map| map.get(&precinct))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn bands_mut(
        &mut self,
        resolution: i32,
        precinct: i32,
    ) -> Option<&mut Vec<PrecinctBand>> {
        self.bands
            .get_mut(&resolution)
            .and_then(|map| map.get_mut(&precinct))
    }

    #[cfg(test)]
    pub(crate) fn all_code_blocks(&self) -> Vec<CodeBlock> {
        let mut blocks = Vec::new();
        for precincts in self.bands.values() {
            for bands in precincts.values() {
                for band in bands {
                    blocks.extend(band.code_blocks.iter().cloned());
                }
            }
        }
        blocks
    }

    pub(crate) fn build_code_blocks(
        &mut self,
        code_block_width: i32,
        code_block_height: i32,
        precinct_sizes: Option<&[(u8, u8)]>,
    ) -> Result<(), String> {
        if !self.bands.is_empty() {
            return Ok(());
        }
        if code_block_width <= 0 || code_block_height <= 0 {
            return Err("invalid code-block size".into());
        }
        let cbw_exp = floor_log2(code_block_width);
        let cbh_exp = floor_log2(code_block_height);
        if 1i64 << cbw_exp != i64::from(code_block_width)
            || 1i64 << cbh_exp != i64::from(code_block_height)
        {
            return Err("code-block size is not a power of two".into());
        }
        let mut global_index = 0usize;
        let mut total_precincts = 0u64;
        for resolution in 0..=self.levels {
            let subbands = subband_geometries(
                self.width,
                self.height,
                self.x0,
                self.y0,
                self.levels,
                resolution,
            );
            let resolution_geom = resolution_geometry(
                self.width,
                self.height,
                self.x0,
                self.y0,
                self.levels,
                resolution,
            );
            let (px_exp, py_exp) = precinct_sizes
                .and_then(|sizes| sizes.get(resolution as usize).copied())
                .unwrap_or((15, 15));
            let precinct_info =
                PrecinctInfo::new(resolution_geom, i32::from(px_exp), i32::from(py_exp))?;
            // Bound the precinct state before allocating the BTreeMap: a
            // malformed codestream must not be able to amplify a tiny header
            // into hundreds of megabytes of empty precinct entries.
            let precincts =
                u64::from(precinct_info.count_x as u32) * u64::from(precinct_info.count_y as u32);
            total_precincts = total_precincts
                .checked_add(precincts)
                .ok_or_else(|| "precinct count overflow".to_string())?;
            if total_precincts > MAX_PRECINCTS {
                return Err("precinct count exceeds the supported limit".into());
            }
            let mut precinct_map: BTreeMap<i32, Vec<PrecinctBand>> = BTreeMap::new();
            for y in 0..precinct_info.count_y {
                for x in 0..precinct_info.count_x {
                    precinct_map.insert(x + y * precinct_info.count_x, Vec::new());
                }
            }
            self.bands.insert(resolution, precinct_map);

            for subband in subbands {
                if subband.width == 0 || subband.height == 0 {
                    continue;
                }
                let block_x_start = floor_div_pow2(subband.band_x0, cbw_exp) << cbw_exp;
                let block_y_start = floor_div_pow2(subband.band_y0, cbh_exp) << cbh_exp;
                let block_x_end = ceil_div_pow2(subband.band_x1, cbw_exp) << cbw_exp;
                let block_y_end = ceil_div_pow2(subband.band_y1, cbh_exp) << cbh_exp;
                let block_count_x = (block_x_end - block_x_start) >> cbw_exp;
                let block_count_y = (block_y_end - block_y_start) >> cbh_exp;
                if block_count_x <= 0 || block_count_y <= 0 {
                    continue;
                }
                for precinct_y in 0..precinct_info.count_y {
                    for precinct_x in 0..precinct_info.count_x {
                        let precinct_index = precinct_x + precinct_y * precinct_info.count_x;
                        let window = precinct_info.subband_window(
                            &subband, resolution, precinct_x, precinct_y, cbw_exp, cbh_exp,
                        );
                        if window.width <= 0 || window.height <= 0 {
                            continue;
                        }
                        let mut band = PrecinctBand {
                            orientation: subband.orientation,
                            code_block_count_x: window.width.max(1),
                            code_block_count_y: window.height.max(1),
                            code_blocks: Vec::new(),
                        };
                        for local_y in 0..window.height {
                            let block_y = window.y + local_y;
                            if block_y < 0 || block_y >= block_count_y {
                                continue;
                            }
                            for local_x in 0..window.width {
                                let block_x = window.x + local_x;
                                if block_x < 0 || block_x >= block_count_x {
                                    continue;
                                }
                                let nominal_x0 = block_x_start + block_x * code_block_width;
                                let nominal_y0 = block_y_start + block_y * code_block_height;
                                let code_block_x0 = nominal_x0.max(subband.band_x0);
                                let code_block_y0 = nominal_y0.max(subband.band_y0);
                                let code_block_x1 =
                                    (nominal_x0 + code_block_width).min(subband.band_x1);
                                let code_block_y1 =
                                    (nominal_y0 + code_block_height).min(subband.band_y1);
                                if code_block_x1 <= code_block_x0 || code_block_y1 <= code_block_y0
                                {
                                    continue;
                                }
                                let coefficient_x0 =
                                    subband.offset_x + code_block_x0 - subband.band_x0;
                                let coefficient_y0 =
                                    subband.offset_y + code_block_y0 - subband.band_y0;
                                band.code_blocks.push(CodeBlock {
                                    index: global_index,
                                    orientation: subband.orientation,
                                    local_x,
                                    local_y,
                                    x0: coefficient_x0,
                                    y0: coefficient_y0,
                                    width: code_block_x1 - code_block_x0,
                                    height: code_block_y1 - code_block_y0,
                                    total_passes: 0,
                                    zero_bitplanes: 0,
                                    data: Vec::new(),
                                    segments: Vec::new(),
                                });
                                global_index += 1;
                            }
                        }
                        if let Some(list) = self
                            .bands
                            .get_mut(&resolution)
                            .and_then(|map| map.get_mut(&precinct_index))
                        {
                            list.push(band);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct PrecinctInfo {
    width_exp: i32,
    height_exp: i32,
    count_x: i32,
    count_y: i32,
    left: i32,
    top: i32,
    resolution: ResolutionGeometry,
}

impl PrecinctInfo {
    fn new(
        resolution: ResolutionGeometry,
        width_exp: i32,
        height_exp: i32,
    ) -> Result<Self, String> {
        if !(0..=15).contains(&width_exp) || !(0..=15).contains(&height_exp) {
            return Err("precinct exponent exceeds 15".into());
        }
        let x1 = resolution.x0 + resolution.width;
        let y1 = resolution.y0 + resolution.height;
        let count_x =
            (ceil_div_pow2(x1, width_exp) - floor_div_pow2(resolution.x0, width_exp)).max(1);
        let count_y =
            (ceil_div_pow2(y1, height_exp) - floor_div_pow2(resolution.y0, height_exp)).max(1);
        if u64::from(count_x as u32) * u64::from(count_y as u32) > MAX_PRECINCTS {
            return Err("precinct count exceeds the supported limit".into());
        }
        let left = floor_div_pow2(resolution.x0, width_exp) << width_exp;
        let top = floor_div_pow2(resolution.y0, height_exp) << height_exp;
        Ok(Self {
            width_exp,
            height_exp,
            count_x,
            count_y,
            left,
            top,
            resolution,
        })
    }

    fn subband_window(
        &self,
        subband: &SubbandGeometry,
        resolution: i32,
        precinct_x: i32,
        precinct_y: i32,
        cbw_exp: i32,
        cbh_exp: i32,
    ) -> Window {
        let res_x0 = self.resolution.x0;
        let res_y0 = self.resolution.y0;
        let res_x1 = self.resolution.x0 + self.resolution.width;
        let res_y1 = self.resolution.y0 + self.resolution.height;
        let region_x0 = res_x0.max(self.left + (precinct_x << self.width_exp));
        let region_x1 = res_x1.min(self.left + ((precinct_x + 1) << self.width_exp));
        let region_y0 = res_y0.max(self.top + (precinct_y << self.height_exp));
        let region_y1 = res_y1.min(self.top + ((precinct_y + 1) << self.height_exp));
        let shift = if resolution == 0 { 0 } else { 1 };
        let region_x0 = ceil_div_pow2(region_x0 - i32::from(subband.orientation & 1), shift);
        let region_x1 = ceil_div_pow2(region_x1 - i32::from(subband.orientation & 1), shift);
        let region_y0 = ceil_div_pow2(region_y0 - i32::from(subband.orientation >> 1), shift);
        let region_y1 = ceil_div_pow2(region_y1 - i32::from(subband.orientation >> 1), shift);
        let x0 = floor_div_pow2(region_x0, cbw_exp) - floor_div_pow2(subband.band_x0, cbw_exp);
        let x1 = ceil_div_pow2(region_x1, cbw_exp) - floor_div_pow2(subband.band_x0, cbw_exp);
        let y0 = floor_div_pow2(region_y0, cbh_exp) - floor_div_pow2(subband.band_y0, cbh_exp);
        let y1 = ceil_div_pow2(region_y1, cbh_exp) - floor_div_pow2(subband.band_y0, cbh_exp);
        Window {
            x: x0,
            y: y0,
            width: (x1 - x0).max(0),
            height: (y1 - y0).max(0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Window {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_and_subband_bounds_match_the_reference() {
        let r = resolution_geometry(65, 49, 0, 0, 2, 0);
        assert_eq!((r.width, r.height), (17, 13));
        let bands = subband_geometries(65, 49, 0, 0, 2, 2);
        assert_eq!(bands.len(), 3);
        assert_eq!((bands[0].width, bands[0].height), (32, 25));
        assert_eq!((bands[1].width, bands[1].height), (33, 24));
        assert_eq!((bands[2].width, bands[2].height), (32, 24));
    }

    #[test]
    fn builds_single_precinct_code_blocks() {
        let mut c = Component::new(0, 0, 0, 65, 49, 2, 8, false).unwrap();
        c.build_code_blocks(64, 64, None).unwrap();
        assert_eq!(c.precincts(0), vec![0]);
        assert_eq!(c.precincts(2), vec![0]);
        let blocks = c.all_code_blocks();
        assert!(blocks.len() >= 4);
        assert!(blocks.iter().all(|b| b.width > 0 && b.height > 0));
    }

    #[test]
    fn multi_precinct_geometry_is_supported() {
        let mut c = Component::new(0, 0, 0, 128, 128, 1, 8, false).unwrap();
        c.build_code_blocks(32, 32, Some(&[(5, 5), (5, 5)]))
            .unwrap();
        assert!(c.precincts(1).len() > 1);
    }

    #[test]
    fn oversized_geometry_is_rejected_before_allocation() {
        assert!(Component::new(0, 0, 0, 65535, 65535, 5, 8, false).is_err());
        assert!(checked_total(65535, 65535, 3).is_err());
        assert!(checked_total(16384, 16384, 1).is_ok());
        assert!(checked_total(16384, 16384, 2).is_err());
    }

    #[test]
    fn precinct_exponent_beyond_the_standard_is_rejected() {
        let mut c = Component::new(0, 0, 0, 32, 32, 0, 8, false).unwrap();
        assert!(c.build_code_blocks(64, 64, Some(&[(16, 5)])).is_err());
    }

    /// A codestream may legally declare 32 decomposition levels; computing the
    /// highest-resolution subband geometry must not overflow `i32` (regression
    /// for a decode panic on a 1x1 SIZ with `decompositions = 32`).
    #[test]
    fn extreme_decomposition_levels_do_not_overflow() {
        let bands = subband_geometries(8, 8, 0, 0, 32, 1);
        assert_eq!(bands.len(), 3);
        for band in bands {
            assert!(band.band_x0 <= band.band_x1);
            assert!(band.band_y0 <= band.band_y1);
        }
    }

    #[test]
    fn oversized_precinct_count_is_rejected_before_allocation() {
        // 4096x4096 with precinct exponent 0 is ~16.7M precincts; this must be
        // rejected by the geometry check before the BTreeMap is populated.
        let geometry = ResolutionGeometry {
            width: 4096,
            height: 4096,
            x0: 0,
            y0: 0,
        };
        let error = PrecinctInfo::new(geometry, 0, 0).unwrap_err();
        assert!(error.contains("precinct count"), "{error}");
    }
}
