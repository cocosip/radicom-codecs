#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    pub precision: u8,
    pub height: u16,
    pub width: u16,
    pub components: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanHeader {
    pub components: u8,
    pub near: u16,
    pub ilv: u8,
    pub point_transform: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedHeader {
    pub frame: FrameHeader,
    pub scan: ScanHeader,
    pub preset: Option<super::preset::Preset>,
    pub scan_start: usize,
}
fn marker(out: &mut Vec<u8>, code: u8) {
    out.extend([0xff, code]);
}
fn segment(out: &mut Vec<u8>, code: u8, p: &[u8]) -> Result<(), String> {
    let n = u16::try_from(p.len() + 2).map_err(|_| "segment too large")?;
    marker(out, code);
    out.extend(n.to_be_bytes());
    out.extend(p);
    Ok(())
}
pub(crate) fn write_soi(out: &mut Vec<u8>) {
    marker(out, 0xd8)
}
pub(crate) fn write_eoi(out: &mut Vec<u8>) {
    marker(out, 0xd9)
}
pub(crate) fn write_sof55(out: &mut Vec<u8>, f: &FrameHeader) -> Result<(), String> {
    let mut p = Vec::with_capacity(6 + 3 * f.components as usize);
    p.push(f.precision);
    p.extend(f.height.to_be_bytes());
    p.extend(f.width.to_be_bytes());
    p.push(f.components);
    for c in 0..f.components {
        p.extend([c + 1, 0x11, 0]);
    }
    segment(out, 0xf7, &p)
}
pub(crate) fn write_sos(
    out: &mut Vec<u8>,
    components: u8,
    near: u16,
    ilv: u8,
) -> Result<(), String> {
    let near = u8::try_from(near).map_err(|_| "NEAR must fit the JPEG-LS SOS field")?;
    let mut p = Vec::with_capacity(2 + 2 * components as usize + 3);
    p.push(components);
    for c in 0..components {
        p.extend([c + 1, 0]);
    }
    p.extend([near, ilv, 0]);
    segment(out, 0xda, &p)
}
#[allow(unused_assignments)]
pub(crate) fn parse_header(data: &[u8]) -> Result<ParsedHeader, String> {
    let mut p = 0;
    if data.get(..2) != Some(&[0xff, 0xd8]) {
        return Err("missing SOI".into());
    }
    p = 2;
    let mut frame = None;
    let mut frame_ids = None;
    let mut preset = None;
    let mut scan = None;
    let mut scan_ids = None;
    loop {
        if p + 2 > data.len() {
            return Err("truncated marker".into());
        }
        if data[p] != 0xff {
            return Err("expected marker".into());
        }
        while p < data.len() && data[p] == 0xff {
            p += 1
        }
        let code = *data.get(p).ok_or("truncated marker code")?;
        p += 1;
        if code == 0xd9 {
            return Err("missing SOS".into());
        }
        if code == 0xda {
            let n = u16::from_be_bytes([
                *data.get(p).ok_or("truncated SOS")?,
                *data.get(p + 1).ok_or("truncated SOS")?,
            ]) as usize;
            if n < 2 {
                return Err("invalid SOS segment length".into());
            }
            p += 2;
            let end = p.checked_add(n - 2).ok_or("SOS overflow")?;
            let q = data.get(p..end).ok_or("truncated SOS payload")?;
            if q.len() < 4 {
                return Err("short SOS".into());
            }
            let ns = q[0];
            if ns == 0 || q.len() != 1 + 2 * ns as usize + 3 {
                return Err("invalid SOS components".into());
            }
            let ids = q[1..1 + 2 * ns as usize]
                .chunks(2)
                .map(|component| component[0])
                .collect::<Vec<_>>();
            if ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id)) {
                return Err("duplicate SOS component id".into());
            }
            if q[1..1 + 2 * ns as usize]
                .chunks(2)
                .any(|component| component[1] != 0)
            {
                return Err("JPEG-LS mapping tables are unsupported".into());
            }
            let base = 1 + 2 * ns as usize;
            let ilv = q[base + 1];
            if ilv > 2 {
                return Err("unsupported JPEG-LS interleave mode".into());
            }
            if scan.is_some() {
                return Err("duplicate SOS".into());
            }
            scan_ids = Some(ids);
            scan = Some(ScanHeader {
                components: ns,
                near: u16::from(q[base]),
                ilv,
                point_transform: q[base + 2],
            });
            p = end;
            break;
        }
        let n = u16::from_be_bytes([
            *data.get(p).ok_or("truncated segment")?,
            *data.get(p + 1).ok_or("truncated segment")?,
        ]) as usize;
        p += 2;
        if n < 2 {
            return Err("invalid segment length".into());
        }
        let end = p.checked_add(n - 2).ok_or("segment overflow")?;
        let q = data.get(p..end).ok_or("truncated segment payload")?;
        match code {
            0xf7 => {
                if q.len() < 6 {
                    return Err("short SOF55".into());
                }
                let f = FrameHeader {
                    precision: q[0],
                    height: u16::from_be_bytes([q[1], q[2]]),
                    width: u16::from_be_bytes([q[3], q[4]]),
                    components: q[5],
                };
                if !(2..=16).contains(&f.precision)
                    || f.width == 0
                    || f.height == 0
                    || f.components == 0
                    || q.len() != 6 + 3 * f.components as usize
                {
                    return Err("invalid SOF55".into());
                }
                let ids = q[6..]
                    .chunks(3)
                    .map(|component| component[0])
                    .collect::<Vec<_>>();
                if ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id)) {
                    return Err("duplicate SOF55 component id".into());
                }
                if q[6..]
                    .chunks(3)
                    .any(|component| component[1] != 0x11 || component[2] != 0)
                {
                    return Err("unsupported SOF55 sampling factors".into());
                }
                if frame.is_some() {
                    return Err("duplicate SOF55".into());
                }
                frame_ids = Some(ids);
                frame = Some(f)
            }
            0xf8 => {
                if q.len() != 11 || q[0] != 1 {
                    return Err("invalid LSE".into());
                }
                let vals = [
                    u16::from_be_bytes([q[1], q[2]]),
                    u16::from_be_bytes([q[3], q[4]]),
                    u16::from_be_bytes([q[5], q[6]]),
                    u16::from_be_bytes([q[7], q[8]]),
                    u16::from_be_bytes([q[9], q[10]]),
                ];
                if preset.is_some() {
                    return Err("duplicate LSE".into());
                }
                preset = Some(super::preset::Preset {
                    maxval: vals[0],
                    t1: vals[1],
                    t2: vals[2],
                    t3: vals[3],
                    reset: vals[4],
                });
            }
            0xe8 if q.starts_with(b"mrfx") => {
                if q.len() != 5 || q[4] != 0 {
                    return Err("HP JPEG-LS color transforms are unsupported".into());
                }
            }
            0xe0..=0xef | 0xfe => {}
            _ => return Err(format!("unsupported marker ff{code:02x}")),
        }
        p = end
    }
    let frame = frame.ok_or("missing SOF55")?;
    let scan = scan.ok_or("missing SOS")?;
    if scan.components != frame.components {
        return Err("SOS component count does not match SOF55".into());
    }
    if frame_ids != scan_ids {
        return Err("SOS component ids do not match SOF55".into());
    }
    let maximum = ((1u32 << frame.precision) - 1) as u16;
    let preset = preset
        .map(|value| value.resolve(maximum, scan.near))
        .transpose()?;
    Ok(ParsedHeader {
        frame,
        scan,
        preset,
        scan_start: p,
    })
}

