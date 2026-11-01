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
pub(crate) struct ScanEntry {
    pub scan: ScanHeader,
    pub ids: Vec<u8>,
    /// Entropy bytes are `data[start..end]`; restart markers stay inside.
    pub start: usize,
    pub end: usize,
    /// Component index of a plane-interleaved scan in the frame component order.
    pub component: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedHeader {
    pub frame: FrameHeader,
    /// `(horizontal, vertical)` sampling factors in frame component order.
    pub sampling: Vec<(u8, u8)>,
    /// CharLS colour transform from an APP8 `mrfx` segment; 0 means none.
    pub color_transformation: u8,
    pub scans: Vec<ScanEntry>,
    pub preset: Option<super::preset::Preset>,
    /// DRI restart interval counted in MCUs; 0 means no restart markers.
    pub restart_interval: u32,
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
    let ids: Vec<u8> = (0..components).map(|c| c + 1).collect();
    write_sos_ids(out, &ids, near, ilv)
}
/// Writes a SOS segment with explicit component ids. Plane-interleaved
/// (`ILV=0`) multi-component images use one single-component scan per plane.
pub(crate) fn write_sos_ids(
    out: &mut Vec<u8>,
    ids: &[u8],
    near: u16,
    ilv: u8,
) -> Result<(), String> {
    let components = u8::try_from(ids.len()).map_err(|_| "too many scan components")?;
    let near = u8::try_from(near).map_err(|_| "NEAR must fit the JPEG-LS SOS field")?;
    let mut p = Vec::with_capacity(2 + 2 * components as usize + 3);
    p.push(components);
    for id in ids {
        p.extend([*id, 0]);
    }
    p.extend([near, ilv, 0]);
    segment(out, 0xda, &p)
}
/// Writes a DRI restart-interval segment; the interval is counted in MCUs.
pub(crate) fn write_dri(out: &mut Vec<u8>, interval: u16) {
    segment(out, 0xdd, &interval.to_be_bytes()).expect("DRI payload fits a segment")
}
/// Writes the CharLS colour-transform APP8 segment (`mrfx` + id). CharLS defines
/// this outside T.87; ids 1..=3 are HP1..HP3.
pub(crate) fn write_color_transform(out: &mut Vec<u8>, transform: u8) -> Result<(), String> {
    if !(1..=3).contains(&transform) {
        return Err("unknown JPEG-LS color transformation".into());
    }
    segment(out, 0xe8, &[b'm', b'r', b'f', b'x', transform])
}
#[allow(unused_assignments)]
pub(crate) fn parse_header(data: &[u8]) -> Result<ParsedHeader, String> {
    if data.get(..2) != Some(&[0xff, 0xd8]) {
        return Err("missing SOI".into());
    }
    let mut p = 2;
    let mut frame = None;
    let mut frame_ids: Option<Vec<u8>> = None;
    let mut sampling_factors: Option<Vec<(u8, u8)>> = None;
    let mut color_transformation: Option<u8> = None;
    let mut preset = None;
    let mut restart_interval = 0u32;
    let mut first_sos = None;
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
            first_sos = Some(p);
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
                // T.81 B.2.2: each component carries horizontal and vertical
                // sampling factors in 1..=4; JPEG-LS sub-sampling (T.87 A.2.1)
                // derives each component plane as ceil(dimension * factor / max).
                let mut sampling = Vec::with_capacity(f.components as usize);
                for component in q[6..].chunks(3) {
                    let (hs, vs) = (component[1] >> 4, component[1] & 0x0f);
                    if !(1..=4).contains(&hs) || !(1..=4).contains(&vs) || component[2] != 0 {
                        return Err("unsupported SOF55 sampling factors".into());
                    }
                    sampling.push((hs, vs));
                }
                if frame.is_some() {
                    return Err("duplicate SOF55".into());
                }
                frame_ids = Some(ids);
                sampling_factors = Some(sampling);
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
            // CharLS colour transform extension: APP8, magic `mrfx`, then
            // 0 = None, 1 = HP1, 2 = HP2, 3 = HP3. Like CharLS, only a five-byte
            // payload is a transform; other APP8 segments are SPIFF or private.
            0xe8 if q.len() == 5 && q.starts_with(b"mrfx") => {
                if q[4] > 3 {
                    return Err("unknown JPEG-LS color transformation".into());
                }
                if color_transformation.is_some_and(|value| value != q[4]) {
                    return Err("conflicting JPEG-LS color transformation".into());
                }
                color_transformation = Some(q[4]);
            }
            // T.87 C.2.5: the restart interval is carried in 2, 3 or 4 bytes.
            0xdd => {
                if !(2..=4).contains(&q.len()) {
                    return Err("invalid DRI segment".into());
                }
                restart_interval = q.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b));
            }
            0xe0..=0xef | 0xfe => {}
            _ => return Err(format!("unsupported marker ff{code:02x}")),
        }
        p = end
    }
    let frame = frame.ok_or("missing SOF55")?;
    let frame_ids = frame_ids.ok_or("missing SOF55")?;
    let sampling = sampling_factors.ok_or("missing SOF55")?;
    let color_transformation = color_transformation.unwrap_or(0);
    if color_transformation != 0 && frame.components != 3 {
        return Err("color transformation requires three components".into());
    }
    // CharLS only defines the HP transforms for the 8- and 16-bit containers.
    if color_transformation != 0 && frame.precision != 8 && frame.precision != 16 {
        return Err("color transformation requires 8 or 16 bit precision".into());
    }

    // Each SOS opens a scan whose entropy ends at the next SOS or EOI. T.87
    // B.4 allows one scan per component (plane interleaved) or a single
    // interleaved scan for all components.
    let mut scans = Vec::new();
    let mut at = first_sos.ok_or("missing SOS")?;
    loop {
        let (scan, ids, after) = read_sos(data, at)?;
        let (end, next, code_at) = scan_end(data, after)?;
        scans.push(ScanEntry {
            scan,
            ids,
            start: after,
            end,
            component: 0,
        });
        if next == 0xd9 {
            break;
        }
        at = code_at + 1;
    }

    let (near, ilv, point_transform) = {
        let first = &scans[0];
        (first.scan.near, first.scan.ilv, first.scan.point_transform)
    };
    let mut covered = vec![false; frame_ids.len()];
    for entry in &mut scans {
        if entry.scan.near != near
            || entry.scan.ilv != ilv
            || entry.scan.point_transform != point_transform
        {
            return Err("scan parameters must match across the image".into());
        }
        if ilv != 0 {
            continue;
        }
        if entry.scan.components != 1 {
            return Err("plane-interleaved scans must hold one component".into());
        }
        let index = frame_ids
            .iter()
            .position(|id| *id == entry.ids[0])
            .ok_or("SOS component id does not match SOF55")?;
        if covered[index] {
            return Err("duplicate component scan".into());
        }
        covered[index] = true;
        entry.component = index;
    }
    if ilv == 0 {
        if scans.len() != frame_ids.len() || covered.iter().any(|done| !done) {
            return Err("missing component scan".into());
        }
    } else {
        if scans.len() != 1 {
            return Err("interleaved scans must be a single scan".into());
        }
        let first = &scans[0];
        if first.scan.components != frame.components {
            return Err("SOS component count does not match SOF55".into());
        }
        if first.ids != frame_ids {
            return Err("SOS component ids do not match SOF55".into());
        }
        // T.87 C.2.3: sample interleaving is allowed only when every component
        // in the scan has the same number of columns and lines.
        if ilv == 2 && sampling.iter().any(|factor| *factor != sampling[0]) {
            return Err("sample interleaving requires equal component dimensions".into());
        }
    }
    let maximum = ((1u32 << frame.precision) - 1) as u16;
    let preset = preset.map(|value| value.resolve(maximum, near)).transpose()?;
    Ok(ParsedHeader {
        frame,
        sampling,
        color_transformation,
        scans,
        preset,
        restart_interval,
    })
}

