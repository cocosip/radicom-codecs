#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    Soi,
    Eoi,
    Sof0,
    Sof1,
    Sof3,
    Dqt,
    Dht,
    Dri,
    Sos,
    App(u8),
    Com,
    Restart(u8),
    Other(u8),
}

impl Marker {
    fn code(self) -> Result<u8, String> {
        Ok(match self {
            Self::Soi => 0xd8,
            Self::Eoi => 0xd9,
            Self::Sof0 => 0xc0,
            Self::Sof1 => 0xc1,
            Self::Sof3 => 0xc3,
            Self::Dqt => 0xdb,
            Self::Dht => 0xc4,
            Self::Dri => 0xdd,
            Self::Sos => 0xda,
            Self::App(index) if index < 16 => 0xe0 + index,
            Self::App(index) => return Err(format!("APP marker index {index} is outside 0..=15")),
            Self::Com => 0xfe,
            Self::Restart(index) if index < 8 => 0xd0 + index,
            Self::Restart(index) => {
                return Err(format!("restart marker index {index} is outside 0..=7"))
            }
            Self::Other(code) if code != 0 && code != 0xff => code,
            Self::Other(code) => return Err(format!("invalid JPEG marker code 0x{code:02x}")),
        })
    }

    fn from_code(code: u8) -> Result<Self, String> {
        Ok(match code {
            0xd8 => Self::Soi,
            0xd9 => Self::Eoi,
            0xc0 => Self::Sof0,
            0xc1 => Self::Sof1,
            0xc3 => Self::Sof3,
            0xdb => Self::Dqt,
            0xc4 => Self::Dht,
            0xdd => Self::Dri,
            0xda => Self::Sos,
            0xe0..=0xef => Self::App(code - 0xe0),
            0xfe => Self::Com,
            0xd0..=0xd7 => Self::Restart(code - 0xd0),
            0..=1 | 0xff => return Err(format!("invalid JPEG marker code 0x{code:02x}")),
            other => Self::Other(other),
        })
    }

    fn is_standalone(self) -> bool {
        matches!(self, Self::Soi | Self::Eoi | Self::Restart(_))
    }
}

#[derive(Debug)]
pub(crate) struct MarkerReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> MarkerReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn seek(&mut self, position: usize) {
        self.position = position;
    }

    pub(crate) fn read_segment(&mut self) -> Result<(Marker, Vec<u8>), String> {
        let marker = self.read_marker()?;
        if marker.is_standalone() {
            return Ok((marker, Vec::new()));
        }
        let length = usize::from(self.read_u16()?);
        if length < 2 {
            return Err(
                "JPEG segment length must include at least its two length bytes".to_owned(),
            );
        }
        let payload_length = length - 2;
        let end = self
            .position
            .checked_add(payload_length)
            .ok_or_else(|| "JPEG segment length overflows usize".to_owned())?;
        let payload = self
            .data
            .get(self.position..end)
            .ok_or_else(|| "JPEG segment exceeds input".to_owned())?
            .to_vec();
        self.position = end;
        Ok((marker, payload))
    }

    fn read_marker(&mut self) -> Result<Marker, String> {
        let prefix = *self
            .data
            .get(self.position)
            .ok_or_else(|| "truncated JPEG marker prefix".to_owned())?;
        self.position += 1;
        if prefix != 0xff {
            return Err(format!("expected JPEG marker prefix, got 0x{prefix:02x}"));
        }
        while self.data.get(self.position) == Some(&0xff) {
            self.position += 1;
        }
        let code = *self
            .data
            .get(self.position)
            .ok_or_else(|| "truncated JPEG marker code".to_owned())?;
        self.position += 1;
        if code == 0 {
            return Err("byte-stuffed 0xff is not a JPEG marker".to_owned());
        }
        Marker::from_code(code)
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        let bytes = self
            .data
            .get(self.position..self.position + 2)
            .ok_or_else(|| "truncated JPEG segment length".to_owned())?;
        self.position += 2;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }
}

#[derive(Debug, Default)]
pub(crate) struct MarkerWriter {
    bytes: Vec<u8>,
}

impl MarkerWriter {
    pub(crate) fn write_marker(&mut self, marker: Marker) -> Result<(), String> {
        self.bytes.extend([0xff, marker.code()?]);
        Ok(())
    }

    pub(crate) fn write_segment(&mut self, marker: Marker, payload: &[u8]) -> Result<(), String> {
        if marker.is_standalone() {
            return Err("standalone JPEG markers cannot carry a segment payload".to_owned());
        }
        let length = payload
            .len()
            .checked_add(2)
            .ok_or_else(|| "JPEG segment length overflows usize".to_owned())?;
        let length = u16::try_from(length).map_err(|_| "JPEG segment is too large".to_owned())?;
        self.write_marker(marker)?;
        self.bytes.extend(length.to_be_bytes());
        self.bytes.extend_from_slice(payload);
        Ok(())
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn write_raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_standalone_and_length_prefixed_markers() {
        let data = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x01, 0x02, 0xff, 0xd9];
        let mut reader = MarkerReader::new(&data);
        assert_eq!(reader.read_segment().unwrap(), (Marker::Soi, Vec::new()));
        assert_eq!(
            reader.read_segment().unwrap(),
            (Marker::App(0), vec![0x01, 0x02])
        );
        assert_eq!(reader.read_segment().unwrap(), (Marker::Eoi, Vec::new()));
        assert!(reader.read_segment().is_err());
    }

    #[test]
    fn rejects_truncated_or_stuffed_marker_headers() {
        assert!(MarkerReader::new(&[0xff]).read_segment().is_err());
        assert!(MarkerReader::new(&[0xff, 0x00]).read_segment().is_err());
        assert!(MarkerReader::new(&[0xff, 0xe0, 0x00])
            .read_segment()
            .is_err());
    }

    #[test]
    fn writes_markers_and_segments() {
        let mut writer = MarkerWriter::default();
        writer.write_marker(Marker::Soi).unwrap();
        writer.write_segment(Marker::Dqt, &[0x11, 0x22]).unwrap();
        writer.write_marker(Marker::Eoi).unwrap();
        assert_eq!(
            writer.into_bytes(),
            vec![0xff, 0xd8, 0xff, 0xdb, 0x00, 0x04, 0x11, 0x22, 0xff, 0xd9]
        );
    }

    #[test]
    fn rejects_out_of_range_app_and_restart_markers() {
        let mut writer = MarkerWriter::default();
        assert!(writer.write_marker(Marker::App(16)).is_err());
        assert!(writer.write_marker(Marker::Restart(8)).is_err());
    }
}
