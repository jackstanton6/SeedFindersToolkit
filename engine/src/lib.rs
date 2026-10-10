//! Seed Finders Toolkit engine: bit-exact Minecraft world generation and parallel seed search.

// Loops and comparisons mirror the Java source on purpose (evaluation order, NaN behaviour).
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::manual_range_contains, clippy::needless_range_loop, clippy::manual_memcpy)]

pub mod biome;
pub mod density;
pub mod noise;
pub mod rng;
pub mod search;
pub mod structure;
pub mod world;

/// Python bindings (`sft._engine`), built by maturin with `--features python`.
#[cfg(feature = "python")]
mod py {
    use crate::search::{self, Eval, Mode, Search};
    use crate::world::{BiomeCtx, Heightmap, Seeded, World};
    use pyo3::exceptions::{PyRuntimeError, PyValueError};
    use pyo3::prelude::*;
    use rayon::prelude::*;
    use serde_json::Value;
    use std::sync::atomic::Ordering::Relaxed;
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

        /// Surface heights (WORLD_SURFACE_WG) on the same grid layout as biome_map.
        fn height_map(&self, py: Python<'_>, seed: i64, x0: i32, z0: i32, w: i32, h: i32, scale: i32) -> Vec<i32> {
            let world = self.w.clone();
            py.detach(move || {
                let mut s = Seeded::new(&world, seed);
                s.prepare_terrain();
                (0..w * h).into_par_iter().map(|k| s.height_at(x0 + (k % w) * scale, z0 + (k / w) * scale, Heightmap::WorldSurfaceWg)).collect()
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

        /// Evaluates a query for one seed: details JSON if it matches, else None.
        fn check(&self, seed: i64, query: &str) -> PyResult<Option<String>> {
            let q: Value = serde_json::from_str(query).map_err(|e| err(e.to_string()))?;
            let cond = search::parse(&self.w, &[], &q).map_err(err)?;
            let mut ev = Eval::new(&self.w, seed, &[]);
            Ok(ev.eval(&cond).then(|| Value::Array(ev.details).to_string()))
        }
    }

    #[pyclass(name = "Search", frozen)]
    struct PySearch {
        inner: Arc<Search>,
        job: Mutex<Option<JoinHandle<u64>>>,
        started: Mutex<Option<Instant>>,
        resume: Mutex<u64>,
    }

    #[pymethods]
    impl PySearch {
        /// mode: "range" (start, count), "structure_seeds" (start, count), "list" (seeds),
        /// "random" (count, start = generator seed). natives: [(plugin name, library path)].
        #[new]
        #[pyo3(signature = (world, query, mode, start=0, count=0, seeds=None, natives=vec![], max_results=0))]
        #[allow(clippy::too_many_arguments)]
        fn new(world: &PyWorld, query: &str, mode: &str, start: i64, count: u64, seeds: Option<Vec<i64>>, natives: Vec<(String, String)>, max_results: u64) -> PyResult<Self> {
            let q: Value = serde_json::from_str(query).map_err(|e| err(e.to_string()))?;
            let mode = match mode {
                "range" => Mode::Range { start, count },
                "structure_seeds" => Mode::StructureSeeds { start: start as u64, count },
                "list" => Mode::List(seeds.unwrap_or_default()),
                "random" => Mode::Random { count, rng_seed: start as u64 },
                m => return Err(err(format!("unknown mode '{m}'"))),
            };
            let mut s = Search::new(world.w.clone(), &q, mode, &natives).map_err(err)?;
            if max_results > 0 {
                s.max_results = max_results;
            }
            Ok(PySearch { inner: Arc::new(s), job: Mutex::new(None), started: Mutex::new(None), resume: Mutex::new(0) })
        }

        /// Total work units (seeds, or structure seeds in structure_seeds mode).
        #[getter]
        fn units(&self) -> u64 {
            self.inner.mode.units()
        }

        /// Runs a short calibration sample; returns JSON with rate, pass rate and total-time estimate.
        fn estimate(&self, py: Python<'_>, sample: u64, threads: usize) -> String {
            let s = self.inner.clone();
            py.detach(move || s.estimate(sample, threads).to_string())
        }

        /// Starts (or resumes from `from_unit`) in a background thread.
        #[pyo3(signature = (threads, from_unit=0))]
        fn start(&self, threads: usize, from_unit: u64) -> PyResult<()> {
            let mut job = self.job.lock().unwrap();
            if job.as_ref().is_some_and(|j| !j.is_finished()) {
                return Err(PyRuntimeError::new_err("search already running"));
            }
            self.inner.stop.store(false, Relaxed);
            *self.started.lock().unwrap() = Some(Instant::now());
            let s = self.inner.clone();
            let to = s.mode.units();
            *job = Some(std::thread::spawn(move || s.run(from_unit, to, threads)));
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
            let elapsed = self.started.lock().unwrap().map_or(0.0, |t| t.elapsed().as_secs_f64());
            std::collections::HashMap::from([
                ("units_done", st.units_done.load(Relaxed) as f64),
                ("units_total", self.inner.mode.units() as f64),
                ("checks", st.checks.load(Relaxed) as f64),
                ("prefilter_passed", st.prefilter_passed.load(Relaxed) as f64),
                ("full_evals", st.full_evals.load(Relaxed) as f64),
                ("matched", st.matched.load(Relaxed) as f64),
                ("elapsed", elapsed),
            ])
        }

        /// Drains matches found since the last call: [(seed, details JSON)].
        fn take_results(&self) -> Vec<(i64, String)> {
            std::mem::take(&mut *self.inner.results.lock().unwrap())
                .into_iter()
                .map(|m| (m.seed, Value::Array(m.details).to_string()))
                .collect()
        }
    }

    #[pymodule]
    #[pyo3(name = "_engine")]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_class::<PyWorld>()?;
        m.add_class::<PySearch>()?;
        m.add("PLUGIN_ABI", search::ABI_VERSION)?;
        m.add("CLIMATE_PARAMS", search::CLIMATE_PARAMS.to_vec())?;
        Ok(())
    }
}
