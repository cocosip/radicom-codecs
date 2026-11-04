#[derive(Clone, Copy, Debug)]
pub(crate) struct RegularContext {
    a: i32,
    b: i32,
    c: i32,
    n: i32,
}
impl RegularContext {
    pub(crate) fn new(range: i32) -> Self {
        Self {
            a: ((range + 32) / 64).max(2),
            b: 0,
            c: 0,
            n: 1,
        }
    }
    pub(crate) fn k(&self) -> u8 {
        // CharLS caps the Golomb parameter at 16 (constants.h). Evaluate the
        // comparison in i64 so a crafted LSE/RESET stream cannot overflow
        // `n << k` before the cap is reached.
        let mut k = 0;
        while k < 16 && (i64::from(self.n) << k) < i64::from(self.a) {
            k += 1;
        }
        k
    }
    pub(crate) fn correction(&self, k: u8) -> i32 {
        if k == 0 && (2 * self.b + self.n - 1) < 0 {
            -1
        } else {
            0
        }
    }
    pub(crate) fn update(&mut self, e: i32, near: i32, reset: i32) {
        // Saturating accumulation keeps a crafted stream from overflowing the
        // i32 accumulators; valid streams stay far below these magnitudes.
        self.a = self.a.saturating_add(e.abs());
        self.b = self.b.saturating_add(e.saturating_mul(2 * near + 1));
        if self.n == reset {
            self.a >>= 1;
            self.b >>= 1;
            self.n >>= 1;
        }
        self.n = self.n.saturating_add(1);
        if self.b.saturating_add(self.n) <= 0 {
            self.b += self.n;
            if self.b <= -self.n {
                self.b = -self.n + 1
            }
            if self.c > -128 {
                self.c -= 1
            }
        } else if self.b > 0 {
            self.b -= self.n;
            if self.b > 0 {
                self.b = 0
            }
            if self.c < 127 {
                self.c += 1
            }
        }
    }
    pub(crate) fn bias(&self) -> i32 {
        self.c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crafted LSE/RESET stream can inflate the adaptive statistics; the
    /// Golomb parameter derivation must saturate/clamp instead of overflowing.
    #[test]
    fn extreme_statistics_do_not_overflow() {
        let mut regular = RegularContext::new(65535);
        regular.a = i32::MAX - 1;
        regular.b = i32::MAX;
        regular.n = 1_500_000_000;
        assert!(regular.k() < 16);
        regular.update(30000, 0, i32::MAX);
        assert!(regular.k() < 16);

        let mut run = RunContext::new(1, 65535);
        run.a = i32::MAX - 1;
        run.n = i32::MAX / 4;
        run.nn = i32::MAX / 4;
        assert!(run.k() < 16);
        run.update(30000, i32::MAX - 1, i32::MAX);
        assert!(run.k() < 16);
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunContext {
    typ: i32,
    a: i32,
    n: i32,
    nn: i32,
}
impl RunContext {
    pub(crate) fn new(typ: i32, range: i32) -> Self {
        Self {
            typ,
            a: ((range + 32) / 64).max(2),
            n: 1,
            nn: 0,
        }
    }
    pub(crate) fn k(&self) -> u8 {
        // i64 with a CharLS-compatible k cap of 16: a crafted run context must
        // not drive `nt <<= 1` into an i32 overflow or an unbounded loop.
        let temp = i64::from(self.a) + i64::from(self.n >> 1) * i64::from(self.typ);
        let mut nt = i64::from(self.n);
        let mut k = 0;
        while k < 16 && nt < temp {
            nt <<= 1;
            k += 1;
        }
        k
    }
    fn map_error(&self, e: i32, k: u8) -> bool {
        (k == 0 && e > 0 && 2 * self.nn < self.n)
            || (e < 0 && 2 * self.nn >= self.n)
            || (e < 0 && k != 0)
    }

    pub(crate) fn encode_value(&mut self, e: i32, k: u8, reset: i32) -> u32 {
        let m = 2 * e.abs() - self.typ - i32::from(self.map_error(e, k));
        self.update(e, m, reset);
        m as u32
    }
    pub(crate) fn decode_value(&mut self, temp: i32, k: u8, reset: i32) -> i32 {
        let map = (temp & 1) != 0;
        let abs = (temp + i32::from(map)) / 2;
        let neg = (k != 0 || (2 * self.nn >= self.n)) == map;
        let e = if neg { -abs } else { abs };
        // The Golomb value is offset by the interruption type for sign
        // reconstruction, while context statistics use the unoffset value.
        self.update(e, temp - self.typ, reset);
        e
    }
    fn update(&mut self, e: i32, m: i32, reset: i32) {
        if e < 0 {
            self.nn += 1
        }
        self.a = self
            .a
            .saturating_add(m.saturating_add(1).saturating_sub(self.typ) >> 1);
        if self.n == reset {
            self.a >>= 1;
            self.n >>= 1;
            self.nn >>= 1
        }
        self.n = self.n.saturating_add(1)
    }
}
