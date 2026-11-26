//! Bit-exact ports of Minecraft's random sources (levelgen/RandomSupport, Xoroshiro128PlusPlus,
//! XoroshiroRandomSource, LegacyRandomSource/WorldgenRandom, *PositionalRandomFactory).

use md5::{Digest, Md5};
use sha2::Sha256;

pub const GOLDEN_RATIO_64: u64 = 0x9E37_79B9_7F4A_7C15;
pub const SILVER_RATIO_64: u64 = 0x6A09_E667_F3BC_C909;
const MASK48: i64 = (1 << 48) - 1;
const MULT: i64 = 0x5_DEEC_E66D;

pub fn mix_stafford13(z: u64) -> u64 {
    let z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// RandomSupport.upgradeSeedTo128bit
pub fn upgrade_seed(seed: i64) -> (u64, u64) {
    let lo = seed as u64 ^ SILVER_RATIO_64;
    let hi = lo.wrapping_add(GOLDEN_RATIO_64);
    (mix_stafford13(lo), mix_stafford13(hi))
}

/// RandomSupport.seedFromHashOf: MD5 of the UTF-8 name, big-endian halves.
pub fn seed_from_hash(name: &str) -> (u64, u64) {
    let h = Md5::digest(name.as_bytes());
    (u64::from_be_bytes(h[0..8].try_into().unwrap()), u64::from_be_bytes(h[8..16].try_into().unwrap()))
}

/// Mth.getSeed(x, y, z). Note `x * 3129871` is an int multiply in Java.
pub fn pos_seed(x: i32, y: i32, z: i32) -> i64 {
    let mut s = (x.wrapping_mul(3129871) as i64) ^ (z as i64).wrapping_mul(116129781) ^ y as i64;
    s = s.wrapping_mul(s).wrapping_mul(42317861).wrapping_add(s.wrapping_mul(11));
    s >> 16
}

/// BiomeManager.obfuscateSeed: Guava's sha256().hashLong (little-endian bytes in) and asLong
/// (first eight bytes, little-endian).
pub fn obfuscate_seed(seed: i64) -> i64 {
    let h = Sha256::digest(seed.to_le_bytes());
    i64::from_le_bytes(h[0..8].try_into().unwrap())
}

/// LinearCongruentialGenerator.next
pub fn lcg_next(rval: i64, c: i64) -> i64 {
    rval.wrapping_mul(rval.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)).wrapping_add(c)
}

/// java.lang.String.hashCode for ASCII/BMP identifiers.
pub fn java_hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

