//! 26.x noise: float Perlin (GradientNoise/PerlinNoise/SmearedPerlinNoise), NoiseStack,
//! NormalNoise (new octave/normalisation model) and BlendedNoise's fbm sets.

use crate::density::Volume;
use crate::rng::{AnyRng, NameHash, Rng};
use serde_json::Value;

const GRADIENT: [[f32; 3]; 16] = [
    [1., 1., 0.], [-1., 1., 0.], [1., -1., 0.], [-1., -1., 0.],
    [1., 0., 1.], [-1., 0., 1.], [1., 0., -1.], [-1., 0., -1.],
    [0., 1., 1.], [0., -1., 1.], [0., 1., -1.], [0., -1., -1.],
    [1., 1., 0.], [0., -1., 1.], [-1., 1., 0.], [0., -1., -1.],
];
const HALF_ROUND_OFF: f64 = 16777215.999999998; // Math.nextDown(16777216.0)

#[inline]
fn smoothstep(x: f32) -> f32 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}
#[inline]
fn lerp(a: f32, p0: f32, p1: f32) -> f32 {
    p0 + a * (p1 - p0)
}
#[inline]
fn wrap(x: f64) -> f64 {
    if x >= -HALF_ROUND_OFF && x < HALF_ROUND_OFF {
        x
    } else {
        x - (x / 33554432.0 + 0.5).floor() * 33554432.0
    }
}

/// One Perlin octave. `smear` is SmearedPerlinNoise's fudgeYScale (None = plain PerlinNoise).
#[derive(Clone)]
pub struct Perlin {
    perm: [u8; 256],
    ox: f64,
    oy: f64,
    oz: f64,
    smear: Option<f64>,
}

impl Perlin {
    pub fn new(r: &mut impl Rng, smear: Option<f64>) -> Self {
        let ox = r.next_double() * 256.0;
        let oy = r.next_double() * 256.0;
        let oz = r.next_double() * 256.0;
        let mut perm = [0u8; 256];
        for (i, p) in perm.iter_mut().enumerate() {
            *p = i as u8;
        }
        for i in 0..256 {
            let j = r.next_bound(256 - i as i32) as usize + i;
            perm.swap(i, j);
        }
        Self { perm, ox, oy, oz, smear }
    }

    #[inline]
    fn p(&self, i: i32) -> i32 {
        self.perm[(i & 0xFF) as usize] as i32
    }

