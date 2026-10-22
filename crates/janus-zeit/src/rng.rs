//! numpy's random generators, for the same draws as Zeit's Python (numpy 2.x, numpy/random): the legacy
//! `RandomState` (MT19937 seeded by an int: `rand`, `randint`) that MiniSom's initialisation uses, and
//! `default_rng(seed)` (PCG64 seeded through a `SeedSequence`) with its `choice(..., replace=False)`.

/// numpy's MT19937 (mt19937.c), seeded as `RandomState(seed)` seeds it (`mt19937_seed`).
pub struct Mt19937 {
    key: [u32; 624],
    pos: usize,
}

impl Mt19937 {
    pub fn new(seed: u32) -> Mt19937 {
        let mut key = [0u32; 624];
        let mut s = seed;
        for (pos, k) in key.iter_mut().enumerate() {
            *k = s;
            s = 1812433253u32.wrapping_mul(s ^ (s >> 30)).wrapping_add(pos as u32 + 1);
        }
        Mt19937 { key, pos: 624 }
    }

    fn generate(&mut self) {
        const N: usize = 624;
        const M: usize = 397;
        let twist = |k: &[u32; N], a: usize, b: usize, c: usize| {
            let y = (k[a] & 0x8000_0000) | (k[b] & 0x7fff_ffff);
            k[c] ^ (y >> 1) ^ (0u32.wrapping_sub(y & 1) & 0x9908_b0df)
        };
        for i in 0..N - M {
            self.key[i] = twist(&self.key, i, i + 1, i + M);
        }
        for i in N - M..N - 1 {
            self.key[i] = twist(&self.key, i, i + 1, i + M - N);
        }
        self.key[N - 1] = twist(&self.key, N - 1, 0, M - 1);
        self.pos = 0;
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.pos == 624 {
            self.generate();
        }
        let mut y = self.key[self.pos];
        self.pos += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `rand()`: a double in [0, 1) of 53 bits.
    pub fn next_double(&mut self) -> f64 {
        let a = (self.next_u32() >> 5) as f64;
        let b = (self.next_u32() >> 6) as f64;
        (a * 67108864.0 + b) / 9007199254740992.0
    }

    /// `randint(high)`: an int in [0, high), by masked rejection (RandomState's).
    pub fn randint(&mut self, high: u64) -> u64 {
        let rng = high.saturating_sub(1);
        if rng == 0 {
            return 0;
        }
        if rng == 0xFFFF_FFFF {
            return u64::from(self.next_u32());
        }
        assert!(rng < 0xFFFF_FFFF, "randint beyond 32 bits");
        let mask = gen_mask(rng) as u32;
        loop {
            let v = self.next_u32() & mask;
            if u64::from(v) <= rng {
                return u64::from(v);
            }
        }
    }
}

/// The smallest mask of low bits covering `max`.
fn gen_mask(max: u64) -> u64 {
    let mut m = max;
    m |= m >> 1;
    m |= m >> 2;
    m |= m >> 4;
    m |= m >> 8;
    m |= m >> 16;
    m |= m >> 32;
    m
}

/// numpy's SeedSequence of an int: its pool (4 words), then `generate_state`.
fn seed_sequence(seed: u64, n_words32: usize) -> Vec<u32> {
    const INIT_A: u32 = 0x43b0_d7e5;
    const MULT_A: u32 = 0x931e_8875;
    const INIT_B: u32 = 0x8b51_f9dd;
    const MULT_B: u32 = 0x58f3_8ded;
    const MIX_L: u32 = 0xca01_f9dd;
    const MIX_R: u32 = 0x4973_f715;
    // the int as 32-bit words, lowest first
    let mut entropy = vec![];
    let mut n = seed;
    if n == 0 {
        entropy.push(0);
    }
    while n > 0 {
        entropy.push((n & 0xFFFF_FFFF) as u32);
        n >>= 32;
    }
    let mut hash_const = INIT_A;
    let mut hashmix = |value: u32| {
        let mut v = value ^ hash_const;
        hash_const = hash_const.wrapping_mul(MULT_A);
        v = v.wrapping_mul(hash_const);
        v ^ (v >> 16)
    };
    let mix = |x: u32, y: u32| {
        let r = MIX_L.wrapping_mul(x).wrapping_sub(MIX_R.wrapping_mul(y));
        r ^ (r >> 16)
    };
    let mut pool = [0u32; 4];
    for (i, p) in pool.iter_mut().enumerate() {
        *p = hashmix(entropy.get(i).copied().unwrap_or(0));
    }
    for src in 0..4 {
        for dst in 0..4 {
            if src != dst {
                let h = hashmix(pool[src]);
                pool[dst] = mix(pool[dst], h);
            }
        }
    }
    for &e in entropy.iter().skip(4) {
        for dst in 0..4 {
            let h = hashmix(e);
            pool[dst] = mix(pool[dst], h);
        }
    }
    let mut hc = INIT_B;
    (0..n_words32)
        .map(|i| {
            let mut v = pool[i % 4] ^ hc;
            hc = hc.wrapping_mul(MULT_B);
            v = v.wrapping_mul(hc);
            v ^ (v >> 16)
        })
        .collect()
}

/// numpy's PCG64 (XSL-RR 128/64), seeded as `default_rng(seed)` seeds it.
pub struct Pcg64 {
    state: u128,
    inc: u128,
    /// the other half of a 64-bit draw, for the next 32-bit one
    spare: Option<u32>,
}

const PCG_MULT: u128 = (2549297995355413924u128 << 64) | 4865540595714422341u128;

impl Pcg64 {
    pub fn new(seed: u64) -> Pcg64 {
        let w = seed_sequence(seed, 8);
        let word = |k: usize| u64::from(w[2 * k]) | (u64::from(w[2 * k + 1]) << 32);
        let initstate = (u128::from(word(0)) << 64) | u128::from(word(1));
        let initseq = (u128::from(word(2)) << 64) | u128::from(word(3));
        let mut r = Pcg64 { state: 0, inc: (initseq << 1) | 1, spare: None };
        r.step();
        r.state = r.state.wrapping_add(initstate);
        r.step();
        r
    }

