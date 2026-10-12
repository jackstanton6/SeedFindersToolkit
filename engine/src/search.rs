//! The parallel seed search and the native plugin ABI.
//!
//! Every condition has a cheap 48-bit pre-check (`Eval::pre`, structure placement only) and the
//! full check (`Eval::eval`). A seed is fully evaluated only if `pre` passes, and in structure-seed
//! mode the 65536 upper-bit siblings are only tried for structure seeds that pass.

use crate::gpu;
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

/// Unit `i` of `n` in shuffled order: a bijection on [0, n) keyed by `key`. Odd multiplies and
/// xor-shifts within the next power of two, cycle-walking past n (fewer than 2 rounds on average).
/// gpu.cu `permute` is a literal port.
pub fn permute(i: u64, n: u64, key: u64) -> u64 {
    if n <= 1 {
        return i;
    }
    let k = 64 - (n - 1).leading_zeros();
    let mask = if k == 64 { u64::MAX } else { (1 << k) - 1 };
    let s = k.div_ceil(2);
    let mut x = i;
    loop {
        x = (x ^ key) & mask;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9) & mask;
        x ^= x >> s;
        x = (x ^ (key >> 32)) & mask;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB) & mask;
        x ^= x >> s;
        if x < n {
            return x;
        }
    }
}

/// Some `and` with two or more children (worth ordering).
fn has_and(c: &Cond) -> bool {
    match c {
        Cond::And(v) if v.len() > 1 => true,
        Cond::And(v) | Cond::Or(v) => v.iter().any(has_and),
        Cond::Not(c) => has_and(c),
        _ => false,
    }
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
    /// Sister seeds already checked in structure seeds that are still being expanded.
    pub partial: AtomicU64,
    /// Units the GPU pre-filter checked and passed.
    pub gpu_units: AtomicU64,
    pub gpu_passed: AtomicU64,
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
    /// First engine panic (unit and message); the search stops when one happens.
    pub error: Mutex<Option<String>>,
    /// GPU pre-filter: device and program (see `use_gpu`).
    gpu: Option<(Arc<gpu::Gpu>, Vec<i32>)>,
    /// Visit the units in shuffled order with this key (`permute`); resume points stay valid.
    pub shuffle: Option<u64>,
}

impl Search {
    /// `natives`: (plugin name, library path) pairs referenced by `native` conditions.
    pub fn new(world: Arc<World>, query: &Value, mode: Mode, natives: &[(String, String)]) -> Result<Search, String> {
        let libs = natives.iter().map(|(_, p)| Native::load(p)).collect::<Result<_, _>>()?;
        let names: Vec<String> = natives.iter().map(|(n, _)| n.clone()).collect();
        let cond = parse(&world, &names, query)?;
        let mut s = Search::with(world, cond, mode, libs);
        if has_and(&s.cond) {
            let seeds = s.sample(32);
            let mut c = std::mem::replace(&mut s.cond, Cond::And(vec![]));
            s.order_ands(&mut c, &seeds);
            s.cond = c;
        }
        Ok(s)
    }

    /// Up to `n` seeds of this search that pass the pre-check (0.3 s at most).
    fn sample(&self, n: usize) -> Vec<i64> {
        let t = Instant::now();
        let mut out = vec![];
        for i in 0..self.mode.units() {
            if out.len() >= n || t.elapsed().as_secs_f64() > 0.3 {
                break;
            }
            // structure seeds: varied upper bits, so biomes differ
            let seed = self.seed(i) | ((out.len() as i64 * 7919) << 48);
            if Eval::new(&self.world, seed, &self.natives).pre(&self.cond) {
                out.push(seed);
            }
        }
        out
    }

