//! JPEG 2000 marker segments (ISO/IEC 15444-1 Annex A).

pub(crate) const SOC: u16 = 0xFF4F;
pub(crate) const SIZ: u16 = 0xFF51;
pub(crate) const COD: u16 = 0xFF52;
pub(crate) const COC: u16 = 0xFF53;
pub(crate) const TLM: u16 = 0xFF55;
pub(crate) const PLM: u16 = 0xFF57;
#[allow(dead_code)]
pub(crate) const PLT: u16 = 0xFF58;
pub(crate) const QCD: u16 = 0xFF5C;
pub(crate) const QCC: u16 = 0xFF5D;
pub(crate) const RGN: u16 = 0xFF5E;
pub(crate) const POC: u16 = 0xFF5F;
pub(crate) const PPM: u16 = 0xFF60;
pub(crate) const PPT: u16 = 0xFF61;
pub(crate) const CRG: u16 = 0xFF63;
pub(crate) const COM: u16 = 0xFF64;
pub(crate) const SOT: u16 = 0xFF90;
#[allow(dead_code)]
pub(crate) const SOP: u16 = 0xFF91;
#[allow(dead_code)]
pub(crate) const EPH: u16 = 0xFF92;
pub(crate) const SOD: u16 = 0xFF93;
pub(crate) const EOC: u16 = 0xFFD9;

/// Bounded forward cursor over codestream bytes.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub(crate) fn u8(&mut self) -> Result<u8, String> {
        let byte = *self
            .data
            .get(self.pos)
            .ok_or_else(|| "codestream truncated".to_string())?;
        self.pos += 1;
        Ok(byte)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes([
            self.u8()?,
            self.u8()?,
            self.u8()?,
            self.u8()?,
        ]))
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| "segment length overflow".to_string())?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| "codestream truncated".to_string())?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn marker(&mut self) -> Result<u16, String> {
        let value = self.u16()?;
        if value & 0xFF00 != 0xFF00 {
            return Err(format!("invalid marker 0x{value:04X}"));
        }
        Ok(value)
    }

    /// Reads the declared length and returns the payload bounded by it.
    pub(crate) fn segment(&mut self) -> Result<&'a [u8], String> {
        let length = self.u16()?;
        if length < 2 {
            return Err("marker segment length below 2".into());
        }
        self.take(usize::from(length - 2))
    }
}

/// One component declaration from SIZ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SizComp {
    pub(crate) ssiz: u8,
    pub(crate) xrsiz: u8,
    pub(crate) yrsiz: u8,
}

impl SizComp {
    pub(crate) fn precision(&self) -> u8 {
        (self.ssiz & 0x7F) + 1
    }

    pub(crate) fn signed(&self) -> bool {
        self.ssiz & 0x80 != 0
    }
}

/// SIZ image and tile geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Siz {
    pub(crate) rsiz: u16,
    pub(crate) xsiz: u32,
    pub(crate) ysiz: u32,
    pub(crate) xosiz: u32,
    pub(crate) yosiz: u32,
    pub(crate) xtsiz: u32,
    pub(crate) ytsiz: u32,
    pub(crate) xtosiz: u32,
    pub(crate) ytosiz: u32,
    pub(crate) comps: Vec<SizComp>,
}

