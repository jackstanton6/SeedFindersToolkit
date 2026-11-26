//! Carvers: CaveWorldCarver and CanyonWorldCarver (literal ports), the carving mask, and
//! NoiseBasedChunkGenerator.generateCarvers / applyCarvingMask.

use crate::density::{f32_of, ns};
use crate::rng::{JavaRandom, Rng};
use crate::world::World;
use serde_json::Value;

/// Mth.SIN: sin sampled at 65536 points.
fn sin_table() -> &'static [f32] {
    static T: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    T.get_or_init(|| (0..65536).map(|i| (i as f64 / 10430.378350470453).sin() as f32).collect())
}

/// Mth.sin(double)
pub fn msin(v: f64) -> f32 {
    sin_table()[((v * 10430.378350470453) as i64 & 65535) as usize]
}

/// Mth.cos(double)
pub fn mcos(v: f64) -> f32 {
    sin_table()[((v * 10430.378350470453 + 16384.0) as i64 & 65535) as usize]
}

/// FloatProvider (the types the carvers use).
#[derive(Clone, Debug)]
pub enum FloatP {
    Const(f32),
    Uniform(f32, f32),
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatP {
    pub fn parse(v: &Value) -> Result<FloatP, String> {
        if v.is_number() {
            return Ok(FloatP::Const(f32_of(v)));
        }
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:constant" => FloatP::Const(f32_of(&v["value"])),
            "minecraft:uniform" => FloatP::Uniform(f32_of(&v["min_inclusive"]), f32_of(&v["max_exclusive"])),
            "minecraft:trapezoid" => FloatP::Trapezoid { min: f32_of(&v["min"]), max: f32_of(&v["max"]), plateau: f32_of(&v["plateau"]) },
            t => return Err(format!("float provider {t} is not ported")),
        })
    }

    pub fn sample(&self, r: &mut impl Rng) -> f32 {
        match *self {
            FloatP::Const(v) => v,
            // Mth.randomBetween
            FloatP::Uniform(a, b) => r.next_float() * (b - a) + a,
            FloatP::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let start = (range - plateau) / 2.0;
                let end = range - start;
                min + r.next_float() * end + r.next_float() * start
            }
        }
    }
}

/// IntProvider (the types the carvers use).
#[derive(Clone, Debug)]
pub enum IntP {
    Const(i32),
    Uniform(i32, i32),
    VeryBiasedToBottom(i32, i32),
}

impl IntP {
    pub fn parse(v: &Value) -> Result<IntP, String> {
        if let Some(n) = v.as_i64() {
            return Ok(IntP::Const(n as i32));
        }
        let i = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:constant" => IntP::Const(i("value")),
            "minecraft:uniform" => IntP::Uniform(i("min_inclusive"), i("max_inclusive")),
            "minecraft:very_biased_to_bottom" => IntP::VeryBiasedToBottom(i("min_inclusive"), i("max_inclusive")),
            t => return Err(format!("int provider {t} is not ported")),
        })
    }

    pub fn sample(&self, r: &mut impl Rng) -> i32 {
        match *self {
            IntP::Const(v) => v,
            IntP::Uniform(a, b) => r.between_inclusive(a, b),
            IntP::VeryBiasedToBottom(a, b) => {
                let k = r.next_bound(b - a + 1) + 1;
                let k = r.next_bound(k) + 1;
                a + r.next_bound(k)
            }
        }
    }
}

/// VerticalAnchor resolved in the overworld's WorldGenerationContext.
pub fn anchor_y(v: &Value, min_y: i32, height: i32) -> Result<i32, String> {
    let i = |k: &str| v[k].as_i64().map(|n| n as i32);
    if let Some(y) = i("absolute") {
        Ok(y)
    } else if let Some(o) = i("above_bottom") {
        Ok(min_y + o)
    } else if let Some(o) = i("below_top") {
        Ok(height - 1 + min_y - o)
    } else {
        Err(format!("bad vertical anchor {v}"))
    }
}

/// UniformHeight (the only height provider the carvers use), resolved.
#[derive(Clone, Copy, Debug)]
pub struct UniformHeight(pub i32, pub i32);