    /// Puts the children of every `and` cheapest-to-reject first (cost / (1 - pass rate), measured
    /// on `seeds`, 0.2 s per child at most). The same seeds match; only speed and the order of
    /// match details change. `and`s with objectives keep their order (score sums stay identical).
    fn order_ands(&self, c: &mut Cond, seeds: &[i64]) {
        match c {
            Cond::And(v) | Cond::Or(v) => v.iter_mut().for_each(|c| self.order_ands(c, seeds)),
            Cond::Not(c) => self.order_ands(c, seeds),
            _ => {}
        }
        let Cond::And(v) = c else { return };
        if seeds.len() < 4 || v.len() < 2 || v.iter().any(Cond::has_objective) {
            return;
        }
        let mut keyed: Vec<(f64, Cond)> = v
            .drain(..)
            .map(|c| {
                let t = Instant::now();
                let (mut n, mut pass) = (0, 0);
                for &s in seeds {
                    n += 1;
                    pass += Eval::new(&self.world, s, &self.natives).eval(&c) as usize;
                    if t.elapsed().as_secs_f64() > 0.2 {
                        break;
                    }
                }
                let (cost, p) = (t.elapsed().as_secs_f64() / n as f64, pass as f64 / n as f64);
                (if p >= 1.0 { f64::INFINITY } else { cost / (1.0 - p) }, c)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        *v = keyed.into_iter().map(|k| k.1).collect();
    }

    /// The 64-bit seed of unit `i` (structure seeds: its low 48 bits).
    fn seed(&self, i: u64) -> i64 {
        match &self.mode {
            Mode::Range { start, .. } => start.wrapping_add(i as i64),
            Mode::List(v) => v[i as usize],
            Mode::Random { rng_seed, .. } => splitmix(rng_seed.wrapping_add(i)) as i64,
            Mode::StructureSeeds { start, .. } => ((start + i) & ((1 << 48) - 1)) as i64,
        }
    }

    fn with(world: Arc<World>, cond: Cond, mode: Mode, natives: Vec<Native>) -> Search {
        let ranked = cond.has_objective();
        let max_results = if ranked { 100 } else { u64::MAX };
        Search { world, cond, mode, natives, stop: AtomicBool::new(false), stats: Stats::default(), results: Mutex::new(vec![]), max_results, ranked, top: Mutex::new(vec![]), error: Mutex::new(None), gpu: None, shuffle: None }
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
                if n > self.max_results {
                    return;
                }
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

    /// Processes part `k` of `of` of unit `i` (structure seeds split their sisters into parts);
    /// false if interrupted part-way (the unit must be redone on resume).
    fn unit(&self, i: u64, (k, of): (u64, u64), local: &mut Local) -> bool {
        let i = self.shuffle.map_or(i, |k| permute(i, self.mode.units(), k));
        match &self.mode {
            Mode::StructureSeeds { .. } => {
                let low = self.seed(i);
                if k == 0 {
                    local.checks += 1;
                }
                if Eval::new(&self.world, low, &self.natives).pre(&self.cond) {
                    if k == 0 {
                        local.passed += 1;
                    }
                    let span = 65536 / of as i64;
                    // flushed per sister so slow queries still show progress
                    for upper in k as i64 * span..(k as i64 + 1) * span {
                        if self.stop.load(Relaxed) {
                            self.stats.partial.fetch_sub((upper - k as i64 * span) as u64, Relaxed);
                            return false;
                        }
                        local.checks += 1;
                        self.full((upper << 48) | low, local);
                        self.flush(local);
                        self.stats.partial.fetch_add(1, Relaxed);
                    }
                    self.stats.partial.fetch_sub(span as u64, Relaxed);
                }
            }
            _ => self.seed_unit(self.seed(i), local),
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

    /// Runs units [from, to) on `threads` workers (after the GPU pre-filter when enabled). Returns
    /// the resume point: every unit before it is done.
    pub fn run(&self, from: u64, to: u64, threads: usize) -> u64 {
        match &self.gpu {
            Some((g, prog)) => self.run_gpu(g, prog, from, to, threads),
            None => from + self.run_cpu(to - from, 1, &|p| from + p, &|_| 0, threads),
        }
    }

    /// Batches of units go through the GPU program; the CPU then checks the survivors as usual. The
    /// GPU filters the next batch while the CPU works on this one.
    fn run_gpu(&self, g: &gpu::Gpu, prog: &[i32], from: u64, to: u64, threads: usize) -> u64 {
        let (mode, base) = match self.mode {
            Mode::Range { start, .. } => (0, start as u64),
            Mode::StructureSeeds { start, .. } => (1, start),
            Mode::Random { rng_seed, .. } => (2, rng_seed),
            Mode::List(_) => unreachable!("use_gpu rejects lists"),
        };
        let shuffle = self.shuffle.map(|k| (self.mode.units(), k));
        // the few survivors of a structure-seed batch share their 65536 sisters among all threads
        let parts = if mode == 1 { 16 } else { 1 };
        let (tx, rx) = std::sync::mpsc::sync_channel::<(u64, u64, Vec<u32>)>(1);
        std::thread::scope(|sc| {
            sc.spawn(move || {
                let (mut lo, mut batch) = (from, 1u64 << 16);
                while lo < to && !self.stop.load(Relaxed) {
                    let hi = (lo + batch).min(to);
                    let t = Instant::now();
                    match g.filter(prog, mode, base, lo, hi - lo, shuffle) {
                        // a closed channel means the CPU side stopped
                        Ok(surv) => if tx.send((lo, hi, surv)).is_err() { return },
                        Err(e) => {
                            self.error.lock().unwrap().get_or_insert(format!("GPU error: {e}"));
                            self.stop.store(true, Relaxed);
                            return;
                        }
                    }
                    batch = gpu::next_batch(batch, t, 1 << 24);
                    lo = hi;
                }
            });
            let mut done = from;
            for (lo, hi, surv) in rx {
                self.stats.gpu_units.fetch_add(hi - lo, Relaxed);
                self.stats.gpu_passed.fetch_add(surv.len() as u64, Relaxed);
                // the GPU-rejected units before a survivor count as done when the CPU finishes it
                let gap = |p: u64| surv[p as usize] as u64 - if p == 0 { 0 } else { surv[p as usize - 1] as u64 + 1 };
                let p = self.run_cpu(surv.len() as u64, parts, &|p| lo + surv[p as usize] as u64, &gap, threads);
                if p < surv.len() as u64 {
                    return lo + surv[p as usize] as u64;
                }
                let tail = hi - lo - surv.last().map_or(0, |&l| l as u64 + 1);
                self.stats.units_done.fetch_add(tail, Relaxed);
                self.stats.checks.fetch_add(tail, Relaxed);
                done = hi;
            }
            done
        })
    }

    /// Runs positions [0, n), position p being unit `unit_at(p)` split into `parts`, on workers
    /// pulling blocks in order; a finished position also credits `skipped(p)` units the GPU already
    /// rejected. Returns the first position not done (n when all are).
    fn run_cpu(&self, n: u64, parts: u64, unit_at: &(dyn Fn(u64) -> u64 + Sync), skipped: &(dyn Fn(u64) -> u64 + Sync), threads: usize) -> u64 {
        // small blocks when there is little work, so slow queries still use every thread
        let block = if matches!(self.mode, Mode::StructureSeeds { .. }) { 1 } else { (n / (threads.max(1) as u64 * 16)).clamp(1, 256) };
        let left: Vec<AtomicU64> = if parts > 1 { (0..n).map(|_| AtomicU64::new(parts)).collect() } else { vec![] };
        let next = AtomicU64::new(0);
        let resume = AtomicU64::new(u64::MAX);
        std::thread::scope(|sc| {
            for _ in 0..threads.max(1) {
                sc.spawn(|| {
                    let mut local = Local::default();
                    loop {
                        let lo = next.fetch_add(block, Relaxed);
                        if lo >= n * parts {
                            break;
                        }
                        for q in lo..(lo + block).min(n * parts) {
                            let (p, part) = (q / parts, (q % parts, parts));
                            let i = unit_at(p);
                            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.unit(i, part, &mut local))).unwrap_or_else(|e| {
                                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                                self.error.lock().unwrap().get_or_insert(format!("engine error at unit {i}: {msg}"));
                                self.stop.store(true, Relaxed);
                                false
                            });
                            if self.stop.load(Relaxed) || !ok {
                                resume.fetch_min(p, Relaxed);
                                self.flush(&mut local);
                                return;
                            }
                            // a split unit is done when its last part is
                            if parts == 1 || left[p as usize].fetch_sub(1, Relaxed) == 1 {
                                local.units += 1 + skipped(p);
                                local.checks += skipped(p);
                            }
                            if local.full > 0 || local.units >= 1024 {
                                self.flush(&mut local); // live progress for slow queries
                            }
                        }
                        self.flush(&mut local);
                    }
                });
            }
        });
        resume.load(Relaxed).min(next.load(Relaxed) / parts).min(n)
    }

    /// Turns on the GPU pre-filter; Err says why it cannot be used for this search.
    pub fn use_gpu(&mut self) -> Result<String, String> {
        if matches!(self.mode, Mode::List(_)) {
            return Err("seed lists run on the CPU".into());
        }
        let prog = gpu::program(&self.world, &self.cond);
        if prog.is_empty() {
            return Err("nothing in this query depends on the seed alone (structure placement, slime chunks)".into());
        }
        let g = gpu::get()?;
        let name = g.name.clone();
        self.gpu = Some((g, prog));
        Ok(name)
    }

    /// Units done, counting partly expanded structure seeds as fractions.
    pub fn units_done(&self) -> f64 {
        self.stats.units_done.load(Relaxed) as f64 + self.stats.partial.load(Relaxed) as f64 / 65536.0
    }

    /// 64-bit seeds one unit stands for.
    fn seeds_per_unit(&self) -> f64 {
        if matches!(self.mode, Mode::StructureSeeds { .. }) { 65536.0 } else { 1.0 }
    }

    /// Runs the search itself for up to `secs` seconds (or until `stop`), measures throughput,
    /// pre-filter pass rate and match rate, then resets. Must not be called while the search runs.
    pub fn estimate(&self, secs: f64, threads: usize) -> Value {
        let t = Instant::now();
        let finished = AtomicBool::new(false);
        let snap = Mutex::new([0.0; 8]);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                while !finished.load(Relaxed) && !self.stop.load(Relaxed) && t.elapsed().as_secs_f64() < secs {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let st = &self.stats;
                let c = |a: &AtomicU64| a.load(Relaxed) as f64;
                *snap.lock().unwrap() = [self.units_done(), c(&st.matched), c(&st.checks), c(&st.prefilter_passed), c(&st.full_evals), c(&st.gpu_units), c(&st.gpu_passed), t.elapsed().as_secs_f64()];
                self.stop.store(true, Relaxed);
            });
            self.run(0, self.mode.units(), threads);
            finished.store(true, Relaxed);
        });
        let [units, matched, checks, passed, full, gpu_units, gpu_passed, secs] = *snap.lock().unwrap();
        self.reset();
        if units <= 0.0 {
            return json!({"note": format!("no seed finished in {secs:.1}s: each one takes longer than that per thread")});
        }
        let structure = matches!(self.mode, Mode::StructureSeeds { .. });
        // pass rate of the 48-bit pre-check (sister evaluations are not pre-checks)
        let p = if gpu_units > 0.0 { gpu_passed / gpu_units } else { passed / (checks - if structure { full } else { 0.0 }).max(1.0) };
        let mut rate = units / secs;
        if structure && p > 0.0 && full > 0.0 {
            // a short window sees the cheap rejected units but few whole 65536-sister expansions;
            // in the long run each unit costs p * 65536 sister evaluations. With the GPU pre-filter
            // the threads only expand sisters (and rejected units are credited late), so the model
            // alone is the estimate.
            let model = full / secs / (p * 65536.0);
            rate = if gpu_units > 0.0 { model } else { rate.min(model) };
        }
        let seeds = units * self.seeds_per_unit();
        json!({
            "sample": seeds,
            "seconds": secs,
            "units_per_sec": rate,
            "seeds_per_sec": rate * self.seeds_per_unit(),
            "prefilter_rate": p,
            "matches": matched,
            "match_rate": matched / seeds,
            "seconds_total": self.mode.units() as f64 / rate,
        })
    }

    fn reset(&self) {
        let st = &self.stats;
        for a in [&st.units_done, &st.checks, &st.prefilter_passed, &st.full_evals, &st.matched, &st.partial, &st.gpu_units, &st.gpu_passed] {
            a.store(0, Relaxed);
        }
        self.results.lock().unwrap().clear();
        self.top.lock().unwrap().clear();
        *self.error.lock().unwrap() = None;
        self.stop.store(false, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::permute;

    #[test]
    fn permute_is_a_bijection() {
        for n in (1..300).chain([1 << 16, 1_000_003]) {
            for key in [0, 7, u64::MAX] {
                let mut seen = vec![false; n as usize];
                for i in 0..n {
                    let p = permute(i, n, key) as usize;
                    assert!(!seen[p], "n {n} key {key}: {p} twice");
                    seen[p] = true;
                }
            }
        }
        assert!(permute(5, u64::MAX, 1) < u64::MAX);
    }
}