/// Reads a SOS segment whose length field starts at `p`, returning the scan
/// header, the component ids and the index where entropy data begins.
fn read_sos(data: &[u8], p: usize) -> Result<(ScanHeader, Vec<u8>, usize), String> {
    let n = u16::from_be_bytes([
        *data.get(p).ok_or("truncated SOS")?,
        *data.get(p + 1).ok_or("truncated SOS")?,
    ]) as usize;
    if n < 2 {
        return Err("invalid SOS segment length".into());
    }
    let start = p + 2;
    let end = start.checked_add(n - 2).ok_or("SOS overflow")?;
    let q = data.get(start..end).ok_or("truncated SOS payload")?;
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
    let scan = ScanHeader {
        components: ns,
        near: u16::from(q[base]),
        ilv,
        point_transform: q[base + 2],
    };
    Ok((scan, ids, end))
}

/// Finds the marker that ends the entropy starting at `start`.
///
/// Returns `(entropy_end, marker_code, marker_code_index)`. A `0xFF` followed
/// by a high-bit-clear byte is bit-stuffed data (T.87 C.2), not a marker, and
/// `RSTm` beats stay inside the scan.
fn scan_end(data: &[u8], start: usize) -> Result<(usize, u8, usize), String> {
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
        if (0xd0..=0xd7).contains(&c) {
            i = j + 1;
            continue;
        }
        if c == 0xd9 || c == 0xda {
            // All consecutive 0xff bytes before the marker code are fill
            // bytes, so none belong to the entropy payload.
            return Ok((i, c, j));
        }
        return Err(format!("unexpected marker ff{c:02x} in scan"));
    }
    Err("missing EOI".into())
}