impl Siz {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self, String> {
        let mut c = Cursor::new(payload);
        let rsiz = c.u16()?;
        let xsiz = c.u32()?;
        let ysiz = c.u32()?;
        let xosiz = c.u32()?;
        let yosiz = c.u32()?;
        let xtsiz = c.u32()?;
        let ytsiz = c.u32()?;
        let xtosiz = c.u32()?;
        let ytosiz = c.u32()?;
        let csiz = c.u16()?;
        if csiz == 0 || csiz > 16384 {
            return Err("invalid component count".into());
        }
        if xsiz <= xosiz || ysiz <= yosiz {
            return Err("invalid image bounds".into());
        }
        if xtsiz == 0 || ytsiz == 0 || xtosiz > xosiz || ytosiz > yosiz {
            return Err("invalid tile bounds".into());
        }
        if xtsiz > xsiz - xosiz || ytsiz > ysiz - yosiz {
            return Err("tile size exceeds image".into());
        }
        let mut comps = Vec::with_capacity(usize::from(csiz));
        for _ in 0..csiz {
            let ssiz = c.u8()?;
            let xrsiz = c.u8()?;
            let yrsiz = c.u8()?;
            if xrsiz == 0 || yrsiz == 0 {
                return Err("zero component subsampling".into());
            }
            if (ssiz & 0x7F) + 1 > 38 {
                return Err("component precision exceeds 38 bits".into());
            }
            comps.push(SizComp { ssiz, xrsiz, yrsiz });
        }
        if !c.is_empty() {
            return Err("trailing bytes in SIZ segment".into());
        }
        Ok(Self {
            rsiz,
            xsiz,
            ysiz,
            xosiz,
            yosiz,
            xtsiz,
            ytsiz,
            xtosiz,
            ytosiz,
            comps,
        })
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&SIZ.to_be_bytes());
        out.extend_from_slice(&(38u16 + 3 * self.comps.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.rsiz.to_be_bytes());
        for value in [
            self.xsiz,
            self.ysiz,
            self.xosiz,
            self.yosiz,
            self.xtsiz,
            self.ytsiz,
            self.xtosiz,
            self.ytosiz,
        ] {
            out.extend_from_slice(&value.to_be_bytes());
        }
        out.extend_from_slice(&(self.comps.len() as u16).to_be_bytes());
        for comp in &self.comps {
            out.push(comp.ssiz);
            out.push(comp.xrsiz);
            out.push(comp.yrsiz);
        }
    }
}

/// Coding style parameters shared by COD and COC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodingStyle {
    pub(crate) decompositions: u8,
    pub(crate) code_block_width: u8,
    pub(crate) code_block_height: u8,
    pub(crate) style: u8,
    pub(crate) transform: u8,
    /// Precinct exponents per resolution; empty means the default 15/15.
    pub(crate) precincts: Vec<(u8, u8)>,
}

impl CodingStyle {
    pub(crate) fn code_block_size(&self) -> Result<(u32, u32), String> {
        if self.code_block_width > 8 || self.code_block_height > 8 {
            return Err("code-block size exponent exceeds 8".into());
        }
        let width = 1u32 << (self.code_block_width + 2);
        let height = 1u32 << (self.code_block_height + 2);
        if width * height > 4096 {
            return Err("code-block area exceeds 4096".into());
        }
        Ok((width, height))
    }
}

/// COD main-header coding style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cod {
    pub(crate) scod: u8,
    pub(crate) progression: u8,
    pub(crate) layers: u16,
    pub(crate) mct: u8,
    pub(crate) style: CodingStyle,
}

impl Cod {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self, String> {
        let mut c = Cursor::new(payload);
        let scod = c.u8()?;
        if scod & 0xFE != 0 {
            return Err("unsupported COD Scod flags".into());
        }
        let progression = c.u8()?;
        if progression > 4 {
            return Err("invalid progression order".into());
        }
        let layers = c.u16()?;
        if layers == 0 {
            return Err("zero quality layers".into());
        }
        let mct = c.u8()?;
        let style = parse_coding_style(&mut c, scod)?;
        if !c.is_empty() {
            return Err("trailing bytes in COD segment".into());
        }
        Ok(Self {
            scod,
            progression,
            layers,
            mct,
            style,
        })
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&COD.to_be_bytes());
        let precinct_bytes = if self.scod & 1 != 0 {
            self.style.precincts.len()
        } else {
            0
        };
        out.extend_from_slice(&(12u16 + precinct_bytes as u16).to_be_bytes());
        out.push(self.scod);
        out.push(self.progression);
        out.extend_from_slice(&self.layers.to_be_bytes());
        out.push(self.mct);
        write_coding_style(out, &self.style);
        if self.scod & 1 != 0 {
            for &(px, py) in &self.style.precincts {
                out.push((px << 4) | py);
            }
        }
    }
}

fn parse_coding_style(c: &mut Cursor<'_>, scod: u8) -> Result<CodingStyle, String> {
    let decompositions = c.u8()?;
    if decompositions > 32 {
        return Err("decomposition level count exceeds 32".into());
    }
    let code_block_width = c.u8()?;
    let code_block_height = c.u8()?;
    let style = c.u8()?;
    if style & 0x04 != 0 || style & 0x10 != 0 {
        return Err("unsupported code-block style (TERMALL/PTERM)".into());
    }
    let transform = c.u8()?;
    if transform > 1 {
        return Err("invalid wavelet transform selector".into());
    }
    let mut precincts = Vec::new();
    if scod & 1 != 0 {
        for _ in 0..=decompositions {
            let byte = c.u8()?;
            precincts.push((byte >> 4, byte & 0x0F));
        }
    }
    Ok(CodingStyle {
        decompositions,
        code_block_width,
        code_block_height,
        style,
        transform,
        precincts,
    })
}

