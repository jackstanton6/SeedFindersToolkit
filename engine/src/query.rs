//! Query conditions with a numeric core, and their per-seed evaluation.
//!
//! Every metric yields a number (a count, distance, area, height, ...). As a condition it passes when
//! `min <= value <= max`; inside `maximize(...)` or a comparison it is used as a number. Structure
//! metrics also have a cheap 48-bit pre-check (`pre`): placement only, no biomes or terrain.

use crate::biome::{contains, BiomeSet};
use crate::rng::{JavaRandom, Rng};
use crate::search::Native;
use crate::structure::{Hit, Placement};
use crate::world::{BiomeCtx, Block, Heightmap, Seeded, World};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::CString;

pub const CLIMATE_PARAMS: [&str; 6] = ["temperature", "humidity", "continentalness", "erosion", "depth", "weirdness"];

/// Which structures a metric looks at: a set, optionally one structure of it, its start biome and
/// piece tags (prefix match, e.g. "template:shipwreck/rightsideup", "basement", "chest:").
#[derive(Clone, Debug)]
pub struct Sel {
    pub set: usize,
    pub only: Option<usize>,
    pub biomes: Option<BiomeSet>,
    pub tags: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Terrain {
    Peak,
    Trench,
    Cliff,
    Overhang,
    Void,
    Lake(Block),
}

#[derive(Clone, Debug)]
pub enum Metric {
    Count { s: Sel, at: (i32, i32), within: i32 },
    Nearest { s: Sel, at: (i32, i32), within: i32 },
    Near { a: Sel, b: Sel, dist: i32, at: (i32, i32), within: i32 },
    Cluster { sels: Vec<Sel>, dist: i32, at: (i32, i32), within: i32, afk: bool },
    Chain { sels: Vec<Sel>, dist: i32, at: (i32, i32), within: i32 },
    Overlap { a: Sel, b: Sel, at: (i32, i32), within: i32, margin: i32 },
    Bingo { sels: Vec<Sel>, at: (i32, i32), within: i32 },
    Biome { biomes: BiomeSet, at: (i32, i32), within: i32, y: i32, step: i32 },
    BiomeArea { biomes: BiomeSet, at: (i32, i32), within: i32, window: i32, y: i32, step: i32 },
    BiomeCount { among: Option<BiomeSet>, at: (i32, i32), within: i32, y: i32, step: i32 },
    BiomeBorder { a: BiomeSet, b: BiomeSet, at: (i32, i32), within: i32, y: i32, step: i32 },
    Island { biomes: Option<BiomeSet>, at: (i32, i32), within: i32, window: i32, y: i32, step: i32, min_circ: f64, max_elong: f64, lagoon: bool },
    Climate { param: usize, pos: (i32, i32, i32) },
    Height { at: (i32, i32), ocean_floor: bool },
    Terrain { kind: Terrain, at: (i32, i32), within: i32, step: i32 },
    Slime { radius: i32, at: (i32, i32), within: i32 },
    /// `item` in the chests of one structure (the best one), or of all of them with `total`.
    Loot { s: Sel, item: String, enchant: Option<String>, at: (i32, i32), within: i32, total: bool },
    /// Best ruined portal: plain obsidian in its most complete frame (`frame`), or in the whole portal.
    Portal { at: (i32, i32), within: i32, frame: bool },
    Native { lib: usize, name: CString, params: CString },
}

#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Metric(Box<Metric>),
    Bin(char, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum Cond {
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Not(Box<Cond>),
    Metric { m: Metric, min: f64, max: f64 },
    Cmp { e: Expr, min: f64, max: f64 },
    Objective { e: Expr, maximize: bool },
    /// `c` holds with its coordinates moved to some anchor within `within` of `at`: grid points
    /// `step` apart, or the starts of `anchor` structures.
    Anywhere { c: Box<Cond>, at: (i32, i32), within: i32, step: i32, anchor: Option<Sel> },
}

// ------------------------------------------------------------------------------------------------
// Parsing (JSON from sft/query.py)
// ------------------------------------------------------------------------------------------------

fn pair(v: &Value, k: &str, default: (i32, i32)) -> (i32, i32) {
    v.get(k).and_then(Value::as_array).map_or(default, |a| (a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32))
}
fn int(v: &Value, k: &str, default: i64) -> i32 {
    v.get(k).and_then(Value::as_i64).unwrap_or(default) as i32
}
fn flt(v: &Value, k: &str, default: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(default)
}

struct Parser<'a> {
    w: &'a World,
    natives: &'a [String],
}

impl Parser<'_> {
    fn biomes(&self, v: &Value) -> Result<BiomeSet, String> {
        let names: Vec<&str> = match v {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => return Err("expected a biome name, '#tag' or a list".into()),
        };
        let mut set = 0;
        for n in names {
            set |= match n.strip_prefix('#') {
                Some(tag) => self.w.biomes.tag(&crate::density::ns(tag)),
                None => 1u128 << self.w.biomes.id(n).ok_or(format!("unknown biome '{n}'"))?,
            };
        }
        Ok(set)
    }

    /// A structure selector; `biome` and `tags` come from the enclosing node.
    fn sel(&self, name: &Value, v: &Value) -> Result<Sel, String> {
        let n = name.as_str().ok_or("structure name must be a string")?;
        let (set, only) = self.w.structures.resolve(n).ok_or(format!("unknown structure '{n}'"))?;
        Ok(Sel {
            set,
            only,
            biomes: v.get("biome").map(|b| self.biomes(b)).transpose()?,
            tags: v.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default(),
        })
    }

    fn sels(&self, v: &Value, k: &str) -> Result<Vec<Sel>, String> {
        match &v[k] {
            Value::Array(a) => a.iter().map(|n| self.sel(n, v)).collect(),
            Value::Null => Err(format!("{} needs '{k}'", v["op"])),
            one => Ok(vec![self.sel(one, v)?]),
        }
    }

