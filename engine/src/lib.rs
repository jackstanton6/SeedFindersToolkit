//! Seed Finders Toolkit engine: bit-exact Minecraft world generation and parallel seed search.

// Loops and comparisons mirror the Java source on purpose (evaluation order, NaN behaviour).
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::manual_range_contains, clippy::needless_range_loop, clippy::manual_memcpy)]

pub mod biome;
pub mod blocks;
pub mod carver;
pub mod chunk;
pub mod crack;
pub mod density;
pub mod gpu;
pub mod loot;
pub mod noise;
pub mod rng;
pub mod query;
pub mod search;
pub mod structure;
pub mod world;

/// Python bindings (`sft._engine`), built by maturin with `--features python`.
#[cfg(feature = "python")]
mod py {
    use crate::query::{self, Eval};
    use crate::search::{self, Mode, Search};
    use crate::world::{BiomeCtx, Heightmap, Seeded, World};
    use pyo3::exceptions::{PyRuntimeError, PyValueError};
    use pyo3::prelude::*;
    use rayon::prelude::*;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Instant;

    fn err(e: String) -> PyErr {
        PyValueError::new_err(e)
    }

    #[pyclass(name = "World", frozen)]
    struct PyWorld {
        w: Arc<World>,
    }

    #[pymethods]
    impl PyWorld {
        #[new]
        fn new(data_dir: &str) -> PyResult<Self> {
            Ok(PyWorld { w: Arc::new(World::load(std::path::Path::new(data_dir)).map_err(err)?) })
        }

        #[getter]
        fn version(&self) -> String {
            self.w.version.clone()
        }

        /// Biome ids, index = biome id used by maps.
        fn biomes(&self) -> Vec<String> {
            self.w.biomes.names.clone()
        }

        /// (structure, structure set, exact, can generate in the overworld)
        fn structures(&self) -> Vec<(String, String, bool, bool)> {
            let st = &self.w.structures;
            st.sets
                .iter()
                .flat_map(|set| set.entries.iter().map(move |(i, _)| {
                    let s = &st.structures[*i];
                    (s.name.clone(), set.name.clone(), s.exact(), set.possible)
                }))
                .collect()
        }

        /// Biome ids on a w*h grid starting at (x0, z0), `scale` blocks per cell, rows along z.
        fn biome_map(&self, py: Python<'_>, seed: i64, x0: i32, z0: i32, w: i32, h: i32, scale: i32, y: i32) -> Vec<u16> {
            let world = self.w.clone();
            py.detach(move || {
                let s = Seeded::new(&world, seed);
                (0..h)
                    .into_par_iter()
                    .flat_map_iter(|j| {
                        let mut bc = BiomeCtx::cached();
                        (0..w).map(|i| s.biome(&mut bc, (x0 + i * scale) >> 2, y >> 2, (z0 + j * scale) >> 2)).collect::<Vec<_>>()
                    })
                    .collect()
            })
        }

        /// Blocks on the biome_map grid from generated chunks: the top block (y None) or the block
        /// at y. Returns (palette, index per cell).
        #[allow(clippy::too_many_arguments)]
        fn block_map(&self, py: Python<'_>, seed: i64, x0: i32, z0: i32, w: i32, h: i32, scale: i32, y: Option<i32>) -> PyResult<(Vec<String>, Vec<u16>)> {
            let world = self.w.clone();
            py.detach(move || {
                let cells: Vec<(i32, i32)> = (0..h).flat_map(|j| (0..w).map(move |i| (x0 + i * scale, z0 + j * scale))).collect();
                let mut chunks: Vec<(i32, i32)> = cells.iter().map(|&(x, z)| (x >> 4, z >> 4)).collect();
                chunks.sort_unstable();
                chunks.dedup();
                let generated: std::collections::HashMap<(i32, i32), std::sync::Arc<crate::chunk::Chunk>> = chunks
                    .par_iter()
                    .map_init(|| crate::chunk::Chunks::new(&world, seed), |ch, &c| ch.terrain(c.0, c.1).map(|g| (c, g)))
                    .collect::<Result<_, String>>()
                    .map_err(PyValueError::new_err)?;
                let mut ids = std::collections::HashMap::new();
                let mut palette = vec![];
                let out = cells
                    .iter()
                    .map(|&(x, z)| {
                        let c = &generated[&(x >> 4, z >> 4)];
                        let yy = y.unwrap_or_else(|| c.height(crate::chunk::WORLD_SURFACE_WG, x, z));
                        let s = c.get(&world, x, yy, z);
                        *ids.entry(s).or_insert_with(|| {
                            palette.push(world.blocks.to_string(s));
                            (palette.len() - 1) as u16
                        })
                    })
                    .collect();
                Ok((palette, out))
            })
        }