/// Shared API of RandomSource, so generic code works with either algorithm.
pub trait Rng {
    fn next_int(&mut self) -> i32;
    fn next_bound(&mut self, bound: i32) -> i32;
    fn next_long(&mut self) -> i64;
    fn next_float(&mut self) -> f32;
    fn next_double(&mut self) -> f64;
    fn next_bool(&mut self) -> bool;
    fn consume(&mut self, n: usize) {
        for _ in 0..n {
            self.next_int();
        }
    }
    /// RandomSource.nextIntBetweenInclusive / Mth.randomBetweenInclusive
    fn between_inclusive(&mut self, min: i32, max: i32) -> i32 {
        self.next_bound(max - min + 1) + min
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Xoroshiro {
    lo: u64,
    hi: u64,
}

impl Xoroshiro {
    pub fn new(seed: i64) -> Self {
        let (lo, hi) = upgrade_seed(seed);
        Self::from_parts(lo, hi)
    }
    pub fn from_parts(lo: u64, hi: u64) -> Self {
        if lo | hi == 0 {
            Self { lo: GOLDEN_RATIO_64, hi: SILVER_RATIO_64 }
        } else {
            Self { lo, hi }
        }
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let (s0, mut s1) = (self.lo, self.hi);
        let r = s0.wrapping_add(s1).rotate_left(17).wrapping_add(s0);
        s1 ^= s0;
        self.lo = s0.rotate_left(49) ^ s1 ^ (s1 << 21);
        self.hi = s1.rotate_left(28);
        r
    }
    fn next_bits(&mut self, bits: u32) -> u64 {
        self.next_u64() >> (64 - bits)
    }
    pub fn fork(&mut self) -> Xoroshiro {
        let lo = self.next_u64();
        Xoroshiro::from_parts(lo, self.next_u64())
    }
    pub fn fork_positional(&mut self) -> Positional {
        let lo = self.next_u64();
        Positional::Xoroshiro { lo, hi: self.next_u64() }
    }
}

impl Rng for Xoroshiro {
    fn next_int(&mut self) -> i32 {
        self.next_u64() as i32
    }
    fn next_bound(&mut self, bound: i32) -> i32 {
        // Lemire's unbiased multiply-shift (XoroshiroRandomSource.nextInt(int))
        let bound64 = bound as u64;
        let mut m = (self.next_int() as u32 as u64) * bound64;
        let mut frac = m & 0xFFFF_FFFF;
        if frac < bound64 {
            let threshold = ((!bound).wrapping_add(1) as u32 % bound as u32) as u64;
            while frac < threshold {
                m = (self.next_int() as u32 as u64) * bound64;
                frac = m & 0xFFFF_FFFF;
            }
        }
        (m >> 32) as i32
    }
    fn next_long(&mut self) -> i64 {
        self.next_u64() as i64
    }
    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * 5.960_464_5e-8
    }
    fn next_double(&mut self) -> f64 {
        self.next_bits(53) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 != 0
    }
    fn consume(&mut self, n: usize) {
        for _ in 0..n {
            self.next_u64();
        }
    }
}

/// java.util.Random / LegacyRandomSource / WorldgenRandom(LegacyRandomSource).
#[derive(Clone, Copy, Debug)]
pub struct JavaRandom {
    seed: i64,
}

impl JavaRandom {
    pub fn new(seed: i64) -> Self {
        Self { seed: (seed ^ MULT) & MASK48 }
    }
    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ MULT) & MASK48;
    }
    #[inline]
    pub fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULT).wrapping_add(0xB) & MASK48;
        (self.seed >> (48 - bits)) as i32
    }
    pub fn fork(&mut self) -> JavaRandom {
        JavaRandom::new(self.next_long())
    }
    pub fn fork_positional(&mut self) -> Positional {
        Positional::Legacy { seed: self.next_long() }
    }

    // WorldgenRandom seeding helpers
    pub fn set_large_feature_seed(&mut self, world_seed: i64, cx: i32, cz: i32) {
        self.set_seed(world_seed);
        let xs = self.next_long();
        let zs = self.next_long();
        self.set_seed((cx as i64).wrapping_mul(xs) ^ (cz as i64).wrapping_mul(zs) ^ world_seed);
    }
    pub fn set_large_feature_with_salt(&mut self, world_seed: i64, x: i32, z: i32, salt: i32) {
        let s = (x as i64).wrapping_mul(341873128712)
            .wrapping_add((z as i64).wrapping_mul(132897987541))
            .wrapping_add(world_seed)
            .wrapping_add(salt as i64);
        self.set_seed(s);
    }
    pub fn set_decoration_seed(&mut self, world_seed: i64, cx: i32, cz: i32) -> i64 {
        self.set_seed(world_seed);
        let xs = self.next_long() | 1;
        let zs = self.next_long() | 1;
        let r = (cx as i64).wrapping_mul(xs).wrapping_add((cz as i64).wrapping_mul(zs)) ^ world_seed;
        self.set_seed(r);
        r
    }
    pub fn set_feature_seed(&mut self, decoration_seed: i64, index: i32, step: i32) {
        self.set_seed(decoration_seed.wrapping_add(index as i64).wrapping_add(10000 * step as i64));
    }
}

