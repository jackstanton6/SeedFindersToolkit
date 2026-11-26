//! Decoration: ChunkGenerator.applyBiomeDecoration (FeatureSorter order, decoration and feature
//! seeds), FeaturePlacer (placement modifiers, depth first), block predicates, state and value
//! providers, and the features (literal ports; feature types not ported yet place nothing).

use crate::blocks::State;
use crate::chunk::{Chunk, Materials, MOTION_BLOCKING, OCEAN_FLOOR, OCEAN_FLOOR_WG, WORLD_SURFACE, WORLD_SURFACE_WG, MOTION_BLOCKING_NO_LEAVES};
use crate::density::{f32_of, ns};
use crate::rng::{Rng, WorldgenXoro};
use crate::world::{BiomeCtx, Seeded, World};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub type P = (i32, i32, i32);

/// Direction order: down, up, north, south, west, east.
pub const STEP: [P; 6] = [(0, -1, 0), (0, 1, 0), (0, 0, -1), (0, 0, 1), (-1, 0, 0), (1, 0, 0)];
/// Direction.Plane.HORIZONTAL: north, east, south, west.
pub const HORIZONTAL: [usize; 4] = [2, 5, 3, 4];

pub fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

pub fn rel(p: P, d: usize) -> P {
    add(p, STEP[d])
}

fn direction(s: &str) -> Result<usize, String> {
    ["down", "up", "north", "south", "west", "east"].iter().position(|d| *d == s).ok_or(format!("bad direction {s}"))
}

fn vec3(v: &Value) -> P {
    match v.as_array() {
        Some(a) if a.len() == 3 => (a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32, a[2].as_i64().unwrap_or(0) as i32),
        _ => (0, 0, 0),
    }
}

/// Mth.nextInt(random, min, max)
pub fn next_int_between(r: &mut WorldgenXoro, min: i32, max: i32) -> i32 {
    if min >= max { min } else { r.next_bound(max - min + 1) + min }
}

// ------------------------------------------------------------------------------------------------
// The level features write into (WorldGenRegion around the chunk being decorated)
// ------------------------------------------------------------------------------------------------

pub struct Level<'a> {
    pub w: &'a World,
    pub m: &'a Materials,
    pub s: &'a Seeded<'a>,
    pub chunks: HashMap<(i32, i32), Chunk>,
    /// The chunk being decorated: writes reach one chunk around it.
    pub centre: (i32, i32),
    pub bc: BiomeCtx,
}

impl<'a> Level<'a> {
    pub fn get(&self, p: P) -> State {
        match self.chunks.get(&(p.0 >> 4, p.2 >> 4)) {
            Some(c) => c.get(self.w, p.0, p.1, p.2),
            // ponytail: chunks outside the generated area read as void air
            None => self.w.blocks.void_air,
        }
    }

    fn writable(&self, p: P) -> bool {
        ((p.0 >> 4) - self.centre.0).abs() <= 1 && ((p.2 >> 4) - self.centre.1).abs() <= 1
    }

    /// WorldGenRegion.setBlock (no neighbour updates; marks the state's post-process position).
    pub fn set(&mut self, p: P, s: State) -> bool {
        self.set_flags(p, s, 2)
    }

    pub fn set_flags(&mut self, p: P, s: State, flags: i32) -> bool {
        if !self.writable(p) {
            return false;
        }
        let (w, m) = (self.w, self.m);
        let Some(c) = self.chunks.get_mut(&(p.0 >> 4, p.2 >> 4)) else { return false };
        c.set(m, w, p.0, p.1, p.2, s);
        if flags & 16 == 0 {
            // BlockBehaviour.PostProcess: the mushrooms mark themselves, soul sand and magma the
            // block above
            let n = w.blocks.name(s);
            let mark = match n {
                "minecraft:brown_mushroom" | "minecraft:red_mushroom" => Some(p),
                "minecraft:soul_sand" | "minecraft:magma_block" => Some(rel(p, 1)),
                _ => None,
            };
            if let Some(q) = mark {
                self.mark(q);
            }
        }
        true
    }

    /// LevelChunkSection.setBlockState: no heightmaps, no marks (ore blobs).
    pub fn set_raw(&mut self, p: P, s: State) {
        let w = self.w;
        if let Some(c) = self.chunks.get_mut(&(p.0 >> 4, p.2 >> 4)) {
            c.set_raw(w, p.0, p.1, p.2, s);
        }
    }

    pub fn mark(&mut self, p: P) {
        if let Some(c) = self.chunks.get_mut(&(p.0 >> 4, p.2 >> 4)) {
            c.mark(p.0, p.1, p.2);
        }
    }

    /// WorldGenRegion.getHeight: the first free y.
    pub fn height(&mut self, hm: usize, x: i32, z: i32) -> i32 {
        let (w, m) = (self.w, self.m);
        match self.chunks.get_mut(&(x >> 4, z >> 4)) {
            Some(c) => c.height_mut(m, w, hm, x, z) + 1,
            None => w.min_y,
        }
    }

    pub fn biome(&mut self, p: P) -> u16 {
        self.s.block_biome(&mut self.bc, p.0, p.1, p.2)
    }

    pub fn inside(&self, y: i32) -> bool {
        y >= self.w.min_y && y < self.w.min_y + self.w.height
    }

    pub fn name(&self, p: P) -> &str {
        self.w.blocks.name(self.get(p))
    }

    pub fn is_air(&self, p: P) -> bool {
        self.w.blocks.is_air(self.get(p))
    }
}

// ------------------------------------------------------------------------------------------------
// Value providers
// ------------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum IntP {
    Const(i32),
    Uniform(i32, i32),
    BiasedToBottom(i32, i32),
    Clamped(Box<IntP>, i32, i32),
    Weighted(Vec<(IntP, i32)>, i32),
    Trapezoid(i32, i32, i32),
    VeryBiasedToBottom(i32, i32),
    /// ClampedNormalInt: mean, deviation, min, max
    ClampedNormal(f32, f32, i32, i32),
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
            "minecraft:biased_to_bottom" => IntP::BiasedToBottom(i("min_inclusive"), i("max_inclusive")),
            "minecraft:very_biased_to_bottom" => IntP::VeryBiasedToBottom(i("min_inclusive"), i("max_inclusive")),
            "minecraft:clamped" => IntP::Clamped(Box::new(IntP::parse(&v["source"])?), i("min_inclusive"), i("max_inclusive")),
            "minecraft:trapezoid" => IntP::Trapezoid(i("min"), i("max"), i("plateau")),
            "minecraft:clamped_normal" => IntP::ClampedNormal(f32_of(&v["mean"]), f32_of(&v["deviation"]), i("min_inclusive"), i("max_inclusive")),
            "minecraft:weighted_list" => {
                let d = v["distribution"].as_array().ok_or("weighted_list")?;
                let items: Vec<(IntP, i32)> = d.iter().map(|e| Ok((IntP::parse(&e["data"])?, e["weight"].as_i64().unwrap_or(1) as i32))).collect::<Result<_, String>>()?;
                let total = items.iter().map(|e| e.1).sum();
                IntP::Weighted(items, total)
            }
            t => return Err(format!("int provider {t} is not ported")),
        })
    }

    pub fn sample(&self, r: &mut WorldgenXoro) -> i32 {
        match self {
            IntP::Const(v) => *v,
            IntP::Uniform(a, b) => r.between_inclusive(*a, *b),
            IntP::BiasedToBottom(a, b) => {
                let k = r.next_bound(b - a + 1) + 1;
                a + r.next_bound(k)
            }
            IntP::VeryBiasedToBottom(a, b) => {
                let k = r.next_bound(b - a + 1) + 1;
                let k = r.next_bound(k) + 1;
                a + r.next_bound(k)
            }
            IntP::Clamped(s, a, b) => s.sample(r).clamp(*a, *b),
            IntP::ClampedNormal(mean, dev, a, b) => {
                // (int)Mth.clamp(Mth.normal(random, mean, deviation), min, max) in float
                let n = mean + r.next_gaussian() as f32 * dev;
                let (lo, hi) = (*a as f32, *b as f32);
                (if n < lo { lo } else { n.min(hi) }) as i32
            }
            IntP::Weighted(items, total) => {
                let mut sel = r.next_bound(*total);
                for (p, w) in items {
                    if sel < *w {
                        return p.sample(r);
                    }
                    sel -= w;
                }
                unreachable!()
            }
            IntP::Trapezoid(min, max, plateau) => {
                if *plateau == 0 && *max == -*min {
                    return r.next_bound(max + 1) - r.next_bound(max + 1);
                }
                let range = max - min;
                if *plateau == range {
                    return r.between_inclusive(*min, *max);
                }
                let start = (range - plateau) / 2;
                let end = range - start;
                min + r.between_inclusive(0, end) + r.between_inclusive(0, start)
            }
        }
    }
}