        /// Surface heights (WORLD_SURFACE_WG) on the same grid layout as biome_map.
        fn height_map(&self, py: Python<'_>, seed: i64, x0: i32, z0: i32, w: i32, h: i32, scale: i32) -> Vec<i32> {
            let world = self.w.clone();
            py.detach(move || {
                let mut s = Seeded::new(&world, seed);
                s.prepare_terrain();
                (0..w * h).into_par_iter().map(|k| s.height_at(x0 + (k % w) * scale, z0 + (k / w) * scale, Heightmap::WorldSurfaceWg)).collect()
            })
        }

        /// The world spawn (block x, z), as conditions with at="spawn" use it.
        fn spawn(&self, seed: i64) -> (i32, i32) {
            Eval::new(&self.w, seed, &[]).spawn()
        }

        /// The block-level biome id at a block (what F3 shows).
        fn block_biome(&self, seed: i64, x: i32, y: i32, z: i32) -> u16 {
            Seeded::new(&self.w, seed).block_biome(&mut BiomeCtx::cached(), x, y, z)
        }

        /// The block at a position ("minecraft:stone"), from the generated chunk.
        fn block(&self, py: Python<'_>, seed: i64, x: i32, y: i32, z: i32) -> PyResult<String> {
            let w = self.w.clone();
            py.detach(move || {
                let c = crate::chunk::Chunks::new(&w, seed).terrain(x >> 4, z >> 4).map_err(PyValueError::new_err)?;
                Ok(w.blocks.to_string(c.get(&w, x, y, z)))
            })
        }

        /// Generated chunks cx0..=cx1 x cz0..=cz1 (in parallel): per chunk (cx, cz, palette of
        /// block states, block indices in (y, z, x) order from min_y, heightmaps WORLD_SURFACE_WG and
        /// OCEAN_FLOOR_WG as highest occupied y per column (z, x order), stored biomes (4x4x4
        /// cells, y fastest, then x, then z, from y = min_y)).
        #[allow(clippy::type_complexity)]
        fn chunks(&self, py: Python<'_>, seed: i64, cx0: i32, cz0: i32, cx1: i32, cz1: i32) -> PyResult<Vec<(i32, i32, Vec<String>, Vec<u16>, Vec<i32>, Vec<String>)>> {
            let w = self.w.clone();
            py.detach(move || {
                let coords: Vec<(i32, i32)> = (cz0..=cz1).flat_map(|z| (cx0..=cx1).map(move |x| (x, z))).collect();
                coords
                    .par_iter()
                    .map_init(
                        || crate::chunk::Chunks::new(&w, seed),
                        |ch, &(cx, cz)| {
                            let c = ch.terrain(cx, cz).map_err(PyValueError::new_err)?;
                            let mut ids = std::collections::HashMap::new();
                            let mut palette = vec![];
                            let blocks = c
                                .blocks
                                .iter()
                                .map(|&s| {
                                    *ids.entry(s).or_insert_with(|| {
                                        palette.push(w.blocks.to_string(s));
                                        (palette.len() - 1) as u16
                                    })
                                })
                                .collect();
                            let heights = (0..2).flat_map(|hm| (0..256).map(move |k| (hm, k))).map(|(hm, k)| c.height(hm, k & 15, k >> 4)).collect();
                            let qy = w.min_y >> 2;
                            let biomes = ch.s.biomes_volume(&mut BiomeCtx::cached(), cx * 4, qy, cz * 4, 4, w.height >> 2, 4).iter().map(|&b| w.biomes.name(b).to_string()).collect();
                            Ok((cx, cz, palette, blocks, heights, biomes))
                        },
                    )
                    .collect()
            })
        }

        fn height(&self, seed: i64, x: i32, z: i32, ocean_floor: bool) -> i32 {
            let hm = if ocean_floor { Heightmap::OceanFloorWg } else { Heightmap::WorldSurfaceWg };
            Seeded::new(&self.w, seed).base_height(x, z, hm)
        }

        /// The six climate parameters at a block position.
        fn climate(&self, seed: i64, x: i32, y: i32, z: i32) -> [f32; 6] {
            Seeded::new(&self.w, seed).climate_at(&mut crate::density::Ctx::uncached(), x, y, z)
        }

