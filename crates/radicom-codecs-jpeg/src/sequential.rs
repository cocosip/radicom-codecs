#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::needless_range_loop,
    clippy::type_complexity
)]

use super::bit::{BitReader, BitWriter};
use super::dct::{forward, inverse, scaled_table, CHROMA_Q, LUMA_Q, MAX_QUANTIZATION, ZIGZAG};
use super::huffman::HuffmanTable;
use super::marker::{check_trailing, Marker, MarkerReader, MarkerWriter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequentialFrame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) precision: u8,
    pub(crate) components: u8,
    pub(crate) samples: Vec<u16>,
    /// Per-component (horizontal, vertical) sampling factors; `[(1, 1); 3]` is
    /// 4:4:4. Decoded frames carry whatever the codestream declared, encoded
    /// frames only ever use 4:4:4 or the 4:2:2 `[(2, 1), (1, 1), (1, 1)]`.
    pub(crate) sampling: [(u8, u8); 3],
}

struct Spec {
    counts: Vec<u8>,
    values: Vec<u8>,
    table: HuffmanTable,
}

impl Spec {
    fn new(counts: Vec<u8>, values: Vec<u8>) -> Result<Self, String> {
        let table = HuffmanTable::new(&counts, &values)?;
        Ok(Self {
            counts,
            values,
            table,
        })
    }

    /// Per-image table from the scan statistics, as libjpeg's
    /// `optimize_coding` does (T.81 Annex K.2).
    fn optimal(frequencies: &[u32; 256]) -> Result<Self, String> {
        let (counts, values) = HuffmanTable::optimal(frequencies);
        Self::new(counts, values)
    }
}

struct Tables {
    dc: [Spec; 2],
    ac: [Spec; 2],
    q: [[u16; 64]; 2],
}

impl Tables {
    fn new(
        q: [[u16; 64]; 2],
        dc_frequencies: &[[u32; 256]; 2],
        ac_frequencies: &[[u32; 256]; 2],
    ) -> Result<Self, String> {
        Ok(Self {
            dc: [
                Spec::optimal(&dc_frequencies[0])?,
                Spec::optimal(&dc_frequencies[1])?,
            ],
            ac: [
                Spec::optimal(&ac_frequencies[0])?,
                Spec::optimal(&ac_frequencies[1])?,
            ],
            q,
        })
    }
}

fn quant_tables(quality: u32, extended: bool) -> [[u16; 64]; 2] {
    let max_qval = if extended { MAX_QUANTIZATION } else { 255 };
    // The extended processes mirror libjpeg's force_baseline = FALSE (what
    // fo-dicom's IJG path uses), so entries may exceed 255.
    [
        scaled_table(&LUMA_Q, quality, max_qval),
        scaled_table(&CHROMA_Q, quality, max_qval),
    ]
}

/// One step of the block walk shared by the statistics and the entropy passes.
enum ScanStep<'a> {
    Block {
        slot: usize,
        diff: i32,
        block: &'a [i32; 64],
    },
    Restart(u8),
}