fn write_coding_style(out: &mut Vec<u8>, style: &CodingStyle) {
    out.push(style.decompositions);
    out.push(style.code_block_width);
    out.push(style.code_block_height);
    out.push(style.style);
    out.push(style.transform);
}

/// COC component-specific coding style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Coc {
    pub(crate) component: u16,
    pub(crate) scoc: u8,
    pub(crate) style: CodingStyle,
}

impl Coc {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self, String> {
        let mut c = Cursor::new(payload);
        let component = c.u16()?;
        let scoc = c.u8()?;
        if scoc & 0xFE != 0 {
            return Err("unsupported COC Scoc flags".into());
        }
        let style = parse_coding_style(&mut c, scoc)?;
        if !c.is_empty() {
            return Err("trailing bytes in COC segment".into());
        }
        Ok(Self {
            component,
            scoc,
            style,
        })
    }
}

/// One quantization step size record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QStep {
    pub(crate) exponent: u8,
    pub(crate) mantissa: u16,
}

/// Quantization parameters shared by QCD and QCC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Quantization {
    pub(crate) guard_bits: u8,
    pub(crate) style: u8,
    pub(crate) steps: Vec<QStep>,
}

impl Quantization {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self, String> {
        let mut c = Cursor::new(payload);
        let sqcd = c.u8()?;
        let guard_bits = sqcd >> 5;
        let style = sqcd & 0x1F;
        if guard_bits > 7 {
            return Err("invalid guard bit count".into());
        }
        let mut steps = Vec::new();
        match style {
            0 => {
                while !c.is_empty() {
                    let byte = c.u8()?;
                    steps.push(QStep {
                        exponent: byte >> 3,
                        mantissa: u16::from(byte & 0x07),
                    });
                }
            }
            1 | 2 => {
                while c.remaining() >= 2 {
                    let value = c.u16()?;
                    steps.push(QStep {
                        exponent: (value >> 11) as u8,
                        mantissa: value & 0x07FF,
                    });
                }
                if !c.is_empty() {
                    return Err("odd scalar quantization segment".into());
                }
            }
            _ => return Err("unsupported quantization style".into()),
        }
        Ok(Self {
            guard_bits,
            style,
            steps,
        })
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&QCD.to_be_bytes());
        let bytes_per_step = if self.style == 0 { 1 } else { 2 };
        out.extend_from_slice(
            &(3u16 + bytes_per_step as u16 * self.steps.len() as u16).to_be_bytes(),
        );
        out.push((self.guard_bits << 5) | self.style);
        for step in &self.steps {
            if self.style == 0 {
                out.push((step.exponent << 3) | (step.mantissa as u8 & 0x07));
            } else {
                out.extend_from_slice(
                    &(((step.exponent as u16) << 11) | (step.mantissa & 0x07FF)).to_be_bytes(),
                );
            }
        }
    }

    /// Scalar quantization step for a subband, in the coefficient domain.
    #[allow(dead_code)]
    pub(crate) fn step_size(&self, subband: usize, nominal_bits: u8) -> Result<f64, String> {
        match self.style {
            0 => Ok(1.0),
            1 | 2 => {
                let step = self
                    .steps
                    .get(subband)
                    .ok_or_else(|| "missing quantization step".to_string())?;
                let shift = i32::from(nominal_bits) - i32::from(step.exponent);
                let scale = if shift >= 0 {
                    2f64.powi(shift)
                } else {
                    1.0 / 2f64.powi(-shift)
                };
                Ok(scale * (1.0 + f64::from(step.mantissa) / 2048.0))
            }
            _ => Err("unsupported quantization style".into()),
        }
    }
}