        fn biome_at(&self, seed: i64, x: i32, y: i32, z: i32) -> String {
            let s = Seeded::new(&self.w, seed);
            self.w.biomes.name(s.biome(&mut BiomeCtx::cached(), x >> 2, y >> 2, z >> 2)).to_string()
        }

        /// Valid structure starts within `radius` of (x, z): (structure, x, y, z, exact).
        /// `names` limits the search to some structures/sets (default: every overworld set).
        #[pyo3(signature = (seed, x, z, radius, names=None))]
        fn structures_near(&self, py: Python<'_>, seed: i64, x: i32, z: i32, radius: i32, names: Option<Vec<String>>) -> PyResult<Vec<(String, i32, i32, i32, bool)>> {
            let st = &self.w.structures;
            let wanted: Vec<(usize, Option<usize>)> = match names {
                Some(n) => n.iter().map(|n| st.resolve(n).ok_or_else(|| err(format!("unknown structure '{n}'")))).collect::<PyResult<_>>()?,
                None => (0..st.sets.len()).filter(|&i| st.sets[i].possible).map(|i| (i, None)).collect(),
            };
            let world = self.w.clone();
            Ok(py.detach(move || {
                let mut ev = Eval::new(&world, seed, &[]);
                let mut out = vec![];
                for (set, only) in wanted {
                    for h in ev.instances(set, only, (x, z), radius) {
                        let s = &world.structures.structures[h.structure];
                        out.push((s.name.clone(), h.pos.0, h.pos.1, h.pos.2, s.exact()));
                    }
                }
                out
            }))
        }

        /// Evaluates a query for one seed: details JSON if it matches, else None. Queries with an
        /// objective get a trailing {"type": "score"} entry. natives: [(plugin name, library path)].
        #[pyo3(signature = (seed, query, natives=vec![]))]
        fn check(&self, py: Python<'_>, seed: i64, query: &str, natives: Vec<(String, String)>) -> PyResult<Option<String>> {
            let (cond, libs) = self.parse(query, &natives)?;
            let world = self.w.clone();
            Ok(py.detach(move || {
                let mut ev = Eval::new(&world, seed, &libs);
                if !ev.eval(&cond) {
                    return None;
                }
                if cond.has_objective() {
                    ev.details.push(serde_json::json!({"type": "score", "value": ev.score}));
                }
                Some(Value::Array(ev.details).to_string())
            }))
        }

        /// Single-seed scan: every anchor within `within` of (x, z) where the query, moved there,
        /// matches. Anchors are grid points `step` apart, or the starts of `anchor` structures.
        /// Returns [(x, z, details JSON)], nearest first.
        #[pyo3(signature = (seed, query, x, z, within, step=64, anchor=None, natives=vec![]))]
        #[allow(clippy::too_many_arguments)]
        fn scan(&self, py: Python<'_>, seed: i64, query: &str, x: i32, z: i32, within: i32, step: i32, anchor: Option<String>, natives: Vec<(String, String)>) -> PyResult<Vec<(i32, i32, String)>> {
            let (cond, libs) = self.parse(query, &natives)?;
            let sel = anchor.map(|a| query::parse_sel(&self.w, &serde_json::json!({"name": a}))).transpose().map_err(err)?;
            let world = self.w.clone();
            Ok(py.detach(move || {
                query::scan(&world, &libs, seed, &cond, (x, z), within, step.max(1), sel.as_ref())
                    .into_iter()
                    .map(|(p, d)| (p.0, p.1, Value::Array(d).to_string()))
                    .collect()
            }))
        }

        /// Every placement attempt of a structure/set within `radius` of (x, z), for heatmaps:
        /// [(x, z, structure or "" if the start failed)] at /locate positions (chunk corner + offset).
        fn attempts(&self, py: Python<'_>, seed: i64, name: &str, x: i32, z: i32, radius: i32) -> PyResult<Vec<(i32, i32, String)>> {
            let (set, only) = self.w.structures.resolve(name).ok_or_else(|| err(format!("unknown structure '{name}'")))?;
            let world = self.w.clone();
            Ok(py.detach(move || {
                let mut ev = Eval::new(&world, seed, &[]);
                let chunks = ev.attempts(set, (x, z), radius);
                let hits = ev.instances(set, only, (x, z), radius);
                chunks
                    .into_iter()
                    .map(|c| {
                        let p = query::locate(&world, set, c);
                        let name = hits.iter().find(|h| h.chunk == c).map(|h| world.structures.structures[h.structure].name.clone());
                        (p.0, p.1, name.unwrap_or_default())
                    })
                    .collect()
            }))
        }

