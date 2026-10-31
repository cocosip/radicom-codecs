#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::needless_range_loop,
    clippy::type_complexity
)]

use super::bit::{BitReader, BitWriter};
use super::dct::{forward, inverse, scaled_table, CHROMA_Q, LUMA_Q, ZIGZAG};
use super::huffman::HuffmanTable;
use super::marker::{Marker, MarkerReader, MarkerWriter};

const COUNTS_DC_LUMA: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const VALUES_DC_LUMA: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const COUNTS_DC_CHROMA: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const VALUES_DC_CHROMA: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const COUNTS_DC_EXTENDED: [u8; 16] = [0, 0, 0, 15, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const VALUES_DC_EXTENDED: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const COUNTS_AC_LUMA: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const VALUES_AC_LUMA: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const COUNTS_AC_CHROMA: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const COUNTS_AC_EXTENDED: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 255, 1, 0, 0, 0, 0, 0, 0, 0];
const VALUES_AC_CHROMA: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequentialFrame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) precision: u8,
    pub(crate) components: u8,
    pub(crate) samples: Vec<u16>,
    pub(crate) sampling_half: bool,
}

struct Tables {
    dc: [HuffmanTable; 2],
    ac: [HuffmanTable; 2],
    q: [[u16; 64]; 2],
}