    fn step(&mut self) {
        self.state = self.state.wrapping_mul(PCG_MULT).wrapping_add(self.inc);
    }

    pub fn next_u64(&mut self) -> u64 {
        self.step();
        let hi = (self.state >> 64) as u64;
        let lo = self.state as u64;
        (hi ^ lo).rotate_right((hi >> 58) as u32)
    }

    pub fn next_u32(&mut self) -> u32 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let n = self.next_u64();
        self.spare = Some((n >> 32) as u32);
        n as u32
    }

    /// An int in [0, rng] (Generator's `random_bounded_uint64`, Lemire's rejection).
    pub fn bounded(&mut self, rng: u64) -> u64 {
        if rng == 0 {
            return 0;
        }
        if rng <= 0xFFFF_FFFF {
            if rng == 0xFFFF_FFFF {
                return u64::from(self.next_u32());
            }
            let excl = (rng + 1) as u32;
            let mut m = u64::from(self.next_u32()) * u64::from(excl);
            let mut left = m as u32;
            if left < excl {
                let threshold = (u32::MAX - rng as u32) % excl;
                while left < threshold {
                    m = u64::from(self.next_u32()) * u64::from(excl);
                    left = m as u32;
                }
            }
            return m >> 32;
        }
        if rng == u64::MAX {
            return self.next_u64();
        }
        let excl = rng + 1;
        let mut m = u128::from(self.next_u64()) * u128::from(excl);
        let mut left = m as u64;
        if left < excl {
            let threshold = (u64::MAX - rng) % excl;
            while left < threshold {
                m = u128::from(self.next_u64()) * u128::from(excl);
                left = m as u64;
            }
        }
        (m >> 64) as u64
    }

    /// Fisher-Yates over data[first..n] from the end (numpy's `_shuffle_int`).
    fn shuffle(&mut self, data: &mut [i64], first: usize) {
        for i in (first..data.len()).rev() {
            let j = self.bounded(i as u64) as usize;
            data.swap(i, j);
        }
    }

    /// `choice(pop_size, size, replace=False)` (shuffle=True): the indices drawn, in numpy's order.
    pub fn choice(&mut self, pop_size: usize, size: usize) -> Vec<i64> {
        assert!(size <= pop_size);
        if pop_size > 10000 && size > pop_size / 50 {
            let mut idx: Vec<i64> = (0..pop_size as i64).collect();
            self.shuffle(&mut idx, (pop_size - size).max(1));
            return idx[pop_size - size..].to_vec();
        }
        // Floyd's algorithm with a hash set
        let mask = gen_mask((1.2 * size as f64) as u64);
        let mut set = vec![u64::MAX; (mask + 1) as usize];
        let mut out = vec![0i64; size];
        for j in pop_size - size..pop_size {
            let val = self.bounded(j as u64);
            let mut loc = (val & mask) as usize;
            while set[loc] != u64::MAX && set[loc] != val {
                loc = (loc + 1) & mask as usize;
            }
            if set[loc] == u64::MAX {
                set[loc] = val;
                out[j - pop_size + size] = val as i64;
            } else {
                let mut loc = (j as u64 & mask) as usize;
                while set[loc] != u64::MAX {
                    loc = (loc + 1) & mask as usize;
                }
                set[loc] = j as u64;
                out[j - pop_size + size] = j as i64;
            }
        }
        self.shuffle(&mut out, 1);
        out
    }
}
