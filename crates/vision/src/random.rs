//! 原版用到的确定性随机数与噪声：`java.util.Random` 同款线性同余（原版
//! `LegacyRandomSource` / `SingleThreadedRandomSource`）与 `SimplexNoise` 的二维取值。

/// `java.util.Random` 同款线性同余。
pub(crate) struct JavaRandom(u64);

impl JavaRandom {
    const MULTIPLIER: u64 = 0x5_DEEC_E66D;
    const MASK: u64 = (1 << 48) - 1;

    pub(crate) fn new(seed: u64) -> Self {
        Self((seed ^ Self::MULTIPLIER) & Self::MASK)
    }

    pub(crate) fn next(&mut self, bits: u32) -> u64 {
        self.0 = (self.0.wrapping_mul(Self::MULTIPLIER).wrapping_add(0xB)) & Self::MASK;
        self.0 >> (48 - bits)
    }

    pub(crate) fn next_float(&mut self) -> f64 {
        self.next(24) as f64 / f64::from(1u32 << 24)
    }

    pub(crate) fn next_double(&mut self) -> f64 {
        ((self.next(26) << 27) + self.next(27)) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// `BitRandomSource.nextInt(bound)`。
    pub(crate) fn next_int(&mut self, bound: u32) -> u32 {
        if bound.is_power_of_two() {
            return ((u64::from(bound) * self.next(31)) >> 31) as u32;
        }
        loop {
            let sample = self.next(31) as i32;
            let modulo = sample % bound as i32;
            if sample.wrapping_sub(modulo).wrapping_add(bound as i32 - 1) >= 0 {
                return modulo as u32;
            }
        }
    }
}

/// 原版 `SimplexNoise`：构造时按随机源取偏移与置换表，这里只用二维取值。
pub(crate) struct SimplexNoise {
    permutation: [u8; 256],
}

const GRADIENT: [[f64; 2]; 12] = [
    [1.0, 1.0],
    [-1.0, 1.0],
    [1.0, -1.0],
    [-1.0, -1.0],
    [1.0, 0.0],
    [-1.0, 0.0],
    [1.0, 0.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [0.0, -1.0],
    [0.0, 1.0],
    [0.0, -1.0],
];

impl SimplexNoise {
    pub(crate) fn new(random: &mut JavaRandom) -> Self {
        // xo、yo、zo 三个偏移只在 useNoiseStart 时用；照样消耗随机数。
        for _ in 0..3 {
            random.next_double();
        }
        let mut permutation: [u8; 256] = std::array::from_fn(|i| i as u8);
        for i in 0..256 {
            let offset = random.next_int(256 - i as u32) as usize;
            permutation.swap(i, offset + i);
        }
        Self { permutation }
    }

    fn p(&self, x: i32) -> i32 {
        i32::from(self.permutation[(x & 0xFF) as usize])
    }

    /// `SimplexNoise.getValue(x, y)`。
    pub(crate) fn value(&self, x: f64, y: f64) -> f64 {
        let sqrt3 = 3.0_f64.sqrt();
        let f2 = 0.5 * (sqrt3 - 1.0);
        let g2 = (3.0 - sqrt3) / 6.0;
        let s = (x + y) * f2;
        let i = (x + s).floor() as i32;
        let j = (y + s).floor() as i32;
        let t = f64::from(i + j) * g2;
        let x0 = x - (f64::from(i) - t);
        let y0 = y - (f64::from(j) - t);
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - f64::from(i1) + g2;
        let y1 = y0 - f64::from(j1) + g2;
        let x2 = x0 - 1.0 + 2.0 * g2;
        let y2 = y0 - 1.0 + 2.0 * g2;
        let (ii, jj) = (i & 0xFF, j & 0xFF);
        let gi0 = self.p(ii + self.p(jj)) % 12;
        let gi1 = self.p(ii + i1 + self.p(jj + j1)) % 12;
        let gi2 = self.p(ii + 1 + self.p(jj + 1)) % 12;
        let corner = |g: i32, x: f64, y: f64| {
            let t = 0.5 - x * x - y * y;
            if t < 0.0 {
                0.0
            } else {
                let gradient = GRADIENT[g as usize];
                t.powi(4) * (gradient[0] * x + gradient[1] * y)
            }
        };
        70.0 * (corner(gi0, x0, y0) + corner(gi1, x1, y1) + corner(gi2, x2, y2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_random_matches_the_jdk_sequence() {
        // new java.util.Random(42).nextInt() == -1170105035
        let mut random = JavaRandom::new(42);
        assert_eq!(random.next(32) as u32 as i32, -1_170_105_035);
        // new java.util.Random(42) 接着 nextInt(10) 依次是 0, 3, 8
        let mut random = JavaRandom::new(42);
        assert_eq!([0; 3].map(|_| random.next_int(10)), [0, 3, 8]);
    }

    #[test]
    fn simplex_noise_stays_in_range() {
        let noise = SimplexNoise::new(&mut JavaRandom::new(2345));
        for i in 0..200 {
            let v = noise.value(f64::from(i) * 0.37, f64::from(i) * -0.21);
            assert!((-1.0..=1.0).contains(&v), "{v}");
        }
    }
}
