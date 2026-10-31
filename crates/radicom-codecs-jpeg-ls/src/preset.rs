#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Preset {
    pub maxval: u16,
    pub t1: u16,
    pub t2: u16,
    pub t3: u16,
    pub reset: u16,
}

impl Preset {
    pub(crate) fn resolve(self, maximum: u16, near: u16) -> Result<Self, String> {
        let maxval = if self.maxval == 0 {
            maximum
        } else {
            self.maxval
        };
        if maxval > maximum {
            return Err("LSE MAXVAL exceeds sample precision".into());
        }
        let defaults = Self::defaults(maxval, near)?;
        let t1 = if self.t1 == 0 { defaults.t1 } else { self.t1 };
        if t1 <= near {
            return Err("LSE threshold T1 must exceed NEAR".into());
        }
        Self::new(
            maxval,
            t1,
            if self.t2 == 0 { defaults.t2 } else { self.t2 },
            if self.t3 == 0 { defaults.t3 } else { self.t3 },
            if self.reset == 0 {
                defaults.reset
            } else {
                self.reset
            },
        )
    }
    pub(crate) fn new(maxval: u16, t1: u16, t2: u16, t3: u16, reset: u16) -> Result<Self, String> {
        if maxval < 1
            || t1 < 1
            || t1 > t2
            || t2 > t3
            || t3 > maxval
            || reset < 3
            || reset > maxval.max(255)
        {
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
        // T.87 C.3 falls back to the lower bound when x exceeds MAXVAL.
        let clamp = |x: i32, lo: i32| if x < lo || x > m { lo } else { x };
        let (t1, t2, t3) = if m >= 128 {
            let f = (m.min(4095) + 128) / 256;
            let t1 = clamp(f + 2 + 3 * n, n + 1);
            let t2 = clamp(f * 4 + 3 + 5 * n, t1);
            (t1, t2, clamp(f * 17 + 4 + 7 * n, t2))
        } else {
            let f = 256 / (m + 1);
            let t1 = clamp(2.max(3 / f + 3 * n), n + 1);
            let t2 = clamp(3.max(7 / f + 5 * n), t1);
            (t1, t2, clamp(4.max(21 / f + 7 * n), t2))
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_thresholds_follow_t87_at_low_precision_and_large_near() {
        for (maxval, near, thresholds) in [
            (3, 0, [2, 3, 3]),
            (15, 0, [2, 3, 4]),
            (127, 1, [4, 8, 17]),
            (255, 0, [3, 7, 21]),
            (255, 127, [128, 128, 128]),
            (65535, 255, [783, 1342, 2061]),
        ] {
            let p = Preset::defaults(maxval, near).unwrap();
            assert_eq!(
                [p.t1, p.t2, p.t3],
                thresholds,
                "MAXVAL={maxval} NEAR={near}"
            );
        }
    }

    #[test]
    fn rejects_reset_larger_than_the_standard_limit() {
        assert!(Preset::new(255, 3, 7, 21, 256).is_err());
    }
}