    #[inline]
    fn grad(&self, i: i32, x: f32, y: f32, z: f32) -> f32 {
        let g = GRADIENT[(self.p(i) & 15) as usize];
        g[0] * x + g[1] * y + g[2] * z
    }

    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let xs = wrap(x) + self.ox;
        let ys = wrap(y) + self.oy;
        let zs = wrap(z) + self.oz;
        let (fx, fy, fz) = (xs.floor() as i32, ys.floor() as i32, zs.floor() as i32);
        let rx = (xs - fx as f64) as f32;
        let rz = (zs - fz as f64) as f32;
        match self.smear {
            None => {
                let ry = (ys - fy as f64) as f32;
                self.sample_and_lerp(fx, fy, fz, rx, ry, rz, ry)
            }
            Some(fudge) => {
                let ry = ys - fy as f64;
                let limit = if y >= 0.0 && y < ry { y } else { ry };
                let fudge_y = (limit / fudge + 1.0e-7f32 as f64).floor() as i32 as f64 * fudge;
                self.sample_and_lerp(fx, fy, fz, rx, (ry - fudge_y) as f32, rz, ry as f32)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(&self, x: i32, y: i32, z: i32, rx: f32, ry: f32, rz: f32, ory: f32) -> f32 {
        let x0 = self.p(x);
        let x1 = self.p(x.wrapping_add(1));
        let xy00 = self.p(x0 + y);
        let xy01 = self.p(x0 + y + 1);
        let xy10 = self.p(x1 + y);
        let xy11 = self.p(x1 + y + 1);
        let d000 = self.grad(xy00 + z, rx, ry, rz);
        let d100 = self.grad(xy10 + z, rx - 1.0, ry, rz);
        let d010 = self.grad(xy01 + z, rx, ry - 1.0, rz);
        let d110 = self.grad(xy11 + z, rx - 1.0, ry - 1.0, rz);
        let d001 = self.grad(xy00 + z + 1, rx, ry, rz - 1.0);
        let d101 = self.grad(xy10 + z + 1, rx - 1.0, ry, rz - 1.0);
        let d011 = self.grad(xy01 + z + 1, rx, ry - 1.0, rz - 1.0);
        let d111 = self.grad(xy11 + z + 1, rx - 1.0, ry - 1.0, rz - 1.0);
        let (ax, ay, az) = (smoothstep(rx), smoothstep(ory), smoothstep(rz));
        lerp(
            az,
            lerp(ay, lerp(ax, d000, d100), lerp(ax, d010, d110)),
            lerp(ay, lerp(ax, d001, d101), lerp(ax, d011, d111)),
        )
    }

    pub fn range(&self) -> f32 {
        match self.smear {
            None => 2.0,
            Some(f) => (f.abs() + 2.0) as f32,
        }
    }
}

/// NoiseStack: sum of amplitude * octave(coord * frequency), accumulated in float.
#[derive(Clone, Default)]
pub struct NoiseStack {
    pub layers: Vec<(Perlin, f64, f32)>, // (noise, frequency, amplitude)
}

impl NoiseStack {
    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut v = 0.0f32;
        for (n, f, a) in &self.layers {
            v += a * n.get(x * f, y * f, z * f);
        }
        v
    }

    /// Noise.addToVolume. Coordinates are `block * (scale * frequency)` (note the association),
    /// which is how the volume path differs from `get` for non power-of-two scales.
    pub fn add_to_volume(&self, buf: &mut [f32], vol: &Volume, xz: f64, ys: f64, amp: f32) {
        for (n, f, a) in &self.layers {
            let (sxz, sy, a) = (xz * f, ys * f, amp * a);
            let mut i = 0;
            for iz in 0..vol.sz {
                let z = vol.bz(iz) as f64 * sxz;
                for ix in 0..vol.sx {
                    let x = vol.bx(ix) as f64 * sxz;
                    for iy in 0..vol.sy {
                        buf[i] += a * n.get(x, vol.by(iy) as f64 * sy, z);
                        i += 1;
                    }
                }
            }
        }
    }
}

/// Java's DoubleStream.sum (Kahan, JDK 18+ final step).
fn java_sum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut sum, mut comp) = (0.0f64, 0.0f64);
    for v in values {
        let t = v - comp;
        let velvel = sum + t;
        comp = (velvel - sum) - t;
        sum = velvel;
    }
    sum - comp
}

#[derive(Clone, Copy, PartialEq)]
enum Normalization {
    Disabled,
    Enabled,
    Legacy,
}

/// NormalNoise parameters, resolved to octaves.
#[derive(Clone)]
pub struct NormalNoise {
    octaves: Vec<(i32, f64, f64)>, // (octave index, frequency, amplitude)
    names: Vec<NameHash>,          // "octave_<index>" per octave, hashed once (every seed needs them)
    pub id: NameHash,              // the noise's registry id
    norm: f64,
    pub range: f32,
}

const STD_DEV: f64 = 0.2702247831245211;

fn build_octaves(base: i32, base_amp: f64, count: i32, normalize: bool, mods: &[f64]) -> Vec<(i32, f64, f64)> {
    let mut freq = 2f64.powi(base);
    let mut amp = base_amp;
    if normalize {
        amp *= 0.5f64.powi(-(count - 1)) / (0.5f64.powi(-count) - 1.0);
    }
    let mut out = vec![];
    for i in 0..count {
        let m = if mods.is_empty() { 1.0 } else { mods[i as usize] };
        if m != 0.0 {
            out.push((base + i, freq, amp * m));
        }
        freq *= 2.0;
        amp *= 0.5;
    }
    out
}

fn normalization_factor(target: f64, octaves: &[(i32, f64, f64)]) -> f64 {
    let dev = octaves.iter().map(|o| STD_DEV * o.2.abs()).map(|d| d * d).sum::<f64>().sqrt();
    if dev == 0.0 {
        return 0.0;
    }
    target * 0.3333333333333333 / (dev * 2f64.sqrt())
}

