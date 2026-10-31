#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Preset {
    pub maxval: u16,
    pub t1: u16,
    pub t2: u16,
    pub t3: u16,
    pub reset: u16,
}
impl Preset {
    pub(crate) fn new(maxval: u16, t1: u16, t2: u16, t3: u16, reset: u16) -> Result<Self, String> {
        if maxval < 1 || t1 < 1 || t1 > t2 || t2 > t3 || t3 > maxval || reset < 3 {
            return Err("invalid JPEG-LS preset".into());
        }
        Ok(Self {
            maxval,
            t1,
            t2,
            t3,
            reset,
        })
    }
    pub(crate) fn defaults(maxval: u16, near: u16) -> Result<Self, String> {
        if near > maxval / 2 {
            return Err("NEAR exceeds JPEG-LS maximum".into());
        }
        let m = maxval as i32;
        let n = near as i32;
        let clamp = |x: i32, lo: i32| x.max(lo).min(m);
        let (t1, t2, t3) = if m >= 128 {
            let f = (m.min(4095) + 128) / 256;
            (
                clamp(f + 2 + 3 * n, n + 1),
                clamp(f * 4 + 3 + 5 * n, 0),
                clamp(f * 17 + 4 + 7 * n, 0),
            )
        } else {
            let f = 256 / (m + 1);
            (
                clamp((3).max(3 * n + 3), n + 1),
                clamp((7 / f).max(3 + 5 * n), 0),
                clamp((21 / f).max(4 + 7 * n), 0),
            )
        };
        let t2 = t2.max(t1);
        let t3 = t3.max(t2);
        Self::new(maxval, t1 as u16, t2 as u16, t3 as u16, 64)
    }
    pub(crate) fn range(&self, near: u16) -> u32 {
        (u32::from(self.maxval) + 2 * u32::from(near)) / (2 * u32::from(near) + 1) + 1
    }
    pub(crate) fn qbpp(&self, near: u16) -> u8 {
        let r = self.range(near);
        (32 - (r - 1).leading_zeros()) as u8
    }
    pub(crate) fn limit(&self, _near: u16) -> u32 {
        let b = (32 - u32::from(self.maxval).leading_zeros()).max(1);
        2 * (b + b.max(8))
    }
}
