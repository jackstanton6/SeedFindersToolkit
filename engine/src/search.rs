//! Query evaluation and the parallel seed search.
//!
//! Every condition has a cheap 48-bit pre-check (`pre`, structure placement only) and the full
//! check (`eval`, biomes/heights/validity). A seed is fully evaluated only if `pre` passes, and in
//! structure-seed mode the 65536 upper-bit siblings are only tried for structure seeds that pass.

use crate::biome::{contains, BiomeSet};
use crate::structure::{Hit, Placement};
use crate::world::{BiomeCtx, Heightmap, Seeded, World};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ------------------------------------------------------------------------------------------------
// Conditions
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Cond {
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Not(Box<Cond>),
    /// `min..=max` valid starts of a set (optionally one structure of it) within `within` of `at`.
    Structure { set: usize, only: Option<usize>, at: (i32, i32), within: i32, min: u32, max: Option<u32> },
    /// Some start of `a` within `within` of `at` has a start of `b` within `dist` of it.
    Near { a: (usize, Option<usize>), b: (usize, Option<usize>), dist: i32, at: (i32, i32), within: i32 },
    /// Fraction of sample points (grid `step`, disc `within` around `at`, height `y`) in `biomes`.
    Biome { biomes: BiomeSet, at: (i32, i32), within: i32, y: i32, step: i32, min_fraction: f32 },
    /// Raw climate parameter (0 temperature .. 5 weirdness) at a block position.
    Climate { param: usize, pos: (i32, i32, i32), min: f32, max: f32 },
    /// Base terrain height (getBaseHeight) at a column.
    Height { at: (i32, i32), min: i32, max: i32, ocean_floor: bool },
    /// Filter exported by a native plugin.
    Native { lib: usize, name: CString, params: CString },
}

fn pair(v: &Value, k: &str, default: (i32, i32)) -> (i32, i32) {
    v.get(k).and_then(Value::as_array).map_or(default, |a| (a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32))
}
fn num(v: &Value, k: &str, default: i64) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(default)
}
fn flt(v: &Value, k: &str, default: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(default)
}

pub const CLIMATE_PARAMS: [&str; 6] = ["temperature", "humidity", "continentalness", "erosion", "depth", "weirdness"];