impl Tables {
    fn new(quality: u32, extended: bool) -> Result<Self, String> {
        let dc_luma = if extended {
            HuffmanTable::new(&COUNTS_DC_EXTENDED, &VALUES_DC_EXTENDED)?
        } else {
            HuffmanTable::new(&COUNTS_DC_LUMA, &VALUES_DC_LUMA)?
        };
        let dc_chroma = if extended {
            HuffmanTable::new(&COUNTS_DC_EXTENDED, &VALUES_DC_EXTENDED)?
        } else {
            HuffmanTable::new(&COUNTS_DC_CHROMA, &VALUES_DC_CHROMA)?
        };
        let extended_values: Vec<u8> = (0..=u8::MAX).collect();
        let ac_luma = if extended {
            HuffmanTable::new(&COUNTS_AC_EXTENDED, &extended_values)?
        } else {
            HuffmanTable::new(&COUNTS_AC_LUMA, &VALUES_AC_LUMA)?
        };
        let ac_chroma = if extended {
            HuffmanTable::new(&COUNTS_AC_EXTENDED, &extended_values)?
        } else {
            HuffmanTable::new(&COUNTS_AC_CHROMA, &VALUES_AC_CHROMA)?
        };
        Ok(Self {
            dc: [dc_luma, dc_chroma],
            ac: [ac_luma, ac_chroma],
            q: [
                scaled_table(&LUMA_Q, quality),
                scaled_table(&CHROMA_Q, quality),
            ],
        })
    }
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
            sampling_half: false,
        })
    }

    pub(crate) fn encode(
        &self,
        quality: u32,
        extended: bool,
        sampling_half: bool,
    ) -> Result<Vec<u8>, String> {
        if sampling_half && self.components != 3 {
            return Err("JPEG 4:2:2 sampling requires three components".into());
        }
        if quality > 100 {
            return Err("JPEG quality must be 0..=100".into());
        }
        let tables = Tables::new(quality.max(1), extended)?;
        let mut out = MarkerWriter::default();
        out.write_marker(Marker::Soi)?;
        out.write_segment(Marker::Dqt, &dqt_payload(&tables.q)?)?;
        out.write_segment(
            if extended { Marker::Sof1 } else { Marker::Sof0 },
            &sof_payload(self, sampling_half),
        )?;
        out.write_segment(Marker::Dht, &dht_payload(extended))?;
        out.write_segment(Marker::Sos, &sos_payload(self.components))?;
        let mut bits = BitWriter::default();
        let blocks_x = if sampling_half {
            self.width.div_ceil(16)
        } else {
            self.width.div_ceil(8)
        };
        let blocks_y = self.height.div_ceil(8);
        let mut previous_dc = [0i32; 3];
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                for c in 0..usize::from(self.components) {
                    let blocks_w = if sampling_half && c == 0 { 2 } else { 1 };
                    for local_block in 0..blocks_w {
                        let mut block = [0i32; 64];
                        for y in 0..8 {
                            for x in 0..8 {
                                let (sx, sy) = if sampling_half && c != 0 {
                                    let sx = (bx * 16 + x * 2).min(self.width - 1);
                                    let sx2 = (sx + 1).min(self.width - 1);
                                    let row = (by * 8 + y).min(self.height - 1);
                                    let a = self.samples[(row * self.width + sx)
                                        * usize::from(self.components)
                                        + c];
                                    let b = self.samples[(row * self.width + sx2)
                                        * usize::from(self.components)
                                        + c];
                                    block[y * 8 + x] = (u32::from(a) + u32::from(b)).div_ceil(2)
                                        as i32
                                        - (1 << (self.precision - 1));
                                    continue;
                                } else {
                                    (
                                        if sampling_half {
                                            bx * 16 + local_block * 8 + x
                                        } else {
                                            bx * 8 + x
                                        }
                                        .min(self.width - 1),
                                        (by * 8 + y).min(self.height - 1),
                                    )
                                };
                                let sample = self.samples
                                    [(sy * self.width + sx) * usize::from(self.components) + c];
                                block[y * 8 + x] = i32::from(sample) - (1 << (self.precision - 1));
                            }
                        }
                        let coeff = forward(&block);
                        let table = &tables.q[c.min(1)];
                        let mut quant = [0i32; 64];
                        for i in 0..64 {
                            quant[i] = (f64::from(coeff[i]) / f64::from(table[i])).round() as i32;
                        }
                        let dc = quant[0];
                        let diff = dc - previous_dc[c];
                        previous_dc[c] = dc;
                        write_value(&mut bits, &tables.dc[c.min(1)], diff)?;
                        let mut run = 0u8;
                        for &index in ZIGZAG.iter().skip(1) {
                            let value = quant[index];
                            if value == 0 {
                                run += 1;
                                if run == 16 {
                                    tables.ac[c.min(1)].encode(0xf0, &mut bits)?;
                                    run = 0;
                                }
                            } else {
                                let size = category(value);
                                tables.ac[c.min(1)].encode((run << 4) | size, &mut bits)?;
                                bits.write_bits(amplitude(value, size), size)?;
                                run = 0;
                            }
                        }
                        if run > 0 {
                            tables.ac[c.min(1)].encode(0, &mut bits)?;
                        }
                    }
                }
            }
        }
        out.write_raw(&bits.finish()?);
        out.write_marker(Marker::Eoi)?;
        Ok(out.into_bytes())
    }

    pub(crate) fn decode(data: &[u8]) -> Result<Self, String> {
        let mut r = MarkerReader::new(data);
        expect(&mut r, Marker::Soi)?;
        let mut q = [[0u16; 64]; 2];
        let mut dcs: [Option<HuffmanTable>; 2] = [None, None];
        let mut acs: [Option<HuffmanTable>; 2] = [None, None];
        let mut sof = None;
        let sos = loop {
            let (m, p) = r.read_segment()?;
            match m {
                Marker::Dqt => parse_dqt(&p, &mut q)?,
                Marker::Dht => parse_dht(&p, &mut dcs, &mut acs)?,
                Marker::Sof0 | Marker::Sof1 => sof = Some(parse_sof(&p, m)?),
                Marker::Sos => break parse_sos(&p, &sof.as_ref().ok_or("missing JPEG SOF")?.6)?,
                Marker::App(_) | Marker::Com => {}
                _ => return Err(format!("unexpected JPEG marker {m:?}")),
            }
        };
        let (width, height, precision, components, sampling_half, qids, _) =
            sof.ok_or("missing JPEG SOF")?;
        let selectors = sos;
        if !matches!(precision, 8 | 12) || usize::from(components) != selectors.len() {
            return Err("unsupported JPEG scan".into());
        }
        if q[0].contains(&0) || (components > 1 && q[1].contains(&0)) {
            return Err("missing JPEG quantization table".into());
        }
        if q[1].iter().all(|&value| value == 0) {
            q[1] = q[0];
        }
        if selectors.iter().any(|&(_, dc, ac, _)| dc > 1 || ac > 1) {
            return Err("unsupported JPEG Huffman table selector".into());
        }
        let tables = Tables {
            dc: [
                dcs[0].clone().ok_or("missing DC Huffman table")?,
                dcs[1].clone().unwrap_or_else(|| dcs[0].clone().unwrap()),
            ],
            ac: [
                acs[0].clone().ok_or("missing AC Huffman table")?,
                acs[1].clone().unwrap_or_else(|| acs[0].clone().unwrap()),
            ],
            q,
        };
        let mut bits = BitReader::new(
            data.get(r.position()..)
                .ok_or("invalid JPEG entropy offset")?,
        );
        let mut samples = vec![
            0u16;
            width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(usize::from(components)))
                .ok_or("JPEG output size overflow")?
        ];
        let mut prev = [0i32; 3];
        let bx = if sampling_half {
            width.div_ceil(16)
        } else {
            width.div_ceil(8)
        };
        let by = height.div_ceil(8);
        for yb in 0..by {
            for xb in 0..bx {
                for c in 0..usize::from(components) {
                    let blocks_w = if sampling_half && c == 0 { 2 } else { 1 };
                    for local_block in 0..blocks_w {
                        let mut quant = [0i32; 64];
                        let dc = read_value(&mut bits, &tables.dc[usize::from(selectors[c].1)])?;
                        prev[c] += dc;
                        quant[0] = prev[c];
                        let mut k = 1;
                        while k < 64 {
                            let sym = tables.ac[usize::from(selectors[c].2)].decode(&mut bits)?;
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
                            coeff[i] = quant[i] * i32::from(tables.q[usize::from(qids[c])][i]);
                        }
                        let block = inverse(&coeff);
                        for yy in 0..8 {
                            for xx in 0..8 {
                                let x = if sampling_half && c != 0 {
                                    (xb * 16 + xx * 2).min(width - 1)
                                } else if sampling_half {
                                    (xb * 16 + local_block * 8 + xx).min(width - 1)
                                } else {
                                    (xb * 8 + xx).min(width - 1)
                                };
                                let y = (yb * 8 + yy).min(height - 1);
                                let v = (block[yy * 8 + xx] + (1 << (precision - 1)))
                                    .clamp(0, (1 << precision) - 1)
                                    as u16;
                                samples[(y * width + x) * usize::from(components) + c] = v;
                                if sampling_half && c != 0 && x + 1 < width {
                                    samples[(y * width + x + 1) * usize::from(components) + c] = v;
                                }
                            }
                        }
                    }
                }
            }
        }
        bits.align_to_byte();
        if bits.take_marker()? != Some(0xd9) {
            return Err("JPEG scan is missing EOI".into());
        }
        let mut frame = Self::new(width, height, precision, components, samples)?;
        frame.sampling_half = sampling_half;
        Ok(frame)
    }
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
fn read_value(r: &mut BitReader<'_>, h: &HuffmanTable) -> Result<i32, String> {
    let n = h.decode(r)?;
    if n == 0 {
        Ok(0)
    } else {
        Ok(decode_amplitude(r.read_bits(n)?, n))
    }
}
fn expect(r: &mut MarkerReader<'_>, m: Marker) -> Result<(), String> {
    let (x, p) = r.read_segment()?;
    if x != m || !p.is_empty() {
        Err("unexpected JPEG marker".into())
    } else {
        Ok(())
    }
}
fn sof_payload(f: &SequentialFrame, sampling_half: bool) -> Vec<u8> {
    let mut p = vec![
        f.precision,
        (f.height as u16 >> 8) as u8,
        f.height as u8,
        (f.width as u16 >> 8) as u8,
        f.width as u8,
        f.components,
    ];
    for c in 0..f.components {
        p.extend([
            c + 1,
            if sampling_half && c == 0 { 0x21 } else { 0x11 },
            if c == 0 { 0 } else { 1 },
        ]);
    }
    p
}
fn sos_payload(n: u8) -> Vec<u8> {
    let mut p = vec![n];
    for c in 0..n {
        p.extend([c + 1, if c == 0 { 0x00 } else { 0x11 }]);
    }
    p.extend([0, 63, 0]);
    p
}
fn dqt_payload(q: &[[u16; 64]; 2]) -> Result<Vec<u8>, String> {
    let mut p = Vec::with_capacity(130);
    for (id, t) in q.iter().enumerate() {
        p.push(id as u8);
        for &index in &ZIGZAG {
            p.push(t[index] as u8);
        }
    }
    Ok(p)
}
fn dht_payload(extended: bool) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(0);
    if extended {
        p.extend(COUNTS_DC_EXTENDED);
        p.extend(VALUES_DC_EXTENDED);
    } else {
        p.extend(COUNTS_DC_LUMA);
        p.extend(VALUES_DC_LUMA);
    }
    p.push(0x10);
    if extended {
        p.extend(COUNTS_AC_EXTENDED);
        p.extend(0..=u8::MAX);
    } else {
        p.extend(COUNTS_AC_LUMA);
        p.extend(VALUES_AC_LUMA);
    }
    p.push(1);
    if extended {
        p.extend(COUNTS_DC_EXTENDED);
        p.extend(VALUES_DC_EXTENDED);
    } else {
        p.extend(COUNTS_DC_CHROMA);
        p.extend(VALUES_DC_CHROMA);
    }
    p.push(0x11);
    if extended {
        p.extend(COUNTS_AC_EXTENDED);
        p.extend(0..=u8::MAX);
    } else {
        p.extend(COUNTS_AC_CHROMA);
        p.extend(VALUES_AC_CHROMA);
    }
    p
}
fn parse_sof(
    p: &[u8],
    _: Marker,
) -> Result<(usize, usize, u8, u8, bool, [u8; 3], [u8; 3]), String> {
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
    let mut qids = [0u8; 3];
    let mut component_ids = [0u8; 3];
    for (i, c) in p[6..].chunks_exact(3).enumerate() {
        if c[2] > 1 || component_ids[..i].contains(&c[0]) {
            return Err("JPEG SOF component definitions are invalid".into());
        }
        qids[i] = c[2];
        component_ids[i] = c[0];
    }
    let half = p[7] == 0x21 && (n == 3) && p[10] == 0x11 && p[13] == 0x11;
    if !half && p[7] != 0x11 {
        return Err("unsupported JPEG sampling factors".into());
    }
    if half && (p[10] != 0x11 || p[13] != 0x11) {
        return Err("unsupported JPEG chroma sampling factors".into());
    }
    Ok((
        usize::from(u16::from_be_bytes([p[3], p[4]])),
        usize::from(u16::from_be_bytes([p[1], p[2]])),
        p[0],
        n,
        half,
        qids,
        component_ids,
    ))
}
fn parse_sos(p: &[u8], component_ids: &[u8; 3]) -> Result<Vec<(u8, u8, u8, u8)>, String> {
    if p.len() < 3 {
        return Err("truncated SOS".into());
    }
    let n = usize::from(p[0]);
    if !(1..=3).contains(&n) || p.len() != 4 + 2 * n {
        return Err("invalid SOS length".into());
    }
    let mut s = [None; 3];
    for i in 0..n {
        let a = 1 + 2 * i;
        let component = component_ids
            .iter()
            .position(|&id| id == p[a])
            .ok_or("JPEG SOS component selectors are invalid")?;
        if s[component].is_some() {
            return Err("JPEG SOS component selectors are invalid".into());
        }
        s[component] = Some((p[a], p[a + 1] >> 4, p[a + 1] & 15, 0x11));
    }
    if p[p.len() - 3] != 0 || p[p.len() - 2] != 63 || p[p.len() - 1] != 0 {
        return Err("unsupported JPEG scan parameters".into());
    }
    Ok(s[..n]
        .iter()
        .map(|selector| selector.ok_or("JPEG SOS component selectors are incomplete"))
        .collect::<Result<Vec<_>, _>>()?)
}
fn parse_dqt(p: &[u8], q: &mut [[u16; 64]; 2]) -> Result<(), String> {
    let mut i = 0;
    while i < p.len() {
        let info = p[i];
        i += 1;
        if info >> 4 != 0 || info & 15 > 1 || i + 64 > p.len() {
            return Err("unsupported DQT".into());
        }
        for k in 0..64 {
            q[usize::from(info & 15)][ZIGZAG[k]] = u16::from(p[i + k]);
        }
        i += 64;
    }
    Ok(())
}
fn parse_dht(
    p: &[u8],
    dc: &mut [Option<HuffmanTable>; 2],
    ac: &mut [Option<HuffmanTable>; 2],
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
        let slot = if info & 15 > 1 {
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
    use super::*;
    #[test]
    fn round_trips_baseline_frame() {
        let f = SequentialFrame::new(9, 7, 8, 1, (0..63).map(|x| x as u16 * 3).collect()).unwrap();
        let d = SequentialFrame::decode(&f.encode(100, false, false).unwrap()).unwrap();
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
        let encoded = frame.encode(90, false, true).unwrap();
        assert!(encoded.windows(3).any(|w| w == [0xff, 0xc0, 0x00]));
        let decoded = SequentialFrame::decode(&encoded).unwrap();
        assert!(decoded.sampling_half);
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
    fn accepts_zero_based_component_identifiers() {
        let samples = (0..8 * 8 * 3).map(|v| (v % 256) as u16).collect();
        let frame = SequentialFrame::new(8, 8, 8, 3, samples).unwrap();
        let mut encoded = frame.encode(90, false, false).unwrap();
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
    fn rejects_more_than_three_components_without_panicking() {
        let mut payload = vec![8, 0, 8, 0, 8, 4];
        for id in 1..=4 {
            payload.extend([id, 0x11, 0]);
        }
        assert!(parse_sof(&payload, Marker::Sof0).is_err());
    }
}