    /// (metric, default min, default max) for a metric node.
    fn metric(&self, v: &Value) -> Result<(Metric, f64, f64), String> {
        let op = v["op"].as_str().unwrap_or_default();
        let at = pair(v, "at", (0, 0));
        let within = int(v, "within", 0);
        let (y, step) = (int(v, "y", 64), int(v, "step", 16).max(4));
        let inf = f64::INFINITY;
        Ok(match op {
            "structure" => (Metric::Count { s: self.sel(&v["name"], v)?, at, within: int(v, "within", 1000) }, 1.0, inf),
            "nearest" => {
                let within = int(v, "within", 3000);
                (Metric::Nearest { s: self.sel(&v["name"], v)?, at, within }, 0.0, within as f64)
            }
            "near" => (Metric::Near { a: self.sel(&v["a"], v)?, b: self.sel(&v["b"], v)?, dist: int(v, "dist", 128), at, within: int(v, "within", 1000) }, 1.0, inf),
            "cluster" => {
                let m = Metric::Cluster { sels: self.sels(v, "names")?, dist: int(v, "dist", 128), at, within: int(v, "within", 1000), afk: v["afk"].as_bool().unwrap_or(false) };
                (m, int(v, "count", 2) as f64, inf)
            }
            "chain" => {
                let sels = self.sels(v, "names")?;
                let n = sels.len() as f64;
                (Metric::Chain { sels, dist: int(v, "dist", 256), at, within: int(v, "within", 1000) }, n, inf)
            }
            "overlap" => (Metric::Overlap { a: self.sel(&v["a"], v)?, b: self.sel(&v["b"], v)?, at, within: int(v, "within", 1000), margin: int(v, "margin", 0) }, 1.0, inf),
            "bingo" => {
                let sels = if v.get("names").is_some() {
                    self.sels(v, "names")?
                } else {
                    let st = &self.w.structures;
                    (0..st.sets.len()).filter(|&i| st.sets[i].possible).map(|set| Sel { set, only: None, biomes: None, tags: vec![] }).collect()
                };
                let n = sels.len() as f64;
                (Metric::Bingo { sels, at, within: int(v, "within", 2000) }, n, inf)
            }
            "biome" => {
                let fraction = flt(v, "fraction", 0.0);
                (Metric::Biome { biomes: self.biomes(&v["name"])?, at, within, y, step }, if fraction > 0.0 { fraction } else { f64::MIN_POSITIVE }, inf)
            }
            "biome_area" => (Metric::BiomeArea { biomes: self.biomes(&v["name"])?, at, within, window: int(v, "window", 512), y, step }, 1.0, inf),
            "biome_count" => {
                let among = v.get("among").map(|b| self.biomes(b)).transpose()?;
                let all = (among.unwrap_or(u128::MAX) & self.w.climate.possible()).count_ones() as f64;
                (Metric::BiomeCount { among, at, within, y, step }, if v["min"].as_str() == Some("all") { all } else { 1.0 }, inf)
            }
            "biome_border" => (Metric::BiomeBorder { a: self.biomes(&v["a"])?, b: self.biomes(&v["b"])?, at, within, y, step }, 1.0, inf),
            "island" => (
                Metric::Island {
                    biomes: v.get("biomes").map(|b| self.biomes(b)).transpose()?,
                    at,
                    within,
                    window: int(v, "window", 1024),
                    y: int(v, "y", 63),
                    step,
                    min_circ: flt(v, "min_circularity", 0.0),
                    max_elong: flt(v, "max_elongation", inf),
                    lagoon: v["lagoon"].as_bool().unwrap_or(false),
                },
                1.0,
                inf,
            ),
            "climate" => (
                Metric::Climate {
                    param: CLIMATE_PARAMS.iter().position(|p| Some(*p) == v["param"].as_str()).ok_or(format!("climate param must be one of {CLIMATE_PARAMS:?}"))?,
                    pos: (at.0, y, at.1),
                },
                -inf,
                inf,
            ),
            "height" => (Metric::Height { at, ocean_floor: v.get("heightmap").and_then(Value::as_str) == Some("ocean_floor") }, -inf, inf),
            "peak" | "trench" | "cliff" | "overhang" | "void" | "lake" => {
                let kind = match op {
                    "peak" => Terrain::Peak,
                    "trench" => Terrain::Trench,
                    "cliff" => Terrain::Cliff,
                    "overhang" => Terrain::Overhang,
                    "void" => Terrain::Void,
                    _ => Terrain::Lake(if v["fluid"].as_str() == Some("water") { Block::Water } else { Block::Lava }),
                };
                let min = if matches!(kind, Terrain::Peak | Terrain::Trench) { -inf } else { 1.0 };
                (Metric::Terrain { kind, at, within, step: int(v, "step", 32).max(1) }, min, inf)
            }
            "slime" => (Metric::Slime { radius: int(v, "radius", 128), at, within }, 1.0, inf),
            "loot" => {
                let item = crate::density::ns(v["item"].as_str().ok_or("loot needs an item")?);
                let enchant = v.get("enchant").and_then(Value::as_str).map(crate::density::ns);
                let m = Metric::Loot { s: self.sel(&v["name"], v)?, item, enchant, at, within: int(v, "within", 1000), total: v["total"].as_bool().unwrap_or(false) };
                (m, 1.0, inf)
            }
            "portal_frame" | "portal_obsidian" => {
                let frame = op == "portal_frame";
                (Metric::Portal { at, within: int(v, "within", 1000), frame }, if frame { 10.0 } else { 1.0 }, inf)
            }
            "native" => {
                let plugin = v["plugin"].as_str().ok_or("native needs 'plugin'")?;
                (
                    Metric::Native {
                        lib: self.natives.iter().position(|p| p == plugin).ok_or(format!("native plugin '{plugin}' not loaded"))?,
                        name: CString::new(v["filter"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?,
                        params: CString::new(v.get("params").map(|p| p.to_string()).unwrap_or_else(|| "{}".into())).map_err(|e| e.to_string())?,
                    },
                    1.0,
                    inf,
                )
            }
            other => return Err(format!("unknown condition '{other}'")),
        })
    }

    fn expr(&self, v: &Value) -> Result<Expr, String> {
        Ok(match v["op"].as_str().unwrap_or_default() {
            "num" => Expr::Num(flt(v, "value", 0.0)),
            "neg" => Expr::Neg(Box::new(self.expr(&v["a"])?)),
            o @ ("add" | "sub" | "mul" | "div") => {
                let c = match o {
                    "add" => '+',
                    "sub" => '-',
                    "mul" => '*',
                    _ => '/',
                };
                Expr::Bin(c, Box::new(self.expr(&v["a"])?), Box::new(self.expr(&v["b"])?))
            }
            _ => Expr::Metric(Box::new(self.metric(v)?.0)),
        })
    }

    fn cond(&self, v: &Value) -> Result<Cond, String> {
        let op = v["op"].as_str().ok_or_else(|| format!("condition without op: {v}"))?;
        let kids = |k: &str| -> Result<Vec<Cond>, String> { v[k].as_array().ok_or(format!("{op} needs '{k}'"))?.iter().map(|c| self.cond(c)).collect() };
        let bound = |k: &str, d: f64| v.get(k).and_then(Value::as_f64).unwrap_or(d);
        Ok(match op {
            "and" => Cond::And(kids("args")?),
            "or" => Cond::Or(kids("args")?),
            "not" => Cond::Not(Box::new(self.cond(&v["arg"])?)),
            "cmp" => Cond::Cmp { e: self.expr(&v["expr"])?, min: bound("min", f64::NEG_INFINITY), max: bound("max", f64::INFINITY) },
            "maximize" | "minimize" => Cond::Objective { e: self.expr(&v["expr"])?, maximize: op == "maximize" },
            "anywhere" => Cond::Anywhere {
                c: Box::new(self.cond(&v["cond"])?),
                at: pair(v, "at", (0, 0)),
                within: int(v, "within", 1000),
                step: int(v, "step", 64).max(1),
                anchor: v.get("anchor").filter(|a| a.is_string()).map(|a| self.sel(a, &Value::Null)).transpose()?,
            },
            _ => {
                let (m, dmin, dmax) = self.metric(v)?;
                Cond::Metric { m, min: bound("min", dmin), max: bound("max", dmax) }
            }
        })
    }
}

/// Parses the JSON condition tree produced by sft/query.py.
pub fn parse(w: &World, natives: &[String], v: &Value) -> Result<Cond, String> {
    Parser { w, natives }.cond(v)
}

/// Parses a structure selector on its own (`{"name", "biome"?, "tags"?}`), e.g. for scan anchors.
pub fn parse_sel(w: &World, v: &Value) -> Result<Sel, String> {
    Parser { w, natives: &[] }.sel(&v["name"], v)
}

// ------------------------------------------------------------------------------------------------
// Moving a query to another anchor (anywhere(), single-seed scans)
// ------------------------------------------------------------------------------------------------

fn mv(p: &mut (i32, i32), d: (i32, i32)) {
    p.0 += d.0;
    p.1 += d.1;
}

impl Metric {
    fn shift(&mut self, d: (i32, i32)) {
        match self {
            Metric::Count { at, .. }
            | Metric::Nearest { at, .. }
            | Metric::Near { at, .. }
            | Metric::Cluster { at, .. }
            | Metric::Chain { at, .. }
            | Metric::Overlap { at, .. }
            | Metric::Bingo { at, .. }
            | Metric::Biome { at, .. }
            | Metric::BiomeArea { at, .. }
            | Metric::BiomeCount { at, .. }
            | Metric::BiomeBorder { at, .. }
            | Metric::Island { at, .. }
            | Metric::Height { at, .. }
            | Metric::Terrain { at, .. }
            | Metric::Slime { at, .. }
            | Metric::Loot { at, .. }
            | Metric::Portal { at, .. } => mv(at, d),
            Metric::Climate { pos, .. } => {
                pos.0 += d.0;
                pos.2 += d.1;
            }
            Metric::Native { .. } => {}
        }
    }
}

impl Expr {
    fn shift(&mut self, d: (i32, i32)) {
        match self {
            Expr::Num(_) => {}
            Expr::Metric(m) => m.shift(d),
            Expr::Bin(_, a, b) => {
                a.shift(d);
                b.shift(d);
            }
            Expr::Neg(a) => a.shift(d),
        }
    }
}

impl Cond {
    /// The same query with every coordinate moved by `d`.
    pub fn shifted(&self, d: (i32, i32)) -> Cond {
        let mut c = self.clone();
        c.shift(d);
        c
    }

    fn shift(&mut self, d: (i32, i32)) {
        match self {
            Cond::And(v) | Cond::Or(v) => v.iter_mut().for_each(|c| c.shift(d)),
            Cond::Not(c) => c.shift(d),
            Cond::Metric { m, .. } => m.shift(d),
            Cond::Cmp { e, .. } | Cond::Objective { e, .. } => e.shift(d),
            Cond::Anywhere { at, .. } => mv(at, d),
        }
    }

    pub fn has_objective(&self) -> bool {
        match self {
            Cond::And(v) | Cond::Or(v) => v.iter().any(Cond::has_objective),
            Cond::Not(c) => c.has_objective(),
            Cond::Objective { .. } => true,
            _ => false,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Geometry helpers
// ------------------------------------------------------------------------------------------------

fn dist2(a: (i32, i32), b: (i32, i32)) -> i64 {
    let (dx, dz) = ((a.0 - b.0) as i64, (a.1 - b.1) as i64);
    dx * dx + dz * dz
}

/// Locate position of a set's start chunk (chunk min corner + locate_offset), as /locate reports.
pub fn locate(w: &World, set: usize, c: (i32, i32)) -> (i32, i32) {
    let o = w.structures.sets[set].locate_offset;
    (c.0 * 16 + o.0, c.1 * 16 + o.2)
}

/// Placement chunks of a random-spread set whose locate position is within `r` of `at`.
/// 48-bit only: no biome or terrain work.
pub fn potential(w: &World, set: usize, seed: i64, at: (i32, i32), r: i32) -> Vec<(i32, i32)> {
    let st = &w.structures;
    let s = &st.sets[set];
    let Placement::Spread { spacing, .. } = s.placement else { return vec![] };
    let (ox, oz) = (s.locate_offset.0, s.locate_offset.2);
    let (gx0, gx1) = ((at.0 - r - ox).div_euclid(16).div_euclid(spacing), (at.0 + r - ox).div_euclid(16).div_euclid(spacing));
    let (gz0, gz1) = ((at.1 - r - oz).div_euclid(16).div_euclid(spacing), (at.1 + r - oz).div_euclid(16).div_euclid(spacing));
    let mut out = vec![];
    for gx in gx0..=gx1 {
        for gz in gz0..=gz1 {
            let c = st.grid_chunk(set, seed, gx, gz).unwrap();
            if dist2(locate(w, set, c), at) <= (r as i64).pow(2) && st.is_structure_chunk(set, seed, c.0, c.1) {
                out.push(c);
            }
        }
    }
    out
}

/// Stronghold positions needed to cover `r` blocks from the origin: ring c starts at (88 + 192c)
/// chunks.
fn rings_needed(r: i32) -> usize {
    let sizes = [3, 6, 10, 15, 21, 28, 36, 9];
    let mut n = 0;
    for (c, s) in sizes.iter().enumerate() {
        if (88 + 192 * c as i32) * 16 - 112 > r {
            break;
        }
        n += s;
    }
    n
}

/// WorldgenRandom.seedSlimeChunk(x, z, seed, 987234911).nextInt(10) == 0, with Java's int
/// overflow in the products that are computed as int.
pub fn is_slime_chunk(seed: i64, x: i32, z: i32) -> bool {
    let s = seed
        .wrapping_add(x.wrapping_mul(x).wrapping_mul(4987142) as i64)
        .wrapping_add(x.wrapping_mul(5947611) as i64)
        .wrapping_add((z.wrapping_mul(z) as i64).wrapping_mul(4392871))
        .wrapping_add(z.wrapping_mul(389711) as i64)
        ^ 987234911;
    JavaRandom::new(s).next_bound(10) == 0
}

/// Slime chunks whose centre is within `radius` of `c`.
fn slime_count(seed: i64, c: (i32, i32), radius: i32) -> i32 {
    let mut n = 0;
    for x in (c.0 - radius) >> 4..=(c.0 + radius) >> 4 {
        for z in (c.1 - radius) >> 4..=(c.1 + radius) >> 4 {
            if dist2((x * 16 + 8, z * 16 + 8), c) <= (radius as i64).pow(2) && is_slime_chunk(seed, x, z) {
                n += 1;
            }
        }
    }
    n
}

/// Points a structure covers for cluster geometry: its box corners (afk), else its start.
fn footprint(h: &Hit, afk: bool) -> Vec<(i32, i32)> {
    match h.bbox {
        Some(b) if afk => vec![(b[0], b[2]), (b[3], b[2]), (b[0], b[5]), (b[3], b[5])],
        _ => vec![(h.pos.0, h.pos.2)],
    }
}

/// Most groups (each all of its points) a circle of radius `r` can cover, with its centre within
/// `within` of `at`, and which groups. The feasible centres for any subset form a convex region
/// whose leftmost point is a circle-circle intersection or the leftmost point of one circle, so
/// those are the only candidates. Points and groups are bucketed in cells of 2r, so only nearby
/// pairs are tried; candidates and members come out in the same order as an all-pairs scan.
/// Stops early once `enough` groups are covered.
fn max_cover(groups: &[Vec<(i32, i32)>], r: i32, at: (i32, i32), within: i32, enough: usize) -> (usize, Vec<usize>) {
    let mut circles: Vec<(f64, f64, f64)> = groups.iter().flatten().map(|p| (p.0 as f64, p.1 as f64, r as f64)).collect();
    let pts = Buckets::new(circles.iter().map(|c| (c.0, c.1)), 2 * r);
    let firsts = Buckets::new(groups.iter().map(|g| (g[0].0 as f64, g[0].1 as f64)), 2 * r);
    let last = circles.len();
    circles.push((at.0 as f64, at.1 as f64, within as f64));
    let mut cands: Vec<(f64, f64)> = circles.iter().map(|c| (c.0 - c.2, c.1)).collect();
    for i in 0..last {
        // circles farther than 2r apart do not intersect; the `at` circle pairs with every point
        let near = pts.near((circles[i].0, circles[i].1));
        for j in near.into_iter().filter(|&j| j > i).chain([last]) {
            let ((x0, z0, r0), (x1, z1, r1)) = (circles[i], circles[j]);
            let d = ((x1 - x0).powi(2) + (z1 - z0).powi(2)).sqrt();
            if d == 0.0 || d > r0 + r1 || d < (r0 - r1).abs() {
                continue;
            }
            let a = (r0 * r0 - r1 * r1 + d * d) / (2.0 * d);
            let h = (r0 * r0 - a * a).max(0.0).sqrt();
            let (mx, mz) = (x0 + a * (x1 - x0) / d, z0 + a * (z1 - z0) / d);
            cands.push((mx - h * (z1 - z0) / d, mz + h * (x1 - x0) / d));
            cands.push((mx + h * (z1 - z0) / d, mz - h * (x1 - x0) / d));
        }
    }
    let inside = |c: (f64, f64), p: (f64, f64), r: f64| (p.0 - c.0).powi(2) + (p.1 - c.1).powi(2) <= r * r + 1e-6;
    let mut best = (0, vec![]);
    for c in cands {
        if !inside(c, (at.0 as f64, at.1 as f64), within as f64) {
            continue;
        }
        let members: Vec<usize> = firsts.near(c).into_iter().filter(|&g| groups[g].iter().all(|p| inside(c, (p.0 as f64, p.1 as f64), r as f64))).collect();
        if members.len() > best.0 {
            best = (members.len(), members);
            if best.0 >= enough {
                break;
            }
        }
    }
    best
}

/// Point indices bucketed by square cells: everything within `cell` of a point is in the 3x3
/// cells around it.
struct Buckets {
    cell: f64,
    map: HashMap<(i64, i64), Vec<usize>>,
}

impl Buckets {
    fn new(pts: impl Iterator<Item = (f64, f64)>, cell: i32) -> Self {
        let mut b = Buckets { cell: cell.max(1) as f64, map: HashMap::new() };
        for (i, p) in pts.enumerate() {
            let k = b.key(p);
            b.map.entry(k).or_default().push(i);
        }
        b
    }

    fn key(&self, p: (f64, f64)) -> (i64, i64) {
        ((p.0 / self.cell).floor() as i64, (p.1 / self.cell).floor() as i64)
    }

    /// Indices in the 3x3 cells around `p`, ascending.
    fn near(&self, p: (f64, f64)) -> Vec<usize> {
        let (kx, kz) = self.key(p);
        let mut v: Vec<usize> = (-1..=1).flat_map(|i| (-1..=1).map(move |j| (kx + i, kz + j))).filter_map(|k| self.map.get(&k)).flatten().copied().collect();
        v.sort_unstable();
        v
    }
}

fn boxes_overlap(a: &Hit, b: &Hit, margin: i32) -> bool {
    let bx = |h: &Hit| h.bbox.unwrap_or([h.pos.0, 0, h.pos.2, h.pos.0, 0, h.pos.2]);
    let (p, q) = (bx(a), bx(b));
    p[0] - margin <= q[3] && q[0] - margin <= p[3] && p[2] - margin <= q[5] && q[2] - margin <= p[5]
}

/// Square grid of point-path biomes, cells `step` apart, centred on the query point.
struct Grid {
    n: i32,
    x0: i32,
    z0: i32,
    step: i32,
    ids: Vec<u16>,
}

impl Grid {
    fn pos(&self, k: usize) -> (i32, i32) {
        (self.x0 + (k as i32 % self.n) * self.step, self.z0 + (k as i32 / self.n) * self.step)
    }
    fn neighbours(&self, k: usize) -> impl Iterator<Item = usize> + '_ {
        let (i, j) = (k as i32 % self.n, k as i32 / self.n);
        [(i - 1, j), (i + 1, j), (i, j - 1), (i, j + 1)].into_iter().filter(|&(a, b)| a >= 0 && b >= 0 && a < self.n && b < self.n).map(|(a, b)| (b * self.n + a) as usize)
    }
    fn on_border(&self, k: usize) -> bool {
        let (i, j) = (k as i32 % self.n, k as i32 / self.n);
        i == 0 || j == 0 || i == self.n - 1 || j == self.n - 1
    }
    /// 4-connected components of the cells where `mask` is true.
    fn components(&self, mask: &[bool]) -> Vec<Vec<usize>> {
        let mut seen = vec![false; mask.len()];
        let mut out = vec![];
        for start in 0..mask.len() {
            if !mask[start] || seen[start] {
                continue;
            }
            let (mut comp, mut stack) = (vec![], vec![start]);
            seen[start] = true;
            while let Some(k) = stack.pop() {
                comp.push(k);
                for nb in self.neighbours(k) {
                    if mask[nb] && !seen[nb] {
                        seen[nb] = true;
                        stack.push(nb);
                    }
                }
            }
            out.push(comp);
        }
        out
    }
}

// ------------------------------------------------------------------------------------------------
// Per-seed evaluation
// ------------------------------------------------------------------------------------------------

type ClusterKey = (usize, i64, Vec<usize>, i32, (i32, i32), i32, u64);
type ClusterMemo = Vec<(ClusterKey, Vec<Vec<(i32, i32)>>)>;

thread_local! {
    /// `Eval::cluster_chunks` of recent random-spread clusters, by low 48 bits and parameters.
    static CLUSTER_CHUNKS: std::cell::RefCell<ClusterMemo> = const { std::cell::RefCell::new(Vec::new()) };
}

pub struct Eval<'w> {
    pub w: &'w World,
    pub seed: i64,
    pub(crate) s: Option<Seeded<'w>>,
    pub(crate) bc: BiomeCtx,
    hits: HashMap<(usize, i32, i32), Option<Hit>>,
    rings: HashMap<usize, Vec<(i32, i32)>>,
    natives: &'w [Native],
    pub details: Vec<Value>,
    /// Sum of the objectives (maximize adds, minimize subtracts).
    pub score: f64,
}

impl<'w> Eval<'w> {
    pub fn new(w: &'w World, seed: i64, natives: &'w [Native]) -> Self {
        Eval { w, seed, s: None, bc: BiomeCtx::cached(), hits: HashMap::new(), rings: HashMap::new(), natives, details: vec![], score: 0.0 }
    }

    pub(crate) fn seeded(&mut self) -> &mut Seeded<'w> {
        self.s.get_or_insert_with(|| Seeded::new(self.w, self.seed))
    }

    fn hit(&mut self, set: usize, c: (i32, i32)) -> Option<Hit> {
        if let Some(h) = self.hits.get(&(set, c.0, c.1)) {
            return h.clone();
        }
        self.seeded();
        let h = self.w.structures.generate_set(self.s.as_mut().unwrap(), &mut self.bc, set, c.0, c.1);
        self.hits.insert((set, c.0, c.1), h.clone());
        h
    }

    /// Placement chunks of a set (stronghold rings included) within `r` of `at`.
    pub fn attempts(&mut self, set: usize, at: (i32, i32), r: i32) -> Vec<(i32, i32)> {
        let w = self.w;
        match w.structures.sets[set].placement {
            Placement::Spread { .. } => potential(w, set, self.seed, at, r),
            Placement::Rings { .. } => {
                let need = rings_needed(r + (at.0 as f64).hypot(at.1 as f64) as i32);
                if self.rings.get(&set).is_none_or(|v| v.len() < need) {
                    self.seeded();
                    let v = w.structures.stronghold_chunks(self.s.as_ref().unwrap(), set, need);
                    self.rings.insert(set, v);
                }
                self.rings[&set].iter().copied().filter(|&c| dist2(locate(w, set, c), at) <= (r as i64).pow(2)).collect()
            }
        }
    }

    /// Valid starts of `set` (optionally only one structure of it) within `r` of `at`, nearest first.
    pub fn instances(&mut self, set: usize, only: Option<usize>, at: (i32, i32), r: i32) -> Vec<Hit> {
        let chunks = self.attempts(set, at, r);
        self.instances_of(set, only, at, chunks)
    }

    /// `instances` among the given placement chunks.
    fn instances_of(&mut self, set: usize, only: Option<usize>, at: (i32, i32), chunks: Vec<(i32, i32)>) -> Vec<Hit> {
        let w = self.w;
        let mut out: Vec<Hit> = chunks.into_iter().filter_map(|c| self.hit(set, c)).filter(|h| only.is_none_or(|o| o == h.structure)).collect();
        out.sort_by_key(|h| dist2(locate(w, set, h.chunk), at));
        out
    }

    /// Starts matching a selector (set, structure, start biome, tags), nearest first.
    pub fn select(&mut self, s: &Sel, at: (i32, i32), r: i32) -> Vec<Hit> {
        let chunks = self.attempts(s.set, at, r);
        self.select_of(s, at, chunks)
    }

    /// `select` among the given placement chunks.
    fn select_of(&mut self, s: &Sel, at: (i32, i32), chunks: Vec<(i32, i32)>) -> Vec<Hit> {
        let mut v = self.instances_of(s.set, s.only, at, chunks);
        v.retain(|h| s.tags.iter().all(|t| h.tags.iter().any(|ht| ht.starts_with(t.as_str()))));
        if let Some(b) = s.biomes {
            self.seeded();
            let (seeded, bc) = (self.s.as_ref().unwrap(), &mut self.bc);
            v.retain(|h| contains(b, seeded.biome(bc, h.pos.0 >> 2, h.pos.1 >> 2, h.pos.2 >> 2)));
        }
        v
    }

    fn note(&mut self, h: &Hit) {
        let name = &self.w.structures.structures[h.structure].name;
        let mut d = json!({"type": "structure", "name": name, "x": h.pos.0, "y": h.pos.1, "z": h.pos.2});
        if !h.tags.is_empty() {
            d["tags"] = json!(h.tags);
        }
        if !self.details.contains(&d) {
            self.details.push(d);
        }
    }

    /// Largest `Structures::reach` of the selectors' sets.
    fn reach(&self, sels: &[Sel]) -> i32 {
        sels.iter().map(|s| self.w.structures.reach(s.set)).max().unwrap_or(128)
    }

    /// Most starts one circle of radius `dist` covers. When only values >= `need` matter, starts
    /// with fewer than need - 1 placement chunks within 2 (dist + reach) are never generated: they
    /// cannot be in such a cover. The count is then exact whenever it reaches `need`; which cover
    /// is reported can differ between equally large ones.
    #[allow(clippy::too_many_arguments)]
    fn cluster(&mut self, sels: &[Sel], dist: i32, at: (i32, i32), within: i32, afk: bool, need: f64, note: bool) -> f64 {
        // random-spread chunks depend on the low 48 bits only, so sister seeds reuse them
        let key = sels.iter().all(|s| self.spread(s)).then(|| (self.w as *const World as usize, self.seed & ((1 << 48) - 1), sels.iter().map(|s| s.set).collect(), dist, at, within, need.to_bits()));
        let cached = key.as_ref().and_then(|k| CLUSTER_CHUNKS.with(|m| m.borrow().iter().find(|e| e.0 == *k).map(|e| e.1.clone())));
        let chunks = cached.unwrap_or_else(|| {
            let c = self.cluster_chunks(sels, dist, at, within, need);
            if let Some(k) = key {
                CLUSTER_CHUNKS.with(|m| {
                    let mut m = m.borrow_mut();
                    if m.len() >= 4 {
                        m.remove(0);
                    }
                    m.push((k, c.clone()));
                });
            }
            c
        });
        let hits: Vec<Hit> = sels.iter().zip(chunks).flat_map(|(s, cs)| self.select_of(s, at, cs)).collect();
        let groups: Vec<Vec<(i32, i32)>> = hits.iter().map(|h| footprint(h, afk)).collect();
        let (n, members) = max_cover(&groups, dist, at, within, usize::MAX);
        if note {
            members.iter().for_each(|&i| self.note(&hits[i]));
        }
        n as f64
    }

    /// Placement chunks per selector that `cluster` generates (see there).
    fn cluster_chunks(&mut self, sels: &[Sel], dist: i32, at: (i32, i32), within: i32, need: f64) -> Vec<Vec<(i32, i32)>> {
        let w = self.w;
        let slack = self.reach(sels);
        let mut chunks: Vec<Vec<(i32, i32)>> = sels.iter().map(|s| self.attempts(s.set, at, within + dist + slack)).collect();
        if need >= 2.0 {
            let pts: Vec<(f64, f64)> = sels.iter().zip(&chunks).flat_map(|(s, cs)| cs.iter().map(move |&c| locate(w, s.set, c))).map(|p| (p.0 as f64, p.1 as f64)).collect();
            let r = 2 * (dist + slack);
            let b = Buckets::new(pts.iter().copied(), r);
            let mut k = 0;
            for cs in &mut chunks {
                cs.retain(|_| {
                    let p = pts[k];
                    k += 1;
                    b.near(p).iter().filter(|&&j| (pts[j].0 - p.0).powi(2) + (pts[j].1 - p.1).powi(2) <= (r as f64).powi(2)).count() as f64 >= need
                });
            }
        }
        chunks
    }

    fn spread(&self, s: &Sel) -> bool {
        matches!(self.w.structures.sets[s.set].placement, Placement::Spread { .. })
    }

    /// Cheap necessary condition from the low 48 bits (false = cannot match).
    pub fn pre(&self, c: &Cond) -> bool {
        let pot = |s: &Sel, at, r| potential(self.w, s.set, self.seed, at, r);
        let loc = |s: &Sel, c| locate(self.w, s.set, c);
        match c {
            Cond::And(v) => v.iter().all(|c| self.pre(c)),
            Cond::Or(v) => v.iter().any(|c| self.pre(c)),
            Cond::Metric { m, min, .. } if *min >= 1.0 => match m {
                Metric::Count { s, at, within } if self.spread(s) => pot(s, *at, *within).len() as f64 >= *min,
                // starts can sit ~64-128 blocks from their locate position, so distances get slack
                Metric::Near { a, b, dist, at, within } if self.spread(a) && self.spread(b) => {
                    let pb = pot(b, *at, within + dist + 128);
                    let slack = (*dist as i64 + 128).pow(2);
                    let n = pot(a, *at, *within).iter().filter(|&&x| pb.iter().any(|&y| (a.set != b.set || x != y) && dist2(loc(a, x), loc(b, y)) <= slack)).count();
                    n as f64 >= *min
                }
                Metric::Cluster { sels, dist, at, within, .. } if sels.iter().all(|s| self.spread(s)) => {
                    let slack = self.reach(sels);
                    let groups: Vec<Vec<(i32, i32)>> = sels.iter().flat_map(|s| pot(s, *at, within + dist + slack).into_iter().map(move |c| vec![loc(s, c)])).collect();
                    groups.len() as f64 >= *min && max_cover(&groups, dist + slack, *at, within + slack, min.ceil() as usize).0 as f64 >= *min
                }
                Metric::Slime { radius, at, within } => self.slime_max(*radius, *at, *within) as f64 >= *min,
                Metric::Loot { s, at, within, .. } if self.spread(s) => !pot(s, *at, *within).is_empty(),
                Metric::Portal { at, within, .. } => self.w.structures.resolve("ruined_portal").is_none_or(|(set, _)| !potential(self.w, set, self.seed, *at, *within).is_empty()),
                Metric::Bingo { sels, at, within } => sels.iter().filter(|s| !self.spread(s) || !pot(s, *at, *within).is_empty()).count() as f64 >= *min,
                _ => true,
            },
            _ => true,
        }
    }

    fn slime_max(&self, radius: i32, at: (i32, i32), within: i32) -> i32 {
        if within <= 0 {
            return slime_count(self.seed, at, radius);
        }
        let mut best = 0;
        for x in (at.0 - within) >> 4..=(at.0 + within) >> 4 {
            for z in (at.1 - within) >> 4..=(at.1 + within) >> 4 {
                let c = (x * 16 + 8, z * 16 + 8);
                if dist2(c, at) <= (within as i64).pow(2) {
                    best = best.max(slime_count(self.seed, c, radius));
                }
            }
        }
        best
    }

    pub fn eval(&mut self, c: &Cond) -> bool {
        match c {
            Cond::And(v) => v.iter().all(|c| self.eval(c)),
            Cond::Or(v) => v.iter().any(|c| self.eval(c)),
            Cond::Not(c) => !self.eval(c),
            Cond::Metric { m, min, max } => {
                let before = self.details.len();
                let v = match m {
                    Metric::Cluster { sels, dist, at, within, afk } => self.cluster(sels, *dist, *at, *within, *afk, *min, true),
                    _ => self.value(m, true),
                };
                let ok = v >= *min && v <= *max;
                if !ok {
                    self.details.truncate(before);
                }
                ok
            }
            Cond::Cmp { e, min, max } => {
                let v = self.expr(e);
                v >= *min && v <= *max
            }
            Cond::Objective { e, maximize } => {
                let v = self.expr(e);
                self.score += if *maximize { v } else { -v };
                v.is_finite()
            }
            Cond::Anywhere { c, at, within, step, anchor } => {
                for p in self.anchors(*at, *within, *step, anchor.as_ref()) {
                    let before = self.details.len();
                    if self.eval(&c.shifted((p.0 - at.0, p.1 - at.1))) {
                        self.details.insert(before, json!({"type": "anchor", "x": p.0, "z": p.1}));
                        return true;
                    }
                    self.details.truncate(before);
                }
                false
            }
        }
    }

    /// Anchor points for anywhere() and scans: structure starts, or grid points in the disc.
    pub fn anchors(&mut self, at: (i32, i32), within: i32, step: i32, anchor: Option<&Sel>) -> Vec<(i32, i32)> {
        if let Some(s) = anchor {
            return self.select(s, at, within).iter().map(|h| (h.pos.0, h.pos.2)).collect();
        }
        let n = within / step;
        let mut pts: Vec<(i32, i32)> = (-n..=n).flat_map(|j| (-n..=n).map(move |i| (at.0 + i * step, at.1 + j * step))).filter(|&p| dist2(p, at) <= (within as i64).pow(2)).collect();
        pts.sort_by_key(|&p| dist2(p, at));
        pts
    }

    fn expr(&mut self, e: &Expr) -> f64 {
        match e {
            Expr::Num(v) => *v,
            Expr::Metric(m) => self.value(m, false),
            Expr::Neg(a) => -self.expr(a),
            Expr::Bin(op, a, b) => {
                let (x, y) = (self.expr(a), self.expr(b));
                match op {
                    '+' => x + y,
                    '-' => x - y,
                    '*' => x * y,
                    _ => x / y,
                }
            }
        }
    }

    /// A metric's value. `note` records the structures that make it (match details).
    pub fn value(&mut self, m: &Metric, note: bool) -> f64 {
        match m {
            Metric::Count { s, at, within } => {
                let found = self.select(s, *at, *within);
                if note {
                    found.iter().take(8).for_each(|h| self.note(h));
                }
                found.len() as f64
            }
            Metric::Nearest { s, at, within } => match self.select(s, *at, *within).first() {
                Some(h) => {
                    if note {
                        self.note(h);
                    }
                    (dist2(locate(self.w, s.set, h.chunk), *at) as f64).sqrt()
                }
                None => f64::INFINITY,
            },
            Metric::Near { a, b, dist, at, within } => {
                let mut n = 0.0;
                for ha in self.select(a, *at, *within) {
                    let here = (ha.pos.0, ha.pos.2);
                    let pb = self.select(b, here, dist + 128);
                    if let Some(hb) = pb.iter().find(|hb| **hb != ha && dist2((hb.pos.0, hb.pos.2), here) <= (*dist as i64).pow(2)) {
                        if note {
                            self.note(&ha);
                            self.note(hb);
                        }
                        n += 1.0;
                    }
                }
                n
            }
            Metric::Cluster { sels, dist, at, within, afk } => self.cluster(sels, *dist, *at, *within, *afk, 0.0, note),
            Metric::Chain { sels, dist, at, within } => {
                // longest prefix a -> b -> c ..., each start within `dist` of the previous one
                let mut best: Vec<Hit> = vec![];
                for h0 in self.select(&sels[0], *at, *within) {
                    self.extend_chain(sels, *dist, &mut vec![h0], &mut best);
                    if best.len() == sels.len() {
                        break;
                    }
                }
                if note {
                    best.iter().for_each(|h| self.note(h));
                }
                best.len() as f64
            }
            Metric::Overlap { a, b, at, within, margin } => {
                let mut n = 0.0;
                for ha in self.select(a, *at, *within) {
                    for hb in self.select(b, (ha.pos.0, ha.pos.2), 384) {
                        // count each pair once when both selectors are the same set
                        if hb != ha && (a.set != b.set || ha.chunk < hb.chunk) && boxes_overlap(&ha, &hb, *margin) {
                            if note {
                                self.note(&ha);
                                self.note(&hb);
                            }
                            n += 1.0;
                        }
                    }
                }
                n
            }
            Metric::Bingo { sels, at, within } => {
                let mut n = 0.0;
                for s in sels {
                    if let Some(h) = self.select(s, *at, *within).first() {
                        if note {
                            self.note(h);
                        }
                        n += 1.0;
                    }
                }
                n
            }
            Metric::Biome { biomes, at, within, y, step } => {
                let g = self.grid(*at, *within, *y, *step);
                let disc: Vec<usize> = (0..g.ids.len()).filter(|&k| dist2(g.pos(k), *at) <= (*within as i64).pow(2)).collect();
                disc.iter().filter(|&&k| contains(*biomes, g.ids[k])).count() as f64 / disc.len().max(1) as f64
            }
            Metric::BiomeArea { biomes, at, within, window, y, step } => {
                // ponytail: an area is cut off at the window edge; raise `window` for huge biomes
                let g = self.grid(*at, within + window, *y, *step);
                let mask: Vec<bool> = g.ids.iter().map(|&b| contains(*biomes, b)).collect();
                let near = |k: usize| dist2(g.pos(k), *at) <= (*within.max(&(step / 2)) as i64).pow(2);
                let cells = g.components(&mask).into_iter().filter(|c| c.iter().any(|&k| near(k))).map(|c| c.len()).max().unwrap_or(0);
                cells as f64 * (*step as f64).powi(2)
            }
            Metric::BiomeCount { among, at, within, y, step } => {
                let g = self.grid(*at, *within, *y, *step);
                let seen = (0..g.ids.len()).filter(|&k| dist2(g.pos(k), *at) <= (*within as i64).pow(2)).fold(0u128, |acc, k| acc | 1u128 << g.ids[k]);
                (seen & among.unwrap_or(u128::MAX)).count_ones() as f64
            }
            Metric::BiomeBorder { a, b, at, within, y, step } => {
                let g = self.grid(*at, *within, *y, *step);
                let inside = |k: usize| dist2(g.pos(k), *at) <= (*within as i64).pow(2);
                let mut edges = 0;
                for k in (0..g.ids.len()).filter(|&k| inside(k)) {
                    for nb in g.neighbours(k).filter(|&nb| nb > k && inside(nb)) {
                        let (p, q) = (g.ids[k], g.ids[nb]);
                        if (contains(*a, p) && contains(*b, q)) || (contains(*b, p) && contains(*a, q)) {
                            edges += 1;
                        }
                    }
                }
                (edges * step) as f64
            }
            Metric::Island { biomes, at, within, window, y, step, min_circ, max_elong, lagoon } => self.island(*biomes, *at, *within, *window, *y, *step, (*min_circ, *max_elong, *lagoon), note),
            Metric::Climate { param, pos } => {
                self.seeded();
                self.s.as_ref().unwrap().climate_at(&mut self.bc.ctx, pos.0, pos.1, pos.2)[*param] as f64
            }
            Metric::Height { at, ocean_floor } => {
                let hm = if *ocean_floor { Heightmap::OceanFloorWg } else { Heightmap::WorldSurfaceWg };
                self.seeded().base_height(at.0, at.1, hm) as f64
            }
            Metric::Terrain { kind, at, within, step } => self.terrain(*kind, *at, *within, *step),
            Metric::Slime { radius, at, within } => self.slime_max(*radius, *at, *within) as f64,
            Metric::Loot { s, item, enchant, at, within, total } => {
                let Ok(loot) = &self.w.loot else { return 0.0 };
                let (mut best, mut sum) = (0, 0);
                let mut keep: Vec<(Hit, Vec<Value>)> = vec![]; // what the details show: best structure, or all with `total`
                for h in self.select(s, *at, *within) {
                    let mut n = 0;
                    let mut found = vec![];
                    for c in &h.chests {
                        let items: Vec<_> = loot.fill(&c.table, c.seed, None).unwrap_or_default().into_iter().flatten().collect();
                        let k: i32 = items.iter().filter(|st| st.item == *item && enchant.as_ref().is_none_or(|e| st.enchants.iter().any(|(n, _)| n == e))).map(|st| st.count).sum();
                        if k > 0 {
                            found.push(json!({"type": "chest", "x": c.pos.0, "y": c.pos.1, "z": c.pos.2, "table": c.table, "seed": c.seed,
                                              "exact": c.exact, "items": items.iter().map(|s| s.to_json()).collect::<Vec<_>>()}));
                        }
                        n += k;
                    }
                    if n > 0 && (*total || n > best) {
                        if !*total {
                            keep.clear();
                        }
                        keep.push((h, found));
                    }
                    best = best.max(n);
                    sum += n;
                }
                if note {
                    for (h, found) in keep {
                        self.note(&h);
                        self.details.extend(found);
                    }
                }
                (if *total { sum } else { best }) as f64
            }
            Metric::Portal { at, within, frame } => {
                let Some((set, only)) = self.w.structures.resolve("ruined_portal") else { return 0.0 };
                let key = if *frame { "frame:" } else { "obsidian:" };
                let mut best: Option<Hit> = None;
                let value = |h: &Hit| h.tags.iter().find_map(|t| t.strip_prefix(key)).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                for h in self.instances(set, only, *at, *within) {
                    if best.as_ref().is_none_or(|b| value(&h) > value(b)) {
                        best = Some(h);
                    }
                }
                match best {
                    Some(h) => {
                        if note {
                            self.note(&h);
                        }
                        value(&h) as f64
                    }
                    None => 0.0,
                }
            }
            Metric::Native { lib, name, params } => {
                let natives = self.natives;
                natives[*lib].call(self, name, params) as f64
            }
        }
    }

    fn extend_chain(&mut self, sels: &[Sel], dist: i32, path: &mut Vec<Hit>, best: &mut Vec<Hit>) {
        if path.len() > best.len() {
            *best = path.clone();
        }
        if path.len() == sels.len() || best.len() == sels.len() {
            return;
        }
        let here = { let l = path.last().unwrap(); (l.pos.0, l.pos.2) };
        for h in self.select(&sels[path.len()], here, dist + 128) {
            if dist2((h.pos.0, h.pos.2), here) <= (dist as i64).pow(2) && !path.contains(&h) {
                path.push(h);
                self.extend_chain(sels, dist, path, best);
                path.pop();
            }
        }
    }

    fn grid(&mut self, at: (i32, i32), r: i32, y: i32, step: i32) -> Grid {
        self.seeded();
        let (s, bc) = (self.s.as_ref().unwrap(), &mut self.bc);
        let n = 2 * (r / step) + 1;
        let (x0, z0) = (at.0 - (n / 2) * step, at.1 - (n / 2) * step);
        let mut ids = Vec::with_capacity((n * n) as usize);
        for j in 0..n {
            for i in 0..n {
                ids.push(s.biome(bc, (x0 + i * step) >> 2, y >> 2, (z0 + j * step) >> 2));
            }
        }
        Grid { n, x0, z0, step, ids }
    }

    /// Largest island (land component fully inside the window, surrounded by ocean) touching the
    /// `within` disc that passes the shape filters; its area in blocks. Land = not `#is_ocean`, or
    /// the given biomes (which must then border only ocean).
    #[allow(clippy::too_many_arguments)]
    fn island(&mut self, biomes: Option<BiomeSet>, at: (i32, i32), within: i32, window: i32, y: i32, step: i32, (min_circ, max_elong, lagoon): (f64, f64, bool), note: bool) -> f64 {
        let g = self.grid(at, within + window, y, step);
        let ocean = self.w.biomes.tag("minecraft:is_ocean");
        let land: Vec<bool> = g.ids.iter().map(|&b| biomes.map_or(!contains(ocean, b), |set| contains(set, b))).collect();
        let near = |k: usize| dist2(g.pos(k), at) <= (within.max(step / 2) as i64).pow(2);
        let water = g.components(&land.iter().map(|l| !l).collect::<Vec<_>>());
        let mut best = 0.0;
        let comps = g.components(&land);
        let mut owner = vec![usize::MAX; land.len()];
        for (ci, comp) in comps.iter().enumerate() {
            comp.iter().for_each(|&k| owner[k] = ci);
        }
        for (ci, comp) in comps.iter().enumerate() {
            if !comp.iter().any(|&k| near(k)) || comp.iter().any(|&k| g.on_border(k)) {
                continue;
            }
            let member = |k: usize| owner[k] == ci;
            if biomes.is_some() && comp.iter().any(|&k| g.neighbours(k).any(|nb| !member(nb) && !contains(ocean, g.ids[nb]))) {
                continue;
            }
            let area = comp.len() as f64;
            let perimeter = comp.iter().map(|&k| 4 - g.neighbours(k).filter(|&nb| member(nb)).count()).sum::<usize>() as f64;
            let circ = 4.0 * std::f64::consts::PI * area / (perimeter * perimeter);
            let (xs, zs): (Vec<i32>, Vec<i32>) = comp.iter().map(|&k| (k as i32 % g.n, k as i32 / g.n)).unzip();
            let (w, h) = ((xs.iter().max().unwrap() - xs.iter().min().unwrap() + 1) as f64, (zs.iter().max().unwrap() - zs.iter().min().unwrap() + 1) as f64);
            let elong = w.max(h) / w.min(h);
            // a lagoon: a water component enclosed by this island alone
            let enclosed = |wc: &Vec<usize>| wc.iter().all(|&k| !g.on_border(k) && g.neighbours(k).all(|nb| member(nb) || !land[nb]));
            if circ < min_circ || elong > max_elong || (lagoon && !water.iter().any(|wc| wc.iter().any(|&k| g.neighbours(k).any(&member)) && enclosed(wc))) {
                continue;
            }
            let blocks = area * (step as f64).powi(2);
            if blocks > best {
                best = blocks;
                if note {
                    let (cx, cz) = ((xs.iter().sum::<i32>() as f64 / area) as i32, (zs.iter().sum::<i32>() as f64 / area) as i32);
                    self.details.retain(|d| d["type"] != "island");
                    self.details.push(json!({"type": "island", "x": g.x0 + cx * step, "z": g.z0 + cz * step, "area": blocks, "circularity": circ, "elongation": elong}));
                }
            }
        }
        best
    }

    /// Height-grid and column-scan metrics over the disc (`step` apart): highest surface, lowest
    /// ocean floor, largest neighbour height step, longest air gap above sea level under solid
    /// ground, longest air run below sea level, longest fluid run under the surface.
    fn terrain(&mut self, kind: Terrain, at: (i32, i32), within: i32, step: i32) -> f64 {
        let n = within / step;
        let pts: Vec<(i32, i32)> = (-n..=n).flat_map(|j| (-n..=n).map(move |i| (i, j))).filter(|&(i, j)| (i * i + j * j) as i64 * (step as i64).pow(2) <= (within as i64).pow(2)).collect();
        let (sea, min_y) = (self.w.sea_level, self.w.min_y);
        let pos = |p: (i32, i32)| (at.0 + p.0 * step, at.1 + p.1 * step);
        let s = self.seeded();
        s.prepare_terrain();
        let height = |s: &Seeded, p, hm| {
            let q = pos(p);
            s.height_at(q.0, q.1, hm)
        };
        match kind {
            Terrain::Peak => pts.iter().map(|&p| height(s, p, Heightmap::WorldSurfaceWg) as f64).fold(f64::NEG_INFINITY, f64::max),
            Terrain::Trench => pts.iter().map(|&p| height(s, p, Heightmap::OceanFloorWg) as f64).fold(f64::INFINITY, f64::min),
            Terrain::Cliff => {
                let hs: HashMap<(i32, i32), i32> = pts.iter().map(|&p| (p, height(s, p, Heightmap::WorldSurfaceWg))).collect();
                let mut best = 0;
                for (p, h) in &hs {
                    for q in [(p.0 + 1, p.1), (p.0, p.1 + 1)] {
                        best = best.max(hs.get(&q).map_or(0, |h2| (h - h2).abs()));
                    }
                }
                best as f64
            }
            Terrain::Overhang | Terrain::Void | Terrain::Lake(_) => {
                let mut best = 0;
                for &p in &pts {
                    let q = pos(p);
                    let col = s.column(q.0, q.1);
                    let Some(top) = col.iter().rposition(|b| *b == Block::Stone) else { continue };
                    // walk runs of equal blocks down from the highest solid block
                    let mut i = top as i32;
                    while i >= 0 {
                        let (b, run_top) = (col[i as usize], i);
                        while i >= 0 && col[i as usize] == b {
                            i -= 1;
                        }
                        let len = run_top - i;
                        let counts = match kind {
                            Terrain::Overhang => b == Block::Air && i >= 0 && col[i as usize] == Block::Stone && i + 1 + min_y > sea,
                            Terrain::Void => b == Block::Air && run_top + min_y < sea,
                            Terrain::Lake(f) => b == f,
                            _ => false,
                        };
                        if counts {
                            best = best.max(len);
                        }
                    }
                }
                best as f64
            }
        }
    }
}

/// Every anchor within `within` of `at` (grid `step`, or `anchor` starts) where `cond`, moved
/// there, holds in one seed; nearest first, with match details.
#[allow(clippy::too_many_arguments)]
pub fn scan(w: &World, natives: &[Native], seed: i64, cond: &Cond, at: (i32, i32), within: i32, step: i32, anchor: Option<&Sel>) -> Vec<((i32, i32), Vec<Value>)> {
    use rayon::prelude::*;
    let anchors = Eval::new(w, seed, natives).anchors(at, within, step, anchor);
    anchors
        .par_chunks(32)
        .flat_map_iter(|chunk| {
            let mut ev = Eval::new(w, seed, natives);
            chunk
                .iter()
                .filter_map(|&p| {
                    ev.details.clear();
                    ev.eval(&cond.shifted((p.0 - at.0, p.1 - at.1))).then(|| (p, std::mem::take(&mut ev.details)))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_cover_finds_tight_cluster() {
        // four points on a 200-wide square fit a radius-142 circle, not a radius-140 one
        let g: Vec<Vec<(i32, i32)>> = vec![vec![(0, 0)], vec![(200, 0)], vec![(0, 200)], vec![(200, 200)], vec![(900, 900)]];
        assert_eq!(max_cover(&g, 142, (0, 0), 1000, usize::MAX).0, 4);
        assert_eq!(max_cover(&g, 140, (0, 0), 1000, usize::MAX).0, 2);
        // centre within 50 of the origin is >= 150 from every other point
        assert_eq!(max_cover(&g, 142, (0, 0), 50, usize::MAX).0, 1);
        // afk groups need all their points inside: (0,0)..(260,10) spans 260.2
        let afk = vec![vec![(0, 0), (10, 10)], vec![(250, 0), (260, 10)]];
        assert_eq!(max_cover(&afk, 131, (0, 0), 1000, usize::MAX).0, 2);
        assert_eq!(max_cover(&afk, 129, (0, 0), 1000, usize::MAX).0, 1);
    }

    #[test]
    fn max_cover_matches_all_pairs() {
        // the bucketed scan must give the same count and members as trying every pair
        let brute = |groups: &[Vec<(i32, i32)>], r: i32, at: (i32, i32), within: i32| {
            let mut c: Vec<(f64, f64, f64)> = groups.iter().flatten().map(|p| (p.0 as f64, p.1 as f64, r as f64)).collect();
            c.push((at.0 as f64, at.1 as f64, within as f64));
            let mut cands: Vec<(f64, f64)> = c.iter().map(|c| (c.0 - c.2, c.1)).collect();
            for i in 0..c.len() {
                for j in i + 1..c.len() {
                    let ((x0, z0, r0), (x1, z1, r1)) = (c[i], c[j]);
                    let d = ((x1 - x0).powi(2) + (z1 - z0).powi(2)).sqrt();
                    if d == 0.0 || d > r0 + r1 || d < (r0 - r1).abs() {
                        continue;
                    }
                    let a = (r0 * r0 - r1 * r1 + d * d) / (2.0 * d);
                    let h = (r0 * r0 - a * a).max(0.0).sqrt();
                    let (mx, mz) = (x0 + a * (x1 - x0) / d, z0 + a * (z1 - z0) / d);
                    cands.push((mx - h * (z1 - z0) / d, mz + h * (x1 - x0) / d));
                    cands.push((mx + h * (z1 - z0) / d, mz - h * (x1 - x0) / d));
                }
            }
            let inside = |c: (f64, f64), p: (f64, f64), r: f64| (p.0 - c.0).powi(2) + (p.1 - c.1).powi(2) <= r * r + 1e-6;
            let mut best = (0, vec![]);
            for c in cands.into_iter().filter(|&c| inside(c, (at.0 as f64, at.1 as f64), within as f64)) {
                let m: Vec<usize> = (0..groups.len()).filter(|&g| groups[g].iter().all(|p| inside(c, (p.0 as f64, p.1 as f64), r as f64))).collect();
                if m.len() > best.0 {
                    best = (m.len(), m);
                }
            }
            best
        };
        let mut rng = JavaRandom::new(7);
        for case in 0..300 {
            let (n, span, afk) = (1 + rng.next_bound(40), 200 + rng.next_bound(3000), case % 2 == 0);
            let groups: Vec<Vec<(i32, i32)>> = (0..n)
                .map(|_| {
                    let (x, z) = (rng.next_bound(span) - span / 2, rng.next_bound(span) - span / 2);
                    if afk { vec![(x, z), (x + 8, z), (x, z + 6), (x + 8, z + 6)] } else { vec![(x, z)] }
                })
                .collect();
            let (r, within) = (20 + rng.next_bound(300), rng.next_bound(span));
            assert_eq!(max_cover(&groups, r, (0, 0), within, usize::MAX), brute(&groups, r, (0, 0), within), "case {case}");
        }
    }
}
