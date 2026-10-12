//! GPU pre-filter parity (needs data/26.3 and a CUDA device): `cargo test --release --test gpu -- --ignored`.
//! The kernel must keep exactly the seeds the CPU pre-check keeps, except where lowering is
//! deliberately weaker (exclusion zones), where it must keep a superset.

use rayon::prelude::*;
use serde_json::{json, Value};
use sft_engine::query::{parse, Eval};
use sft_engine::search::permute;
use sft_engine::world::World;
use sft_engine::{crack, gpu};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn world() -> World {
    World::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/26.3")).expect("run `python -m sft data 26.3` first")
}

fn s(name: &str, within: i32) -> Value {
    json!({"op": "structure", "name": name, "within": within})
}

#[test]
#[ignore]
fn gpu_prefilter_matches_cpu() {
    let w = world();
    let g = gpu::get().expect("no usable CUDA device");
    eprintln!("GPU: {}", g.name);
    // (query, exact): exact = no exclusion zones, so GPU and CPU keep the same seeds
    let cases = [
        (s("village", 300), true),
        (json!({"op": "and", "args": [{"op": "structure", "name": "village", "within": 600, "min": 2}, s("swamp_hut", 1000)]}), true),
        (s("buried_treasure", 200), true),      // legacy_type_2
        (s("mineshaft", 120), true),            // legacy_type_3
        (s("pillager_outpost", 400), false),    // legacy_type_1 + exclusion zone
        (s("monument", 1000), true),            // triangular
        (s("mansion", 6000), true),             // triangular, wide
        (json!({"op": "structure", "name": "ruined_portal", "at": [-3000, 1700], "within": 250}), true),
        (json!({"op": "slime", "radius": 64, "min": 7}), true),
        (json!({"op": "slime", "radius": 24, "within": 100, "min": 3}), true),
        (json!({"op": "or", "args": [s("desert_pyramid", 200), s("jungle_pyramid", 200)]}), true),
        // cluster: the GPU checks pair distances only, so it keeps a superset
        (json!({"op": "cluster", "names": "swamp_hut", "count": 3, "dist": 128, "within": 3000, "afk": true}), false),
        (json!({"op": "cluster", "names": "swamp_hut", "count": 2, "dist": 100, "within": 1500}), false),
        (json!({"op": "cluster", "names": "village", "count": 3, "dist": 200, "within": 2000}), false),
    ];
    let n = 1u64 << 20;
    let shuffled = Some((3_000_000_007u64, 0xDEAD_BEEF_1234));
    for (mode, base, shuffle) in [(1, 0x1234_5678_9ABCu64, None), (0, (-(1i64 << 40)) as u64, None), (2, 42, None), (0, 1 << 40, shuffled), (1, 0, shuffled)] {
        for (q, exact) in &cases {
            let cond = parse(&w, &[], q).unwrap();
            let prog = gpu::program(&w, &cond);
            assert!(!prog.is_empty(), "{q}");
            let seed_of = |i: u64| -> i64 {
                let u = shuffle.map_or(i, |(t, k)| permute(i, t, k)).wrapping_add(base);
                match mode {
                    1 => (u & ((1 << 48) - 1)) as i64,
                    2 => {
                        // search.rs splitmix
                        let mut z = u.wrapping_add(0x9E37_79B9_7F4A_7C15);
                        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                        (z ^ (z >> 31)) as i64
                    }
                    _ => u as i64,
                }
            };
            let cpu: Vec<u32> = (0..n).into_par_iter().filter(|&i| Eval::new(&w, seed_of(i), &[]).pre(&cond)).map(|i| i as u32).collect();
            let got = g.filter(&prog, mode, base, 0, n, shuffle).unwrap();
            let missing = cpu.iter().filter(|i| got.binary_search(i).is_err()).count();
            eprintln!("mode {mode} shuffle {} {q}: cpu {} gpu {}", shuffle.is_some(), cpu.len(), got.len());
            assert_eq!(missing, 0, "GPU dropped seeds the CPU keeps: {q}");
            if *exact {
                assert_eq!(cpu, got, "{q}");
            }
        }
    }
}

#[test]
#[ignore]
fn gpu_cracks_own_structures() {
    let w = world();
    let st = &w.structures;
    let seed = 0x1234_5678_9ABCi64;
    let mut obs = vec![];
    for name in ["swamp_huts", "pillager_outposts", "desert_pyramids", "shipwrecks", "villages"] {
        let set = st.sets.iter().position(|s| s.name.ends_with(name)).unwrap();
        for (gx, gz) in [(0, 0), (1, -1)] {
            let c = st.grid_chunk(set, seed, gx, gz).unwrap();
            if st.is_structure_chunk(set, seed, c.0, c.1) {
                obs.push(crack::Obs { set, chunk: c });
            }
        }
    }
    let p = crack::plan(st, &obs).unwrap();
    let t = std::time::Instant::now();
    let found = crack::run_gpu(st, &p, &gpu::get().unwrap(), &AtomicBool::new(false), &AtomicU64::new(0)).unwrap();
    eprintln!("{} observations, work {} seeds, {:?}", obs.len(), p.work, t.elapsed());
    assert_eq!(found, crack::run(st, &p, &AtomicBool::new(false), &AtomicU64::new(0)));
    assert!(found.contains(&seed));
}

#[test]
#[ignore]
fn gpu_speed() {
    let w = world();
    let g = gpu::get().unwrap();
    let q = json!({"op": "and", "args": [s("village", 100), s("swamp_hut", 200), s("desert_pyramid", 300)]});
    let cond = parse(&w, &[], &q).unwrap();
    let prog = gpu::program(&w, &cond);
    for n in [1u64 << 20, 1 << 24] {
        let t = std::time::Instant::now();
        let k = g.filter(&prog, 0, 0, 0, n, None).unwrap().len();
        eprintln!("GPU {n}: {k} pass, {:.0}/s", n as f64 / t.elapsed().as_secs_f64());
    }
    let t = std::time::Instant::now();
    let n = 1u64 << 22;
    let k = (0..n).into_par_iter().filter(|&i| Eval::new(&w, i as i64, &[]).pre(&cond)).count();
    eprintln!("CPU pre {n}: {k} pass, {:.0}/s", n as f64 / t.elapsed().as_secs_f64());
    let t = std::time::Instant::now();
    let k = (0..n).into_par_iter().filter(|&i| Eval::new(&w, i as i64, &[]).pre(&cond) && Eval::new(&w, i as i64, &[]).eval(&cond)).count();
    eprintln!("CPU pre+eval {n}: {k} match, {:.0}/s", n as f64 / t.elapsed().as_secs_f64());
}