impl Rng for JavaRandom {
    fn next_int(&mut self) -> i32 {
        self.next(32)
    }
    fn next_bound(&mut self, bound: i32) -> i32 {
        if bound & (bound - 1) == 0 {
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let sample = self.next(31);
            let modulo = sample % bound;
            if sample.wrapping_sub(modulo).wrapping_add(bound - 1) >= 0 {
                return modulo;
            }
        }
    }
    fn next_long(&mut self) -> i64 {
        let hi = self.next(32) as i64;
        let lo = self.next(32) as i64;
        (hi << 32).wrapping_add(lo)
    }
    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * 5.960_464_5e-8
    }
    fn next_double(&mut self) -> f64 {
        let hi = self.next(26) as i64;
        let lo = self.next(27) as i64;
        ((hi << 27) + lo) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }
}

/// WorldgenRandom(XoroshiroRandomSource): the 26.x decoration random (features, structure
/// postProcess). Java-random methods (BitRandomSource) over `next(bits)` = top bits of nextLong.
#[derive(Clone, Copy, Debug)]
pub struct WorldgenXoro(Xoroshiro, Option<f64>);

impl WorldgenXoro {
    pub fn new(seed: i64) -> Self {
        WorldgenXoro(Xoroshiro::new(seed), None)
    }
    /// WorldgenRandom.setSeed reseeds the wrapped source only: a cached gaussian survives it.
    pub fn set_seed(&mut self, seed: i64) {
        self.0 = Xoroshiro::new(seed);
    }
    /// nextGaussian: the outer LegacyRandomSource's MarsagliaPolarGaussian over this random.
    pub fn next_gaussian(&mut self) -> f64 {
        if let Some(g) = self.1.take() {
            return g;
        }
        loop {
            let x = 2.0 * self.next_double() - 1.0;
            let y = 2.0 * self.next_double() - 1.0;
            let r2 = x * x + y * y;
            if r2 >= 1.0 || r2 == 0.0 {
                continue;
            }
            let m = (-2.0 * r2.ln() / r2).sqrt();
            self.1 = Some(y * m);
            return x * m;
        }
    }
    fn next(&mut self, bits: u32) -> i32 {
        self.0.next_bits(bits) as i32
    }
    /// setDecorationSeed(world seed, chunk min block x, chunk min block z)
    pub fn set_decoration_seed(&mut self, world_seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(world_seed);
        let xs = self.next_long() | 1;
        let zs = self.next_long() | 1;
        let r = (x as i64).wrapping_mul(xs).wrapping_add((z as i64).wrapping_mul(zs)) ^ world_seed;
        self.set_seed(r);
        r
    }
    pub fn set_feature_seed(&mut self, decoration_seed: i64, index: i32, step: i32) {
        self.set_seed(decoration_seed.wrapping_add(index as i64).wrapping_add(10000 * step as i64));
    }
}

impl Rng for WorldgenXoro {
    fn next_int(&mut self) -> i32 {
        self.next(32)
    }
    fn next_bound(&mut self, bound: i32) -> i32 {
        if bound & (bound - 1) == 0 {
            return ((bound as i64 * self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let sample = self.next(31);
            let modulo = sample % bound;
            if sample.wrapping_sub(modulo).wrapping_add(bound - 1) >= 0 {
                return modulo;
            }
        }
    }
    fn next_long(&mut self) -> i64 {
        let hi = self.next(32) as i64;
        let lo = self.next(32) as i64;
        (hi << 32).wrapping_add(lo)
    }
    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * 5.960_464_5e-8
    }
    fn next_double(&mut self) -> f64 {
        let hi = self.next(26) as i64;
        let lo = self.next(27) as i64;
        // BitRandomSource multiplies by the float literal 1.110223E-16F
        ((hi << 27) + lo) as f64 * 1.110_223e-16_f32 as f64
    }
    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }
}

/// Either kind of random source, for code paths that support both algorithms.
#[derive(Clone, Copy, Debug)]
pub enum AnyRng {
    X(Xoroshiro),
    J(JavaRandom),
}

macro_rules! dispatch {
    ($s:ident, $r:ident => $e:expr) => {
        match $s {
            AnyRng::X($r) => $e,
            AnyRng::J($r) => $e,
        }
    };
}