pub(crate) fn split_scan(data: &[u8], start: usize) -> Result<(&[u8], usize), String> {
    let mut i = start;
    while i < data.len() {
        if data[i] != 0xff {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < data.len() && data[j] == 0xff {
            j += 1
        }
        let c = *data.get(j).ok_or("truncated scan marker")?;
        if c == 0 || c & 0x80 == 0 {
            i = j + 1;
            continue;
        }
        if c == 0xd9 {
            // All consecutive 0xff bytes before the marker code are fill
            // bytes, so none belong to the entropy payload.
            return Ok((&data[start..i], j - 1));
        }
        if (0xd0..=0xd7).contains(&c) {
            return Err("restart markers are not supported".into());
        }
        return Err(format!("unexpected marker ff{c:02x} in scan"));
    }
    Err("missing EOI".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_with_segments(segments: &[(u8, &[u8])], near: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_soi(&mut bytes);
        write_sof55(
            &mut bytes,
            &FrameHeader {
                precision: 8,
                width: 1,
                height: 1,
                components: 1,
            },
        )
        .unwrap();
        for (code, payload) in segments {
            segment(&mut bytes, *code, payload).unwrap();
        }
        write_sos(&mut bytes, 1, near, 0).unwrap();
        bytes
    }

    #[test]
    fn rejects_hp_transform_instead_of_skipping_its_app8_signal() {
        let bytes = header_with_segments(&[(0xe8, b"mrfx\x01")], 0);
        assert!(parse_header(&bytes).is_err());
    }

    #[test]
    fn rejects_sos_mapping_table_selector() {
        let mut bytes = header_with_segments(&[], 0);
        let length = bytes.len();
        bytes[length - 4] = 1;
        assert!(parse_header(&bytes).is_err());
    }

    #[test]
    fn resolves_zero_lse_fields_and_checks_near_thresholds() {
        let p = [1, 0, 255, 0, 0, 0, 0, 0, 0, 0, 8];
        let parsed = parse_header(&header_with_segments(&[(0xf8, &p)], 3)).unwrap();
        let preset = parsed.preset.unwrap();
        assert_eq!(
            [preset.t1, preset.t2, preset.t3, preset.reset],
            [12, 22, 42, 8]
        );
        let invalid = [1, 0, 255, 0, 1, 0, 7, 0, 21, 0, 64];
        assert!(parse_header(&header_with_segments(&[(0xf8, &invalid)], 3)).is_err());
    }

    #[test]
    fn sos_rejects_near_values_that_do_not_fit_the_wire_field() {
        let mut out = Vec::new();
        assert!(write_sos(&mut out, 1, 256, 0).is_err());
    }

    #[test]
    fn split_scan_rejects_missing_eoi_and_trailing_data_is_visible() {
        assert!(split_scan(&[0x01, 0x02], 0).is_err());
        let data = [0x01, 0xff, 0xd9, 0x00, 0x01];
        let (scan, tail) = split_scan(&data, 0).unwrap();
        assert_eq!(scan, &[0x01]);
        assert_eq!(&data[tail + 2..], &[0x00, 0x01]);

        let (scan, tail) = split_scan(&[0x01, 0xff, 0xff, 0xd9], 0).unwrap();
        assert_eq!(scan, &[0x01]);
        assert_eq!(tail, 2);
    }

    #[test]
    fn parse_header_rejects_restart_interval_marker() {
        let data = [0xff, 0xd8, 0xff, 0xdd, 0x00, 0x04, 0x00, 0x01];
        assert!(parse_header(&data)
            .unwrap_err()
            .contains("unsupported marker ffdd"));
    }
}
