#[derive(Clone, Copy, Debug)]
pub(crate) struct RegularContext {
    a: i32,
    b: i32,
    n: i32,
    c: i8,
}
impl RegularContext {
    pub(crate) fn new(range: i32) -> Self {
        let a = ((range + 32) / 64).max(2);
        Self {
            a,
            b: 0,
            n: 1,
            c: 0,
        }
    }
    #[inline]
    pub(crate) fn k(&self) -> u8 {
        let (a, n) = (self.a as u32, self.n as u32);
        if a <= n * 2 {
            return u8::from(a > n);
        }
        // Setting the low bit keeps the leading-zero count and gives the
        // compiler nonzero inputs for finding the highest set bit.
        let shift = (n | 1).leading_zeros() - (a | 1).leading_zeros();
        (shift + u32::from((n << shift) < a)).min(16) as u8
    }
    #[inline]
    pub(crate) fn correction(&self, k: u8) -> i32 {
        if k == 0 && (2 * self.b + self.n - 1) < 0 {
            -1
        } else {
            0
        }
    }
    #[inline]
    pub(crate) fn update(&mut self, e: i32, near: i32, reset: i32) {
        // A starts below 32768. With bounded errors and halvings at RESET,
        // A stays below N * 32768 before this addition, even for RESET=65535.
        self.a += e.abs();
        // B starts in [1 - N, 0] and NEAR fits in u8.
        self.b += e * (2 * near + 1);
        if self.n == reset {
            self.a >>= 1;
            self.b >>= 1;
            self.n >>= 1;
        }
        self.n += 1;
        if self.b > 0 {
            self.b = (self.b - self.n).min(0);
            self.c = self.c.saturating_add(1);
        } else if self.b <= -self.n {
            self.b = (self.b + self.n).max(1 - self.n);
            self.c = self.c.saturating_sub(1);
        }
    }
    #[inline(always)]
    pub(crate) fn accepts_zero_rice(&self) -> bool {
        self.a <= self.n
    }

    #[inline(always)]
    pub(crate) fn update_zero_error(&mut self, reset: i32) {
        // B is already in [1-N, 0], and increasing N preserves the bias.
        if self.n == reset {
            self.a >>= 1;
            self.b >>= 1;
            self.n >>= 1;
        }
        self.n += 1;
    }

    #[inline(always)]
    pub(crate) fn update_nonpositive_unit_error(&mut self, error: i32) {
        debug_assert!((-1..=0).contains(&error));
        // After the count advances, B remains in the new bias interval.
        self.a -= error;
        self.b += error;
        self.update_zero_error(64);
    }

    #[inline(always)]
    pub(crate) fn accepts_unit_rice_triplet(&self) -> bool {
        self.a <= self.n && self.n <= 61
    }

    #[inline(always)]
    pub(crate) fn update_unit_rice_triplet(&mut self, mapped: u8) -> [i32; 3] {
        debug_assert!(self.accepts_unit_rice_triplet());
        let correction = 2 * self.b + self.n - 1;
        let first = -i32::from(mapped & 1) ^ (correction >> 31);
        let correction = correction + 1 + 2 * first;
        let second = -i32::from((mapped >> 1) & 1) ^ (correction >> 31);
        let correction = correction + 1 + 2 * second;
        let third = -i32::from(mapped >> 2) ^ (correction >> 31);
        let sum = first + second + third;
        self.a -= sum;
        self.b += sum;
        self.n += 3;
        [first, second, third]
    }

    #[inline]
    pub(crate) fn bias(&self) -> i32 {
        i32::from(self.c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_error_update_matches_the_adaptive_model_at_reset() {
        for n in 1..=64 {
            for b in 1 - n..=0 {
                let mut regular = RegularContext { a: n, b, n, c: 7 };
                let mut zero = regular;
                regular.update(0, 1, 64);
                zero.update_zero_error(64);
                assert_eq!(
                    (zero.a, zero.b, zero.n, zero.c),
                    (regular.a, regular.b, regular.n, regular.c)
                );
            }
        }
    }

    #[test]
    fn nonpositive_unit_update_matches_generic_lossless_model() {
        for n in 1..=64 {
            for b in 1 - n..=0 {
                for error in -1..=0 {
                    let mut generic = RegularContext { a: n, b, n, c: -7 };
                    let mut fast = generic;
                    generic.update(error, 0, 64);
                    fast.update_nonpositive_unit_error(error);
                    assert_eq!(
                        (fast.a, fast.b, fast.n, fast.c),
                        (generic.a, generic.b, generic.n, generic.c)
                    );
                }
            }
        }
    }

    #[test]
    fn unit_rice_triplets_match_sequential_lossless_updates() {
        for n in 1..=61 {
            for b in 1 - n..=0 {
                for mapped in 0..8 {
                    let mut generic = RegularContext { a: n, b, n, c: 7 };
                    let mut fast = generic;
                    let decoded = fast.update_unit_rice_triplet(mapped);
                    for (channel, sample) in decoded.into_iter().enumerate() {
                        assert_eq!(generic.k(), 0);
                        let error = -i32::from((mapped >> channel) & 1) ^ generic.correction(0);
                        assert_eq!(sample, error);
                        generic.update(error, 0, 64);
                    }
                    assert_eq!(
                        (fast.a, fast.b, fast.n, fast.c),
                        (generic.a, generic.b, generic.n, generic.c),
                    );
                }
            }
        }
    }

    /// A crafted LSE/RESET stream can inflate the adaptive statistics; the
    /// Golomb parameter derivation must saturate/clamp instead of overflowing.
    #[test]
    fn extreme_statistics_do_not_overflow() {
        let mut regular = RegularContext::new(65535);
        regular.n = i32::from(u16::MAX);
        regular.a = regular.n * 32768 - 1;
        regular.b = 1 - regular.n;
        regular.update(-32768, 0, i32::from(u16::MAX));
        assert_eq!(regular.a, i32::MAX >> 1);
        assert_eq!(regular.k(), golomb_k(i64::from(regular.a), regular.n));

        let mut run = RunContext::new(1, 65535);
        run.a = i32::MAX - 1;
        run.n = i32::MAX / 4;
        run.nn = i32::MAX / 4;
        assert!(run.k() < 16);
        run.update(30000, i32::MAX - 1, i32::MAX);
        assert!(run.k() < 16);
    }

    #[test]
    fn regular_parameter_matches_the_adaptive_statistics() {
        for reset in [3, 64, i32::from(u16::MAX)] {
            let mut context = RegularContext::new(256);
            for error in (0..512).map(|i| i % 256 - 128).chain([0; 512]) {
                assert_eq!(context.k(), golomb_k(i64::from(context.a), context.n));
                context.update(error, 0, reset);
            }
        }
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
        let a = i64::from(self.a) + i64::from(self.n >> 1) * i64::from(self.typ);
        golomb_k(a, self.n)
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

/// Compare at the difference in bit widths, then resolve the one-bit ambiguity.
#[inline]
fn golomb_k(a: i64, n: i32) -> u8 {
    let n = i64::from(n);
    let shift = n.leading_zeros().saturating_sub(a.leading_zeros()).min(16);
    (shift + u32::from((n << shift) < a)).min(16) as u8
}