impl Rng for AnyRng {
    fn next_int(&mut self) -> i32 {
        dispatch!(self, r => r.next_int())
    }
    fn next_bound(&mut self, bound: i32) -> i32 {
        dispatch!(self, r => r.next_bound(bound))
    }
    fn next_long(&mut self) -> i64 {
        dispatch!(self, r => r.next_long())
    }
    fn next_float(&mut self) -> f32 {
        dispatch!(self, r => r.next_float())
    }
    fn next_double(&mut self) -> f64 {
        dispatch!(self, r => r.next_double())
    }
    fn next_bool(&mut self) -> bool {
        dispatch!(self, r => r.next_bool())
    }
    fn consume(&mut self, n: usize) {
        dispatch!(self, r => r.consume(n))
    }
}

impl AnyRng {
    pub fn fork_positional(&mut self) -> Positional {
        dispatch!(self, r => r.fork_positional())
    }
}

/// Both hashes `Positional::from_hash` can need of a name. Names are fixed per version while
/// every seed forks from them, so they are hashed once at load.
#[derive(Clone, Copy, Debug)]
pub struct NameHash {
    md5: (u64, u64),
    java: i32,
}

impl NameHash {
    pub fn of(name: &str) -> Self {
        NameHash { md5: seed_from_hash(name), java: java_hash(name) }
    }
    /// The MD5 halves (the GPU forks from them).
    pub fn md5(&self) -> (u64, u64) {
        self.md5
    }
}

/// PositionalRandomFactory
#[derive(Clone, Copy, Debug)]
pub enum Positional {
    Xoroshiro { lo: u64, hi: u64 },
    Legacy { seed: i64 },
}

impl Positional {
    pub fn at(&self, x: i32, y: i32, z: i32) -> AnyRng {
        match *self {
            Positional::Xoroshiro { lo, hi } => AnyRng::X(Xoroshiro::from_parts(pos_seed(x, y, z) as u64 ^ lo, hi)),
            Positional::Legacy { seed } => AnyRng::J(JavaRandom::new(pos_seed(x, y, z) ^ seed)),
        }
    }
    pub fn from_hash(&self, name: &str) -> AnyRng {
        self.from_name(NameHash::of(name))
    }
    /// `from_hash` for a name hashed ahead of time.
    pub fn from_name(&self, h: NameHash) -> AnyRng {
        match *self {
            Positional::Xoroshiro { lo, hi } => AnyRng::X(Xoroshiro::from_parts(h.md5.0 ^ lo, h.md5.1 ^ hi)),
            Positional::Legacy { seed } => AnyRng::J(JavaRandom::new(h.java as i64 ^ seed)),
        }
    }
    pub fn from_seed(&self, s: i64) -> AnyRng {
        match *self {
            Positional::Xoroshiro { lo, hi } => AnyRng::X(Xoroshiro::from_parts(s as u64 ^ lo, s as u64 ^ hi)),
            Positional::Legacy { .. } => AnyRng::J(JavaRandom::new(s)),
        }
    }
}

/// Util.shuffle (Fisher-Yates from the end, as Minecraft does it)
pub fn shuffle<T>(v: &mut [T], r: &mut impl Rng) {
    for i in (2..=v.len()).rev() {
        let j = r.next_bound(i as i32) as usize;
        v.swap(i - 1, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_random_matches_jdk() {
        // new java.util.Random(0).nextInt() == -1155484576, nextLong() == -4962768465676381896 (fresh)
        assert_eq!(JavaRandom::new(0).next_int(), -1155484576);
        assert_eq!(JavaRandom::new(0).next_long(), -4962768465676381896);
        assert_eq!(JavaRandom::new(42).next_bound(10), 0);
    }

    #[test]
    fn md5_seed() {
        // md5("") = d41d8cd98f00b204e9800998ecf8427e
        assert_eq!(seed_from_hash(""), (0xd41d8cd98f00b204, 0xe9800998ecf8427e));
    }
}