/// How one component of a scan is pooled into block samples: the source step in
/// each direction, the component itself, and libjpeg's input smoothing factor.
#[derive(Clone, Copy)]
struct Pooling {
    step_x: usize,
    step_y: usize,
    component: usize,
    smoothing: u32,
}
impl SequentialFrame {
    pub(crate) fn new(
        width: usize,
        height: usize,
        precision: u8,
        components: u8,
        samples: Vec<u16>,
    ) -> Result<Self, String> {
        if width == 0
            || height == 0
            || !(1..=3).contains(&components)
            || !matches!(precision, 8 | 12)
        {
            return Err("unsupported JPEG sequential dimensions, components, or precision".into());
        }
        let count = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(usize::from(components)))
            .ok_or("JPEG sample count overflows usize")?;
        if samples.len() != count || samples.iter().any(|&v| u32::from(v) >= (1u32 << precision)) {
            return Err("JPEG sample exceeds precision".into());
        }
        Ok(Self {
            width,
            height,
            precision,
            components,
            samples,
            sampling: [(1, 1); 3],
        })
    }

    /// Encode with libjpeg's input smoothing_factor (`0..=100`). It only shapes
    /// the encoder input; DICOM has no decode-side field for it.
    pub(crate) fn encode_with_smoothing(
        &self,
        quality: u32,
        extended: bool,
        sampling_half: bool,
        smoothing: u32,
        restart_interval: Option<u16>,
    ) -> Result<Vec<u8>, String> {
        let components = (0..self.components).collect::<Vec<_>>();
        self.encode_scans(
            quality,
            extended,
            sampling_half,
            smoothing,
            restart_interval,
            std::slice::from_ref(&components),
        )
    }

    /// Encode one or more scans. A scan is a list of component indices carried
    /// by that scan; T.81 permits a non-interleaved scan per component.
    fn encode_scans(
        &self,
        quality: u32,
        extended: bool,
        sampling_half: bool,
        smoothing: u32,
        restart_interval: Option<u16>,
        scans: &[Vec<u8>],
    ) -> Result<Vec<u8>, String> {
        if sampling_half && self.components != 3 {
            return Err("JPEG 4:2:2 sampling requires three components".into());
        }
        // DICOM only allows 4:4:4 and 4:2:2 for 8-bit lossy JPEG. Re-encoding a
        // frame that another layout decoded would silently resample it, so the
        // wider T.81 layouts stay decode-only.
        if self.sampling != [(1, 1); 3] && self.sampling != [(2, 1), (1, 1), (1, 1)] {
            return Err(format!(
                "JPEG encoder supports 4:4:4 (1x1) and 4:2:2 (2x1) sampling only; {:?} is decode-only",
                &self.sampling[..usize::from(self.components)]
            ));
        }
        let sampling = if sampling_half {
            [(2, 1), (1, 1), (1, 1)]
        } else {
            [(1, 1); 3]
        };
        self.encode_scans_with(
            quality,
            extended,
            &sampling,
            smoothing,
            restart_interval,
            scans,
        )
    }

    /// Encode with an explicit sampling matrix. The public encoder only writes
    /// 4:4:4 and 4:2:2; the wider support exists so tests can build the 4:2:0
    /// and other T.81 layouts that this decoder accepts.
    fn encode_scans_with(
        &self,
        quality: u32,
        extended: bool,
        sampling: &[(u8, u8); 3],
        smoothing: u32,
        restart_interval: Option<u16>,
        scans: &[Vec<u8>],
    ) -> Result<Vec<u8>, String> {
        if quality > 100 {
            return Err("JPEG quality must be 0..=100".into());
        }
        if smoothing > 100 {
            return Err("JPEG smoothing_factor must be 0..=100".into());
        }
        let (hmax, vmax) = max_sampling(sampling, self.components);
        for c in 0..usize::from(self.components) {
            let (h, v) = sampling[c];
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                return Err(format!("JPEG sampling factor {h}x{v} must be 1..=4"));
            }
            if hmax % h != 0 || vmax % v != 0 {
                return Err(format!(
                    "JPEG sampling factor {h}x{v} does not divide the frame maximum {hmax}x{vmax}"
                ));
            }
        }
        let restart_interval = restart_interval.filter(|&interval| interval != 0);
        let mut seen = [false; 3];
        for scan in scans {
            if scan.is_empty() {
                return Err("JPEG scan has no components".to_owned());
            }
            for &component in scan {
                let slot = usize::from(component);
                if slot >= usize::from(self.components) {
                    return Err(format!(
                        "JPEG scan references component {component} outside the frame"
                    ));
                }
                if seen[slot] {
                    return Err(format!("JPEG scan repeats component {component}"));
                }
                seen[slot] = true;
            }
        }
        let q = quant_tables(quality.max(1), extended);
        let mut dc_frequencies = [[0u32; 256]; 2];
        let mut ac_frequencies = [[0u32; 256]; 2];
        for scan in scans {
            self.walk_scan(&q, sampling, smoothing, scan, restart_interval, |step| {
                if let ScanStep::Block { slot, diff, block } = step {
                    dc_frequencies[slot][usize::from(category(diff))] += 1;
                    for_each_ac(block, |symbol, _, _| {
                        ac_frequencies[slot][usize::from(symbol)] += 1;
                        Ok(())
                    })?;
                }
                Ok(())
            })?;
        }
        let tables = Tables::new(q, &dc_frequencies, &ac_frequencies)?;
        let mut out = MarkerWriter::default();
        out.write_marker(Marker::Soi)?;
        out.write_segment(Marker::Dqt, &dqt_payload(&tables.q)?)?;
        out.write_segment(
            if extended { Marker::Sof1 } else { Marker::Sof0 },
            &sof_payload(self, sampling),
        )?;
        out.write_segment(Marker::Dht, &dht_payload(&tables))?;
        if let Some(interval) = restart_interval {
            out.write_segment(Marker::Dri, &interval.to_be_bytes())?;
        }
        for scan in scans {
            out.write_segment(Marker::Sos, &sos_payload(scan))?;
            self.write_scan(
                &mut out,
                &tables,
                sampling,
                smoothing,
                scan,
                restart_interval,
            )?;
        }
        out.write_marker(Marker::Eoi)?;
        Ok(out.into_bytes())
    }

    /// Walks one scan's blocks in coding order, yielding the quantized block and
    /// its DC difference. The statistics pass and the entropy pass both use it,
    /// so they always agree on the symbol sequence.
    fn walk_scan(
        &self,
        q: &[[u16; 64]; 2],
        sampling: &[(u8, u8); 3],
        smoothing: u32,
        components: &[u8],
        restart_interval: Option<u16>,
        mut visit: impl FnMut(ScanStep<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let interleaved = components.len() > 1;
        let (hmax, vmax) = max_sampling(sampling, self.components);
        let (mcu_w, mcu_h) = scan_grid(
            self.width,
            self.height,
            (hmax, vmax),
            interleaved,
            sampling[usize::from(components[0])],
        );
        let mut previous_dc = [0i32; 3];
        let mut mcu = 0usize;
        let mut restart_index = 0u8;
        for my in 0..mcu_h {
            for mx in 0..mcu_w {
                if let Some(interval) = restart_interval {
                    if mcu != 0 && mcu.is_multiple_of(usize::from(interval)) {
                        visit(ScanStep::Restart(restart_index))?;
                        restart_index = (restart_index + 1) & 7;
                        previous_dc = [0i32; 3];
                    }
                }
                mcu += 1;
                for &component in components {
                    let c = usize::from(component);
                    let (h, v) = sampling[c];
                    let (blocks_x, blocks_y) = if interleaved {
                        (usize::from(h), usize::from(v))
                    } else {
                        (1, 1)
                    };
                    let (step_x, step_y) = upsampling(sampling, self.components, c);
                    let pooling = Pooling {
                        step_x,
                        step_y,
                        component: c,
                        smoothing,
                    };
                    for local_y in 0..blocks_y {
                        for local_x in 0..blocks_x {
                            let block_x = mx * blocks_x + local_x;
                            let block_y = my * blocks_y + local_y;
                            let mut block = [0i32; 64];
                            self.load_block(pooling, block_x, block_y, &mut block);
                            let coeff = forward(&block);
                            let table = &q[c.min(1)];
                            let mut quant = [0i32; 64];
                            for i in 0..64 {
                                quant[i] =
                                    (f64::from(coeff[i]) / f64::from(table[i])).round() as i32;
                            }
                            let dc = quant[0];
                            let diff = dc - previous_dc[c];
                            previous_dc[c] = dc;
                            visit(ScanStep::Block {
                                slot: c.min(1),
                                diff,
                                block: &quant,
                            })?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn write_scan(
        &self,
        out: &mut MarkerWriter,
        tables: &Tables,
        sampling: &[(u8, u8); 3],
        smoothing: u32,
        components: &[u8],
        restart_interval: Option<u16>,
    ) -> Result<(), String> {
        let mut bits = BitWriter::default();
        self.walk_scan(
            &tables.q,
            sampling,
            smoothing,
            components,
            restart_interval,
            |step| match step {
                ScanStep::Restart(index) => {
                    bits.flush_to_byte();
                    out.write_raw(&bits.take_bytes());
                    out.write_marker(Marker::Restart(index))
                }
                ScanStep::Block { slot, diff, block } => {
                    write_value(&mut bits, &tables.dc[slot].table, diff)?;
                    for_each_ac(block, |symbol, value, size| {
                        tables.ac[slot].table.encode(symbol, &mut bits)?;
                        if size > 0 {
                            bits.write_bits(value, size)?;
                        }
                        Ok(())
                    })
                }
            },
        )?;
        out.write_raw(&bits.finish()?);
        Ok(())
    }

    fn load_block(&self, pooling: Pooling, block_x: usize, block_y: usize, block: &mut [i32; 64]) {
        let offset = 1 << (self.precision - 1);
        for y in 0..8 {
            for x in 0..8 {
                let sample_x = block_x * 8 + x;
                let sample_y = block_y * 8 + y;
                let value = if pooling.smoothing == 0 {
                    self.cell_sample(pooling, sample_x, sample_y)
                } else {
                    self.smooth_sample(pooling, sample_x, sample_y)
                };
                block[y * 8 + x] = value as i32 - offset;
            }
        }
    }

    /// Average the `step_x * step_y` source pixels behind one downsampled
    /// sample. Source coordinates clamp to the last pixel and the sum rounds up,
    /// which is the existing (and reference) downsampling behavior.
    fn cell_sample(&self, pooling: Pooling, u: usize, v: usize) -> u32 {
        let mut sum = 0u32;
        for dy in 0..pooling.step_y {
            let row = (v * pooling.step_y + dy).min(self.height - 1);
            for dx in 0..pooling.step_x {
                let column = (u * pooling.step_x + dx).min(self.width - 1);
                sum += self.pixel(pooling.component, column, row);
            }
        }
        sum.div_ceil((pooling.step_x * pooling.step_y) as u32)
    }

    /// One source sample with libjpeg's edge replication (`expand_right_edge`).
    fn pixel(&self, component: usize, x: usize, y: usize) -> u32 {
        let stride = usize::from(self.components);
        let column = x.min(self.width - 1);
        let row = y.min(self.height - 1);
        u32::from(self.samples[(row * self.width + column) * stride + component])
    }

    /// libjpeg's input smoothing (jcsample.c in libjpeg-turbo). Only full-size
    /// (1x1) and 2x2 components are smoothed: `h2v1_downsample` and
    /// `int_downsample` ignore the factor (`_jinit_downsampler` clears
    /// `smoothok` and traces `JTRC_SMOOTH_NOTIMPL`), and this libjpeg-turbo has
    /// no `h2v1_smooth_downsample`.
    fn smooth_sample(&self, pooling: Pooling, u: usize, v: usize) -> u32 {
        match (pooling.step_x, pooling.step_y) {
            (1, 1) => self.fullsize_smooth_sample(pooling, u, v),
            (2, 2) => self.h2v2_smooth_sample(pooling, u, v),
            _ => self.cell_sample(pooling, u, v),
        }
    }

    /// `fullsize_smooth_downsample` (jcsample.c:396-456): the eight neighbours
    /// contribute `SF` each and the sample itself `1-8*SF`, scaled by 2^16.
    fn fullsize_smooth_sample(&self, pooling: Pooling, u: usize, v: usize) -> u32 {
        debug_assert!(pooling.smoothing <= 100);
        let memberscale = 65536 - pooling.smoothing * 512;
        let neighscale = pooling.smoothing * 64;
        let colsum = |x: usize| {
            self.pixel(pooling.component, x, v.saturating_sub(1))
                + self.pixel(pooling.component, x, v + 1)
                + self.pixel(pooling.component, x, v)
        };
        let membersum = self.pixel(pooling.component, u, v);
        let lastcolsum = colsum(u.saturating_sub(1));
        let colsum_here = colsum(u);
        let nextcolsum = colsum(u + 1);
        let neighsum = lastcolsum + (colsum_here - membersum) + nextcolsum;
        ((u64::from(membersum) * u64::from(memberscale)
            + u64::from(neighsum) * u64::from(neighscale)
            + 32768)
            >> 16) as u32
    }

    /// `h2v2_smooth_downsample` (jcsample.c:307-389): each of the four samples
    /// behind the 2x2 downsample contributes `(1-5*SF)/4`, edge neighbours
    /// `SF/2` and corner neighbours `SF/4`, scaled by 2^16.
    fn h2v2_smooth_sample(&self, pooling: Pooling, u: usize, v: usize) -> u32 {
        debug_assert!(pooling.smoothing <= 100);
        let memberscale = 16384 - pooling.smoothing * 80;
        let neighscale = pooling.smoothing * 16;
        let x0 = 2 * u;
        let x1 = 2 * u + 1;
        let y0 = 2 * v;
        let y1 = 2 * v + 1;
        let membersum = self.pixel(pooling.component, x0, y0)
            + self.pixel(pooling.component, x1, y0)
            + self.pixel(pooling.component, x0, y1)
            + self.pixel(pooling.component, x1, y1);
        let above = y0.saturating_sub(1);
        let below = y1 + 1;
        let xm = x0.saturating_sub(1);
        let xp = 2 * u + 2;
        let mut neighsum = self.pixel(pooling.component, x0, above)
            + self.pixel(pooling.component, x1, above)
            + self.pixel(pooling.component, x0, below)
            + self.pixel(pooling.component, x1, below)
            + self.pixel(pooling.component, xm, y0)
            + self.pixel(pooling.component, xp, y0)
            + self.pixel(pooling.component, xm, y1)
            + self.pixel(pooling.component, xp, y1);
        neighsum *= 2;
        neighsum += self.pixel(pooling.component, xm, above)
            + self.pixel(pooling.component, xp, above)
            + self.pixel(pooling.component, xm, below)
            + self.pixel(pooling.component, xp, below);
        ((u64::from(membersum) * u64::from(memberscale)
            + u64::from(neighsum) * u64::from(neighscale)
            + 32768)
            >> 16) as u32
    }

    /// Decode one codestream. `expected` is the destination contract: it is
    /// checked against the SOF header before the frame buffer is allocated, so a
    /// hostile SOF cannot request a huge allocation. `baseline` is the process
    /// owned by the transfer syntax: `true` is Process 1 (`.50`, SOF0/8-bit/
    /// Pq=0), `false` is Process 2/4 (`.51`, SOF1).
    pub(crate) fn decode_expected(
        data: &[u8],
        expected: Option<&FrameExpectation>,
        baseline: bool,
    ) -> Result<Self, String> {
        let mut r = MarkerReader::new(data);
        expect(&mut r, Marker::Soi)?;
        let mut q = [[0u16; 64]; 4];
        let mut dcs: [Option<HuffmanTable>; 4] = [None, None, None, None];
        let mut acs: [Option<HuffmanTable>; 4] = [None, None, None, None];
        let mut restart_interval = None;
        let mut header = None;
        let mut samples = Vec::new();
        let mut decoded = [false; 3];
        loop {
            let (marker, payload) = r.read_segment()?;
            match marker {
                Marker::Dqt => parse_dqt(&payload, &mut q, baseline)?,
                Marker::Dht => parse_dht(&payload, &mut dcs, &mut acs)?,
                Marker::Dri => restart_interval = parse_dri(&payload)?,
                Marker::Sof0 | Marker::Sof1 => {
                    if header.is_some() {
                        return Err("JPEG frame has more than one SOF segment".to_owned());
                    }
                    let frame = parse_sof(&payload, marker, baseline)?;
                    if let Some(expected) = expected {
                        if frame.width != expected.width
                            || frame.height != expected.height
                            || frame.components != expected.components
                            || frame.precision != expected.precision
                        {
                            return Err("codestream layout does not match destination".into());
                        }
                    }
                    let count =
                        (frame.width as u64) * (frame.height as u64) * u64::from(frame.components);
                    if count > crate::MAX_DECODED_SAMPLES {
                        return Err(format!(
                            "JPEG SOF declares {count} samples, above the {} sample limit",
                            crate::MAX_DECODED_SAMPLES
                        ));
                    }
                    let count = usize::try_from(count)
                        .map_err(|_| "JPEG output size overflows usize".to_owned())?;
                    samples = allocate_samples(count)?;
                    header = Some(frame);
                }
                Marker::Sos => {
                    let frame = header.as_ref().ok_or("missing JPEG SOF")?;
                    let selectors =
                        parse_sos(&payload, &frame.ids[..usize::from(frame.components)])?;
                    let mut scan = Vec::with_capacity(selectors.len());
                    for &(component, dc_selector, ac_selector) in &selectors {
                        let slot = usize::from(component);
                        if decoded[slot] {
                            return Err(format!(
                                "JPEG component {} appears in more than one scan",
                                frame.ids[slot]
                            ));
                        }
                        let qid = frame.qids[slot];
                        if q[usize::from(qid)].contains(&0) {
                            return Err(format!(
                                "missing JPEG quantization table {qid} for component {component}"
                            ));
                        }
                        if dc_selector > 3 || ac_selector > 3 {
                            return Err("unsupported JPEG Huffman table selector".into());
                        }
                        let dc = dcs[usize::from(dc_selector)].as_ref().ok_or_else(|| {
                            format!("missing JPEG DC Huffman table {dc_selector}")
                        })?;
                        let ac = acs[usize::from(ac_selector)].as_ref().ok_or_else(|| {
                            format!("missing JPEG AC Huffman table {ac_selector}")
                        })?;
                        scan.push((component, dc, ac));
                    }
                    for &(component, _, _) in &selectors {
                        decoded[usize::from(component)] = true;
                    }
                    let start = r.position();
                    let consumed = decode_scan(
                        data,
                        start,
                        &scan,
                        frame,
                        &q,
                        restart_interval,
                        &mut samples,
                    )?;
                    r.seek(start + consumed);
                }
                Marker::Eoi => break,
                Marker::App(_) | Marker::Com => {}
                other => return Err(format!("unexpected JPEG marker {other:?}")),
            }
        }
        check_trailing(data, r.position())?;
        let header = header.ok_or("missing JPEG SOF")?;
        if decoded[..usize::from(header.components)]
            .iter()
            .any(|&seen| !seen)
        {
            return Err("JPEG stream does not decode every component".to_owned());
        }
        let mut frame = Self::new(
            header.width,
            header.height,
            header.precision,
            header.components,
            samples,
        )?;
        frame.sampling = header.sampling;
        Ok(frame)
    }

    /// Decode without a destination contract; the marker and entropy tests use
    /// this to exercise the codestream layers on their own. The process is
    /// inferred from the SOF marker so the strict SOF/precision/Pq rules still
    /// apply to crafted test streams.
    #[cfg(test)]
    pub(crate) fn decode(data: &[u8]) -> Result<Self, String> {
        Self::decode_expected(data, None, baseline_from_sof(data)?)
    }
}

/// Test-only helper: report whether the codestream's SOF is baseline (SOF0).
#[cfg(test)]
fn baseline_from_sof(data: &[u8]) -> Result<bool, String> {
    let mut reader = MarkerReader::new(data);
    let (first, _) = reader.read_segment()?;
    if first != Marker::Soi {
        return Err("expected SOI".to_owned());
    }
    loop {
        let (marker, _) = reader.read_segment()?;
        match marker {
            Marker::Sof0 => return Ok(true),
            Marker::Sof1 => return Ok(false),
            Marker::Sos | Marker::Eoi => return Err("no SOF before SOS/EOI".to_owned()),
            _ => {}
        }
    }
}

/// Destination contract the SOF header must satisfy before a frame buffer is
/// allocated. A crafted SOF may declare up to 65535x65535, so allocation waits
/// until the layout is known to fit the caller's destination.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameExpectation {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) components: u8,
    pub(crate) precision: u8,
}

/// Frame geometry shared by every scan of a sequential JPEG codestream.
#[derive(Debug)]
struct FrameHeader {
    width: usize,
    height: usize,
    precision: u8,
    components: u8,
    sampling: [(u8, u8); 3],
    qids: [u8; 3],
    ids: [u8; 3],
    /// T.81 Table F.1: baseline DC differences use categories 0..=11, the
    /// extended processes allow 0..=15.
    max_dc_category: u8,
}

/// Frame-level (horizontal, vertical) sampling maxima; T.81 A.2.3 normalizes
/// every component plane against them.
fn max_sampling(sampling: &[(u8, u8); 3], components: u8) -> (u8, u8) {
    let mut hmax = 1;
    let mut vmax = 1;
    for &(h, v) in &sampling[..usize::from(components)] {
        hmax = hmax.max(h);
        vmax = vmax.max(v);
    }
    (hmax, vmax)
}

/// Pixel expansion of one component: `Hmax/H_i` by `Vmax/V_i`.
fn upsampling(sampling: &[(u8, u8); 3], components: u8, c: usize) -> (usize, usize) {
    let (hmax, vmax) = max_sampling(sampling, components);
    let (h, v) = sampling[c];
    (usize::from(hmax / h), usize::from(vmax / v))
}

/// T.81 A.2.3/B.2.3 MCU grid. An interleaved scan is laid out on the frame MCU
/// grid; a non-interleaved scan covers exactly one component plane.
fn scan_grid(
    width: usize,
    height: usize,
    (hmax, vmax): (u8, u8),
    interleaved: bool,
    (h, v): (u8, u8),
) -> (usize, usize) {
    let (hmax, vmax) = (usize::from(hmax), usize::from(vmax));
    if interleaved {
        (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax))
    } else {
        let (h, v) = (usize::from(h), usize::from(v));
        (
            (width * h).div_ceil(hmax).div_ceil(8),
            (height * v).div_ceil(vmax).div_ceil(8),
        )
    }
}

/// Chroma (and any other subsampled component) upsampling for a decoded frame.
///
/// The sequential decoder replicates every component sample over its pixel
/// rectangle, so the samples at whole multiples of the sampling ratio are the
/// true plane samples. Rebuild each plane and run libjpeg-turbo's
/// `h2v1_fancy_upsample`, `h1v2_fancy_upsample` or `h2v2_fancy_upsample`
/// triangle filters (jdsample.c) so a converted RGB frame matches the reference
/// decoder. libjpeg rounds even output positions with a bias one higher than odd
/// positions so rounding ties alternate instead of biasing every sample
/// upwards. Ratios libjpeg does not filter — and 2:1 planes with two or fewer
/// samples — keep the plain replication, which is libjpeg's `int_upsample`.
pub(crate) fn fancy_upsample(
    samples: &mut [u16],
    width: usize,
    height: usize,
    precision: u8,
    components: u8,
    sampling: &[(u8, u8); 3],
) {
    if width == 0 || height == 0 {
        return;
    }
    let (hmax, vmax) = max_sampling(sampling, components);
    let stride = usize::from(components);
    let max = (1u32 << precision) - 1;
    let mut plane = Vec::new();
    let mut colsum = Vec::new();
    for c in 0..usize::from(components) {
        let (h, v) = sampling[c];
        let hx = usize::from(hmax / h);
        let vy = usize::from(vmax / v);
        if hx == 1 && vy == 1 {
            continue;
        }
        let plane_w = (width * usize::from(h)).div_ceil(usize::from(hmax));
        let plane_h = (height * usize::from(v)).div_ceil(usize::from(vmax));
        plane.clear();
        plane.resize(plane_w * plane_h, 0);
        for py in 0..plane_h {
            let y = (py * vy).min(height - 1);
            for px in 0..plane_w {
                let x = (px * hx).min(width - 1);
                plane[py * plane_w + px] = samples[(y * width + x) * stride + c];
            }
        }
        match (hx, vy) {
            (2, 1) if plane_w > 2 => {
                for py in 0..plane_h {
                    for x in 0..width {
                        let u = x / 2;
                        let neighbor = if x % 2 == 0 {
                            u.saturating_sub(1)
                        } else {
                            (u + 1).min(plane_w - 1)
                        };
                        let bias = if x % 2 == 0 { 1 } else { 2 };
                        let value = (3 * u32::from(plane[py * plane_w + u])
                            + u32::from(plane[py * plane_w + neighbor])
                            + bias)
                            >> 2;
                        samples[(py * width + x) * stride + c] = value.min(max) as u16;
                    }
                }
            }
            (1, 2) => {
                for py in 0..plane_h {
                    let above = py.saturating_sub(1);
                    let below = (py + 1).min(plane_h - 1);
                    for x in 0..width {
                        let near = u32::from(plane[py * plane_w + x]);
                        let even = (3 * near + u32::from(plane[above * plane_w + x]) + 1) >> 2;
                        let odd = (3 * near + u32::from(plane[below * plane_w + x]) + 2) >> 2;
                        samples[(2 * py * width + x) * stride + c] = even.min(max) as u16;
                        if 2 * py + 1 < height {
                            samples[((2 * py + 1) * width + x) * stride + c] = odd.min(max) as u16;
                        }
                    }
                }
            }
            (2, 2) if plane_w > 2 => {
                colsum.clear();
                colsum.resize(plane_w, 0);
                for py in 0..plane_h {
                    let above = py.saturating_sub(1);
                    let below = (py + 1).min(plane_h - 1);
                    for vbit in 0..2 {
                        let y = 2 * py + vbit;
                        if y >= height {
                            break;
                        }
                        let far = if vbit == 0 { above } else { below };
                        for u in 0..plane_w {
                            colsum[u] = 3 * u32::from(plane[py * plane_w + u])
                                + u32::from(plane[far * plane_w + u]);
                        }
                        for x in 0..width {
                            let u = x / 2;
                            let neighbor = if x % 2 == 0 {
                                u.saturating_sub(1)
                            } else {
                                (u + 1).min(plane_w - 1)
                            };
                            let bias = if x % 2 == 0 { 8 } else { 7 };
                            let value = (3 * colsum[u] + colsum[neighbor] + bias) >> 4;
                            samples[(y * width + x) * stride + c] = value.min(max) as u16;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Decode one scan and return how many entropy bytes it consumed.
fn decode_scan(
    data: &[u8],
    start: usize,
    scan: &[(u8, &HuffmanTable, &HuffmanTable)],
    frame: &FrameHeader,
    q: &[[u16; 64]; 4],
    restart_interval: Option<u16>,
    samples: &mut [u16],
) -> Result<usize, String> {
    let entropy = data.get(start..).ok_or("invalid JPEG entropy offset")?;
    let mut bits = BitReader::new(entropy);
    let width = frame.width;
    let height = frame.height;
    let stride = usize::from(frame.components);
    let offset = 1i32 << (frame.precision - 1);
    let max = (1i32 << frame.precision) - 1;
    let interleaved = scan.len() > 1;
    let (hmax, vmax) = max_sampling(&frame.sampling, frame.components);
    let (mcu_w, mcu_h) = scan_grid(
        width,
        height,
        (hmax, vmax),
        interleaved,
        frame.sampling[usize::from(scan[0].0)],
    );
    let mut prev = [0i64; 3];
    let mut mcu = 0usize;
    let mut restart_index = 0u8;
    for my in 0..mcu_h {
        for mx in 0..mcu_w {
            if let Some(interval) = restart_interval {
                if mcu != 0 && mcu.is_multiple_of(usize::from(interval)) {
                    bits.align_to_byte();
                    let marker = bits.take_marker()?.ok_or_else(|| {
                        "expected JPEG restart marker at restart interval boundary".to_owned()
                    })?;
                    let expected = 0xd0 + restart_index;
                    if marker != expected {
                        return Err(format!(
                            "unexpected JPEG restart marker 0xff{marker:02x}, expected 0xff{expected:02x}"
                        ));
                    }
                    restart_index = (restart_index + 1) & 7;
                    prev = [0i64; 3];
                }
            }
            mcu += 1;
            for &(component, dc_table, ac_table) in scan {
                let c = usize::from(component);
                let (h, v) = frame.sampling[c];
                let (blocks_x, blocks_y) = if interleaved {
                    (usize::from(h), usize::from(v))
                } else {
                    (1, 1)
                };
                let (step_x, step_y) = upsampling(&frame.sampling, frame.components, c);
                for local_y in 0..blocks_y {
                    for local_x in 0..blocks_x {
                        let block_x = mx * blocks_x + local_x;
                        let block_y = my * blocks_y + local_y;
                        let mut quant = [0i32; 64];
                        let dc = read_value(&mut bits, dc_table, frame.max_dc_category)?;
                        prev[c] = prev[c]
                            .checked_add(i64::from(dc))
                            .ok_or_else(|| "JPEG DC predictor overflows i64".to_owned())?;
                        quant[0] = i32::try_from(prev[c]).map_err(|_| {
                            "JPEG DC coefficient is outside the i32 range".to_owned()
                        })?;
                        let mut k = 1;
                        while k < 64 {
                            let sym = ac_table.decode(&mut bits)?;
                            if sym == 0 {
                                break;
                            }
                            let run = sym >> 4;
                            let size = sym & 15;
                            if size == 0 {
                                if run != 15 {
                                    return Err("invalid JPEG AC run".into());
                                }
                                k += 16;
                                if k > 64 {
                                    return Err("JPEG AC run exceeds block".into());
                                }
                            } else {
                                k += usize::from(run);
                                if k >= 64 {
                                    return Err("JPEG AC run exceeds block".into());
                                }
                                quant[ZIGZAG[k]] = decode_amplitude(bits.read_bits(size)?, size);
                                k += 1;
                            }
                        }
                        let mut coeff = [0i32; 64];
                        for i in 0..64 {
                            let value =
                                i64::from(quant[i]) * i64::from(q[usize::from(frame.qids[c])][i]);
                            coeff[i] = i32::try_from(value).map_err(|_| {
                                "JPEG dequantized coefficient is outside the i32 range".to_owned()
                            })?;
                        }
                        let block = inverse(&coeff);
                        // ponytail: trailing blocks clamp out-of-plane samples onto
                        // the last visible column/row instead of dropping them, which
                        // keeps single-scan output byte-compatible with the original
                        // implementation. T.81 leaves those padding blocks undefined;
                        // edge-replicated padding (as libjpeg and this encoder write)
                        // makes the clamped values harmless.
                        for yy in 0..8 {
                            for xx in 0..8 {
                                let px = (block_x * 8 + xx) * step_x;
                                let py = (block_y * 8 + yy) * step_y;
                                // Widen before the bias: a crafted Pq=1 table can
                                // saturate a dequantized coefficient near i32::MAX,
                                // and `block + offset` would then overflow.
                                let value = (i64::from(block[yy * 8 + xx]) + i64::from(offset))
                                    .clamp(0, i64::from(max))
                                    as u16;
                                for dy in 0..step_y {
                                    let y = (py + dy).min(height - 1);
                                    for dx in 0..step_x {
                                        let x = (px + dx).min(width - 1);
                                        samples[(y * width + x) * stride + c] = value;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    bits.align_to_byte();
    Ok(bits.position())
}

fn category(v: i32) -> u8 {
    if v == 0 {
        0
    } else {
        (32 - v.unsigned_abs().leading_zeros()) as u8
    }
}
fn amplitude(v: i32, n: u8) -> u32 {
    if v >= 0 {
        v as u32
    } else {
        ((1i32 << n) + v - 1) as u32
    }
}
fn decode_amplitude(v: u32, n: u8) -> i32 {
    if v < (1u32 << (n - 1)) {
        v as i32 + 1 - (1i32 << n)
    } else {
        v as i32
    }
}
fn write_value(w: &mut BitWriter, h: &HuffmanTable, v: i32) -> Result<(), String> {
    let n = category(v);
    h.encode(n, w)?;
    if n > 0 {
        w.write_bits(amplitude(v, n), n)
    } else {
        Ok(())
    }
}
/// Emits `(symbol, extra bits, bit count)` for every AC coefficient of a
/// quantized block: runs of sixteen zeros as `0xF0`, the end of block as `0x00`,
/// and otherwise `(run << 4) | size`.
fn for_each_ac(
    quant: &[i32; 64],
    mut emit: impl FnMut(u8, u32, u8) -> Result<(), String>,
) -> Result<(), String> {
    let mut run = 0u8;
    for &index in ZIGZAG.iter().skip(1) {
        let value = quant[index];
        if value == 0 {
            run += 1;
            if run == 16 {
                emit(0xf0, 0, 0)?;
                run = 0;
            }
        } else {
            let size = category(value);
            emit((run << 4) | size, amplitude(value, size), size)?;
            run = 0;
        }
    }
    if run > 0 {
        emit(0, 0, 0)?;
    }
    Ok(())
}
fn read_value(r: &mut BitReader<'_>, h: &HuffmanTable, max_category: u8) -> Result<i32, String> {
    let n = h.decode(r)?;
    if n > max_category {
        return Err(format!(
            "JPEG DC category {n} exceeds the {max_category} limit"
        ));
    }
    if n == 0 {
        Ok(0)
    } else {
        Ok(decode_amplitude(r.read_bits(n)?, n))
    }
}
/// Allocate the frame buffer without aborting when the request cannot be met.
fn allocate_samples(length: usize) -> Result<Vec<u16>, String> {
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(length)
        .map_err(|_| "JPEG frame is too large to allocate".to_owned())?;
    samples.resize(length, 0);
    Ok(samples)
}
fn expect(r: &mut MarkerReader<'_>, m: Marker) -> Result<(), String> {
    let (x, p) = r.read_segment()?;
    if x != m || !p.is_empty() {
        Err("unexpected JPEG marker".into())
    } else {
        Ok(())
    }
}
fn sof_payload(f: &SequentialFrame, sampling: &[(u8, u8); 3]) -> Vec<u8> {
    let mut p = vec![
        f.precision,
        (f.height as u16 >> 8) as u8,
        f.height as u8,
        (f.width as u16 >> 8) as u8,
        f.width as u8,
        f.components,
    ];
    for c in 0..f.components {
        let (h, v) = sampling[usize::from(c)];
        p.extend([c + 1, (h << 4) | v, if c == 0 { 0 } else { 1 }]);
    }
    p
}
fn sos_payload(components: &[u8]) -> Vec<u8> {
    let mut p = vec![components.len() as u8];
    for &component in components {
        p.extend([component + 1, if component == 0 { 0x00 } else { 0x11 }]);
    }
    p.extend([0, 63, 0]);
    p
}
fn dqt_payload(q: &[[u16; 64]; 2]) -> Result<Vec<u8>, String> {
    let mut p = Vec::with_capacity(130);
    for (id, t) in q.iter().enumerate() {
        // libjpeg emits 16-bit entries (Pq=1) as soon as one table value
        // exceeds 255; that cannot happen on the baseline path, where the
        // tables are clamped to 255 like force_baseline = TRUE.
        let wide = t.iter().any(|&value| value > 255);
        p.push((u8::from(wide) << 4) | id as u8);
        for &index in &ZIGZAG {
            let value = t[index];
            if wide {
                p.extend(value.to_be_bytes());
            } else {
                p.push(value as u8);
            }
        }
    }
    Ok(p)
}
fn dht_payload(tables: &Tables) -> Vec<u8> {
    let mut p = Vec::new();
    for (identifier, spec) in [
        (0x00u8, &tables.dc[0]),
        (0x10u8, &tables.ac[0]),
        (0x01u8, &tables.dc[1]),
        (0x11u8, &tables.ac[1]),
    ] {
        p.push(identifier);
        p.extend(&spec.counts);
        p.extend(&spec.values);
    }
    p
}
fn parse_sof(p: &[u8], marker: Marker, baseline: bool) -> Result<FrameHeader, String> {
    if p.len() < 6 {
        return Err("truncated SOF".into());
    }
    let n = p[5];
    if p.len() != 6 + usize::from(n) * 3 {
        return Err("invalid SOF length".into());
    }
    if !(1..=3).contains(&n) {
        return Err("JPEG SOF has no components".into());
    }
    if !matches!(p[0], 8 | 12) {
        return Err(format!("unsupported JPEG frame precision {}", p[0]));
    }
    // The transfer syntax owns the process: Process 1 (`.50`) is baseline SOF0
    // with 8-bit samples. Process 2/4 (`.51`) is SOF1 for 8/12-bit, but
    // fo-dicom/libjpeg emit 8-bit extended streams with the baseline SOF0 marker,
    // so SOF0 is also accepted at 8-bit (never at 12-bit).
    if baseline {
        if marker != Marker::Sof0 {
            return Err("baseline JPEG requires a SOF0 marker".into());
        }
        if p[0] != 8 {
            return Err(format!(
                "baseline JPEG requires 8-bit precision, got {}",
                p[0]
            ));
        }
    } else if marker == Marker::Sof0 {
        if p[0] != 8 {
            return Err(format!(
                "extended sequential JPEG with a SOF0 marker requires 8-bit precision, got {}",
                p[0]
            ));
        }
    } else if marker != Marker::Sof1 {
        return Err("extended sequential JPEG requires a SOF0 or SOF1 marker".into());
    }
    let mut qids = [0u8; 3];
    let mut component_ids = [0u8; 3];
    let mut sampling = [(1u8, 1u8); 3];
    for (i, c) in p[6..].chunks_exact(3).enumerate() {
        if c[2] > 3 || component_ids[..i].contains(&c[0]) {
            return Err("JPEG SOF component definitions are invalid".into());
        }
        qids[i] = c[2];
        component_ids[i] = c[0];
        let (h, v) = (c[1] >> 4, c[1] & 15);
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
            return Err(format!(
                "unsupported JPEG sampling factor {h}x{v} for component {}",
                c[0]
            ));
        }
        sampling[i] = (h, v);
    }
    if n == 2 {
        return Err("JPEG two-component images are unsupported".into());
    }
    // Upsampling a component by a fractional ratio has no libjpeg/T.81
    // behaviour to match, so require H_i to divide Hmax and V_i to divide Vmax.
    let (hmax, vmax) = max_sampling(&sampling, n);
    for i in 0..usize::from(n) {
        let (h, v) = sampling[i];
        if hmax % h != 0 || vmax % v != 0 {
            return Err(format!(
                "JPEG sampling factor {h}x{v} for component {} does not divide the frame maximum {hmax}x{vmax}",
                component_ids[i]
            ));
        }
    }
    Ok(FrameHeader {
        width: usize::from(u16::from_be_bytes([p[3], p[4]])),
        height: usize::from(u16::from_be_bytes([p[1], p[2]])),
        precision: p[0],
        components: n,
        sampling,
        qids,
        ids: component_ids,
        max_dc_category: if marker == Marker::Sof0 { 11 } else { 15 },
    })
}
fn parse_dri(p: &[u8]) -> Result<Option<u16>, String> {
    if p.len() != 2 {
        return Err("JPEG DRI payload must contain two bytes".into());
    }
    let interval = u16::from_be_bytes([p[0], p[1]]);
    Ok(if interval == 0 { None } else { Some(interval) })
}
fn parse_sos(p: &[u8], component_ids: &[u8]) -> Result<Vec<(u8, u8, u8)>, String> {
    if p.len() < 3 {
        return Err("truncated SOS".into());
    }
    let n = usize::from(p[0]);
    if n == 0 || n > component_ids.len() || p.len() != 4 + 2 * n {
        return Err("invalid SOS length".into());
    }
    if p[p.len() - 3] != 0 || p[p.len() - 2] != 63 || p[p.len() - 1] != 0 {
        return Err("unsupported JPEG scan parameters".into());
    }
    let mut selectors = Vec::with_capacity(n);
    for index in 0..n {
        let at = 1 + 2 * index;
        let id = p[at];
        let component = component_ids
            .iter()
            .position(|&candidate| candidate == id)
            .ok_or_else(|| format!("JPEG SOS references unknown component {id}"))?;
        if selectors
            .iter()
            .any(|&(existing, _, _)| usize::from(existing) == component)
        {
            return Err(format!("JPEG SOS repeats component {id}"));
        }
        selectors.push((component as u8, p[at + 1] >> 4, p[at + 1] & 15));
    }
    Ok(selectors)
}
fn parse_dqt(p: &[u8], q: &mut [[u16; 64]; 4], baseline: bool) -> Result<(), String> {
    let mut i = 0;
    while i < p.len() {
        let info = p[i];
        i += 1;
        let precision = info >> 4;
        // Pq=1 (16-bit entries) is what libjpeg writes once a value exceeds
        // 255 and what fo-dicom emits for low-quality 12-bit encodes. Baseline
        // (Process 1) keeps the 8-bit Pq=0 form.
        if precision > 1 || info & 15 > 3 {
            return Err("unsupported DQT".into());
        }
        if baseline && precision != 0 {
            return Err("baseline JPEG requires 8-bit (Pq=0) quantization tables".into());
        }
        let entry_bytes = 1 + usize::from(precision);
        if i + 64 * entry_bytes > p.len() {
            return Err("unsupported DQT".into());
        }
        for k in 0..64 {
            let value = if precision == 0 {
                u16::from(p[i + k])
            } else {
                let at = i + 2 * k;
                u16::from_be_bytes([p[at], p[at + 1]])
            };
            q[usize::from(info & 15)][ZIGZAG[k]] = value;
        }
        i += 64 * entry_bytes;
    }
    Ok(())
}
fn parse_dht(
    p: &[u8],
    dc: &mut [Option<HuffmanTable>; 4],
    ac: &mut [Option<HuffmanTable>; 4],
) -> Result<(), String> {
    let mut i = 0;
    while i < p.len() {
        let info = p[i];
        i += 1;
        if i + 16 > p.len() {
            return Err("truncated DHT".into());
        }
        let counts = &p[i..i + 16];
        i += 16;
        let total = counts.iter().map(|&x| usize::from(x)).sum::<usize>();
        if i + total > p.len() {
            return Err("truncated DHT values".into());
        }
        let t = HuffmanTable::new(counts, &p[i..i + total])?;
        i += total;
        let slot = if info & 15 > 3 {
            return Err("unsupported Huffman table id".into());
        } else {
            usize::from(info & 15)
        };
        if info >> 4 == 0 {
            dc[slot] = Some(t)
        } else if info >> 4 == 1 {
            ac[slot] = Some(t)
        } else {
            return Err("unsupported Huffman class".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn pooling(step_x: usize, step_y: usize, component: usize, smoothing: u32) -> Pooling {
        Pooling {
            step_x,
            step_y,
            component,
            smoothing,
        }
    }
    use super::*;
    #[test]
    fn round_trips_baseline_frame() {
        let f = SequentialFrame::new(9, 7, 8, 1, (0..63).map(|x| x as u16 * 3).collect()).unwrap();
        let d =
            SequentialFrame::decode(&f.encode_with_smoothing(100, false, false, 0, None).unwrap())
                .unwrap();
        assert_eq!(d.width, f.width);
        assert_eq!(d.samples.len(), f.samples.len());
        let max = d
            .samples
            .iter()
            .zip(f.samples.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(max < 32, "max={max} first={:?}", &d.samples[..10]);
    }

    #[test]
    fn round_trips_422_component_sampling() {
        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let encoded = frame
            .encode_with_smoothing(90, false, true, 0, None)
            .unwrap();
        assert!(encoded.windows(3).any(|w| w == [0xff, 0xc0, 0x00]));
        let decoded = SequentialFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.sampling, [(2, 1), (1, 1), (1, 1)]);
        let max = decoded
            .samples
            .iter()
            .zip(frame.samples.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(max < 160, "max={max}");
    }

    #[test]
    fn restart_intervals_preserve_reconstructed_samples() {
        let samples = (0..17 * 9).map(|v| ((v * 37) % 256) as u16).collect();
        let mono = SequentialFrame::new(17, 9, 8, 1, samples).unwrap();
        let plain = SequentialFrame::decode(
            &mono
                .encode_with_smoothing(90, false, false, 0, None)
                .unwrap(),
        )
        .unwrap();
        let restarted = mono
            .encode_with_smoothing(90, false, false, 0, Some(1))
            .unwrap();
        assert!(restarted.windows(2).any(|w| w == [0xff, 0xd0]));
        let decoded = SequentialFrame::decode(&restarted).unwrap();
        assert_eq!(decoded.samples, plain.samples);

        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let colour = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let plain = SequentialFrame::decode(
            &colour
                .encode_with_smoothing(90, false, true, 0, None)
                .unwrap(),
        )
        .unwrap();
        let restarted = colour
            .encode_with_smoothing(90, false, true, 0, Some(2))
            .unwrap();
        assert!(restarted.windows(2).any(|w| w == [0xff, 0xd0]));
        assert_eq!(
            SequentialFrame::decode(&restarted).unwrap().samples,
            plain.samples
        );
    }

    /// Rewrites every table 0 reference to table 3 in DQT/DHT/SOF/SOS while
    /// leaving table 1 alone, so the entropy data stays valid. Only the table
    /// identifiers differ from the original codestream.
    fn renumber_table_zero_to_three(stream: &[u8]) -> Vec<u8> {
        let mut out = stream.to_vec();
        let mut at = 2; // after SOI
        while at + 4 <= out.len() {
            let marker = out[at + 1];
            let length = usize::from(u16::from_be_bytes([out[at + 2], out[at + 3]]));
            let payload = at + 4;
            match marker {
                0xdb => {
                    let mut p = payload;
                    while p < at + 2 + length {
                        let precision = out[p] >> 4;
                        if out[p] & 0x0f == 0 {
                            out[p] = (out[p] & 0xf0) | 3;
                        }
                        p += 1 + 64 * if precision == 1 { 2 } else { 1 };
                    }
                }
                0xc4 => {
                    let mut p = payload;
                    while p < at + 2 + length {
                        let total: usize = out[p + 1..p + 17].iter().map(|&c| usize::from(c)).sum();
                        if out[p] & 0x0f == 0 {
                            out[p] = (out[p] & 0xf0) | 3;
                        }
                        p += 17 + total;
                    }
                }
                0xc0 | 0xc1 => {
                    let count = usize::from(out[payload + 5]);
                    for component in 0..count {
                        let qid = payload + 6 + 3 * component + 2;
                        if out[qid] == 0 {
                            out[qid] = 3;
                        }
                    }
                }
                0xda => {
                    let count = usize::from(out[payload]);
                    for component in 0..count {
                        let selector = payload + 1 + 2 * component + 1;
                        if out[selector] == 0 {
                            out[selector] = 0x33;
                        }
                    }
                    break; // entropy data follows; nothing else to renumber
                }
                0xd9 => break,
                _ => {}
            }
            at += 2 + length;
        }
        out
    }

    #[test]
    fn decodes_quantization_and_huffman_table_ids_three() {
        let samples = (0..8 * 8).map(|v| ((v * 37) % 256) as u16).collect();
        let frame = SequentialFrame::new(8, 8, 8, 1, samples).unwrap();
        let encoded = frame
            .encode_with_smoothing(90, false, false, 0, None)
            .unwrap();
        let reference = SequentialFrame::decode(&encoded).unwrap();
        let renumbered = renumber_table_zero_to_three(&encoded);
        assert_eq!(
            SequentialFrame::decode(&renumbered).unwrap().samples,
            reference.samples,
            "table ids 0..=3 are legal (T.81 B.2.4.2/B.2.4.3)"
        );
    }

    /// 16x8 single-component stream with a custom DC table (one code), an AC
    /// table holding only EOB, and the given quantization value. Each of the
    /// two blocks codes `category` followed by the largest positive amplitude.
    fn crafted_dc_stream(sof: Marker, precision: u8, category: u8, quant: u16) -> Vec<u8> {
        let mut out = MarkerWriter::default();
        out.write_marker(Marker::Soi).unwrap();
        // Baseline (SOF0) streams require Pq=0 (8-bit) tables, so only widen the
        // table to Pq=1 when the value does not fit a byte (the extended case).
        let wide = quant > 255;
        let mut dqt = vec![u8::from(wide) << 4];
        for _ in 0..64 {
            if wide {
                dqt.extend(quant.to_be_bytes());
            } else {
                dqt.push(quant as u8);
            }
        }
        out.write_segment(Marker::Dqt, &dqt).unwrap();
        let sof_payload = vec![precision, 0, 8, 0, 16, 1, 1, 0x11, 0];
        out.write_segment(sof, &sof_payload).unwrap();
        // DC table 0: one code of length 1 for `category`; AC table 0: EOB.
        let mut dht = vec![0x00, 1];
        dht.extend([0u8; 15]);
        dht.push(category);
        dht.push(0x10);
        dht.push(1);
        dht.extend([0u8; 15]);
        dht.push(0);
        out.write_segment(Marker::Dht, &dht).unwrap();
        out.write_segment(Marker::Sos, &[1, 1, 0x00, 0, 63, 0])
            .unwrap();
        let mut bits = BitWriter::default();
        for _ in 0..2 {
            bits.write_bits(0, 1).unwrap(); // the single DC code
            bits.write_bits((1u32 << category) - 1, category).unwrap();
            bits.write_bits(0, 1).unwrap(); // EOB
        }
        out.write_raw(&bits.finish().unwrap());
        out.write_marker(Marker::Eoi).unwrap();
        out.into_bytes()
    }

    #[test]
    fn rejects_dc_categories_above_the_process_limit() {
        let error =
            SequentialFrame::decode(&crafted_dc_stream(Marker::Sof0, 8, 12, 1)).unwrap_err();
        assert!(error.contains("exceeds the 11 limit"), "{error}");
    }

    #[test]
    fn dequantization_overflow_is_a_managed_error() {
        // Category 15 reaches 32767; two blocks at q=65535 overflow i32.
        let error =
            SequentialFrame::decode(&crafted_dc_stream(Marker::Sof1, 12, 15, 65535)).unwrap_err();
        assert!(error.contains("outside the i32 range"), "{error}");
    }

    #[test]
    fn rejects_an_oversized_sof_before_allocating() {
        let mut stream = MarkerWriter::default();
        stream.write_marker(Marker::Soi).unwrap();
        // 65535x65535 single component: 4.29e9 samples, above 1<<28.
        stream
            .write_segment(Marker::Sof1, &[8, 0xff, 0xff, 0xff, 0xff, 1, 1, 0x11, 0])
            .unwrap();
        stream.write_marker(Marker::Eoi).unwrap();
        let error = SequentialFrame::decode(&stream.into_bytes()).unwrap_err();
        assert!(error.contains("sample limit"), "{error}");
    }

    #[test]
    fn rejects_wrong_restart_marker_and_truncated_interval() {
        let frame =
            SequentialFrame::new(17, 9, 8, 1, (0..17 * 9).map(|v| (v % 251) as u16).collect())
                .unwrap();
        let mut encoded = frame
            .encode_with_smoothing(100, false, false, 0, Some(1))
            .unwrap();
        let restart = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xd0])
            .unwrap();
        let mut wrong = encoded.clone();
        wrong[restart + 1] = 0xd1;
        assert!(SequentialFrame::decode(&wrong).is_err());

        encoded.drain(restart..restart + 2);
        assert!(SequentialFrame::decode(&encoded).is_err());
    }

    /// (start of the DQT marker, total segment length including the marker)
    fn dqt_segment(stream: &[u8]) -> (usize, usize) {
        let start = stream
            .windows(2)
            .position(|window| window == [0xff, 0xdb])
            .unwrap();
        let length = usize::from(u16::from_be_bytes([stream[start + 2], stream[start + 3]]));
        (start, 2 + length)
    }

    /// Per-table `Pq` nibbles in the order the segment carries them.
    fn dqt_precisions(payload: &[u8]) -> Vec<u8> {
        let mut precisions = Vec::new();
        let mut at = 0;
        while at < payload.len() {
            let info = payload[at];
            let precision = info >> 4;
            precisions.push(precision);
            at += 1 + 64 * if precision == 0 { 1 } else { 2 };
        }
        precisions
    }

    #[test]
    fn twelve_bit_quality_ten_emits_sixteen_bit_quantization_tables() {
        let samples = (0..8 * 8).map(|v| ((v * 53) % 4096) as u16).collect();
        let frame = SequentialFrame::new(8, 8, 12, 1, samples).unwrap();
        let encoded = frame
            .encode_with_smoothing(10, true, false, 0, None)
            .unwrap();
        let (start, total) = dqt_segment(&encoded);
        assert_eq!(
            dqt_precisions(&encoded[start + 4..start + total]),
            [1, 1],
            "a 12-bit table above 255 uses Pq=1, like libjpeg with force_baseline=FALSE"
        );
        let decoded = SequentialFrame::decode(&encoded).unwrap();
        let max = decoded
            .samples
            .iter()
            .zip(frame.samples.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(max < 1024, "max={max}");
    }

    #[test]
    fn twelve_bit_default_quality_keeps_eight_bit_quantization_tables() {
        let samples = (0..8 * 8).map(|v| ((v * 53) % 4096) as u16).collect();
        let frame = SequentialFrame::new(8, 8, 12, 1, samples).unwrap();
        let encoded = frame
            .encode_with_smoothing(90, true, false, 0, None)
            .unwrap();
        let (start, total) = dqt_segment(&encoded);
        assert_eq!(dqt_precisions(&encoded[start + 4..start + total]), [0, 0]);
        // Two tables, one byte per entry: the default path is byte-identical.
        assert_eq!(total, 2 + 2 + 130);
    }

    #[test]
    fn decodes_sixteen_bit_quantization_tables() {
        let samples = (0..8 * 8).map(|v| ((v * 53) % 4096) as u16).collect();
        let frame = SequentialFrame::new(8, 8, 12, 1, samples).unwrap();
        let encoded = frame
            .encode_with_smoothing(90, true, false, 0, None)
            .unwrap();
        let (start, total) = dqt_segment(&encoded);
        let payload = &encoded[start + 4..start + total];
        // Same table values, widened to the Pq=1 (16-bit) representation that
        // libjpeg writes once an entry exceeds 255.
        let mut wide = Vec::new();
        let mut at = 0;
        while at < payload.len() {
            wide.push(0x10 | (payload[at] & 0x0f));
            for &value in &payload[at + 1..at + 65] {
                wide.extend(u16::from(value).to_be_bytes());
            }
            at += 65;
        }
        let mut stream = encoded[..start].to_vec();
        stream.extend_from_slice(&[0xff, 0xdb]);
        stream.extend_from_slice(&u16::try_from(wide.len() + 2).unwrap().to_be_bytes());
        stream.extend_from_slice(&wide);
        stream.extend_from_slice(&encoded[start + total..]);
        assert_eq!(
            SequentialFrame::decode(&stream).unwrap(),
            SequentialFrame::decode(&encoded).unwrap()
        );
    }

    #[test]
    fn rejects_missing_quantization_table_for_a_selector() {
        let frame = SequentialFrame::new(
            8,
            8,
            8,
            3,
            (0..8 * 8 * 3).map(|v| (v % 256) as u16).collect(),
        )
        .unwrap();
        let mut encoded = frame
            .encode_with_smoothing(90, false, false, 0, None)
            .unwrap();
        let dqt = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xdb])
            .unwrap();
        let length = usize::from(u16::from_be_bytes([encoded[dqt + 2], encoded[dqt + 3]]));
        let payload_start = dqt + 4;
        let keep = 1 + 64;
        encoded.drain(payload_start + keep..payload_start + length - 2);
        let shortened = (2 + keep) as u16;
        encoded[dqt + 2..dqt + 4].copy_from_slice(&shortened.to_be_bytes());
        let error = SequentialFrame::decode(&encoded).unwrap_err();
        assert!(error.contains("quantization table 1"), "{error}");
    }

    #[test]
    fn validates_component_sampling_factors() {
        // T.81 allows any 1..=4 factors; 4:2:0 is 2x2 on the luma component.
        let mut payload = vec![8, 0, 8, 0, 8, 3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0];
        assert_eq!(
            parse_sof(&payload, Marker::Sof0, true).unwrap().sampling,
            [(2, 2), (1, 1), (1, 1)]
        );
        // Mixed factors stay legal as long as they divide the frame maximum.
        payload = vec![8, 0, 8, 0, 8, 3, 1, 0x21, 0, 2, 0x22, 0, 3, 0x11, 0];
        assert_eq!(
            parse_sof(&payload, Marker::Sof0, true).unwrap().sampling,
            [(2, 1), (2, 2), (1, 1)]
        );
        // Zero and out-of-range factors are rejected.
        payload = vec![8, 0, 8, 0, 8, 3, 1, 0x01, 0, 2, 0x11, 0, 3, 0x11, 0];
        let error = parse_sof(&payload, Marker::Sof0, true).unwrap_err();
        assert!(error.contains("sampling factor"), "{error}");
        payload = vec![8, 0, 8, 0, 8, 3, 1, 0x51, 0, 2, 0x11, 0, 3, 0x11, 0];
        let error = parse_sof(&payload, Marker::Sof0, true).unwrap_err();
        assert!(error.contains("sampling factor 5x1"), "{error}");
        // A factor that does not divide the maximum cannot be upsampled.
        payload = vec![8, 0, 8, 0, 8, 3, 1, 0x31, 0, 2, 0x21, 0, 3, 0x11, 0];
        let error = parse_sof(&payload, Marker::Sof0, true).unwrap_err();
        assert!(error.contains("does not divide"), "{error}");
        // Two components remain unsupported.
        payload = vec![8, 0, 8, 0, 8, 2, 1, 0x11, 0, 2, 0x11, 0];
        assert!(parse_sof(&payload, Marker::Sof0, true).is_err());
    }

    #[test]
    fn parses_restart_interval_segments() {
        assert_eq!(parse_dri(&[0, 0]).unwrap(), None);
        assert_eq!(parse_dri(&[0, 3]).unwrap(), Some(3));
        assert!(parse_dri(&[0]).is_err());
        assert!(parse_dri(&[0, 1, 2]).is_err());
    }

    #[test]
    fn accepts_zero_based_component_identifiers() {
        let samples = (0..8 * 8 * 3).map(|v| (v % 256) as u16).collect();
        let frame = SequentialFrame::new(8, 8, 8, 3, samples).unwrap();
        let mut encoded = frame
            .encode_with_smoothing(90, false, false, 0, None)
            .unwrap();
        let sof = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xc0])
            .unwrap();
        for (offset, id) in [10usize, 13, 16].into_iter().zip([0u8, 1, 2]) {
            encoded[sof + offset] = id;
        }
        let sos = encoded
            .windows(2)
            .position(|window| window == [0xff, 0xda])
            .unwrap();
        for (offset, id) in [5usize, 7, 9].into_iter().zip([0u8, 1, 2]) {
            encoded[sos + offset] = id;
        }
        let decoded = SequentialFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.width, frame.width);
        assert_eq!(decoded.height, frame.height);
    }

    #[test]
    fn decodes_non_interleaved_multi_scan_streams() {
        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let single = SequentialFrame::decode(
            &frame
                .encode_with_smoothing(90, false, false, 0, None)
                .unwrap(),
        )
        .unwrap();
        let per_component = frame
            .encode_scans(90, false, false, 0, None, &[vec![0], vec![1], vec![2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&per_component).unwrap().samples,
            single.samples,
            "one scan per component"
        );
        let mixed = frame
            .encode_scans(90, false, false, 0, None, &[vec![0, 1], vec![2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&mixed).unwrap().samples,
            single.samples,
            "interleaved subset scan"
        );

        let samples = (0..16 * 8 * 3)
            .map(|v| ((v * 61) & 0x0fff) as u16)
            .collect();
        let frame = SequentialFrame::new(16, 8, 12, 3, samples).unwrap();
        let single = SequentialFrame::decode(
            &frame
                .encode_with_smoothing(90, true, false, 0, None)
                .unwrap(),
        )
        .unwrap();
        let per_component = frame
            .encode_scans(90, true, false, 0, None, &[vec![0], vec![1], vec![2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&per_component).unwrap().samples,
            single.samples,
            "12-bit scans"
        );
    }

    #[test]
    fn fancy_upsamples_422_chroma_with_libjpeg_triangle_filter() {
        let (width, height) = (17, 2);
        // The decoder replicates chroma across pixel pairs, so these are the
        // true half-resolution rows. Row 1 is chosen so that a swapped rounding
        // bias (+2 instead of +1 on even columns) would change the result.
        let rows = [
            [0u16, 40, 80, 120, 160, 200, 240, 20, 60],
            [0, 2, 6, 10, 14, 18, 22, 26, 30],
        ];
        let expected = [
            [
                0u16, 10, 30, 50, 70, 90, 110, 130, 150, 170, 190, 210, 230, 185, 75, 30, 50,
            ],
            [0, 1, 1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 27, 29],
        ];
        let mut samples = vec![0u16; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                samples[index] = 77;
                samples[index + 1] = rows[y][x / 2];
                samples[index + 2] = rows[y][x / 2];
            }
        }
        fancy_upsample(&mut samples, width, height, 8, 3, &[(2, 1), (1, 1), (1, 1)]);
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                assert_eq!(samples[index], 77, "luma at {x},{y}");
                assert_eq!(samples[index + 1], expected[y][x], "cb at {x},{y}");
                assert_eq!(samples[index + 2], expected[y][x], "cr at {x},{y}");
            }
        }

        // A single chroma value survives the filter unchanged.
        let mut flat = vec![100u16; width * height * 3];
        fancy_upsample(&mut flat, width, height, 8, 3, &[(2, 1), (1, 1), (1, 1)]);
        assert!(flat.iter().all(|&value| value == 100));
    }

    #[test]
    fn decodes_non_interleaved_422_scans() {
        // Column counts that are multiples of 16 keep the interleaved and the
        // non-interleaved luma block grids identical, so the decoded samples
        // must equal those of the single interleaved scan exactly.
        let samples = (0..32 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(32, 9, 8, 3, samples).unwrap();
        let single = SequentialFrame::decode(
            &frame
                .encode_with_smoothing(90, false, true, 0, None)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(single.sampling, [(2, 1), (1, 1), (1, 1)]);
        let luma_then_chroma = frame
            .encode_scans(90, false, true, 0, None, &[vec![0], vec![1, 2]])
            .unwrap();
        let decoded = SequentialFrame::decode(&luma_then_chroma).unwrap();
        assert_eq!(decoded.sampling, [(2, 1), (1, 1), (1, 1)]);
        assert_eq!(
            decoded.samples, single.samples,
            "luma scan then chroma scan"
        );
        let two_then_one = frame
            .encode_scans(90, false, true, 0, None, &[vec![0, 1], vec![2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&two_then_one).unwrap().samples,
            single.samples,
            "luma+cb scan then cr scan"
        );

        // An odd column count exercises the non-interleaved luma grid
        // (ceil(width/8) blocks instead of the interleaved 2*ceil(width/16)).
        // The interleaved scan's trailing padding block encodes the replicated
        // edge column, so both layouts decode to the same visible samples; the
        // lossy difference against the source (max 114 here) is plain 4:2:2
        // subsampling error, not a scan-layout artefact.
        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let single = SequentialFrame::decode(
            &frame
                .encode_with_smoothing(90, false, true, 0, None)
                .unwrap(),
        )
        .unwrap();
        let luma_then_chroma = frame
            .encode_scans(90, false, true, 0, None, &[vec![0], vec![1, 2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&luma_then_chroma).unwrap().samples,
            single.samples,
            "odd width luma scan then chroma scan"
        );
        let two_then_one = frame
            .encode_scans(90, false, true, 0, None, &[vec![0, 1], vec![2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&two_then_one).unwrap().samples,
            single.samples,
            "odd width luma+cb scan then cr scan"
        );
    }

    #[test]
    fn multi_scan_restart_intervals_preserve_reconstructed_samples() {
        let samples = (0..32 * 9 * 3).map(|v| ((v * 23) % 256) as u16).collect();
        let frame = SequentialFrame::new(32, 9, 8, 3, samples).unwrap();
        let plain = SequentialFrame::decode(
            &frame
                .encode_scans(90, false, true, 0, None, &[vec![0], vec![1, 2]])
                .unwrap(),
        )
        .unwrap();
        // The luma scan spends more than eight restart markers, so the chroma
        // scan only decodes when each scan restarts the RST index at zero.
        let restarted = frame
            .encode_scans(90, false, true, 0, Some(1), &[vec![0], vec![1, 2]])
            .unwrap();
        assert!(restarted.windows(2).any(|w| w == [0xff, 0xd0]));
        assert_eq!(
            SequentialFrame::decode(&restarted).unwrap().samples,
            plain.samples
        );
    }

    #[test]
    fn rejects_repeated_or_missing_components_across_scans() {
        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let per_component = frame
            .encode_scans(90, false, false, 0, None, &[vec![0], vec![1], vec![2]])
            .unwrap();
        let sos: Vec<usize> = per_component
            .windows(2)
            .enumerate()
            .filter(|(_, window)| *window == [0xff, 0xda])
            .map(|(index, _)| index)
            .collect();
        assert_eq!(sos.len(), 3);
        let mut repeated = per_component.clone();
        repeated[sos[1] + 5] = 1;
        let error = SequentialFrame::decode(&repeated).unwrap_err();
        assert!(error.contains("more than one scan"), "{error}");
        let missing = frame
            .encode_scans(90, false, false, 0, None, &[vec![0], vec![1]])
            .unwrap();
        let error = SequentialFrame::decode(&missing).unwrap_err();
        assert!(error.contains("every component"), "{error}");
    }

    #[test]
    fn rejects_unknown_component_in_a_non_interleaved_scan() {
        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let mut stream = frame
            .encode_scans(90, false, false, 0, None, &[vec![0], vec![1], vec![2]])
            .unwrap();
        let third = stream
            .windows(2)
            .enumerate()
            .filter(|(_, window)| *window == [0xff, 0xda])
            .map(|(index, _)| index)
            .nth(2)
            .unwrap();
        stream[third + 5] = 9;
        let error = SequentialFrame::decode(&stream).unwrap_err();
        assert!(error.contains("unknown component 9"), "{error}");
    }

    #[test]
    fn rejects_more_than_three_components_without_panicking() {
        let mut payload = vec![8, 0, 8, 0, 8, 4];
        for id in 1..=4 {
            payload.extend([id, 0x11, 0]);
        }
        assert!(parse_sof(&payload, Marker::Sof0, true).is_err());
    }

    #[test]
    fn round_trips_420_component_sampling() {
        let sampling = [(2, 2), (1, 1), (1, 1)];
        for (width, height) in [(32, 9), (17, 9)] {
            let samples: Vec<u16> = (0..width * height * 3)
                .map(|v| ((v * 19) % 256) as u16)
                .collect();
            let frame = SequentialFrame::new(width, height, 8, 3, samples).unwrap();
            let interleaved = frame
                .encode_scans_with(90, false, &sampling, 0, None, &[vec![0, 1, 2]])
                .unwrap();
            let decoded = SequentialFrame::decode(&interleaved).unwrap();
            assert_eq!(decoded.sampling, sampling);
            // The decoder replicates each chroma sample over a 2x2 pixel block.
            for y in 0..height {
                for x in 0..width {
                    let anchor = ((y / 2) * 2 * width + (x / 2) * 2) * 3;
                    let index = (y * width + x) * 3;
                    assert_eq!(decoded.samples[index + 1], decoded.samples[anchor + 1]);
                    assert_eq!(decoded.samples[index + 2], decoded.samples[anchor + 2]);
                }
            }
            // Every scan layout decodes to the same samples.
            for scans in [
                vec![vec![0u8], vec![1, 2]],
                vec![vec![0u8], vec![1], vec![2]],
                vec![vec![0u8, 1], vec![2]],
            ] {
                let split = frame
                    .encode_scans_with(90, false, &sampling, 0, None, &scans)
                    .unwrap();
                assert_eq!(
                    SequentialFrame::decode(&split).unwrap().samples,
                    decoded.samples,
                    "{width}x{height} scans {scans:?}"
                );
            }
        }

        // With aligned block grids the 4:2:0 luma decodes like 4:4:4 luma.
        let samples = (0..32 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(32, 9, 8, 3, samples).unwrap();
        let plain = SequentialFrame::decode(
            &frame
                .encode_with_smoothing(90, false, false, 0, None)
                .unwrap(),
        )
        .unwrap();
        let sampled = SequentialFrame::decode(
            &frame
                .encode_scans_with(90, false, &sampling, 0, None, &[vec![0, 1, 2]])
                .unwrap(),
        )
        .unwrap();
        for (index, (a, b)) in plain.samples.iter().zip(sampled.samples.iter()).enumerate() {
            if index % 3 == 0 {
                assert_eq!(a, b, "luma sample {index}");
            }
        }
    }

    #[test]
    fn decodes_generic_component_sampling_factors() {
        // 4:1:1 keeps luma full size and repeats each chroma sample over four
        // pixels; it exercises the generic integral upsampling path.
        let sampling = [(4, 1), (1, 1), (1, 1)];
        let samples = (0..32 * 9 * 3).map(|v| ((v * 11) % 256) as u16).collect();
        let frame = SequentialFrame::new(32, 9, 8, 3, samples).unwrap();
        let interleaved = frame
            .encode_scans_with(90, false, &sampling, 0, None, &[vec![0, 1, 2]])
            .unwrap();
        let decoded = SequentialFrame::decode(&interleaved).unwrap();
        assert_eq!(decoded.sampling, sampling);
        for y in 0..9 {
            for x in 0..32 {
                let anchor = (y * 32 + (x / 4) * 4) * 3;
                let index = (y * 32 + x) * 3;
                assert_eq!(decoded.samples[index + 1], decoded.samples[anchor + 1]);
                assert_eq!(decoded.samples[index + 2], decoded.samples[anchor + 2]);
            }
        }
        let split = frame
            .encode_scans_with(90, false, &sampling, 0, None, &[vec![0], vec![1, 2]])
            .unwrap();
        assert_eq!(
            SequentialFrame::decode(&split).unwrap().samples,
            decoded.samples
        );
    }

    #[test]
    fn rejects_decoding_only_sampling_on_encode() {
        let samples = (0..32 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(32, 9, 8, 3, samples).unwrap();
        let sampling = [(2, 2), (1, 1), (1, 1)];
        let encoded = frame
            .encode_scans_with(90, false, &sampling, 0, None, &[vec![0, 1, 2]])
            .unwrap();
        let decoded = SequentialFrame::decode(&encoded).unwrap();
        let error = decoded
            .encode_with_smoothing(90, false, false, 0, None)
            .unwrap_err();
        assert!(error.contains("decode-only"), "{error}");
        // The encoder's own sampling validation rejects fractional ratios too.
        let error = frame
            .encode_scans_with(
                90,
                false,
                &[(3, 1), (2, 1), (1, 1)],
                0,
                None,
                &[vec![0, 1, 2]],
            )
            .unwrap_err();
        assert!(error.contains("does not divide"), "{error}");
    }

    #[test]
    fn fancy_upsamples_420_chroma_with_libjpeg_triangle_filter() {
        let (width, height) = (8, 4);
        // Half-resolution chroma plane; the decoder spreads each sample over a
        // 2x2 pixel block before the filter runs.
        let plane = [[0u16, 40, 80, 120], [160, 200, 240, 20]];
        let expected = [
            [0u16, 10, 30, 50, 70, 90, 110, 120],
            [40, 50, 70, 90, 110, 114, 101, 95],
            [120, 130, 150, 170, 190, 161, 84, 45],
            [160, 170, 190, 210, 230, 185, 75, 20],
        ];
        let mut samples = vec![0u16; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                samples[index] = 77;
                samples[index + 1] = plane[y / 2][x / 2];
                samples[index + 2] = plane[y / 2][x / 2];
            }
        }
        fancy_upsample(&mut samples, width, height, 8, 3, &[(2, 2), (1, 1), (1, 1)]);
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                assert_eq!(samples[index], 77, "luma at {x},{y}");
                assert_eq!(samples[index + 1], expected[y][x], "cb at {x},{y}");
                assert_eq!(samples[index + 2], expected[y][x], "cr at {x},{y}");
            }
        }
    }

    #[test]
    fn fancy_upsamples_440_chroma_with_libjpeg_triangle_filter() {
        let (width, height) = (6, 4);
        let plane = [[10u16, 20, 30, 40, 50, 60], [110, 120, 130, 140, 150, 160]];
        let expected = [
            [10u16, 20, 30, 40, 50, 60],
            [35, 45, 55, 65, 75, 85],
            [85, 95, 105, 115, 125, 135],
            [110, 120, 130, 140, 150, 160],
        ];
        let mut samples = vec![0u16; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                samples[index] = 77;
                samples[index + 1] = plane[y / 2][x];
                samples[index + 2] = plane[y / 2][x];
            }
        }
        // Luma is 1x2 so the frame maximum is 1x2; chroma is 1x1 and therefore
        // expands vertically only.
        fancy_upsample(&mut samples, width, height, 8, 3, &[(1, 2), (1, 1), (1, 1)]);
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                assert_eq!(samples[index], 77, "luma at {x},{y}");
                assert_eq!(samples[index + 1], expected[y][x], "cb at {x},{y}");
                assert_eq!(samples[index + 2], expected[y][x], "cr at {x},{y}");
            }
        }
    }
    #[test]
    fn fullsize_smoothing_matches_libjpeg_kernel() {
        // libjpeg `fullsize_smooth_downsample` (jcsample.c:396): the sample keeps
        // weight 1-8*SF and each of its eight neighbours SF, SF = factor/1024.
        let impulse: Vec<u16> = vec![0, 0, 0, 0, 100, 0, 0, 0, 0];
        let frame = SequentialFrame::new(3, 3, 8, 1, impulse).unwrap();
        let expected = [[10u32, 10, 10], [10, 22, 10], [10, 10, 10]];
        for v in 0..3 {
            for u in 0..3 {
                assert_eq!(
                    frame.smooth_sample(pooling(1, 1, 0, 100), u, v),
                    expected[v][u],
                    "impulse at {u},{v}"
                );
            }
        }
        // A one-column frame clamps both horizontal neighbours.
        let frame = SequentialFrame::new(1, 3, 8, 1, vec![10, 20, 30]).unwrap();
        let column: Vec<u32> = (0..3)
            .map(|v| frame.smooth_sample(pooling(1, 1, 0, 100), 0, v))
            .collect();
        assert_eq!(column, [13, 20, 27]);
        // factor 25 shrinks the neighbour weight and leaves the centre exact.
        let gradient: Vec<u16> = vec![10, 20, 30, 40, 50, 60, 70, 80, 90];
        let frame = SequentialFrame::new(3, 3, 8, 1, gradient).unwrap();
        let expected = [[13u32, 22, 31], [41, 50, 59], [69, 78, 87]];
        for v in 0..3 {
            for u in 0..3 {
                assert_eq!(
                    frame.smooth_sample(pooling(1, 1, 0, 25), u, v),
                    expected[v][u],
                    "gradient at {u},{v}"
                );
            }
        }
    }

    #[test]
    fn h2v2_smoothing_matches_libjpeg_kernel() {
        // libjpeg `h2v2_smooth_downsample` (jcsample.c:307): the four pooled
        // samples share (1-5*SF)/4, edge neighbours SF/2, corner neighbours SF/4.
        let mut samples = vec![0u16; 4 * 4 * 3];
        for y in 0..4 {
            let index = (y * 4 + 2) * 3 + 1;
            samples[index] = if y == 2 { 100 } else { 0 };
            samples[index + 1] = samples[index];
        }
        let frame = SequentialFrame::new(4, 4, 8, 3, samples).unwrap();
        // A 2x2 plane covers a corner, both edges and the interior.
        let expected = [[2u32, 5], [5, 13]];
        for v in 0..2 {
            for u in 0..2 {
                assert_eq!(
                    frame.smooth_sample(pooling(2, 2, 1, 100), u, v),
                    expected[v][u],
                    "chroma at {u},{v}"
                );
            }
        }
    }

    #[test]
    fn smoothing_keeps_unsmoothed_sampling_ratios() {
        // libjpeg only smooths fullsize and h2v2 components; 2:1 horizontal and
        // other integral ratios keep the plain downsample (`smoothok` is cleared
        // and JTRC_SMOOTH_NOTIMPL is traced).
        let samples = (0..32 * 8 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(32, 8, 8, 3, samples).unwrap();
        for v in 0..4 {
            for u in 0..16 {
                assert_eq!(
                    frame.smooth_sample(pooling(2, 1, 1, 100), u, v),
                    frame.cell_sample(pooling(2, 1, 1, 0), u, v)
                );
                assert_eq!(
                    frame.smooth_sample(pooling(4, 1, 1, 100), u, v),
                    frame.cell_sample(pooling(4, 1, 1, 0), u, v)
                );
            }
        }
    }

    #[test]
    fn smoothing_leaves_flat_samples_unchanged() {
        for components in [1u8, 3] {
            let samples = vec![77u16; 17 * 9 * usize::from(components)];
            let frame = SequentialFrame::new(17, 9, 8, components, samples.clone()).unwrap();
            let stream = frame
                .encode_with_smoothing(100, false, components == 3, 100, None)
                .unwrap();
            assert_eq!(SequentialFrame::decode(&stream).unwrap().samples, samples);
        }
    }

    #[test]
    fn smoothing_changes_the_codestream_and_rejects_out_of_range() {
        let samples = (0..17 * 9 * 3).map(|v| ((v * 19) % 256) as u16).collect();
        let frame = SequentialFrame::new(17, 9, 8, 3, samples).unwrap();
        let plain = frame
            .encode_with_smoothing(90, false, true, 0, None)
            .unwrap();
        let smoothed = frame
            .encode_with_smoothing(90, false, true, 100, None)
            .unwrap();
        assert_ne!(plain, smoothed);
        assert_eq!(
            SequentialFrame::decode(&smoothed).unwrap().samples.len(),
            17 * 9 * 3
        );
        let error = frame
            .encode_with_smoothing(90, false, true, 101, None)
            .unwrap_err();
        assert!(error.contains("0..=100"), "{error}");
    }

    #[test]
    fn baseline_rejects_twelve_bit_precision_and_sixteen_bit_tables() {
        // SOF0 with 12-bit precision is not the baseline process.
        let payload = vec![12u8, 0, 8, 0, 8, 1, 1, 0x11, 0];
        let error = parse_sof(&payload, Marker::Sof0, true).unwrap_err();
        assert!(error.contains("8-bit"), "{error}");
        // A Pq=1 (16-bit) quantization table is not baseline.
        let mut dqt = vec![0x10u8];
        dqt.extend([1u8; 128]);
        let mut q = [[0u16; 64]; 4];
        let error = parse_dqt(&dqt, &mut q, true).unwrap_err();
        assert!(error.contains("Pq=0"), "{error}");
    }

    #[test]
    fn accepts_at_most_one_trailing_pad_byte() {
        let frame = SequentialFrame::new(8, 8, 8, 1, (0..64).map(|v| v as u16).collect()).unwrap();
        let encoded = frame
            .encode_with_smoothing(90, false, false, 0, None)
            .unwrap();
        assert!(encoded.ends_with(&[0xffu8, 0xd9]), "stream must end in EOI");

        // One 0x00 is the DICOM even-length pad and is accepted; adding it must
        // not change the reconstructed samples (baseline is lossy, so compare
        // against the unpadded decode, not the source).
        let mut padded = encoded.clone();
        padded.push(0x00);
        assert_eq!(
            SequentialFrame::decode(&padded).unwrap().samples,
            SequentialFrame::decode(&encoded).unwrap().samples
        );

        // Two trailing bytes (even zeros) is a spliced/over-padded stream.
        let mut two = encoded.clone();
        two.extend([0x00, 0x00]);
        let error = SequentialFrame::decode(&two).unwrap_err();
        assert!(error.contains("trailing"), "{error}");

        // A single non-zero byte after EOI is garbage.
        let mut garbage = encoded.clone();
        garbage.push(0x01);
        assert!(SequentialFrame::decode(&garbage).is_err());

        // Zero then non-zero is also rejected.
        let mut mixed = encoded.clone();
        mixed.extend([0x00, 0x01]);
        assert!(SequentialFrame::decode(&mixed).is_err());
    }
}