        /// Valid starts of a structure/set within `radius` of (x, z) for many seeds, in parallel:
        /// one [(x, z)] list per seed (placement statistics, probability heatmaps).
        fn placement_stats(&self, py: Python<'_>, name: &str, x: i32, z: i32, radius: i32, seeds: Vec<i64>) -> PyResult<Vec<Vec<(i32, i32)>>> {
            let (set, only) = self.w.structures.resolve(name).ok_or_else(|| err(format!("unknown structure '{name}'")))?;
            let world = self.w.clone();
            Ok(py.detach(move || {
                seeds.par_iter().map(|&seed| Eval::new(&world, seed, &[]).instances(set, only, (x, z), radius).iter().map(|h| (h.pos.0, h.pos.2)).collect()).collect()
            }))
        }

        /// LootTable.fill of an empty chest: 27 slots, each None or {"item", "count", ...} (JSON).
        fn loot(&self, table: &str, seed: i64) -> PyResult<String> {
            let loot = self.w.loot.as_ref().map_err(|e| err(e.clone()))?;
            let chest = loot.fill(&crate::density::ns(table), seed, None).map_err(err)?;
            Ok(Value::Array(chest.iter().map(|s| s.as_ref().map_or(Value::Null, |s| s.to_json())).collect()).to_string())
        }

        /// Loot chests of the structures within `radius` of (x, z) (buried treasure, shipwrecks, ruined
        /// portals, igloo basements, desert pyramids, ocean ruins, jungle temples) with their contents: JSON list.
        fn chests(&self, py: Python<'_>, seed: i64, name: &str, x: i32, z: i32, radius: i32) -> PyResult<String> {
            let (set, only) = self.w.structures.resolve(name).ok_or_else(|| err(format!("unknown structure '{name}'")))?;
            let loot = self.w.loot.as_ref().map_err(|e| err(e.clone()))?;
            let world = self.w.clone();
            py.detach(move || {
                let mut ev = Eval::new(&world, seed, &[]);
                let mut out = vec![];
                for h in ev.instances(set, only, (x, z), radius) {
                    for c in &h.chests {
                        let items: Vec<Value> = loot.fill(&c.table, c.seed, None).map_err(err)?.iter().flatten().map(|s| s.to_json()).collect();
                        out.push(serde_json::json!({"structure": world.structures.structures[h.structure].name, "x": c.pos.0, "y": c.pos.1, "z": c.pos.2,
                                                    "table": c.table, "seed": c.seed, "exact": c.exact, "items": items}));
                    }
                }
                Ok(Value::Array(out).to_string())
            })
        }
    }

    impl PyWorld {
        /// The query and the native libraries it may call (loaded in `natives` order).
        fn parse(&self, query: &str, natives: &[(String, String)]) -> PyResult<(query::Cond, Vec<search::Native>)> {
            let libs = natives.iter().map(|(_, p)| search::Native::load(p)).collect::<Result<Vec<_>, _>>().map_err(err)?;
            let names: Vec<String> = natives.iter().map(|(n, _)| n.clone()).collect();
            let q: Value = serde_json::from_str(query).map_err(|e| err(e.to_string()))?;
            Ok((query::parse(&self.w, &names, &q).map_err(err)?, libs))
        }
    }

    /// Structure-seed cracking from observed starts [(structure or set, block x, block z)]: `run`
    /// finds the 48-bit structure seeds placing all of them. Refused if more than `max_work` seeds
    /// would have to be brute-forced (default 2^38 on the CPU, 2^44 with `gpu`; add observations
    /// of huts, temples, outposts, shipwrecks).
    #[pyclass(name = "Crack", frozen)]
    struct PyCrack {
        w: Arc<World>,
        plan: Arc<crate::crack::Plan>,
        gpu: bool,
        stop: Arc<AtomicBool>,
        done: Arc<AtomicU64>,
    }