impl UniformHeight {
    fn parse(v: &Value, w: &World) -> Result<Self, String> {
        if v["type"].as_str() != Some("minecraft:uniform") {
            return Err(format!("height provider {} is not ported", v["type"]));
        }
        Ok(UniformHeight(anchor_y(&v["min_inclusive"], w.min_y, w.height)?, anchor_y(&v["max_inclusive"], w.min_y, w.height)?))
    }

    fn sample(&self, r: &mut impl Rng) -> i32 {
        if self.0 > self.1 { self.0 } else { r.between_inclusive(self.0, self.1) }
    }
}

#[derive(Clone, Debug)]
pub enum Carver {
    Cave {
        probability: f32,
        y: UniformHeight,
        count: IntP,
        thickness: FloatP,
        weird_bias: bool,
        room_vertical: FloatP,
        horizontal: FloatP,
        vertical: FloatP,
        start_vertical: FloatP,
        floor: FloatP,
    },
    Canyon {
        probability: f32,
        y: UniformHeight,
        vertical_rotation: FloatP,
        distance_factor: FloatP,
        thickness: FloatP,
        width_smoothness: i32,
        horizontal_radius_factor: FloatP,
        vertical_default: f32,
        vertical_center: f32,
        y_scale: FloatP,
    },
}

/// The carving mask (CarvingMask): x, z local, y in [min_y, max_y].
pub struct Mask {
    pub min_y: i32,
    pub max_y: i32,
    height: i32,
    bits: Vec<bool>,
}

impl Mask {
    fn new(min_y: i32, max_y: i32) -> Mask {
        let height = max_y - min_y + 1;
        Mask { min_y, max_y, height, bits: vec![false; (256 * height) as usize] }
    }

    fn carve(&mut self, x: i32, y: i32, z: i32) {
        self.bits[(y - self.min_y + (z + (x << 4)) * self.height) as usize] = true;
    }

    /// CarvingMask.visit: (x, z, bottom y, top y) per run of set bits, in index order.
    pub fn visit(&self, mut f: impl FnMut(i32, i32, i32, i32)) {
        let n = self.bits.len();
        let mut i = 0;
        while i < n {
            if !self.bits[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && self.bits[i] {
                i += 1;
            }
            let end = i - 1;
            let (sc, ec) = (start as i32 / self.height, end as i32 / self.height);
            for col in sc..=ec {
                let base = col * self.height;
                let bottom = (start as i32 - base).max(0) + self.min_y;
                let top = (end as i32 - base).min(self.height - 1) + self.min_y;
                f(col >> 4 & 15, col & 15, bottom, top);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.bits.iter().any(|&b| b)
    }
}

impl Carver {
    pub fn parse(v: &Value, w: &World) -> Result<Carver, String> {
        let f = |k: &str| FloatP::parse(&v[k]);
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:cave" => Carver::Cave {
                probability: f32_of(&v["probability"]),
                y: UniformHeight::parse(&v["y"], w)?,
                count: IntP::parse(&v["count"])?,
                thickness: f("thickness")?,
                weird_bias: v["weird_thickness_bias"].as_bool().unwrap_or(false),
                room_vertical: f("room_vertical_radius_multiplier")?,
                horizontal: f("horizontal_radius_multiplier")?,
                vertical: f("vertical_radius_multiplier")?,
                start_vertical: if v.get("start_vertical_radius_multiplier").is_some() { f("start_vertical_radius_multiplier")? } else { FloatP::Const(1.0) },
                floor: f("floor_level")?,
            },
            "minecraft:canyon" => {
                let s = &v["shape"];
                let sf = |k: &str| FloatP::parse(&s[k]);
                Carver::Canyon {
                    probability: f32_of(&v["probability"]),
                    y: UniformHeight::parse(&v["y"], w)?,
                    vertical_rotation: f("vertical_rotation")?,
                    distance_factor: sf("distance_factor")?,
                    thickness: sf("thickness")?,
                    width_smoothness: s["width_smoothness"].as_i64().unwrap_or(1) as i32,
                    horizontal_radius_factor: sf("horizontal_radius_factor")?,
                    vertical_default: f32_of(&s["vertical_radius_default_factor"]),
                    vertical_center: f32_of(&s["vertical_radius_center_factor"]),
                    y_scale: sf("y_scale")?,
                }
            }
            t => return Err(format!("carver type {t} is not ported")),
        })
    }