/// Parsed main header (SOC through the last main-header marker).
#[derive(Debug, Clone)]
pub(crate) struct MainHeader {
    pub(crate) siz: Siz,
    pub(crate) cod: Cod,
    pub(crate) cocs: Vec<Coc>,
    pub(crate) qcd: Quantization,
    pub(crate) qccs: Vec<(u16, Quantization)>,
    #[allow(dead_code)]
    pub(crate) tile_part_offsets: Vec<(u32, u64)>,
}

impl MainHeader {
    pub(crate) fn parse(data: &[u8]) -> Result<(Self, usize), String> {
        let mut c = Cursor::new(data);
        if c.marker()? != SOC {
            return Err("missing SOC marker".into());
        }
        let mut siz = None;
        let mut cod = None;
        let mut cocs = Vec::new();
        let mut qcd = None;
        let mut qccs = Vec::new();
        let mut tile_part_offsets = Vec::new();
        loop {
            let marker = c.marker()?;
            match marker {
                SIZ => {
                    if siz.is_some() {
                        return Err("duplicate SIZ marker".into());
                    }
                    siz = Some(Siz::parse(c.segment()?)?);
                }
                COD => {
                    if cod.is_some() {
                        return Err("duplicate COD marker".into());
                    }
                    cod = Some(Cod::parse(c.segment()?)?);
                }
                COC => cocs.push(Coc::parse(c.segment()?)?),
                QCD => {
                    if qcd.is_some() {
                        return Err("duplicate QCD marker".into());
                    }
                    qcd = Some(Quantization::parse(c.segment()?)?);
                }
                QCC => {
                    let payload = c.segment()?;
                    let mut cc = Cursor::new(payload);
                    let component = cc.u16()?;
                    qccs.push((component, Quantization::parse(&payload[2..])?));
                }
                TLM => {
                    let payload = c.segment()?;
                    parse_tlm(payload, &mut tile_part_offsets)?;
                }
                COM => {
                    c.segment()?;
                }
                POC | PPM | PPT | PLM | RGN | CRG => {
                    return Err(format!("unsupported JPEG 2000 marker 0x{marker:04X}"));
                }
                SOT => {
                    // SOT has a fixed 10-byte length and belongs to the tile
                    // stream, so rewind and hand control to the caller.
                    return finish_header(c, siz, cod, cocs, qcd, qccs, tile_part_offsets);
                }
                EOC => return Err("codestream has no tile data".into()),
                other => {
                    return Err(format!("unexpected marker 0x{other:04X} in main header"));
                }
            }
        }
    }

    pub(crate) fn quantization(&self, component: usize) -> &Quantization {
        self.qccs
            .iter()
            .find(|(index, _)| usize::from(*index) == component)
            .map(|(_, q)| q)
            .unwrap_or(&self.qcd)
    }

    pub(crate) fn coding_style(&self, component: usize) -> &CodingStyle {
        self.cocs
            .iter()
            .find(|coc| usize::from(coc.component) == component)
            .map(|coc| &coc.style)
            .unwrap_or(&self.cod.style)
    }
}

fn finish_header(
    c: Cursor<'_>,
    siz: Option<Siz>,
    cod: Option<Cod>,
    cocs: Vec<Coc>,
    qcd: Option<Quantization>,
    qccs: Vec<(u16, Quantization)>,
    tile_part_offsets: Vec<(u32, u64)>,
) -> Result<(MainHeader, usize), String> {
    let siz = siz.ok_or("missing SIZ marker")?;
    let cod = cod.ok_or("missing COD marker")?;
    let qcd = qcd.ok_or("missing QCD marker")?;
    for coc in &cocs {
        if usize::from(coc.component) >= siz.comps.len() {
            return Err("COC references an unknown component".into());
        }
    }
    for (component, _) in &qccs {
        if usize::from(*component) >= siz.comps.len() {
            return Err("QCC references an unknown component".into());
        }
    }
    let expected_steps = crate::quantization::subband_count(cod.style.decompositions);
    if qcd.steps.len() < expected_steps {
        return Err("QCD has fewer steps than the decomposition level count requires".into());
    }
    for (component, quantization) in &qccs {
        let levels = cocs
            .iter()
            .find(|coc| coc.component == *component)
            .map(|coc| coc.style.decompositions)
            .unwrap_or(cod.style.decompositions);
        if quantization.steps.len() < crate::quantization::subband_count(levels) {
            return Err("QCC has fewer steps than the decomposition level count requires".into());
        }
    }
    let position = c.position() - 2;
    Ok((
        MainHeader {
            siz,
            cod,
            cocs,
            qcd,
            qccs,
            tile_part_offsets,
        },
        position,
    ))
}