pub fn float_p(v: &Value) -> Result<crate::carver::FloatP, String> {
    crate::carver::FloatP::parse(v)
}

/// HeightProvider, anchors resolved.
#[derive(Debug)]
pub enum HeightP {
    Const(i32),
    Uniform(i32, i32),
    Trapezoid(i32, i32, i32),
    BiasedToBottom(i32, i32, i32),
    VeryBiasedToBottom(i32, i32, i32),
}

impl HeightP {
    fn parse(v: &Value, w: &World) -> Result<HeightP, String> {
        let a = |k: &str| crate::carver::anchor_y(&v[k], w.min_y, w.height);
        if v.get("type").is_none() {
            return Ok(HeightP::Const(crate::carver::anchor_y(v, w.min_y, w.height)?));
        }
        let inner = v["inner"].as_i64().unwrap_or(1) as i32;
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:constant" => HeightP::Const(crate::carver::anchor_y(&v["value"], w.min_y, w.height)?),
            "minecraft:uniform" => HeightP::Uniform(a("min_inclusive")?, a("max_inclusive")?),
            "minecraft:trapezoid" => HeightP::Trapezoid(a("min_inclusive")?, a("max_inclusive")?, v["plateau"].as_i64().unwrap_or(0) as i32),
            "minecraft:biased_to_bottom" => HeightP::BiasedToBottom(a("min_inclusive")?, a("max_inclusive")?, inner),
            "minecraft:very_biased_to_bottom" => HeightP::VeryBiasedToBottom(a("min_inclusive")?, a("max_inclusive")?, inner),
            t => return Err(format!("height provider {t} is not ported")),
        })
    }

    fn sample(&self, r: &mut WorldgenXoro) -> i32 {
        match *self {
            HeightP::Const(y) => y,
            HeightP::Uniform(a, b) => if a > b { a } else { r.between_inclusive(a, b) },
            HeightP::Trapezoid(min, max, plateau) => {
                if min > max {
                    return min;
                }
                let range = max - min;
                if plateau >= range {
                    return r.between_inclusive(min, max);
                }
                let start = (range - plateau) / 2;
                let end = range - start;
                min + r.between_inclusive(0, end) + r.between_inclusive(0, start)
            }
            HeightP::BiasedToBottom(min, max, inner) => {
                if max - min - inner + 1 <= 0 {
                    return min;
                }
                let limit = r.next_bound(max - min - inner + 1);
                r.next_bound(limit + inner) + min
            }
            HeightP::VeryBiasedToBottom(min, max, inner) => {
                if max - min - inner + 1 <= 0 {
                    return min;
                }
                let upper = next_int_between(r, min + inner, max);
                let biased = next_int_between(r, min, upper - 1);
                next_int_between(r, min, biased - 1 + inner)
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Block sets, predicates, rule tests, state providers
// ------------------------------------------------------------------------------------------------

/// A HolderSet<Block>: per-block table.
pub fn block_set(w: &World, v: &Value) -> Result<Arc<Vec<bool>>, String> {
    let b = &w.blocks;
    let mut t = vec![false; b.count()];
    let mut add = |n: &str| -> Result<(), String> {
        match n.strip_prefix('#') {
            Some(tag) => b.tag_table(&ns(tag)).iter().enumerate().for_each(|(i, &x)| t[i] |= x),
            None => t[b.index(b.state(n).ok_or(format!("unknown block {n}"))?)] = true,
        }
        Ok(())
    };
    match v {
        Value::String(s) => add(s)?,
        Value::Array(a) => a.iter().filter_map(Value::as_str).try_for_each(&mut add)?,
        _ => {}
    }
    Ok(Arc::new(t))
}

#[derive(Debug)]
pub enum Pred {
    True,
    Blocks(P, Arc<Vec<bool>>),
    Fluids(P, u8, bool),
    Survive(P, State),
    Solid(P),
    Sturdy(P, usize),
    Replaceable(P),
    Inside(P),
    Height(i32, i32),
    Volume(P, P, Box<Pred>),
    Not(Box<Pred>),
    Any(Vec<Pred>),
    All(Vec<Pred>),
}

impl Pred {
    pub fn parse(v: &Value, w: &World) -> Result<Pred, String> {
        let off = vec3(&v["offset"]);
        let list = |k: &str| -> Result<Vec<Pred>, String> { v[k].as_array().ok_or(format!("{k}"))?.iter().map(|x| Pred::parse(x, w)).collect() };
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:true" => Pred::True,
            "minecraft:matching_blocks" => Pred::Blocks(off, block_set(w, &v["blocks"])?),
            "minecraft:matching_block_tag" => Pred::Blocks(off, block_set(w, &Value::String(format!("#{}", v["tag"].as_str().unwrap_or_default())))?),
            "minecraft:matching_fluids" => {
                // HolderSet<Fluid>: water / flowing_water / lava / flowing_lava (or the tags)
                let names: Vec<String> = match &v["fluids"] {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => vec![],
                };
                let water = names.iter().any(|n| n.contains("water"));
                let lava = names.iter().any(|n| n.contains("lava"));
                let flowing_only = names.iter().all(|n| n.contains("flowing"));
                Pred::Fluids(off, (water as u8) | (lava as u8) << 1, flowing_only)
            }
            "minecraft:would_survive" => Pred::Survive(off, w.blocks.parse(&v["state"])?),
            "minecraft:solid" => Pred::Solid(off),
            "minecraft:has_sturdy_face" => Pred::Sturdy(off, direction(v["direction"].as_str().unwrap_or("up"))?),
            "minecraft:replaceable" => Pred::Replaceable(off),
            "minecraft:inside_world_bounds" => Pred::Inside(off),
            "minecraft:height_range" => Pred::Height(crate::carver::anchor_y(&v["min_inclusive"], w.min_y, w.height)?, crate::carver::anchor_y(&v["max_inclusive"], w.min_y, w.height)?),
            "minecraft:volume_match" => Pred::Volume(vec3(&v["min"]), vec3(&v["max"]), Box::new(Pred::parse(&v["match"], w)?)),
            "minecraft:not" => Pred::Not(Box::new(Pred::parse(&v["predicate"], w)?)),
            "minecraft:any_of" => Pred::Any(list("predicates")?),
            "minecraft:all_of" => Pred::All(list("predicates")?),
            t => return Err(format!("block predicate {t} is not ported")),
        })
    }

    pub fn test(&self, l: &mut Level, p: P) -> bool {
        let b = &l.w.blocks;
        match self {
            Pred::True => true,
            Pred::Blocks(o, t) => b.in_table(t, l.get(add(p, *o))),
            Pred::Fluids(o, kinds, flowing_only) => {
                let (ty, _, source, _) = b.fluid(l.get(add(p, *o)));
                ty != 0 && kinds >> (ty - 1) & 1 != 0 && !(*flowing_only && source)
            }
            Pred::Survive(o, s) => can_survive(l, *s, add(p, *o)),
            Pred::Solid(o) => b.is_solid(l.get(add(p, *o))),
            Pred::Sturdy(o, d) => b.face_sturdy(l.get(add(p, *o)), *d),
            Pred::Replaceable(o) => b.can_be_replaced(l.get(add(p, *o))),
            Pred::Inside(o) => l.inside(p.1 + o.1),
            Pred::Height(lo, hi) => p.1 >= *lo && p.1 <= *hi,
            Pred::Volume(lo, hi, m) => {
                for ox in lo.0..=hi.0 {
                    for oz in lo.2..=hi.2 {
                        for oy in lo.1..=hi.1 {
                            if !m.test(l, add(p, (ox, oy, oz))) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
            Pred::Not(x) => !x.test(l, p),
            Pred::Any(v) => v.iter().any(|x| x.test(l, p)),
            Pred::All(v) => v.iter().all(|x| x.test(l, p)),
        }
    }
}

/// RuleTest (structure processors and ore targets).
#[derive(Debug)]
pub enum RuleTest {
    Always,
    Tag(Arc<Vec<bool>>),
    Block(State),
    State(State),
    RandomBlock(State, f32),
    Height(i32, i32),
    Not(Box<RuleTest>),
    Any(Vec<RuleTest>),
    All(Vec<RuleTest>),
}

impl RuleTest {
    pub fn parse(v: &Value, w: &World) -> Result<RuleTest, String> {
        let list = || -> Result<Vec<RuleTest>, String> { v["rules"].as_array().ok_or("rules")?.iter().map(|x| RuleTest::parse(x, w)).collect() };
        Ok(match v["predicate_type"].as_str().unwrap_or_default() {
            "minecraft:always_true" => RuleTest::Always,
            "minecraft:tag_match" => RuleTest::Tag(block_set(w, &Value::String(format!("#{}", v["tag"].as_str().unwrap_or_default())))?),
            "minecraft:block_match" => RuleTest::Block(w.blocks.parse(&v["block"])?),
            "minecraft:blockstate_match" => RuleTest::State(w.blocks.parse(&v["block_state"])?),
            "minecraft:random_block_match" => RuleTest::RandomBlock(w.blocks.parse(&v["block"])?, f32_of(&v["probability"])),
            "minecraft:height_match" => RuleTest::Height(v["min_inclusive"].as_i64().unwrap_or(0) as i32, v["max_inclusive"].as_i64().unwrap_or(0) as i32),
            "minecraft:not" => RuleTest::Not(Box::new(RuleTest::parse(&v["rule"], w)?)),
            "minecraft:any_of" => RuleTest::Any(list()?),
            "minecraft:all_of" => RuleTest::All(list()?),
            t => return Err(format!("rule test {t} is not ported")),
        })
    }

    pub fn test(&self, w: &World, s: State, p: P, r: &mut WorldgenXoro) -> bool {
        let b = &w.blocks;
        match self {
            RuleTest::Always => true,
            RuleTest::Tag(t) => b.in_table(t, s),
            RuleTest::Block(x) => b.name(s) == b.name(*x),
            RuleTest::State(x) => s == *x,
            RuleTest::RandomBlock(x, prob) => b.name(s) == b.name(*x) && r.next_float() < *prob,
            RuleTest::Height(lo, hi) => *lo <= p.1 && p.1 <= *hi,
            RuleTest::Not(x) => !x.test(w, s, p, r),
            RuleTest::Any(v) => v.iter().any(|x| x.test(w, s, p, r)),
            RuleTest::All(v) => v.iter().all(|x| x.test(w, s, p, r)),
        }
    }
}

pub enum Provider {
    Simple(State),
    Weighted(Vec<(State, i32)>, i32),
    Rule(Vec<(Pred, Provider)>, Option<Box<Provider>>),
    Rotated(Box<Provider>, Option<usize>),
    RandomizedInt(Box<Provider>, String, IntP),
    /// NoiseProvider: noise, scale, states
    Noise(crate::noise::NoiseStack, f32, Vec<State>),
    /// NoiseThresholdProvider: noise, scale, threshold, high chance, default, low, high
    NoiseThreshold(crate::noise::NoiseStack, f32, f32, f32, State, Vec<State>, Vec<State>),
    /// DualNoiseProvider: variety, slow noise, slow scale, noise, scale, states
    Dual((i32, i32), crate::noise::NoiseStack, f32, crate::noise::NoiseStack, f32, Vec<State>),
}

/// NoiseBasedStateProvider's noise: NormalNoise.create(WorldgenRandom(LegacyRandomSource(seed)))
fn provider_noise(v: &Value, seed: i64) -> crate::noise::NoiseStack {
    let n = crate::noise::NormalNoise::from_json("", v);
    n.create(&mut crate::rng::AnyRng::J(crate::rng::JavaRandom::new(seed)))
}

/// NoiseProvider.getRandomState(states, value)
fn noise_pick(states: &[State], v: f32) -> State {
    let place = ((1.0f32 + v) / 2.0).clamp(0.0, 0.9999);
    states[(place * states.len() as f32) as usize]
}

impl Provider {
    pub fn parse(v: &Value, w: &World) -> Result<Provider, String> {
        // a name: a block_state_provider registry entry, else a block
        if let Some(name) = v.as_str() {
            let path = w.dir.join("minecraft/worldgen/block_state_provider").join(format!("{}.json", ns(name).trim_start_matches("minecraft:")));
            if let Ok(text) = std::fs::read_to_string(&path) {
                let j: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                return Provider::parse(&j, w);
            }
            return Ok(Provider::Simple(w.blocks.parse(v)?));
        }
        let t = v["type"].as_str();
        if t.is_none() {
            return Ok(Provider::Simple(w.blocks.parse(v)?));
        }
        let states = |k: &str| -> Result<Vec<State>, String> { v[k].as_array().ok_or(format!("{k}"))?.iter().map(|x| w.blocks.parse(x)).collect() };
        let seed = v["seed"].as_i64().unwrap_or(0);
        Ok(match t.unwrap() {
            "minecraft:simple_state_provider" => Provider::Simple(w.blocks.parse(&v["state"])?),
            "minecraft:weighted_state_provider" | "minecraft:weighted" => {
                let e = v["entries"].as_array().ok_or("entries")?;
                let items: Vec<(State, i32)> = e.iter().map(|x| Ok((w.blocks.parse(&x["data"])?, x["weight"].as_i64().unwrap_or(1) as i32))).collect::<Result<_, String>>()?;
                let total = items.iter().map(|x| x.1).sum();
                Provider::Weighted(items, total)
            }
            "minecraft:rule_based_state_provider" | "minecraft:rule_based" => {
                let rules = v["rules"].as_array().ok_or("rules")?.iter().map(|r| Ok((Pred::parse(&r["if_true"], w)?, Provider::parse(&r["then"], w)?))).collect::<Result<_, String>>()?;
                let fallback = match v.get("fallback") {
                    Some(f) if !f.is_null() => Some(Box::new(Provider::parse(f, w)?)),
                    _ => None,
                };
                Provider::Rule(rules, fallback)
            }
            "minecraft:rotated_block_provider" | "minecraft:rotated" => {
                Provider::Rotated(Box::new(Provider::parse(&v["state"], w)?), v["direction"].as_str().map(direction).transpose()?)
            }
            "minecraft:randomized_int_state_provider" | "minecraft:randomized_int" => {
                Provider::RandomizedInt(Box::new(Provider::parse(&v["source"], w)?), v["property"].as_str().unwrap_or_default().to_string(), IntP::parse(&v["values"])?)
            }
            "minecraft:noise" => Provider::Noise(provider_noise(&v["noise"], seed), f32_of(&v["scale"]), states("states")?),
            "minecraft:noise_threshold" => Provider::NoiseThreshold(
                provider_noise(&v["noise"], seed), f32_of(&v["scale"]), f32_of(&v["threshold"]), f32_of(&v["high_chance"]),
                w.blocks.parse(&v["default_state"])?, states("low_states")?, states("high_states")?,
            ),
            "minecraft:dual_noise" => {
                let var = v["variety"].as_array().ok_or("variety")?;
                Provider::Dual(
                    (var[0].as_i64().unwrap_or(1) as i32, var[1].as_i64().unwrap_or(1) as i32),
                    provider_noise(&v["slow_noise"], seed), f32_of(&v["slow_scale"]),
                    provider_noise(&v["noise"], seed), f32_of(&v["scale"]), states("states")?,
                )
            }
            t => return Err(format!("state provider {t} is not ported")),
        })
    }

    pub fn state(&self, l: &mut Level, r: &mut WorldgenXoro, p: P) -> State {
        match self {
            Provider::Rule(..) => self.optional(l, r, p).unwrap_or_else(|| l.get(p)),
            _ => self.optional(l, r, p).unwrap(),
        }
    }

    pub fn optional(&self, l: &mut Level, r: &mut WorldgenXoro, p: P) -> Option<State> {
        let b = &l.w.blocks;
        Some(match self {
            Provider::Simple(s) => *s,
            Provider::Weighted(items, total) => {
                let mut sel = r.next_bound(*total);
                let mut out = items[0].0;
                for (s, w) in items {
                    if sel < *w {
                        out = *s;
                        break;
                    }
                    sel -= w;
                }
                out
            }
            Provider::Rule(rules, fallback) => {
                for (pred, then) in rules {
                    if pred.test(l, p) {
                        if let Some(s) = then.optional(l, r, p) {
                            return Some(s);
                        }
                    }
                }
                return fallback.as_ref().and_then(|f| f.optional(l, r, p));
            }
            Provider::Rotated(inner, dir) => {
                let d = dir.unwrap_or_else(|| r.next_bound(6) as usize);
                let s = inner.state(l, r, p);
                let b = &l.w.blocks;
                let axis = ["y", "y", "z", "z", "x", "x"][d];
                let name = ["down", "up", "north", "south", "west", "east"][d];
                let mut s2 = b.with(s, "axis", axis).unwrap_or(s);
                s2 = b.with(s2, "facing", name).unwrap_or(s2);
                if d >= 2 {
                    s2 = b.with(s2, "facing", name).unwrap_or(s2);
                }
                s2
            }
            Provider::Noise(n, scale, states) => {
                let sc = *scale as f64;
                noise_pick(states, n.get(p.0 as f64 * sc, p.1 as f64 * sc, p.2 as f64 * sc))
            }
            Provider::NoiseThreshold(n, scale, threshold, high, default, low, highs) => {
                let sc = *scale as f64;
                let v = n.get(p.0 as f64 * sc, p.1 as f64 * sc, p.2 as f64 * sc) as f64;
                if v < *threshold as f64 {
                    low[r.next_bound(low.len() as i32) as usize]
                } else if r.next_float() < *high {
                    highs[r.next_bound(highs.len() as i32) as usize]
                } else {
                    *default
                }
            }
            Provider::Dual((vmin, vmax), slow, slow_scale, n, scale, states) => {
                let ss = *slow_scale as f64;
                let slow_at = |q: P| slow.get(q.0 as f64 * ss, q.1 as f64 * ss, q.2 as f64 * ss);
                let vn = slow_at(p) as f64;
                // Mth.clampedMap(v, -1, 1, min, max + 1)
                let t = (vn - -1.0) / (1.0 - -1.0);
                let lv = if t < 0.0 { *vmin as f64 } else if t > 1.0 { (*vmax + 1) as f64 } else { *vmin as f64 + t * ((*vmax + 1) as f64 - *vmin as f64) };
                let local: Vec<State> = (0..lv as i32).map(|i| noise_pick(states, slow_at(add(p, (i * 54545, 0, i * 34234))))).collect();
                let sc = *scale as f64;
                noise_pick(&local, n.get(p.0 as f64 * sc, p.1 as f64 * sc, p.2 as f64 * sc))
            }
            Provider::RandomizedInt(src, prop, values) => {
                let s = src.state(l, r, p);
                if l.w.blocks.get(s, prop).is_none() {
                    return Some(s);
                }
                let v = values.sample(r);
                l.w.blocks.with(s, prop, &v.to_string()).unwrap_or(s)
            }
        })
        .map(|s| {
            let _ = b;
            s
        })
    }
}

// ------------------------------------------------------------------------------------------------
// BlockState.canSurvive for the blocks features place (by block class)
// ------------------------------------------------------------------------------------------------

pub fn can_survive(l: &mut Level, s: State, p: P) -> bool {
    let w = l.w;
    let b = &w.blocks;
    let below = rel(p, 0);
    let bs = l.get(below);
    let class = b.class(s);
    let tag = |t: &str, st: State| b.is(st, t);
    match class {
        "SugarCaneBlock" => {
            if b.name(bs) == b.name(s) {
                return true;
            }
            if tag("minecraft:supports_sugar_cane", bs) {
                for &d in &HORIZONTAL {
                    let q = l.get(rel(below, d));
                    let (ty, ..) = b.fluid(q);
                    if (ty == 1 && tag_fluid_water_adjacent()) || tag("minecraft:supports_sugar_cane_adjacently", q) {
                        return true;
                    }
                }
            }
            false
        }
        "CactusBlock" => {
            for &d in &HORIZONTAL {
                let n = l.get(rel(p, d));
                if b.is_solid(n) || b.fluid(n).0 == 2 {
                    return false;
                }
            }
            (b.name(bs) == b.name(s) || tag("minecraft:supports_cactus", bs)) && !b.is_class(l.get(rel(p, 1)), "LiquidBlock") && !liquid(b, l.get(rel(p, 1)))
        }
        "MushroomBlock" => {
            // ponytail: light during generation is taken as dark (no light engine yet)
            tag("minecraft:overrides_mushroom_light_requirement", bs) || b.is_solid_render(bs)
        }
        "SeagrassBlock" => b.face_sturdy(bs, 1) && !tag("minecraft:cannot_support_seagrass", bs),
        "TallSeagrassBlock" => {
            if b.get(s, "half") == Some("upper") {
                return b.name(bs) == b.name(s) && b.get(bs, "half") == Some("lower");
            }
            let (ty, amount, ..) = b.fluid(l.get(p));
            b.face_sturdy(bs, 1) && !tag("minecraft:cannot_support_seagrass", bs) && ty == 1 && amount == 8
        }
        "MangrovePropaguleBlock" => {
            if b.get(s, "hanging") == Some("true") {
                tag("minecraft:supports_hanging_mangrove_propagule", l.get(rel(p, 1)))
            } else {
                tag("minecraft:supports_mangrove_propagule", bs)
            }
        }
        "SnowLayerBlock" => {
            if tag("minecraft:cannot_support_snow_layer", bs) {
                return false;
            }
            tag("minecraft:support_override_snow_layer", bs) || b.collision_face_full(bs, 1) || (b.name(bs) == b.name(s) && b.get(bs, "layers") == Some("8"))
        }
        _ if b.is_class(s, "DoublePlantBlock") && b.get(s, "half") == Some("upper") => b.name(bs) == b.name(s) && b.get(bs, "half") == Some("lower"),
        // VegetationBlock.mayPlaceOn: supports_vegetation (subclasses with their own rules are
        // matched above as they come up)
        _ if b.is_class(s, "VegetationBlock") => tag("minecraft:supports_vegetation", bs),
        // ponytail: other blocks' survival rules are not ported yet; they survive
        _ => true,
    }
}

fn tag_fluid_water_adjacent() -> bool {
    // fluid tag supports_sugar_cane_adjacently: water
    true
}

fn liquid(b: &crate::blocks::Blocks, s: State) -> bool {
    let n = b.name(s);
    n == "minecraft:water" || n == "minecraft:lava"
}

// ------------------------------------------------------------------------------------------------
// Placement modifiers
// ------------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum Placement {
    Count(IntP),
    NoiseThresholdCount(f64, i32, i32),
    NoiseBasedCount(i32, f64, f64),
    Biome,
    InSquare,
    Filter(Pred),
    Heightmap(usize),
    Offset(IntP, IntP, IntP),
    HeightRange(HeightP),
    Rarity(i32),
    SurfaceWaterDepth(i32),
    EnvironmentScan(usize, Pred, Pred, i32),
    RandomChance(f32),
    SurfaceRelative(usize, i32, i32),
    RandomlySelected(Vec<Placement>),
    Cuboid(IntP, IntP, bool, bool),
    Fixed(Vec<P>),
    CountOnEveryLayer(IntP),
}

fn heightmap(s: &str) -> Result<usize, String> {
    Ok(match s {
        "WORLD_SURFACE_WG" => WORLD_SURFACE_WG,
        "OCEAN_FLOOR_WG" => OCEAN_FLOOR_WG,
        "WORLD_SURFACE" => WORLD_SURFACE,
        "OCEAN_FLOOR" => OCEAN_FLOOR,
        "MOTION_BLOCKING" => MOTION_BLOCKING,
        "MOTION_BLOCKING_NO_LEAVES" => MOTION_BLOCKING_NO_LEAVES,
        h => return Err(format!("heightmap {h}")),
    })
}

impl Placement {
    fn parse(v: &Value, w: &World) -> Result<Placement, String> {
        let i = |k: &str| IntP::parse(&v[k]);
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:count" => Placement::Count(i("count")?),
            "minecraft:noise_threshold_count" => Placement::NoiseThresholdCount(v["noise_level"].as_f64().unwrap_or(0.0), v["below_noise"].as_i64().unwrap_or(0) as i32, v["above_noise"].as_i64().unwrap_or(0) as i32),
            "minecraft:noise_based_count" => Placement::NoiseBasedCount(v["noise_to_count_ratio"].as_i64().unwrap_or(0) as i32, v["noise_factor"].as_f64().unwrap_or(1.0), v["noise_offset"].as_f64().unwrap_or(0.0)),
            "minecraft:biome" => Placement::Biome,
            "minecraft:in_square" => Placement::InSquare,
            "minecraft:block_predicate_filter" => Placement::Filter(Pred::parse(&v["predicate"], w)?),
            "minecraft:heightmap" => Placement::Heightmap(heightmap(v["heightmap"].as_str().unwrap_or_default())?),
            "minecraft:offset" => Placement::Offset(i("x")?, i("y")?, i("z")?),
            "minecraft:height_range" => Placement::HeightRange(HeightP::parse(&v["height"], w)?),
            "minecraft:rarity_filter" => Placement::Rarity(v["chance"].as_i64().unwrap_or(1) as i32),
            "minecraft:surface_water_depth_filter" => Placement::SurfaceWaterDepth(v["max_water_depth"].as_i64().unwrap_or(0) as i32),
            "minecraft:environment_scan" => Placement::EnvironmentScan(
                direction(v["direction_of_search"].as_str().unwrap_or("down"))?,
                Pred::parse(&v["target_condition"], w)?,
                if v.get("allowed_search_condition").is_some() { Pred::parse(&v["allowed_search_condition"], w)? } else { Pred::True },
                v["max_steps"].as_i64().unwrap_or(1) as i32,
            ),
            "minecraft:random_chance" => Placement::RandomChance(f32_of(&v["chance"])),
            "minecraft:surface_relative_threshold_filter" => Placement::SurfaceRelative(
                heightmap(v["heightmap"].as_str().unwrap_or_default())?,
                v["min_inclusive"].as_i64().map_or(i32::MIN, |n| n as i32),
                v["max_inclusive"].as_i64().map_or(i32::MAX, |n| n as i32),
            ),
            "minecraft:randomly_selected" => Placement::RandomlySelected(v["placements"].as_array().ok_or("placements")?.iter().map(|x| Placement::parse(x, w)).collect::<Result<_, _>>()?),
            "minecraft:cuboid" => Placement::Cuboid(i("xz_size")?, i("y_size")?, v["include_edges"].as_bool().unwrap_or(true), v["include_interior"].as_bool().unwrap_or(true)),
            "minecraft:fixed_placement" => Placement::Fixed(v["positions"].as_array().map(|a| a.iter().map(vec3).collect()).unwrap_or_default()),
            "minecraft:count_on_every_layer" => Placement::CountOnEveryLayer(i("count")?),
            t => return Err(format!("placement {t} is not ported")),
        })
    }

    fn modify(&self, cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, p: P, out: &mut Vec<P>) {
        let filter = |ok: bool, out: &mut Vec<P>| {
            if ok {
                out.push(p)
            }
        };
        match self {
            Placement::Count(c) => {
                let n = c.sample(r);
                out.extend(std::iter::repeat_n(p, n.max(0) as usize));
            }
            Placement::NoiseThresholdCount(level, below, above) => {
                let n = l.m.info_noise.get2(p.0 as f64 / 200.0, p.2 as f64 / 200.0) as f64;
                let c = if n < *level { *below } else { *above };
                out.extend(std::iter::repeat_n(p, c.max(0) as usize));
            }
            Placement::NoiseBasedCount(ratio, factor, offset) => {
                let n = l.m.info_noise.get2(p.0 as f64 / factor, p.2 as f64 / factor) as f64;
                let c = ((n + offset) * *ratio as f64).ceil() as i32;
                out.extend(std::iter::repeat_n(p, c.max(0) as usize));
            }
            Placement::Biome => {
                let b = l.biome(p);
                let ok = cx.top.as_ref().is_some_and(|f| cx.deco.biome_features[b as usize].contains(f));
                filter(ok, out)
            }
            Placement::InSquare => {
                let x = r.next_bound(16) + p.0;
                let z = r.next_bound(16) + p.2;
                out.push((x, p.1, z));
            }
            Placement::Filter(pred) => filter(pred.test(l, p), out),
            Placement::Heightmap(hm) => {
                let h = l.height(*hm, p.0, p.2);
                if h > l.w.min_y {
                    out.push((p.0, h, p.2));
                }
            }
            Placement::Offset(x, y, z) => {
                let (dx, dy, dz) = (x.sample(r), y.sample(r), z.sample(r));
                out.push(add(p, (dx, dy, dz)));
            }
            Placement::HeightRange(h) => out.push((p.0, h.sample(r), p.2)),
            Placement::Rarity(c) => filter(r.next_float() < 1.0 / *c as f32, out),
            Placement::SurfaceWaterDepth(d) => {
                let floor = l.height(OCEAN_FLOOR, p.0, p.2);
                let surface = l.height(WORLD_SURFACE, p.0, p.2);
                filter(surface - floor <= *d, out)
            }
            Placement::EnvironmentScan(dir, target, allowed, max) => {
                let mut q = p;
                if allowed.test(l, q) {
                    for _ in 0..*max {
                        if target.test(l, q) {
                            out.push(q);
                            return;
                        }
                        q = rel(q, *dir);
                        if !l.inside(q.1) {
                            return;
                        }
                        if !allowed.test(l, q) {
                            break;
                        }
                    }
                    if target.test(l, q) {
                        out.push(q);
                    }
                }
            }
            Placement::RandomChance(c) => filter(r.next_float() < *c, out),
            Placement::SurfaceRelative(hm, lo, hi) => {
                let s = l.height(*hm, p.0, p.2) as i64;
                filter(s + *lo as i64 <= p.1 as i64 && p.1 as i64 <= s + *hi as i64, out)
            }
            Placement::RandomlySelected(v) => {
                let i = r.next_bound(v.len() as i32) as usize;
                v[i].modify(cx, l, r, p, out)
            }
            Placement::Cuboid(xz, ys, edges, interior) => {
                let height = ys.sample(r);
                let width = xz.sample(r);
                let length = xz.sample(r);
                for x in 0..=width {
                    for y in 0..=height {
                        for z in 0..=length {
                            let ok = (*edges || x != 0 && x != width || y != 0 && y != height)
                                && (*edges || z != 0 && z != length || y != 0 && y != height)
                                && (*edges || x != 0 && x != width || z != 0 && z != length)
                                && (*interior || x == 0 || x == width || y == 0 || y == height || z == 0 || z == length);
                            if ok {
                                out.push(add(p, (x, y, z)));
                            }
                        }
                    }
                }
            }
            Placement::Fixed(v) => out.extend(v.iter().copied()),
            // ponytail: count_on_every_layer (nether only in vanilla data) is not ported
            Placement::CountOnEveryLayer(_) => {}
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Features
// ------------------------------------------------------------------------------------------------

pub struct PlacedFeature {
    pub feature: Arc<Feature>,
    pub placement: Vec<Placement>,
}

pub enum Feature {
    NoOp,
    Ore { targets: Vec<(RuleTest, State)>, size: i32, discard: f32, scattered: bool },
    RandomSelector(Vec<(f32, Arc<PlacedFeature>)>, Arc<PlacedFeature>),
    SimpleRandom(Vec<Arc<PlacedFeature>>),
    WeightedRandom(Vec<(Arc<PlacedFeature>, i32)>, i32),
    RandomBoolean(Arc<PlacedFeature>, Arc<PlacedFeature>),
    Sequence(Vec<Arc<PlacedFeature>>),
    Disk { provider: Provider, target: Pred, radius: IntP, half_height: i32 },
    SimpleBlock { to_place: Provider },
    Freeze,
    Geode(Box<crate::geode::Geode>),
    /// A feature type that is not ported yet: places nothing.
    Unported(String),
}

/// Feature and placed feature definitions, parsed on first use.
pub struct Registry {
    placed: Mutex<HashMap<String, Arc<PlacedFeature>>>,
    features: Mutex<HashMap<String, Arc<Feature>>>,
}

impl Registry {
    fn new() -> Self {
        Registry { placed: Mutex::new(HashMap::new()), features: Mutex::new(HashMap::new()) }
    }

    fn file(w: &World, dir: &str, name: &str) -> Result<Value, String> {
        let path = w.dir.join("minecraft/worldgen").join(dir).join(format!("{}.json", ns(name).trim_start_matches("minecraft:")));
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| e.to_string())
    }

    /// A placed feature: a name in placed_feature/ or an inline {"feature", "placement"}.
    pub fn placed(&self, w: &World, v: &Value) -> Result<Arc<PlacedFeature>, String> {
        if let Some(name) = v.as_str() {
            let key = ns(name);
            if let Some(p) = self.placed.lock().unwrap().get(&key) {
                return Ok(p.clone());
            }
            let f = Self::file(w, "placed_feature", &key)?;
            let p = self.placed(w, &f)?;
            self.placed.lock().unwrap().insert(key, p.clone());
            return Ok(p);
        }
        let feature = self.feature(w, &v["feature"])?;
        let placement = v["placement"].as_array().map(|a| a.iter().map(|x| Placement::parse(x, w)).collect::<Result<_, _>>()).transpose()?.unwrap_or_default();
        Ok(Arc::new(PlacedFeature { feature, placement }))
    }

    pub fn feature(&self, w: &World, v: &Value) -> Result<Arc<Feature>, String> {
        if let Some(name) = v.as_str() {
            let key = ns(name);
            if let Some(f) = self.features.lock().unwrap().get(&key) {
                return Ok(f.clone());
            }
            let j = Self::file(w, "feature", &key)?;
            let f = self.feature(w, &j)?;
            self.features.lock().unwrap().insert(key, f.clone());
            return Ok(f);
        }
        let placed_list = |k: &str| -> Result<Vec<Arc<PlacedFeature>>, String> { v[k].as_array().ok_or(format!("{k}"))?.iter().map(|x| self.placed(w, x)).collect() };
        Ok(Arc::new(match v["type"].as_str().unwrap_or_default() {
            "minecraft:no_op" => Feature::NoOp,
            t @ ("minecraft:ore" | "minecraft:scattered_ore") => Feature::Ore {
                targets: v["targets"].as_array().ok_or("targets")?.iter().map(|x| Ok((RuleTest::parse(&x["target"], w)?, w.blocks.parse(&x["state"])?))).collect::<Result<_, String>>()?,
                size: v["size"].as_i64().unwrap_or(0) as i32,
                discard: f32_of(&v["discard_chance_on_air_exposure"]),
                scattered: t == "minecraft:scattered_ore",
            },
            "minecraft:random_selector" => Feature::RandomSelector(
                v["features"].as_array().ok_or("features")?.iter().map(|x| Ok((f32_of(&x["chance"]), self.placed(w, &x["feature"])?))).collect::<Result<_, String>>()?,
                self.placed(w, &v["default"])?,
            ),
            "minecraft:simple_random_selector" => Feature::SimpleRandom(placed_list("features")?),
            "minecraft:weighted_random_selector" => {
                let items: Vec<(Arc<PlacedFeature>, i32)> = v["features"].as_array().ok_or("features")?.iter().map(|x| Ok((self.placed(w, &x["data"])?, x["weight"].as_i64().unwrap_or(1) as i32))).collect::<Result<_, String>>()?;
                let total = items.iter().map(|x| x.1).sum();
                Feature::WeightedRandom(items, total)
            }
            "minecraft:random_boolean_selector" => Feature::RandomBoolean(self.placed(w, &v["feature_true"])?, self.placed(w, &v["feature_false"])?),
            "minecraft:sequence" => Feature::Sequence(placed_list("features")?),
            "minecraft:disk" => Feature::Disk {
                provider: Provider::parse(&v["state_provider"], w)?,
                target: Pred::parse(&v["target"], w)?,
                radius: IntP::parse(&v["radius"])?,
                half_height: v["half_height"].as_i64().unwrap_or(0) as i32,
            },
            "minecraft:simple_block" => Feature::SimpleBlock { to_place: Provider::parse(&v["to_place"], w)? },
            "minecraft:freeze_top_layer" => Feature::Freeze,
            "minecraft:geode" => Feature::Geode(Box::new(crate::geode::Geode::parse(v, w)?)),
            t => Feature::Unported(t.to_string()),
        }))
    }
}

/// The per-world decoration data: the registry, FeatureSorter's order and each biome's features.
pub struct Deco {
    pub reg: Registry,
    /// Per step, the placed feature names in FeatureSorter order.
    pub steps: Vec<Vec<String>>,
    pub index: Vec<HashMap<String, usize>>,
    /// Per biome: per step its placed feature names; and the flattened set (BiomeFilter).
    pub biome_steps: Vec<Vec<Vec<String>>>,
    pub biome_features: Vec<HashSet<String>>,
    /// The biome source's biomes (possibleBiomes), as a table.
    pub source: Vec<bool>,
}

impl Deco {
    pub fn load(w: &World) -> Result<Deco, String> {
        let n = w.biomes.names.len();
        let mut biome_steps = vec![vec![]; n];
        let mut biome_features = vec![HashSet::new(); n];
        for (i, b) in w.biomes.json.iter().enumerate() {
            let steps: Vec<Vec<String>> = b["features"].as_array().map(|a| a.iter().map(|s| s.as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(ns)).collect()).unwrap_or_default()).collect()).unwrap_or_default();
            biome_features[i] = steps.iter().flatten().cloned().collect();
            biome_steps[i] = steps;
        }
        // possibleBiomes: the parameter list's biomes in order (an ImmutableSet keeps it)
        let path = w.dir.join("reports/biome_parameters/minecraft/overworld.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| e.to_string())?;
        let mut order: Vec<u16> = vec![];
        for e in v["biomes"].as_array().ok_or("biome_parameters")? {
            if let Some(id) = e["biome"].as_str().and_then(|b| w.biomes.id(b)) {
                if !order.contains(&id) {
                    order.push(id);
                }
            }
        }
        let mut source = vec![false; n];
        order.iter().for_each(|&b| source[b as usize] = true);
        let steps = sort_features(&order.iter().map(|&b| biome_steps[b as usize].clone()).collect::<Vec<_>>())?;
        let index = steps.iter().map(|s| s.iter().enumerate().map(|(i, f)| (f.clone(), i)).collect()).collect();
        Ok(Deco { reg: Registry::new(), steps, index, biome_steps, biome_features, source })
    }
}

/// FeatureSorter.buildFeaturesPerStep (placed features are identified by name).
fn sort_features(sources: &[Vec<Vec<String>>]) -> Result<Vec<Vec<String>>, String> {
    let mut index: HashMap<String, usize> = HashMap::new();
    // FeatureData ordered by (step, featureIndex)
    type Fd = (usize, usize);
    let mut names: HashMap<Fd, String> = HashMap::new();
    let mut edges: BTreeMap<Fd, BTreeSet<Fd>> = BTreeMap::new();
    let mut max_step = 0;
    for src in sources {
        let mut list: Vec<Fd> = vec![];
        max_step = max_step.max(src.len());
        for (step, fs) in src.iter().enumerate() {
            for f in fs {
                let n = index.len();
                let i = *index.entry(f.clone()).or_insert(n);
                names.insert((step, i), f.clone());
                list.push((step, i));
            }
        }
        for i in 0..list.len() {
            let e = edges.entry(list[i]).or_default();
            if i + 1 < list.len() {
                e.insert(list[i + 1]);
            }
        }
    }
    let mut discovered = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut sorted = vec![];
    fn dfs(e: &BTreeMap<Fd, BTreeSet<Fd>>, disc: &mut BTreeSet<Fd>, vis: &mut BTreeSet<Fd>, out: &mut Vec<Fd>, cur: Fd) -> bool {
        if disc.contains(&cur) {
            return false;
        }
        if vis.contains(&cur) {
            return true;
        }
        vis.insert(cur);
        if let Some(next) = e.get(&cur) {
            for &n in next {
                if dfs(e, disc, vis, out, n) {
                    return true;
                }
            }
        }
        vis.remove(&cur);
        disc.insert(cur);
        out.push(cur);
        false
    }
    let keys: Vec<Fd> = edges.keys().copied().collect();
    for k in keys {
        if !discovered.contains(&k) && dfs(&edges, &mut discovered, &mut visiting, &mut sorted, k) {
            return Err("feature order cycle".into());
        }
    }
    sorted.reverse();
    Ok((0..max_step).map(|s| sorted.iter().filter(|f| f.0 == s).map(|f| names[f].clone()).collect()).collect())
}

impl World {
    pub fn deco(&self) -> Result<&Deco, String> {
        self.deco.get_or_init(|| Deco::load(self)).as_ref().map_err(Clone::clone)
    }
}

/// PlacementContext's feature data.
pub struct Ctx<'d> {
    pub deco: &'d Deco,
    pub top: Option<String>,
}

/// FeaturePlacer.place: the modifiers depth first, each final position placed at once.
pub fn place(cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, pf: &PlacedFeature, origin: P) -> bool {
    if pf.placement.is_empty() {
        return place_feature(cx, l, r, &pf.feature, origin);
    }
    let mut placed = false;
    let mut stack: Vec<(P, usize)> = vec![(origin, 0)];
    let mut out = vec![];
    while let Some((p, i)) = stack.pop() {
        out.clear();
        pf.placement[i].modify(cx, l, r, p, &mut out);
        if i + 1 < pf.placement.len() {
            for &q in out.iter().rev() {
                stack.push((q, i + 1));
            }
        } else {
            for &q in &out.clone() {
                placed |= place_feature(cx, l, r, &pf.feature, q);
            }
        }
    }
    placed
}

/// PlacedFeature.place (no biome check: a new FeaturePlacer without a top feature).
fn place_nested(cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, pf: &PlacedFeature, origin: P) -> bool {
    let top = cx.top.take();
    let ok = place(cx, l, r, pf, origin);
    cx.top = top;
    ok
}

fn place_feature(cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, f: &Feature, p: P) -> bool {
    match f {
        Feature::NoOp => true,
        Feature::Unported(_) => false,
        Feature::Ore { targets, size, discard, scattered } => if *scattered { scattered_ore(l, r, targets, *size, *discard, p) } else { ore(l, r, targets, *size, *discard, p) },
        Feature::RandomSelector(list, default) => {
            for (chance, pf) in list {
                if r.next_float() < *chance {
                    return place_nested(cx, l, r, pf, p);
                }
            }
            place_nested(cx, l, r, default, p)
        }
        Feature::SimpleRandom(list) => {
            let i = r.next_bound(list.len() as i32) as usize;
            place_nested(cx, l, r, &list[i], p)
        }
        Feature::WeightedRandom(list, total) => {
            if list.is_empty() {
                return false;
            }
            let mut sel = r.next_bound(*total);
            for (pf, w) in list {
                if sel < *w {
                    return place_nested(cx, l, r, pf, p);
                }
                sel -= w;
            }
            false
        }
        Feature::RandomBoolean(t, f) => {
            let b = r.next_bool();
            place_nested(cx, l, r, if b { t } else { f }, p)
        }
        Feature::Sequence(list) => {
            for pf in list {
                if !place_nested(cx, l, r, pf, p) {
                    return false;
                }
            }
            true
        }
        Feature::Disk { provider, target, radius, half_height } => disk(l, r, provider, target, radius, *half_height, p),
        Feature::SimpleBlock { to_place } => simple_block(l, r, to_place, p),
        Feature::Freeze => freeze(l, p),
        Feature::Geode(g) => crate::geode::geode(l, r, g, p),
    }
}

// --- ore -------------------------------------------------------------------------------------

fn ore_can_place(l: &Level, r: &mut WorldgenXoro, rule: &RuleTest, s: State, p: P, discard: f32) -> bool {
    if !rule.test(l.w, s, p, r) {
        return false;
    }
    // shouldSkipAirCheck
    let skip = if discard <= 0.0 { true } else if discard >= 1.0 { false } else { r.next_float() >= discard };
    skip || !(0..6).any(|d| l.is_air(rel(p, d)))
}

/// OreFeature.place / doPlace
fn ore(l: &mut Level, r: &mut WorldgenXoro, targets: &[(RuleTest, State)], size: i32, discard: f32, o: P) -> bool {
    use crate::carver::msin;
    let dir = r.next_float() * std::f32::consts::PI;
    let spread = size as f32 / 8.0;
    let max_radius = ((size as f32 / 16.0 * 2.0 + 1.0) / 2.0).ceil() as i32;
    let x0 = o.0 as f64 + (dir as f64).sin() * spread as f64;
    let x1 = o.0 as f64 - (dir as f64).sin() * spread as f64;
    let z0 = o.2 as f64 + (dir as f64).cos() * spread as f64;
    let z1 = o.2 as f64 - (dir as f64).cos() * spread as f64;
    let y0 = (o.1 + r.next_bound(3) - 2) as f64;
    let y1 = (o.1 + r.next_bound(3) - 2) as f64;
    let xs = o.0 - spread.ceil() as i32 - max_radius;
    let ys = o.1 - 2 - max_radius;
    let zs = o.2 - spread.ceil() as i32 - max_radius;
    let sxz = 2 * (spread.ceil() as i32 + max_radius);
    let sy = 2 * (2 + max_radius);
    let mut any = false;
    'probe: for xp in xs..=xs + sxz {
        for zp in zs..=zs + sxz {
            if ys <= l.height(OCEAN_FLOOR_WG, xp, zp) {
                any = true;
                break 'probe;
            }
        }
    }
    if !any {
        return false;
    }
    let n = size as usize;
    let mut data = vec![0.0f64; n * 4];
    for i in 0..n {
        let step = i as f32 / size as f32;
        let xx = x0 + step as f64 * (x1 - x0);
        let yy = y0 + step as f64 * (y1 - y0);
        let zz = z0 + step as f64 * (z1 - z0);
        let ss = r.next_double() * size as f64 / 16.0;
        let rr = (((msin((std::f32::consts::PI * step) as f64) + 1.0) as f64) * ss + 1.0) / 2.0;
        data[i * 4..i * 4 + 4].copy_from_slice(&[xx, yy, zz, rr]);
    }
    for i1 in 0..n.saturating_sub(1) {
        if data[i1 * 4 + 3] <= 0.0 {
            continue;
        }
        for i2 in i1 + 1..n {
            if data[i2 * 4 + 3] <= 0.0 {
                continue;
            }
            let dx = data[i1 * 4] - data[i2 * 4];
            let dy = data[i1 * 4 + 1] - data[i2 * 4 + 1];
            let dz = data[i1 * 4 + 2] - data[i2 * 4 + 2];
            let dr = data[i1 * 4 + 3] - data[i2 * 4 + 3];
            if dr * dr > dx * dx + dy * dy + dz * dz {
                if dr > 0.0 {
                    data[i2 * 4 + 3] = -1.0;
                } else {
                    data[i1 * 4 + 3] = -1.0;
                }
            }
        }
    }
    let mut tested: HashSet<i32> = HashSet::new();
    let mut placed = 0;
    for i in 0..n {
        let rr = data[i * 4 + 3];
        if rr < 0.0 {
            continue;
        }
        let (xx, yy, zz) = (data[i * 4], data[i * 4 + 1], data[i * 4 + 2]);
        let xmin = ((xx - rr).floor() as i32).max(xs);
        let ymin = ((yy - rr).floor() as i32).max(ys);
        let zmin = ((zz - rr).floor() as i32).max(zs);
        let xmax = ((xx + rr).floor() as i32).max(xmin);
        let ymax = ((yy + rr).floor() as i32).max(ymin);
        let zmax = ((zz + rr).floor() as i32).max(zmin);
        for x in xmin..=xmax {
            let xd = (x as f64 + 0.5 - xx) / rr;
            if xd * xd >= 1.0 {
                continue;
            }
            for y in ymin..=ymax {
                let yd = (y as f64 + 0.5 - yy) / rr;
                if xd * xd + yd * yd >= 1.0 {
                    continue;
                }
                for z in zmin..=zmax {
                    let zd = (z as f64 + 0.5 - zz) / rr;
                    if !(xd * xd + yd * yd + zd * zd < 1.0 && l.inside(y)) {
                        continue;
                    }
                    let bit = x - xs + (y - ys) * sxz + (z - zs) * sxz * sy;
                    if !tested.insert(bit) {
                        continue;
                    }
                    let p = (x, y, z);
                    if !l.writable(p) {
                        continue;
                    }
                    let s = l.get(p);
                    for (rule, state) in targets {
                        if ore_can_place(l, r, rule, s, p, discard) {
                            l.set_raw(p, *state);
                            placed += 1;
                            break;
                        }
                    }
                }
            }
        }
    }
    placed > 0
}

/// ScatteredOreFeature.place
fn scattered_ore(l: &mut Level, r: &mut WorldgenXoro, targets: &[(RuleTest, State)], size: i32, discard: f32, o: P) -> bool {
    let tries = r.next_bound(size + 1);
    for i in 0..tries {
        let m = i.min(7);
        let off = |r: &mut WorldgenXoro| crate::chunk::java_round_f((r.next_float() - r.next_float()) * m as f32);
        let (dx, dy, dz) = (off(r), off(r), off(r));
        let p = add(o, (dx, dy, dz));
        let s = l.get(p);
        for (rule, state) in targets {
            if ore_can_place(l, r, rule, s, p, discard) {
                l.set(p, *state);
                break;
            }
        }
    }
    true
}

// --- disk, simple block, freeze ------------------------------------------------------------------

fn mark_above(l: &mut Level, p: P) {
    let mut q = p;
    for _ in 0..2 {
        q = rel(q, 1);
        if l.is_air(q) {
            return;
        }
        l.mark(q);
    }
}

fn disk(l: &mut Level, r: &mut WorldgenXoro, provider: &Provider, target: &Pred, radius: &IntP, half: i32, o: P) -> bool {
    let mut any = false;
    let (top, bottom) = (o.1 + half, o.1 - half - 1);
    let rad = radius.sample(r);
    // BlockPos.betweenClosed: x fastest, then y, then z
    for z in o.2 - rad..=o.2 + rad {
        for x in o.0 - rad..=o.0 + rad {
            let (xd, zd) = (x - o.0, z - o.2);
            if xd * xd + zd * zd > rad * rad {
                continue;
            }
            let mut above = false;
            let mut y = top;
            while y > bottom {
                let p = (x, y, z);
                if target.test(l, p) {
                    if let Some(s) = provider.optional(l, r, p) {
                        l.set(p, s);
                        if !above {
                            mark_above(l, p);
                        }
                        any = true;
                        above = true;
                    }
                } else {
                    above = false;
                }
                y -= 1;
            }
        }
    }
    any
}

fn simple_block(l: &mut Level, r: &mut WorldgenXoro, to_place: &Provider, p: P) -> bool {
    let Some(s) = to_place.optional(l, r, p) else { return false };
    if !can_survive(l, s, p) {
        return false;
    }
    let b = &l.w.blocks;
    if b.is_class(s, "DoublePlantBlock") {
        let above = rel(p, 1);
        let a = l.get(above);
        if !b.is_air(a) && (b.fluid(s) != b.fluid(a) || !b.can_be_replaced(a)) {
            return false;
        }
        // DoublePlantBlock.placeAt, copying waterlogged from the water there
        let lower = b.with(s, "half", "lower").unwrap_or(s);
        let upper = b.with(s, "half", "upper").unwrap_or(s);
        let lw = waterlog(l, lower, p);
        let uw = waterlog(l, upper, above);
        l.set(p, lw);
        l.set(above, uw);
    } else {
        // ponytail: mossy carpets (pale garden) are placed as a plain block
        l.set(p, s);
    }
    true
}

fn waterlog(l: &Level, s: State, p: P) -> State {
    let b = &l.w.blocks;
    if b.get(s, "waterlogged").is_some() {
        let water = b.fluid(l.get(p)).0 == 1 && b.fluid(l.get(p)).2;
        b.with(s, "waterlogged", if water { "true" } else { "false" }).unwrap_or(s)
    } else {
        s
    }
}

/// SnowAndFreezeFeature.place
fn freeze(l: &mut Level, o: P) -> bool {
    let (ice, snow) = (l.w.blocks.state("ice").unwrap(), l.w.blocks.state("snow").unwrap());
    for dx in 0..16 {
        for dz in 0..16 {
            let (x, z) = (o.0 + dx, o.2 + dz);
            let y = l.height(MOTION_BLOCKING, x, z);
            let top = (x, y, z);
            let below = (x, y - 1, z);
            let biome = l.biome(top);
            // Biome.shouldFreeze (no neighbour check): cold enough, water source block there
            // ponytail: block light during generation is taken as 0
            let t_below = l.m.temperature(l.w, biome, below.0, below.1, below.2);
            if t_below < 0.15 && l.inside(below.1) {
                let s = l.get(below);
                let (ty, _, source, _) = l.w.blocks.fluid(s);
                if ty == 1 && source && l.w.blocks.name(s) == "minecraft:water" {
                    l.set(below, ice);
                }
            }
            // Biome.shouldSnow: precipitation and air (or snow) with a snow layer surviving
            let has_precip = l.w.biomes.json[biome as usize]["has_precipitation"].as_bool().unwrap_or(true);
            if has_precip && l.m.temperature(l.w, biome, top.0, top.1, top.2) < 0.15 && l.inside(top.1) {
                let s = l.get(top);
                if (l.w.blocks.is_air(s) || l.w.blocks.name(s) == "minecraft:snow") && can_survive(l, snow, top) {
                    l.set(top, snow);
                    let bs = l.get(below);
                    if l.w.blocks.get(bs, "snowy").is_some() {
                        let snowy = l.w.blocks.with(bs, "snowy", "true").unwrap();
                        l.set(below, snowy);
                    }
                }
            }
        }
    }
    true
}

// ------------------------------------------------------------------------------------------------
// ChunkGenerator.applyBiomeDecoration
// ------------------------------------------------------------------------------------------------

/// Decorates chunk `l.centre` (its features; structures come with the structure pieces).
pub fn decorate(l: &mut Level, seed: i64) -> Result<(), String> {
    let w = l.w;
    let deco = w.deco()?;
    let (cx, cz) = l.centre;
    let origin = (cx * 16, w.min_y, cz * 16);
    let mut r = WorldgenXoro::new(0);
    let dseed = r.set_decoration_seed(seed, origin.0, origin.2);
    // the biomes in the 3x3 chunks' palettes, of the biome source
    let mut possible: BTreeSet<u16> = BTreeSet::new();
    for dz in -1..=1 {
        for dx in -1..=1 {
            for b in l.s.stored_biomes(cx + dx, cz + dz) {
                if deco.source[b as usize] {
                    possible.insert(b);
                }
            }
        }
    }
    let mut ctx = Ctx { deco, top: None };
    for step in 0..deco.steps.len() {
        let mut idx: Vec<usize> = vec![];
        for &b in &possible {
            if let Some(fs) = deco.biome_steps[b as usize].get(step) {
                for f in fs {
                    if let Some(&i) = deco.index[step].get(f) {
                        idx.push(i);
                    }
                }
            }
        }
        idx.sort_unstable();
        idx.dedup();
        for i in idx {
            let name = &deco.steps[step][i];
            r.set_feature_seed(dseed, i as i32, step as i32);
            let pf = deco.reg.placed(w, &Value::String(name.clone()))?;
            ctx.top = Some(name.clone());
            place(&mut ctx, l, &mut r, &pf, origin);
        }
    }
    Ok(())
}