    fn is_start_chunk(&self, r: &mut JavaRandom) -> bool {
        let p = match self {
            Carver::Cave { probability, .. } | Carver::Canyon { probability, .. } => *probability,
        };
        r.next_float() <= p
    }

    fn carve(&self, w: &World, r: &mut JavaRandom, chunk: (i32, i32), source: (i32, i32), out: &mut Mask) {
        match self {
            Carver::Cave { y, count, thickness, weird_bias, room_vertical, horizontal, vertical, start_vertical, floor, .. } => {
                let max_distance = (4 * 2 - 1) * 16;
                let caves = count.sample(r);
                for _ in 0..caves {
                    let x = (source.0 * 16 + r.next_bound(16)) as f64;
                    let yy = y.sample(r) as f64;
                    let z = (source.1 * 16 + r.next_bound(16)) as f64;
                    let hm = horizontal.sample(r) as f64;
                    let vm = vertical.sample(r) as f64;
                    let svm = start_vertical.sample(r) as f64;
                    let floor_level = floor.sample(r) as f64;
                    let skip = |xd: f64, yd: f64, zd: f64, _y: i32| yd <= floor_level || xd * xd + yd * yd + zd * zd >= 1.0;
                    let mut tunnels = 1;
                    if r.next_bound(4) == 0 {
                        let y_scale = room_vertical.sample(r) as f64;
                        let t = 1.0f32 + r.next_float() * 6.0;
                        // createRoom
                        let hr = 1.5 + (msin(std::f32::consts::FRAC_PI_2 as f64) * t) as f64;
                        carve_ellipsoid(chunk, x + 1.0, yy, z, hr, hr * y_scale, out, &skip);
                        tunnels += r.next_bound(4);
                    }
                    for _ in 0..tunnels {
                        let hrot = r.next_float() * (std::f64::consts::PI * 2.0) as f32;
                        let vrot = (r.next_float() - 0.5) / 4.0;
                        let mut t = thickness.sample(r);
                        if *weird_bias && r.next_bound(10) == 0 {
                            t *= r.next_float() * r.next_float() * 3.0 + 1.0;
                        }
                        let distance = max_distance - r.next_bound(max_distance / 4);
                        let seed = r.next_long();
                        cave_tunnel(chunk, seed, x, yy, z, hm, vm, t, hrot, vrot, 0, distance, svm, out, &skip);
                    }
                }
                let _ = w;
            }
            Carver::Canyon { y, vertical_rotation, distance_factor, thickness, width_smoothness, horizontal_radius_factor, vertical_default, vertical_center, y_scale, .. } => {
                let max_distance = (4 * 2 - 1) * 16;
                let x = (source.0 * 16 + r.next_bound(16)) as f64;
                let yy = y.sample(r);
                let z = (source.1 * 16 + r.next_bound(16)) as f64;
                let hrot = r.next_float() * (std::f64::consts::PI * 2.0) as f32;
                let vrot = vertical_rotation.sample(r);
                let ys = y_scale.sample(r) as f64;
                let t = thickness.sample(r);
                let distance = (max_distance as f32 * distance_factor.sample(r)) as i32;
                let seed = r.next_long();
                // doCarve
                let mut r = JavaRandom::new(seed);
                let depth = w.height;
                let mut widths = vec![0.0f32; depth as usize];
                let mut wf = 1.0f32;
                for (i, slot) in widths.iter_mut().enumerate() {
                    if i == 0 || r.next_bound(*width_smoothness) == 0 {
                        wf = 1.0 + r.next_float() * r.next_float();
                    }
                    *slot = wf * wf;
                }
                let (mut x, mut y, mut z) = (x, yy as f64, z);
                let (mut hrot, mut vrot) = (hrot, vrot);
                let (mut yrota, mut xrota) = (0.0f32, 0.0f32);
                let min_y = w.min_y;
                let skip = |xd: f64, yd: f64, zd: f64, yy: i32| (xd * xd + zd * zd) * widths[(yy - min_y - 1) as usize] as f64 + yd * yd / 6.0 >= 1.0;
                for step in 0..distance {
                    let mut hr = 1.5 + (msin((step as f32 * std::f32::consts::PI / distance as f32) as f64) * t) as f64;
                    let mut vr = hr * ys;
                    hr *= horizontal_radius_factor.sample(&mut r) as f64;
                    // updateVerticalRadius
                    let vmul = 1.0f32 - (0.5f32 - step as f32 / distance as f32).abs() * 2.0;
                    let factor = vertical_default + vertical_center * vmul;
                    vr = factor as f64 * vr * (r.next_float() * (1.0 - 0.75) + 0.75) as f64;
                    let xc = mcos(vrot as f64);
                    let xs = msin(vrot as f64);
                    x += (mcos(hrot as f64) * xc) as f64;
                    y += xs as f64;
                    z += (msin(hrot as f64) * xc) as f64;
                    vrot *= 0.7;
                    vrot += xrota * 0.05;
                    hrot += yrota * 0.05;
                    xrota *= 0.8;
                    yrota *= 0.5;
                    xrota += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
                    yrota += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
                    if r.next_bound(4) != 0 {
                        if !can_reach(chunk, x, z, step, distance, t) {
                            return;
                        }
                        carve_ellipsoid(chunk, x, y, z, hr, vr, out, &skip);
                    }
                }
            }
        }
    }
}