fn parse_tlm(payload: &[u8], out: &mut Vec<(u32, u64)>) -> Result<(), String> {
    let mut c = Cursor::new(payload);
    let ztlm = c.u8()?;
    let stlm = ztlm & 0x3F;
    let index_bytes = if ztlm & 0x40 != 0 { 0 } else { 1 };
    let length_bytes = ((stlm & 0x03) + 1) as usize;
    while !c.is_empty() {
        let index = if index_bytes == 0 {
            out.len() as u32
        } else {
            u32::from(c.u8()?)
        };
        let mut length = 0u64;
        for _ in 0..length_bytes {
            length = (length << 8) | u64::from(c.u8()?);
        }
        out.push((index, length));
    }
    Ok(())
}

/// Tile-part header returned by the tile scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sot {
    pub(crate) tile: u16,
    pub(crate) length: u32,
    pub(crate) part: u8,
    pub(crate) parts: u8,
}

impl Sot {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self, String> {
        if payload.len() != 8 {
            return Err("invalid SOT segment length".into());
        }
        let mut c = Cursor::new(payload);
        Ok(Self {
            tile: c.u16()?,
            length: c.u32()?,
            part: c.u8()?,
            parts: c.u8()?,
        })
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&SOT.to_be_bytes());
        out.extend_from_slice(&10u16.to_be_bytes());
        out.extend_from_slice(&self.tile.to_be_bytes());
        out.extend_from_slice(&self.length.to_be_bytes());
        out.push(self.part);
        out.push(self.parts);
    }
}

pub(crate) fn write_soc(out: &mut Vec<u8>) {
    out.extend_from_slice(&SOC.to_be_bytes());
}

pub(crate) fn write_sod(out: &mut Vec<u8>) {
    out.extend_from_slice(&SOD.to_be_bytes());
}

pub(crate) fn write_eoc(out: &mut Vec<u8>) {
    out.extend_from_slice(&EOC.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_siz() -> Siz {
        Siz {
            rsiz: 0,
            xsiz: 64,
            ysiz: 48,
            xosiz: 0,
            yosiz: 0,
            xtsiz: 64,
            ytsiz: 48,
            xtosiz: 0,
            ytosiz: 0,
            comps: vec![SizComp {
                ssiz: 7,
                xrsiz: 1,
                yrsiz: 1,
            }],
        }
    }

    #[test]
    fn siz_round_trips() {
        let siz = sample_siz();
        let mut bytes = Vec::new();
        siz.write(&mut bytes);
        let parsed = Siz::parse(&bytes[4..]).unwrap();
        assert_eq!(parsed, siz);
    }

    #[test]
    fn cod_and_qcd_round_trip() {
        let cod = Cod {
            scod: 0,
            progression: 0,
            layers: 1,
            mct: 0,
            style: CodingStyle {
                decompositions: 5,
                code_block_width: 4,
                code_block_height: 4,
                style: 0,
                transform: 0,
                precincts: Vec::new(),
            },
        };
        let mut bytes = Vec::new();
        cod.write(&mut bytes);
        assert_eq!(Cod::parse(&bytes[4..]).unwrap(), cod);

        let qcd = Quantization {
            guard_bits: 2,
            style: 0,
            steps: vec![
                QStep {
                    exponent: 8,
                    mantissa: 0,
                };
                16
            ],
        };
        let mut bytes = Vec::new();
        qcd.write(&mut bytes);
        assert_eq!(Quantization::parse(&bytes[4..]).unwrap(), qcd);
    }

    #[test]
    fn rejects_malformed_segments() {
        assert!(Siz::parse(&[0, 0]).is_err());
        assert!(Cod::parse(&[0, 0, 0, 1, 0, 0, 0, 0, 0]).is_err());
        assert!(Quantization::parse(&[0x02]).is_ok());
        assert!(Quantization::parse(&[0x1F]).is_err());
    }

    #[test]
    fn main_header_requires_required_markers() {
        let mut bytes = Vec::new();
        write_soc(&mut bytes);
        sample_siz().write(&mut bytes);
        assert!(MainHeader::parse(&bytes).is_err());
    }
}