    #[pymethods]
    impl PyCrack {
        #[new]
        #[pyo3(signature = (world, observations, max_work=None, gpu=false))]
        fn new(py: Python<'_>, world: &PyWorld, observations: Vec<(String, i32, i32)>, max_work: Option<u64>, gpu: bool) -> PyResult<Self> {
            let st = &world.w.structures;
            let obs = observations
                .iter()
                .map(|(n, x, z)| Ok(crate::crack::Obs { set: st.resolve(n).ok_or_else(|| err(format!("unknown structure '{n}'")))?.0, chunk: (x >> 4, z >> 4) }))
                .collect::<PyResult<Vec<_>>>()?;
            let w = world.w.clone();
            let plan = py.detach(|| crate::crack::plan(&w.structures, &obs)).map_err(err)?;
            let max_work = max_work.unwrap_or(if gpu { 1 << 44 } else { 1 << 38 });
            if plan.work > max_work {
                return Err(err(format!("{} seeds to brute-force (limit {max_work}); add more observations", plan.work)));
            }
            Ok(PyCrack { w, plan: Arc::new(plan), gpu, stop: Arc::default(), done: Arc::default() })
        }

        /// Seeds the brute force tries in all.
        #[getter]
        fn work(&self) -> u64 {
            self.plan.work
        }

        /// Seeds tried so far.
        #[getter]
        fn done(&self) -> u64 {
            self.done.load(Relaxed)
        }

        /// Ends a running `run` early (its result is then incomplete).
        fn stop(&self) {
            self.stop.store(true, Relaxed);
        }

        /// Runs the brute force from the start (blocks, without the GIL): sorted structure seeds.
        fn run(&self, py: Python<'_>) -> PyResult<Vec<i64>> {
            self.stop.store(false, Relaxed);
            self.done.store(0, Relaxed);
            let (w, p, stop, done, gpu) = (self.w.clone(), self.plan.clone(), self.stop.clone(), self.done.clone(), self.gpu);
            py.detach(move || {
                if gpu {
                    let g = crate::gpu::get().map_err(err)?;
                    return crate::crack::run_gpu(&w.structures, &p, &g, &stop, &done).map_err(err);
                }
                crate::crack::run(&w.structures, &p, &stop, &done).map_err(err)
            })
        }
    }

    #[pyclass(name = "Search", frozen)]
    struct PySearch {
        inner: Arc<Search>,
        job: Mutex<Option<JoinHandle<u64>>>,
        /// Run time: seconds of finished runs, and when the current run started. Resuming keeps counting.
        clock: Arc<Mutex<(f64, Option<Instant>)>>,
        resume: Mutex<u64>,
        gpu_note: String,
    }

    #[pymethods]
    impl PySearch {
        /// mode: "range" (start, count), "structure_seeds" (start, count), "list" (seeds),
        /// "structure_list" (seeds: structure seeds, each searched with its 65536 sisters),
        /// "random" (count, start = generator seed). natives: [(plugin name, library path)].
        /// gpu: pre-filter on the CUDA device when the query allows it (see `gpu_note`).
        /// shuffle: visit the units in a shuffled order with this key (no repeats, resumable).
        #[new]
        #[pyo3(signature = (world, query, mode, start=0, count=0, seeds=None, natives=vec![], max_results=0, gpu=false, shuffle=None))]
        #[allow(clippy::too_many_arguments)]
        fn new(world: &PyWorld, query: &str, mode: &str, start: i64, count: u64, seeds: Option<Vec<i64>>, natives: Vec<(String, String)>, max_results: u64, gpu: bool, shuffle: Option<u64>) -> PyResult<Self> {
            let q: Value = serde_json::from_str(query).map_err(|e| err(e.to_string()))?;
            let mode = match mode {
                "range" => Mode::Range { start, count },
                "structure_seeds" => Mode::StructureSeeds { start: start as u64, count },
                "list" => Mode::List(seeds.unwrap_or_default()),
                "structure_list" => Mode::StructureList(seeds.unwrap_or_default()),
                "random" => Mode::Random { count, rng_seed: start as u64 },
                m => return Err(err(format!("unknown mode '{m}'"))),
            };
            let mut s = Search::new(world.w.clone(), &q, mode, &natives).map_err(err)?;
            if max_results > 0 {
                s.max_results = max_results;
            }
            s.shuffle = shuffle;
            let gpu_note = if !gpu {
                String::new()
            } else {
                match s.use_gpu() {
                    Ok(name) => format!("GPU pre-filter on {name}"),
                    Err(e) => format!("GPU not used: {e}"),
                }
            };
            Ok(PySearch { inner: Arc::new(s), job: Mutex::new(None), clock: Arc::default(), resume: Mutex::new(0), gpu_note })
        }

        /// Total work units (seeds, or structure seeds in structure_seeds mode).
        #[getter]
        fn units(&self) -> u64 {
            self.inner.mode.units()
        }