fn parity_factor(base_amp: f64, count: i32, mods: &[f64]) -> f64 {
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for i in 0..count {
        let m = if mods.is_empty() { 1.0 } else { mods[i as usize] };
        if m != 0.0 {
            lo = lo.min(i);
            hi = hi.max(i);
        }
    }
    base_amp * 0.5 * 0.3333333333333333 / (0.1 * (1.0 + 1.0 / (hi - lo + 1) as f64))
}

impl NormalNoise {
    pub fn from_json(id: &str, v: &Value) -> Self {
        let base = v["base_octave"].as_i64().unwrap() as i32;
        let count = v.get("octave_count").and_then(Value::as_i64).unwrap_or(1) as i32;
        let base_amp = v.get("base_amplitude").and_then(Value::as_f64).unwrap_or(1.0);
        let mods: Vec<f64> = v.get("amplitude_modifiers")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|x| x.as_f64().unwrap()).collect())
            .unwrap_or_default();
        let norm = match v.get("normalize") {
            Some(Value::Bool(false)) => Normalization::Disabled,
            Some(Value::String(_)) => Normalization::Legacy,
            _ => Normalization::Enabled,
        };
        let octaves = build_octaves(base, base_amp, count, norm != Normalization::Disabled, &mods);
        let mut target = java_sum(octaves.iter().map(|o| o.2.abs()));
        let mut factor = normalization_factor(target, &octaves);
        if norm == Normalization::Legacy && factor != 0.0 {
            let parity = parity_factor(base_amp, count, &mods);
            target *= parity / factor;
            factor = parity;
        }
        let names = octaves.iter().map(|o| NameHash::of(&format!("octave_{}", o.0))).collect();
        Self { octaves, names, id: NameHash::of(id), norm: factor, range: (target * 0.3333333333333333 * 6.0) as f32 }
    }

    /// What `create` needs besides the random, for the GPU: per octave the name hash, frequency and
    /// float amplitude of its first layer (the second layer has frequency * 1.0181268882175227).
    pub fn octave_layers(&self) -> Vec<(NameHash, f64, f32)> {
        self.octaves.iter().zip(&self.names).map(|(&(_, freq, amp), &name)| (name, freq, (self.norm * amp) as f32)).collect()
    }

    /// NormalNoise.create(random)
    pub fn create(&self, r: &mut AnyRng) -> NoiseStack {
        let first = r.fork_positional();
        let second = r.fork_positional();
        let mut stack = NoiseStack::default();
        for (&(_, freq, amp), &name) in self.octaves.iter().zip(&self.names) {
            let a = Perlin::new(&mut first.from_name(name), None);
            let b = Perlin::new(&mut second.from_name(name), None);
            let vf = (self.norm * amp) as f32;
            stack.layers.push((a, freq, vf));
            stack.layers.push((b, freq * 1.0181268882175227, vf));
        }
        stack
    }
}

/// BlendedNoise.createFbm
pub fn blended_fbm(r: &mut AnyRng, first_octave: i32, smear_y: f64, mut value_factor: f64) -> NoiseStack {
    let octaves = -first_octave + 1;
    let mut factor = 1.0;
    value_factor /= 2f64.powi(octaves) - 1.0;
    let mut stack = NoiseStack::default();
    for _ in 0..octaves {
        stack.layers.push((Perlin::new(r, Some(smear_y * factor)), factor, value_factor as f32));
        factor /= 2.0;
        value_factor *= 2.0;
    }
    stack
}

/// BlendedNoise.computeFbmRange as an Interval [-r, r] accumulated like Interval.add/mul.
pub fn blended_fbm_range(first_octave: i32, smear_y: f64, mut value_factor: f64) -> f32 {
    let octaves = -first_octave + 1;
    let mut factor = 1.0;
    value_factor /= 2f64.powi(octaves) - 1.0;
    let mut hi = 0.0f32;
    for _ in 0..octaves {
        let r = ((smear_y * factor).abs() + 2.0) as f32;
        hi += r * value_factor as f32;
        factor /= 2.0;
        value_factor *= 2.0;
    }
    hi
}
