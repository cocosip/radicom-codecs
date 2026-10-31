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
        let mut k = 0;
        while k < 31 && (self.n << k) < self.a {
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
        self.a += e.abs();
        self.b += e * (2 * near + 1);
        if self.n == reset {
            self.a >>= 1;
            self.b >>= 1;
            self.n >>= 1;
        }
        self.n += 1;
        if self.b + self.n <= 0 {
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
        let temp = self.a + (self.n >> 1) * self.typ;
        let mut nt = self.n;
        let mut k = 0;
        while nt < temp {
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
        self.a += (m + 1 - self.typ) >> 1;
        if self.n == reset {
            self.a >>= 1;
            self.n >>= 1;
            self.nn >>= 1
        }
        self.n += 1
    }
}
