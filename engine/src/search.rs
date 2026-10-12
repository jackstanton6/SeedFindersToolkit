//! The parallel seed search and the native plugin ABI.
//!
//! Every condition has a cheap 48-bit pre-check (`Eval::pre`, structure placement only) and the
//! full check (`Eval::eval`). A seed is fully evaluated only if `pre` passes, and in structure-seed
//! mode the 65536 upper-bit siblings are only tried for structure seeds that pass.

use crate::query::{parse, Cond, Eval};
use crate::world::{Heightmap, World};
use serde_json::{json, Value};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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

    /// Runs a filter; > 0 passes, and the value itself is the metric in expressions.
    pub(crate) fn call(&self, ev: &mut Eval, name: &CStr, params: &CStr) -> i32 {
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
        unsafe { (self.filter)(&host, name.as_ptr(), params.as_ptr()) }
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
    pub score: f64,
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
    /// Stop after this many matches; with an objective, keep only the best this many instead.
    pub max_results: u64,
    ranked: bool,
    top: Mutex<Vec<f64>>,
}

impl Search {
    /// `natives`: (plugin name, library path) pairs referenced by `native` conditions.
    pub fn new(world: Arc<World>, query: &Value, mode: Mode, natives: &[(String, String)]) -> Result<Search, String> {
        let libs = natives.iter().map(|(_, p)| Native::load(p)).collect::<Result<_, _>>()?;
        let names: Vec<String> = natives.iter().map(|(n, _)| n.clone()).collect();
        let cond = parse(&world, &names, query)?;
        Ok(Search::with(world, cond, mode, libs))
    }

    fn with(world: Arc<World>, cond: Cond, mode: Mode, natives: Vec<Native>) -> Search {
        let ranked = cond.has_objective();
        let max_results = if ranked { 100 } else { u64::MAX };
        Search { world, cond, mode, natives, stop: AtomicBool::new(false), stats: Stats::default(), results: Mutex::new(vec![]), max_results, ranked, top: Mutex::new(vec![]) }
    }

    fn full(&self, seed: i64, local: &mut Local) {
        let mut ev = Eval::new(&self.world, seed, &self.natives);
        local.full += 1;
        if ev.eval(&self.cond) {
            let n = self.stats.matched.fetch_add(1, Relaxed) + 1;
            if self.ranked {
                // only seeds entering the running top `max_results` scores are reported
                let mut top = self.top.lock().unwrap();
                let k = top.partition_point(|&s| s >= ev.score);
                if k as u64 >= self.max_results {
                    return;
                }
                top.insert(k, ev.score);
                top.truncate(self.max_results.min(usize::MAX as u64) as usize);
            } else if n >= self.max_results {
                self.stop.store(true, Relaxed);
            }
            self.results.lock().unwrap().push(Match { seed, details: ev.details, score: ev.score });
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
        // small blocks when there is little work, so slow queries still use every thread
        let block = if matches!(self.mode, Mode::StructureSeeds { .. }) { 1 } else { ((to - from) / (threads.max(1) as u64 * 16)).clamp(1, 256) };
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
        let probe = Search::with(self.world.clone(), self.cond.clone(), self.mode.clone(), vec![]);
        let t = Instant::now();
        probe.run(0, n, threads);
        let secs = t.elapsed().as_secs_f64().max(1e-9);
        let rate = n as f64 / secs;
        let checks = probe.stats.checks.load(Relaxed).max(1) as f64;
        json!({
            "sample": n,
            "units_per_sec": rate,
            "prefilter_rate": probe.stats.prefilter_passed.load(Relaxed) as f64 / checks,
            "matches": probe.stats.matched.load(Relaxed),
            "match_rate": probe.stats.matched.load(Relaxed) as f64 / n as f64,
            "seconds_total": self.mode.units() as f64 / rate,
        })
    }
}