        /// Runs the search for up to `seconds` (stop() ends it early); returns JSON with rate, pass
        /// rate, match rate and total-time estimate. Results found meanwhile are discarded.
        fn estimate(&self, py: Python<'_>, seconds: f64, threads: usize) -> PyResult<String> {
            if self.running() {
                return Err(PyRuntimeError::new_err("search already running"));
            }
            let s = self.inner.clone();
            *self.clock.lock().unwrap() = (0.0, None); // the estimate resets the counters too
            Ok(py.detach(move || s.estimate(seconds, threads).to_string()))
        }

        /// Starts (or resumes from `from_unit`) in a background thread.
        #[pyo3(signature = (threads, from_unit=0))]
        fn start(&self, threads: usize, from_unit: u64) -> PyResult<()> {
            let mut job = self.job.lock().unwrap();
            if job.as_ref().is_some_and(|j| !j.is_finished()) {
                return Err(PyRuntimeError::new_err("search already running"));
            }
            self.inner.stop.store(false, Relaxed);
            self.clock.lock().unwrap().1 = Some(Instant::now());
            let (s, clock) = (self.inner.clone(), self.clock.clone());
            let to = s.mode.units();
            *job = Some(std::thread::spawn(move || {
                let r = s.run(from_unit, to, threads);
                let mut c = clock.lock().unwrap();
                c.0 += c.1.take().map_or(0.0, |t| t.elapsed().as_secs_f64());
                r
            }));
            Ok(())
        }

        fn stop(&self) {
            self.inner.stop.store(true, Relaxed);
        }

        fn running(&self) -> bool {
            self.job.lock().unwrap().as_ref().is_some_and(|j| !j.is_finished())
        }

        /// Waits for the run to finish; returns the unit index to resume from.
        fn join(&self, py: Python<'_>) -> u64 {
            let job = self.job.lock().unwrap().take();
            if let Some(j) = job {
                let r = py.detach(move || j.join().unwrap_or(0));
                *self.resume.lock().unwrap() = r;
            }
            *self.resume.lock().unwrap()
        }

        /// Live counters for the UI (rates/ETA are derived in Python).
        fn stats(&self) -> std::collections::HashMap<&'static str, f64> {
            let st = &self.inner.stats;
            let c = *self.clock.lock().unwrap();
            let elapsed = c.0 + c.1.map_or(0.0, |t| t.elapsed().as_secs_f64());
            std::collections::HashMap::from([
                ("units_done", self.inner.units_done()),
                ("units_total", self.inner.mode.units() as f64),
                ("checks", st.checks.load(Relaxed) as f64),
                ("prefilter_passed", st.prefilter_passed.load(Relaxed) as f64),
                ("full_evals", st.full_evals.load(Relaxed) as f64),
                ("matched", st.matched.load(Relaxed) as f64),
                ("elapsed", elapsed),
            ])
        }

        /// Whether the GPU pre-filter is on, and on which device or why not ("" when not asked).
        #[getter]
        fn gpu_note(&self) -> String {
            self.gpu_note.clone()
        }

        /// The engine error that stopped the search, if any.
        #[getter]
        fn error(&self) -> Option<String> {
            self.inner.error.lock().unwrap().clone()
        }

        /// Drains matches found since the last call: [(seed, details JSON, score)].
        fn take_results(&self) -> Vec<(i64, String, f64)> {
            std::mem::take(&mut *self.inner.results.lock().unwrap())
                .into_iter()
                .map(|m| (m.seed, Value::Array(m.details).to_string(), m.score))
                .collect()
        }

        /// True when the query has maximize()/minimize(): results are the best `max_results`.
        #[getter]
        fn ranked(&self) -> bool {
            self.inner.cond.has_objective()
        }
    }

    /// (usable, device name or the reason it cannot be used). Initialises the device on first call.
    #[pyfunction]
    fn cuda_status(py: Python<'_>) -> (bool, String) {
        match py.detach(crate::gpu::status) {
            Ok(name) => (true, name),
            Err(e) => (false, e),
        }
    }

    #[pymodule]
    #[pyo3(name = "_engine")]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_class::<PyWorld>()?;
        m.add_class::<PySearch>()?;
        m.add_class::<PyCrack>()?;
        m.add("PLUGIN_ABI", search::ABI_VERSION)?;
        m.add_function(wrap_pyfunction!(cuda_status, m)?)?;
        m.add("CLIMATE_PARAMS", query::CLIMATE_PARAMS.to_vec())?;
        Ok(())
    }
}