/// Parses the JSON condition tree produced by the Python query layer.
pub fn parse(w: &World, natives: &[String], v: &Value) -> Result<Cond, String> {
    let op = v["op"].as_str().ok_or_else(|| format!("condition without op: {v}"))?;
    let kids = |k: &str| -> Result<Vec<Cond>, String> {
        v[k].as_array().ok_or(format!("{op} needs '{k}'"))?.iter().map(|c| parse(w, natives, c)).collect()
    };
    let structure = |k: &str| -> Result<(usize, Option<usize>), String> {
        let name = v[k].as_str().ok_or(format!("{op} needs '{k}'"))?;
        w.structures.resolve(name).ok_or(format!("unknown structure '{name}'"))
    };
    Ok(match op {
        "and" => Cond::And(kids("args")?),
        "or" => Cond::Or(kids("args")?),
        "not" => Cond::Not(Box::new(parse(w, natives, &v["arg"])?)),
        "structure" => {
            let (set, only) = structure("name")?;
            Cond::Structure {
                set, only,
                at: pair(v, "at", (0, 0)),
                within: num(v, "within", 1000) as i32,
                min: num(v, "min", 1) as u32,
                max: v.get("max").and_then(Value::as_i64).map(|m| m as u32),
            }
        }
        "near" => Cond::Near {
            a: structure("a")?,
            b: structure("b")?,
            dist: num(v, "dist", 128) as i32,
            at: pair(v, "at", (0, 0)),
            within: num(v, "within", 1000) as i32,
        },
        "biome" => {
            let names: Vec<&str> = match &v["name"] {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => return Err("biome needs 'name'".into()),
            };
            let mut biomes = 0;
            for n in names {
                biomes |= match n.strip_prefix('#') {
                    Some(tag) => w.biomes.tag(&crate::density::ns(tag)),
                    None => 1u128 << w.biomes.id(n).ok_or(format!("unknown biome '{n}'"))?,
                };
            }
            Cond::Biome {
                biomes,
                at: pair(v, "at", (0, 0)),
                within: num(v, "within", 0) as i32,
                y: num(v, "y", 64) as i32,
                step: num(v, "step", 16).max(4) as i32,
                min_fraction: flt(v, "fraction", 0.0) as f32,
            }
        }
        "climate" => Cond::Climate {
            param: CLIMATE_PARAMS.iter().position(|p| Some(*p) == v["param"].as_str()).ok_or(format!("climate param must be one of {CLIMATE_PARAMS:?}"))?,
            pos: { let (x, z) = pair(v, "at", (0, 0)); (x, num(v, "y", 64) as i32, z) },
            min: flt(v, "min", f64::NEG_INFINITY) as f32,
            max: flt(v, "max", f64::INFINITY) as f32,
        },
        "height" => Cond::Height {
            at: pair(v, "at", (0, 0)),
            min: num(v, "min", i32::MIN as i64) as i32,
            max: num(v, "max", i32::MAX as i64) as i32,
            ocean_floor: v.get("heightmap").and_then(Value::as_str) == Some("ocean_floor"),
        },
        "native" => {
            let plugin = v["plugin"].as_str().ok_or("native needs 'plugin'")?;
            Cond::Native {
                lib: natives.iter().position(|p| p == plugin).ok_or(format!("native plugin '{plugin}' not loaded"))?,
                name: CString::new(v["filter"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?,
                params: CString::new(v.get("params").map(|p| p.to_string()).unwrap_or_else(|| "{}".into())).map_err(|e| e.to_string())?,
            }
        }
        other => return Err(format!("unknown condition '{other}'")),
    })
}

// ------------------------------------------------------------------------------------------------
// Per-seed evaluation
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

/// Stronghold positions needed to cover `r` blocks: ring c starts at (88 + 192c) chunks.
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

pub struct Eval<'w> {
    pub w: &'w World,
    pub seed: i64,
    s: Option<Seeded<'w>>,
    bc: BiomeCtx,
    hits: HashMap<(usize, i32, i32), Option<Hit>>,
    rings: HashMap<usize, Vec<(i32, i32)>>,
    natives: &'w [Native],
    pub details: Vec<Value>,
}

impl<'w> Eval<'w> {
    pub fn new(w: &'w World, seed: i64, natives: &'w [Native]) -> Self {
        Eval { w, seed, s: None, bc: BiomeCtx::cached(), hits: HashMap::new(), rings: HashMap::new(), natives, details: vec![] }
    }

    fn seeded(&mut self) -> &mut Seeded<'w> {
        self.s.get_or_insert_with(|| Seeded::new(self.w, self.seed))
    }

    fn hit(&mut self, set: usize, c: (i32, i32)) -> Option<Hit> {
        if let Some(h) = self.hits.get(&(set, c.0, c.1)) {
            return *h;
        }
        self.seeded();
        let h = self.w.structures.generate_set(self.s.as_mut().unwrap(), &mut self.bc, set, c.0, c.1);
        self.hits.insert((set, c.0, c.1), h);
        h
    }

    /// Valid starts of `set` (filtered to `only`) whose locate position is within `r` of `at`,
    /// nearest first.
    pub fn instances(&mut self, set: usize, only: Option<usize>, at: (i32, i32), r: i32) -> Vec<Hit> {
        let w = self.w;
        let chunks: Vec<(i32, i32)> = match w.structures.sets[set].placement {
            Placement::Spread { .. } => potential(w, set, self.seed, at, r),
            Placement::Rings { .. } => {
                let need = rings_needed(r);
                if self.rings.get(&set).is_none_or(|v| v.len() < need) {
                    self.seeded();
                    let v = w.structures.stronghold_chunks(self.s.as_ref().unwrap(), set, need);
                    self.rings.insert(set, v);
                }
                self.rings[&set].iter().copied().filter(|&c| dist2(locate(w, set, c), at) <= (r as i64).pow(2)).collect()
            }
        };
        let mut out: Vec<Hit> = chunks.into_iter().filter_map(|c| self.hit(set, c)).filter(|h| only.is_none_or(|o| o == h.structure)).collect();
        out.sort_by_key(|h| dist2(locate(w, set, h.chunk), at));
        out
    }

    fn note(&mut self, h: &Hit) {
        let name = &self.w.structures.structures[h.structure].name;
        let d = json!({"type": "structure", "name": name, "x": h.pos.0, "y": h.pos.1, "z": h.pos.2});
        if !self.details.contains(&d) {
            self.details.push(d);
        }
    }

    /// Cheap necessary condition from the low 48 bits (false = cannot match).
    pub fn pre(&self, c: &Cond) -> bool {
        let spread = |s: usize| matches!(self.w.structures.sets[s].placement, Placement::Spread { .. });
        match c {
            Cond::And(v) => v.iter().all(|c| self.pre(c)),
            Cond::Or(v) => v.iter().any(|c| self.pre(c)),
            Cond::Structure { set, at, within, min, .. } if spread(*set) => potential(self.w, *set, self.seed, *at, *within).len() as u32 >= *min,
            Cond::Near { a, b, dist, at, within } if spread(a.0) && spread(b.0) => {
                let pa = potential(self.w, a.0, self.seed, *at, *within);
                let pb = potential(self.w, b.0, self.seed, *at, within + dist + 128);
                // starts can sit ~64 blocks from their locate position; keep the pre-check loose
                let slack = (*dist as i64 + 128).pow(2);
                pa.iter().any(|&x| pb.iter().any(|&y| (a.0 != b.0 || x != y) && dist2(locate(self.w, a.0, x), locate(self.w, b.0, y)) <= slack))
            }
            _ => true,
        }
    }

    pub fn eval(&mut self, c: &Cond) -> bool {
        match c {
            Cond::And(v) => v.iter().all(|c| self.eval(c)),
            Cond::Or(v) => v.iter().any(|c| self.eval(c)),
            Cond::Not(c) => !self.eval(c),
            Cond::Structure { set, only, at, within, min, max } => {
                let found = self.instances(*set, *only, *at, *within);
                let n = found.len() as u32;
                let ok = n >= *min && max.is_none_or(|m| n <= m);
                if ok {
                    found.iter().take(8).for_each(|h| self.note(h));
                }
                ok
            }
            Cond::Near { a, b, dist, at, within } => {
                for ha in self.instances(a.0, a.1, *at, *within) {
                    let here = (ha.pos.0, ha.pos.2);
                    let pb = self.instances(b.0, b.1, here, dist + 128);
                    if let Some(hb) = pb.iter().find(|hb| **hb != ha && dist2((hb.pos.0, hb.pos.2), here) <= (*dist as i64).pow(2)) {
                        self.note(&ha);
                        self.note(hb);
                        return true;
                    }
                }
                false
            }
            Cond::Biome { biomes, at, within, y, step, min_fraction } => {
                self.seeded();
                let (s, bc) = (self.s.as_ref().unwrap(), &mut self.bc);
                let (mut total, mut hit) = (0u32, 0u32);
                let r = *within;
                let first = -r - (-r).rem_euclid(*step);
                for dz in (first..=r).step_by(*step as usize) {
                    for dx in (first..=r).step_by(*step as usize) {
                        if (dx as i64).pow(2) + (dz as i64).pow(2) > (r as i64).pow(2) {
                            continue;
                        }
                        total += 1;
                        if contains(*biomes, s.biome(bc, (at.0 + dx) >> 2, y >> 2, (at.1 + dz) >> 2)) {
                            hit += 1;
                            if *min_fraction <= 0.0 {
                                return true;
                            }
                        }
                    }
                }
                hit > 0 && hit as f32 >= min_fraction * total as f32
            }
            Cond::Climate { param, pos, min, max } => {
                self.seeded();
                let v = self.s.as_ref().unwrap().climate_at(&mut self.bc.ctx, pos.0, pos.1, pos.2)[*param];
                v >= *min && v <= *max
            }
            Cond::Height { at, min, max, ocean_floor } => {
                let hm = if *ocean_floor { Heightmap::OceanFloorWg } else { Heightmap::WorldSurfaceWg };
                let h = self.seeded().base_height(at.0, at.1, hm);
                h >= *min && h <= *max
            }
            Cond::Native { lib, name, params } => {
                let natives = self.natives;
                natives[*lib].call(self, name, params)
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Native (Rust/C) plugins: a cdylib exporting `sft_abi_version` and `sft_filter`
// ------------------------------------------------------------------------------------------------

pub const ABI_VERSION: u32 = 1;

/// Host API handed to native filters. Coordinates are blocks; biome ids index `World.biomes`.
#[repr(C)]
pub struct SftHost {
    pub abi: u32,
    pub ctx: *mut c_void,
    pub seed: i64,
    pub biome_at: extern "C" fn(*mut c_void, i32, i32, i32) -> i32,
    pub biome_id: extern "C" fn(*mut c_void, *const c_char) -> i32,
    pub surface_height: extern "C" fn(*mut c_void, i32, i32) -> i32,
    pub count_structures: extern "C" fn(*mut c_void, *const c_char, i32, i32, i32) -> i32,
}

type FilterFn = unsafe extern "C" fn(*const SftHost, *const c_char, *const c_char) -> i32;

pub struct Native {
    _lib: libloading::Library,
    filter: FilterFn,
}

extern "C" fn host_biome_at(ctx: *mut c_void, x: i32, y: i32, z: i32) -> i32 {
    let ev = unsafe { &mut *(ctx as *mut Eval) };
    ev.seeded();
    ev.s.as_ref().unwrap().biome(&mut ev.bc, x >> 2, y >> 2, z >> 2) as i32
}
extern "C" fn host_biome_id(ctx: *mut c_void, name: *const c_char) -> i32 {
    let ev = unsafe { &*(ctx as *mut Eval) };
    let n = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    ev.w.biomes.id(&n).map_or(-1, |i| i as i32)
}
extern "C" fn host_surface_height(ctx: *mut c_void, x: i32, z: i32) -> i32 {
    let ev = unsafe { &mut *(ctx as *mut Eval) };
    ev.seeded().base_height(x, z, Heightmap::WorldSurfaceWg)
}
extern "C" fn host_count_structures(ctx: *mut c_void, name: *const c_char, x: i32, z: i32, r: i32) -> i32 {
    let ev = unsafe { &mut *(ctx as *mut Eval) };
    let n = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    match ev.w.structures.resolve(&n) {
        Some((set, only)) => ev.instances(set, only, (x, z), r).len() as i32,
        None => -1,
    }
}

impl Native {
    pub fn load(path: &str) -> Result<Native, String> {
        // SAFETY: loading runs the library's initialisers; plugins are code the user chose to install.
        unsafe {
            let lib = libloading::Library::new(path).map_err(|e| format!("{path}: {e}"))?;
            let abi = *lib.get::<extern "C" fn() -> u32>(b"sft_abi_version").map_err(|e| format!("{path}: {e}"))?;
            if abi() != ABI_VERSION {
                return Err(format!("{path}: plugin ABI {} != host ABI {ABI_VERSION}", abi()));
            }
            let filter = *lib.get::<FilterFn>(b"sft_filter").map_err(|e| format!("{path}: {e}"))?;
            Ok(Native { _lib: lib, filter })
        }
    }

    fn call(&self, ev: &mut Eval, name: &CStr, params: &CStr) -> bool {
        let host = SftHost {
            abi: ABI_VERSION,
            ctx: ev as *mut Eval as *mut c_void,
            seed: ev.seed,
            biome_at: host_biome_at,
            biome_id: host_biome_id,
            surface_height: host_surface_height,
            count_structures: host_count_structures,
        };
        // SAFETY: `host` outlives the call; the plugin must not keep the pointers.
        unsafe { (self.filter)(&host, name.as_ptr(), params.as_ptr()) > 0 }
    }
}

// ------------------------------------------------------------------------------------------------
// Search runner
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Mode {
    /// Consecutive 64-bit seeds.
    Range { start: i64, count: u64 },
    /// Consecutive 48-bit structure seeds; survivors are expanded to all 65536 upper bits.
    StructureSeeds { start: u64, count: u64 },
    /// Explicit seeds (file, cracker output, plugin seed source...).
    List(Vec<i64>),
    /// Pseudo-random 64-bit seeds from a fixed generator seed (reproducible).
    Random { count: u64, rng_seed: u64 },
}

impl Mode {
    pub fn units(&self) -> u64 {
        match self {
            Mode::Range { count, .. } | Mode::StructureSeeds { count, .. } | Mode::Random { count, .. } => *count,
            Mode::List(v) => v.len() as u64,
        }
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[derive(Clone, Debug)]
pub struct Match {
    pub seed: i64,
    pub details: Vec<Value>,
}

/// Live counters. `checks` counts every seed (48- or 64-bit) put through `pre`/`eval`.
#[derive(Default)]
pub struct Stats {
    pub units_done: AtomicU64,
    pub checks: AtomicU64,
    pub prefilter_passed: AtomicU64,
    pub full_evals: AtomicU64,
    pub matched: AtomicU64,
}

#[derive(Default)]
struct Local {
    units: u64,
    checks: u64,
    passed: u64,
    full: u64,
}

pub struct Search {
    pub world: Arc<World>,
    pub cond: Cond,
    pub mode: Mode,
    pub natives: Vec<Native>,
    pub stop: AtomicBool,
    pub stats: Stats,
    pub results: Mutex<Vec<Match>>,
    pub max_results: u64,
}

impl Search {
    /// `natives`: (plugin name, library path) pairs referenced by `native` conditions.
    pub fn new(world: Arc<World>, query: &Value, mode: Mode, natives: &[(String, String)]) -> Result<Search, String> {
        let libs = natives.iter().map(|(_, p)| Native::load(p)).collect::<Result<_, _>>()?;
        let names: Vec<String> = natives.iter().map(|(n, _)| n.clone()).collect();
        let cond = parse(&world, &names, query)?;
        Ok(Search { world, cond, mode, natives: libs, stop: AtomicBool::new(false), stats: Stats::default(), results: Mutex::new(vec![]), max_results: u64::MAX })
    }

    fn full(&self, seed: i64, local: &mut Local) {
        let mut ev = Eval::new(&self.world, seed, &self.natives);
        local.full += 1;
        if ev.eval(&self.cond) {
            let n = self.stats.matched.fetch_add(1, Relaxed) + 1;
            self.results.lock().unwrap().push(Match { seed, details: ev.details });
            if n >= self.max_results {
                self.stop.store(true, Relaxed);
            }
        }
    }

    fn seed_unit(&self, seed: i64, local: &mut Local) {
        local.checks += 1;
        if Eval::new(&self.world, seed, &self.natives).pre(&self.cond) {
            local.passed += 1;
            self.full(seed, local);
        }
    }

    /// Processes unit `i`; false if interrupted part-way (the unit must be redone on resume).
    fn unit(&self, i: u64, local: &mut Local) -> bool {
        match &self.mode {
            Mode::Range { start, .. } => self.seed_unit(start.wrapping_add(i as i64), local),
            Mode::List(v) => self.seed_unit(v[i as usize], local),
            Mode::Random { rng_seed, .. } => self.seed_unit(splitmix(rng_seed.wrapping_add(i)) as i64, local),
            Mode::StructureSeeds { start, .. } => {
                let low = ((start + i) & ((1 << 48) - 1)) as i64;
                local.checks += 1;
                if Eval::new(&self.world, low, &self.natives).pre(&self.cond) {
                    local.passed += 1;
                    for upper in 0..65536i64 {
                        if self.stop.load(Relaxed) {
                            return false;
                        }
                        local.checks += 1;
                        self.full((upper << 48) | low, local);
                    }
                }
            }
        }
        true
    }

    fn flush(&self, l: &mut Local) {
        self.stats.units_done.fetch_add(l.units, Relaxed);
        self.stats.checks.fetch_add(l.checks, Relaxed);
        self.stats.prefilter_passed.fetch_add(l.passed, Relaxed);
        self.stats.full_evals.fetch_add(l.full, Relaxed);
        *l = Local::default();
    }

    /// Runs units [from, to) on `threads` workers pulling blocks in order. Returns the resume
    /// point: every unit before it is done.
    pub fn run(&self, from: u64, to: u64, threads: usize) -> u64 {
        let block = if matches!(self.mode, Mode::StructureSeeds { .. }) { 1 } else { 256 };
        let next = AtomicU64::new(from);
        let resume = AtomicU64::new(u64::MAX);
        std::thread::scope(|sc| {
            for _ in 0..threads.max(1) {
                sc.spawn(|| {
                    let mut local = Local::default();
                    loop {
                        let lo = next.fetch_add(block, Relaxed);
                        if lo >= to {
                            break;
                        }
                        for i in lo..(lo + block).min(to) {
                            if self.stop.load(Relaxed) || !self.unit(i, &mut local) {
                                resume.fetch_min(i, Relaxed);
                                self.flush(&mut local);
                                return;
                            }
                            local.units += 1;
                        }
                        self.flush(&mut local);
                    }
                });
            }
        });
        resume.load(Relaxed).min(next.load(Relaxed)).min(to)
    }

    /// Times a sample of units to estimate throughput, pre-filter pass rate and total time.
    pub fn estimate(&self, sample: u64, threads: usize) -> Value {
        if !self.natives.is_empty() {
            return json!({"note": "native filters are not sampled; no estimate"});
        }
        let n = sample.min(self.mode.units()).max(1);
        let probe = Search {
            world: self.world.clone(),
            cond: self.cond.clone(),
            mode: self.mode.clone(),
            natives: vec![],
            stop: AtomicBool::new(false),
            stats: Stats::default(),
            results: Mutex::new(vec![]),
            max_results: u64::MAX,
        };
        let t = Instant::now();
        probe.run(0, n, threads);
        let secs = t.elapsed().as_secs_f64().max(1e-9);
        let rate = n as f64 / secs;
        let checks = probe.stats.checks.load(Relaxed).max(1) as f64;
        json!({
            "sample": n,
            "units_per_sec": rate,
            "prefilter_rate": probe.stats.prefilter_passed.load(Relaxed) as f64 / checks,
            "match_rate": probe.stats.matched.load(Relaxed) as f64 / n as f64,
            "seconds_total": self.mode.units() as f64 / rate,
        })
    }
}