/// WorldCarver.canReach
fn can_reach(chunk: (i32, i32), x: f64, z: f64, step: i32, total: i32, thickness: f32) -> bool {
    let (xd, zd) = (x - (chunk.0 * 16 + 8) as f64, z - (chunk.1 * 16 + 8) as f64);
    let remaining = (total - step) as f64;
    let rr = (thickness + 2.0 + 16.0) as f64;
    xd * xd + zd * zd - remaining * remaining <= rr * rr
}

/// WorldCarver.carveEllipsoid
#[allow(clippy::too_many_arguments)]
fn carve_ellipsoid(chunk: (i32, i32), x: f64, y: f64, z: f64, hr: f64, vr: f64, out: &mut Mask, skip: &dyn Fn(f64, f64, f64, i32) -> bool) {
    let (cx, cz) = ((chunk.0 * 16 + 8) as f64, (chunk.1 * 16 + 8) as f64);
    let max_delta = 16.0 + hr * 2.0;
    if (x - cx).abs() > max_delta || (z - cz).abs() > max_delta {
        return;
    }
    let (min_x, min_z) = (chunk.0 * 16, chunk.1 * 16);
    let x0 = ((x - hr).floor() as i32 - min_x - 1).max(0);
    let x1 = ((x + hr).floor() as i32 - min_x).min(15);
    let y0 = ((y - vr).floor() as i32 - 1).max(out.min_y);
    let y1 = ((y + vr).floor() as i32 + 1).min(out.max_y);
    let z0 = ((z - hr).floor() as i32 - min_z - 1).max(0);
    let z1 = ((z + hr).floor() as i32 - min_z).min(15);
    for xi in x0..=x1 {
        let wx = min_x + xi;
        let xd = (wx as f64 + 0.5 - x) / hr;
        for zi in z0..=z1 {
            let wz = min_z + zi;
            let zd = (wz as f64 + 0.5 - z) / hr;
            if xd * xd + zd * zd >= 1.0 {
                continue;
            }
            let mut wy = y1;
            while wy > y0 {
                let yd = (wy as f64 - 0.5 - y) / vr;
                if !skip(xd, yd, zd, wy) {
                    out.carve(xi, wy, zi);
                }
                wy -= 1;
            }
        }
    }
}