/// Splits the scan bytes at RSTm markers.
///
/// `restart_interval` is the DRI value: 0 means no restart marker is legal.
/// Markers must appear as `RST0`, `RST1`, ... cycling through `RST7`, and the
/// scan must hold exactly `expected_segments` of them (an image with `m` MCUs
/// needs `ceil(m / restart_interval)` segments). The count is checked while
/// scanning so a marker flood cannot grow the segment list past that bound.
pub(crate) fn split_restart_segments(
    scan: &[u8],
    restart_interval: u32,
    expected_segments: u64,
) -> Result<Vec<&[u8]>, String> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut expected = 0u8;
    while i < scan.len() {
        if scan[i] != 0xff {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < scan.len() && scan[j] == 0xff {
            j += 1
        }
        let code = *scan.get(j).ok_or("truncated scan marker")?;
        if code & 0x80 == 0 {
            // Stuffed data byte, not a marker.
            i = j + 1;
            continue;
        }
        if !(0xd0..=0xd7).contains(&code) {
            return Err(format!("unexpected marker ff{code:02x} in scan"));
        }
        if restart_interval == 0 {
            return Err("restart marker without a DRI restart interval".into());
        }
        if code - 0xd0 != expected {
            return Err(format!(
                "unexpected restart marker ff{code:02x}, expected ff{:02x}",
                0xd0 + expected
            ));
        }
        if segments.len() as u64 + 2 > expected_segments {
            return Err("too many restart markers for the restart interval".into());
        }
        segments.push(&scan[start..i]);
        expected = (expected + 1) % 8;
        i = j + 1;
        start = i;
    }
    segments.push(&scan[start..]);
    if segments.len() as u64 != expected_segments {
        return Err("restart marker count does not match the restart interval".into());
    }
    Ok(segments)
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
        write_eoi(&mut bytes);
        bytes
    }

    #[test]
    fn parses_the_hp_color_transform_app8_signal() {
        let base = header_with_sampling(&[(1, 1), (1, 1), (1, 1)], 2);
        let with_segment = |payload: &[u8]| {
            let mut bytes = base[..2].to_vec();
            segment(&mut bytes, 0xe8, payload).unwrap();
            bytes.extend(&base[2..]);
            bytes
        };
        for id in 0..=3u8 {
            assert_eq!(
                parse_header(&with_segment(&[b'm', b'r', b'f', b'x', id]))
                    .unwrap()
                    .color_transformation,
                id
            );
        }
        // CharLS only treats a five-byte APP8 payload as a transform; other
        // private segments (SPIFF, vendor data) are skipped.
        for id in 0..=3u8 {
            assert_eq!(
                parse_header(&with_segment(&[b'm', b'r', b'f', b'x', id, 0]))
                    .unwrap()
                    .color_transformation,
                0
            );
        }
        assert!(parse_header(&with_segment(&[b'm', b'r', b'f', b'x', 4]))
            .unwrap_err()
            .contains("unknown JPEG-LS color transformation"));
    }

    #[test]
    fn rejects_a_color_transform_on_a_single_component_image() {
        let bytes = header_with_segments(&[(0xe8, b"mrfx\x01")], 0);
        assert!(parse_header(&bytes)
            .unwrap_err()
            .contains("three components"));
    }

    #[test]
    fn rejects_sos_mapping_table_selector() {
        let mut bytes = header_with_segments(&[], 0);
        // The mapping selector of the first scan component is the third byte of
        // the SOS payload, which sits six bytes before the trailing EOI.
        let length = bytes.len();
        bytes[length - 6] = 1;
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
    fn rejects_a_scan_without_eoi_and_ignores_bytes_after_eoi() {
        // Truncated scan: the entropy is not followed by an EOI marker.
        let mut truncated = header_with_segments(&[], 0);
        truncated.truncate(truncated.len() - 2);
        truncated.extend([0x01, 0x02]);
        assert!(parse_header(&truncated)
            .unwrap_err()
            .contains("missing EOI"));

        // Bytes after EOI are fragment padding (fo-dicom sizes fragments by
        // ArrayPool bucket), not scan data, so they are ignored.
        let mut data = header_with_segments(&[], 0);
        let scan_start = data.len() - 2;
        data.insert(scan_start, 0x01);
        data.extend([0x00, 0x01]);
        let parsed = parse_header(&data).unwrap();
        assert_eq!(parsed.scans.len(), 1);
        assert_eq!(parsed.scans[0].start, scan_start);
        assert_eq!(parsed.scans[0].end, scan_start + 1);
    }

    /// Minimal codestream with explicit `(horizontal, vertical)` sampling
    /// factors, one interleaved scan and no entropy.
    fn header_with_sampling(sampling: &[(u8, u8)], ilv: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_soi(&mut bytes);
        let mut payload = vec![8, 0, 1, 0, 1, sampling.len() as u8];
        for (index, (hs, vs)) in sampling.iter().enumerate() {
            payload.extend([index as u8 + 1, (hs << 4) | vs, 0]);
        }
        segment(&mut bytes, 0xf7, &payload).unwrap();
        write_sos(&mut bytes, sampling.len() as u8, 0, ilv).unwrap();
        bytes.extend([0x00, 0xff, 0xd9]);
        bytes
    }

    #[test]
    fn parses_component_sampling_factors_and_rejects_invalid_ones() {
        // T.87 E.1.2 test 7 sampling: red H2V4, green H2V1, blue H1V2.
        let parsed = parse_header(&header_with_sampling(&[(2, 4), (2, 1), (1, 2)], 1)).unwrap();
        assert_eq!(parsed.sampling, vec![(2, 4), (2, 1), (1, 2)]);

        let uniform = parse_header(&header_with_sampling(&[(1, 1), (1, 1), (1, 1)], 2)).unwrap();
        assert_eq!(uniform.sampling, vec![(1, 1); 3]);

        // T.81 B.2.2: factors are in 1..=4 and the table selector must be zero.
        assert!(parse_header(&header_with_sampling(&[(0, 1), (1, 1), (1, 1)], 1))
            .unwrap_err()
            .contains("unsupported SOF55 sampling factors"));
        assert!(parse_header(&header_with_sampling(&[(5, 1), (1, 1), (1, 1)], 1)).is_err());
        assert!(parse_header(&header_with_sampling(&[(1, 1), (1, 1), (1, 1)], 1)).is_ok());
    }

    #[test]
    fn rejects_sample_interleaving_of_unequal_components() {
        // T.87 C.2.3: sample interleaving needs identical columns and lines.
        assert!(parse_header(&header_with_sampling(&[(2, 4), (2, 1), (1, 2)], 2))
            .unwrap_err()
            .contains("sample interleaving requires equal component dimensions"));
        // Line interleaving carries sub-sampled components, so it stays legal.
        assert!(parse_header(&header_with_sampling(&[(2, 4), (2, 1), (1, 2)], 1)).is_ok());
    }

    #[test]
    fn keeps_restart_markers_and_stuffed_bytes_inside_the_scan() {
        // RSTm beats, 0xFF-stuffed data and the T.87 bit-stuffed `ff 01`
        // sequence (as produced by the reference encoder) are entropy, not
        // marker structure.
        let mut data = header_with_segments(&[], 0);
        data.truncate(data.len() - 2);
        let start = data.len();
        data.extend([
            0x01, 0xff, 0x00, 0xff, 0xd0, 0x02, 0xff, 0x01, 0x03, 0xff, 0xd9,
        ]);
        let parsed = parse_header(&data).unwrap();
        assert_eq!(parsed.scans[0].start, start);
        // The EOI marker is excluded from the entropy.
        assert_eq!(parsed.scans[0].end, data.len() - 2);
    }

    #[test]
    fn validates_plane_interleaved_scan_coverage() {
        // Three plane-interleaved scans cover components 1, 2 and 3.
        let parsed = parse_header(&multi_scan(&[&[1], &[2], &[3]], 0, 0)).unwrap();
        assert_eq!(parsed.scans.len(), 3);
        assert_eq!(parsed.scans[1].component, 1);
        assert_eq!(parsed.scans[2].component, 2);

        let missing = multi_scan(&[&[1], &[2]], 0, 0);
        assert!(parse_header(&missing)
            .unwrap_err()
            .contains("missing component scan"));

        let duplicate = multi_scan(&[&[1], &[1], &[3]], 0, 0);
        assert!(parse_header(&duplicate)
            .unwrap_err()
            .contains("duplicate component scan"));

        let unknown = multi_scan(&[&[1], &[2], &[4]], 0, 0);
        assert!(parse_header(&unknown)
            .unwrap_err()
            .contains("SOS component id does not match SOF55"));

        // A plane-interleaved scan may only hold one component.
        let wide = multi_scan(&[&[1, 2, 3]], 0, 0);
        assert!(parse_header(&wide)
            .unwrap_err()
            .contains("plane-interleaved scans must hold one component"));

        // An interleaved image is exactly one scan with all components.
        let split = multi_scan(&[&[1, 2, 3], &[1, 2, 3]], 0, 2);
        assert!(parse_header(&split)
            .unwrap_err()
            .contains("interleaved scans must be a single scan"));

        let short = multi_scan(&[&[1]], 0, 1);
        assert!(parse_header(&short)
            .unwrap_err()
            .contains("SOS component count does not match SOF55"));

        // Every scan of the image must use the same NEAR, ILV and mapping.
        let mut mixed = multi_scan(&[&[1], &[2], &[3]], 0, 0);
        let sos2 = mixed
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] == 0xff && w[1] == 0xda)
            .nth(1)
            .map(|(i, _)| i)
            .unwrap();
        // SOS payload layout for one component: ns, id, map, NEAR, ILV, point.
        mixed[sos2 + 4 + 3] = 3;
        assert!(parse_header(&mixed)
            .unwrap_err()
            .contains("scan parameters must match across the image"));
    }

    fn multi_scan(scans: &[&[u8]], near: u16, ilv: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_soi(&mut bytes);
        write_sof55(
            &mut bytes,
            &FrameHeader {
                precision: 8,
                width: 1,
                height: 1,
                components: 3,
            },
        )
        .unwrap();
        for ids in scans {
            write_sos_ids(&mut bytes, ids, near, ilv).unwrap();
            bytes.push(0x00);
        }
        write_eoi(&mut bytes);
        bytes
    }

    #[test]
    fn parses_dri_restart_intervals_of_two_three_and_four_bytes() {
        let two = header_with_segments(&[(0xdd, &[0, 3])], 0);
        assert_eq!(parse_header(&two).unwrap().restart_interval, 3);
        let three = header_with_segments(&[(0xdd, &[1, 0, 0])], 0);
        assert_eq!(parse_header(&three).unwrap().restart_interval, 0x1_0000);
        let four = header_with_segments(&[(0xdd, &[0, 0, 0, 7])], 0);
        assert_eq!(parse_header(&four).unwrap().restart_interval, 7);
        // Zero disables restarts and an impossible length is rejected.
        let zero = header_with_segments(&[(0xdd, &[0, 0])], 0);
        assert_eq!(parse_header(&zero).unwrap().restart_interval, 0);
        let short = header_with_segments(&[(0xdd, &[3])], 0);
        assert!(parse_header(&short).is_err());
        let long = header_with_segments(&[(0xdd, &[0, 0, 0, 0, 3])], 0);
        assert!(parse_header(&long).is_err());
    }

    #[test]
    fn split_restart_segments_validates_sequence_and_dri_presence() {
        let scan = [0x01, 0xff, 0xd0, 0x02, 0xff, 0xd1, 0x03];
        assert_eq!(
            split_restart_segments(&scan, 2, 3).unwrap(),
            vec![&[0x01u8][..], &[0x02][..], &[0x03][..]]
        );
        // A stuffed 0xff is data, not a marker.
        let stuffed = [0xff, 0x00, 0x07];
        assert_eq!(
            split_restart_segments(&stuffed, 1, 1).unwrap(),
            vec![stuffed.as_slice()]
        );
        // RSTm must follow RST0..RST7 in order.
        let out_of_order = [0x01, 0xff, 0xd1, 0x02];
        assert!(split_restart_segments(&out_of_order, 1, 2)
            .unwrap_err()
            .contains("expected ffd0"));
        // Restart markers are illegal without a DRI interval.
        let no_dri = [0x01, 0xff, 0xd0, 0x02];
        assert!(split_restart_segments(&no_dri, 0, 1).is_err());
        // Fill bytes before a marker are not part of the segment.
        let filled = [0x01, 0xff, 0xff, 0xd0, 0x02];
        assert_eq!(
            split_restart_segments(&filled, 1, 2).unwrap(),
            vec![&[0x01u8][..], &[0x02][..]]
        );
        // The segment count must match the restart interval.
        let two = [0x01, 0xff, 0xd0, 0x02];
        assert!(split_restart_segments(&two, 1, 3)
            .unwrap_err()
            .contains("count does not match"));
        assert!(split_restart_segments(&two, 1, 1)
            .unwrap_err()
            .contains("too many restart markers"));
    }
}