/// CaveWorldCarver.createTunnel
#[allow(clippy::too_many_arguments)]
fn cave_tunnel(
    chunk: (i32, i32), seed: i64, mut x: f64, mut y: f64, mut z: f64, hm: f64, vm: f64, thickness: f32, mut hrot: f32, mut vrot: f32,
    step: i32, dist: i32, y_scale: f64, out: &mut Mask, skip: &dyn Fn(f64, f64, f64, i32) -> bool,
) {
    let mut r = JavaRandom::new(seed);
    let split = r.next_bound(dist / 2) + dist / 4;
    let steep = r.next_bound(6) == 0;
    let (mut yrota, mut xrota) = (0.0f32, 0.0f32);
    for cur in step..dist {
        let hr = 1.5 + (msin((std::f32::consts::PI * cur as f32 / dist as f32) as f64) * thickness) as f64;
        let vr = hr * y_scale;
        let cos_x = mcos(vrot as f64);
        x += (mcos(hrot as f64) * cos_x) as f64;
        y += msin(vrot as f64) as f64;
        z += (msin(hrot as f64) * cos_x) as f64;
        vrot *= if steep { 0.92 } else { 0.7 };
        vrot += xrota * 0.1;
        hrot += yrota * 0.1;
        xrota *= 0.9;
        yrota *= 0.75;
        xrota += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
        yrota += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
        if cur == split && thickness > 1.0 {
            let s1 = r.next_long();
            let t1 = r.next_float() * 0.5 + 0.5;
            cave_tunnel(chunk, s1, x, y, z, hm, vm, t1, hrot - std::f32::consts::FRAC_PI_2, vrot / 3.0, cur, dist, 1.0, out, skip);
            let s2 = r.next_long();
            let t2 = r.next_float() * 0.5 + 0.5;
            cave_tunnel(chunk, s2, x, y, z, hm, vm, t2, hrot + std::f32::consts::FRAC_PI_2, vrot / 3.0, cur, dist, 1.0, out, skip);
            return;
        }
        if r.next_bound(4) != 0 {
            if !can_reach(chunk, x, z, cur, dist, thickness) {
                return;
            }
            carve_ellipsoid(chunk, x, y, z, hr * hm, vr * vm, out, skip);
        }
    }
}

/// Every carver and each biome's carver list (indices into `carvers`).
pub struct Carvers {
    pub carvers: Vec<Carver>,
    pub by_biome: Vec<Vec<usize>>,
}

impl Carvers {
    pub fn load(w: &World) -> Result<Carvers, String> {
        let mut names: Vec<String> = vec![];
        let mut carvers = vec![];
        let mut by_biome = vec![];
        for b in &w.biomes.json {
            let list: Vec<String> = match &b["carvers"] {
                Value::String(s) => vec![ns(s)],
                Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(ns)).collect(),
                _ => vec![],
            };
            let mut ids = vec![];
            for n in list {
                let i = match names.iter().position(|x| *x == n) {
                    Some(i) => i,
                    None => {
                        let path = w.dir.join("minecraft/worldgen/carver").join(format!("{}.json", n.trim_start_matches("minecraft:")));
                        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| e.to_string())?;
                        carvers.push(Carver::parse(&v, w)?);
                        names.push(n);
                        names.len() - 1
                    }
                };
                ids.push(i);
            }
            by_biome.push(ids);
        }
        Ok(Carvers { carvers, by_biome })
    }

    /// generateCarvers' mask for a chunk: every carver of the biomes of the 17x17 source chunks
    /// around it. `biome_at(qx, qz)` is the point-path biome at quart (qx, 0, qz).
    pub fn mask(&self, w: &World, seed: i64, cx: i32, cz: i32, mut biome_at: impl FnMut(i32, i32) -> u16) -> Mask {
        let protected_top = 7;
        let max_y = w.min_y + w.height - 1 - protected_top;
        let mut mask = Mask::new(w.min_y + 1, max_y);
        let mut r = JavaRandom::new(0);
        for dx in -8..=8 {
            for dz in -8..=8 {
                let (sx, sz) = (cx + dx, cz + dz);
                let biome = biome_at((sx * 16) >> 2, (sz * 16) >> 2);
                for (index, &k) in self.by_biome[biome as usize].iter().enumerate() {
                    r.set_large_feature_seed(seed.wrapping_add(index as i64), sx, sz);
                    let c = &self.carvers[k];
                    if c.is_start_chunk(&mut r) {
                        c.carve(w, &mut r, (cx, cz), (sx, sz), &mut mask);
                    }
                }
            }
        }
        mask
    }
}
