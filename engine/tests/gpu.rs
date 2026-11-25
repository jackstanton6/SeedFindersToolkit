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
            let prog = gpu::program(&w, &cond, false);
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
            let got = g.filter(&prog, None, mode, base, 0, n, shuffle).unwrap();
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
    assert_eq!(found, crack::run(st, &p, &AtomicBool::new(false), &AtomicU64::new(0)).unwrap());
    assert!(found.contains(&seed));
}

#[test]
#[ignore]
fn gpu_speed() {
    let w = world();
    let g = gpu::get().unwrap();
    let q = json!({"op": "and", "args": [s("village", 100), s("swamp_hut", 200), s("desert_pyramid", 300)]});
    let cond = parse(&w, &[], &q).unwrap();
    let prog = gpu::program(&w, &cond, false);
    for n in [1u64 << 20, 1 << 24] {
        let t = std::time::Instant::now();
        let k = g.filter(&prog, None, 0, 0, 0, n, None).unwrap().len();
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

/// The device climate (noises, samplers, R-tree) against Seeded::biome with a fresh context, on
/// random seeds and positions: zero mismatches; ties (the GPU says unsure) are counted and must be rare.
#[test]
#[ignore]
fn gpu_biomes_match_cpu() {
    let w = world();
    let g = gpu::get().expect("no usable CUDA device");
    let clim = g.upload_climate(&gpu::ClimateProgram::new(&w).unwrap()).unwrap();
    let mut r = sft_engine::rng::JavaRandom::new(7);
    use sft_engine::rng::Rng;
    let n = 200_000;
    let seeds: Vec<i64> = (0..n).map(|i| if i % 1000 == 0 { r.next_long() } else { 0 }).collect();
    let seeds: Vec<i64> = seeds.iter().scan(0, |last, &s| { if s != 0 { *last = s; } Some(*last) }).collect();
    let quarts: Vec<[i32; 3]> = (0..n).map(|_| [r.next_bound(20_000) - 10_000, r.next_bound(96) - 16, r.next_bound(20_000) - 10_000]).collect();
    let t = std::time::Instant::now();
    let got = g.biomes(&clim, &seeds, &quarts).unwrap();
    eprintln!("GPU: {n} biomes in {:.2}s", t.elapsed().as_secs_f64());
    let (wr, sr, qr) = (&w, &seeds, &quarts);
    let want: Vec<i32> = (0..n)
        .into_par_iter()
        .chunks(1000)
        .flat_map_iter(|ix| {
            let (seeds, quarts) = (sr, qr);
            let s = sft_engine::world::Seeded::new(wr, seeds[ix[0]]);
            ix.into_iter().map(move |i| {
                let q = quarts[i];
                s.biome(&mut sft_engine::world::BiomeCtx::cached(), q[0], q[1], q[2]) as i32
            }).collect::<Vec<_>>()
        })
        .collect();
    let unsure = got.iter().filter(|&&b| b < 0).count();
    let wrong: Vec<usize> = (0..n).filter(|&i| got[i] >= 0 && got[i] != want[i]).collect();
    eprintln!("{} mismatches, {unsure} unsure", wrong.len());
    for &i in wrong.iter().take(5) {
        eprintln!("  seed {} quart {:?}: gpu {} cpu {}", seeds[i], quarts[i], w.biomes.name(got[i] as u16), w.biomes.name(want[i] as u16));
    }
    assert!(wrong.is_empty());
    assert!(unsure * 1000 < n, "{unsure} unsure");
}

/// Biome atoms (start columns, biome(...)): every seed the CPU matches must survive the GPU, in range
/// and random modes. Prints how much more the GPU now rejects than the placement-only pre-check.
#[test]
#[ignore]
fn gpu_biome_atoms_keep_every_match() {
    let w = world();
    let g = gpu::get().expect("no usable CUDA device");
    let clim = g.upload_climate(&gpu::ClimateProgram::new(&w).unwrap()).unwrap();
    let b = |name: &str, within: i32| json!({"op": "biome", "name": name, "within": within});
    let cases = [
        s("desert_pyramid", 300),
        s("swamp_hut", 400),
        s("igloo", 600),
        s("monument", 800),
        s("jungle_pyramid", 500),
        json!({"op": "structure", "name": "shipwreck", "within": 200, "min": 2}),
        b("mushroom_fields", 1500),
        json!({"op": "biome", "name": "#is_jungle", "within": 400, "fraction": 0.2}),
        json!({"op": "biome", "name": "#is_ocean", "within": 300, "max": 0.1}),
        json!({"op": "and", "args": [s("village", 400), b("cherry_grove", 600)]}),
        json!({"op": "or", "args": [s("desert_pyramid", 300), b("ice_spikes", 500)]}),
    ];
    let n = 1u64 << 15;
    for (mode, base) in [(0, 1u64 << 33), (2, 99)] {
        for q in &cases {
            let cond = parse(&w, &[], q).unwrap();
            let prog = gpu::program(&w, &cond, true);
            assert!(gpu::uses_climate(&prog), "{q}");
            let seed_of = |i: u64| -> i64 {
                let u = i.wrapping_add(base);
                if mode == 2 {
                    let mut z = u.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                    (z ^ (z >> 31)) as i64
                } else {
                    u as i64
                }
            };
            let t = std::time::Instant::now();
            let got = g.filter(&prog, Some(&clim), mode, base, 0, n, None).unwrap();
            let gpu_s = t.elapsed().as_secs_f64();
            // placement only: the same program without the climate (biome checks pass)
            let pre = g.filter(&prog, None, mode, base, 0, n, None).unwrap();
            assert!(got.iter().all(|i| pre.binary_search(i).is_ok()), "biome stage kept seeds the placement stage rejected: {q}");
            let pre = pre.len();
            let matches: Vec<u32> = (0..n).into_par_iter().filter(|&i| Eval::new(&w, seed_of(i), &[]).eval(&cond)).map(|i| i as u32).collect();
            let missing: Vec<&u32> = matches.iter().filter(|i| got.binary_search(i).is_err()).collect();
            eprintln!("mode {mode} {q}: match {} gpu {} (placement only {pre}), {:.0} seeds/s", matches.len(), got.len(), n as f64 / gpu_s);
            assert!(missing.is_empty(), "GPU dropped matching seeds {:?}: {q}", missing.iter().take(5).map(|&&i| seed_of(i as u64)).collect::<Vec<_>>());
        }
    }
}

/// Seeds per second: the GPU filter (placement + biomes) vs the CPU's full check, on whole seeds.
#[test]
#[ignore]
fn gpu_biome_speed() {
    let w = world();
    let g = gpu::get().unwrap();
    let clim = g.upload_climate(&gpu::ClimateProgram::new(&w).unwrap()).unwrap();
    for q in [
        s("desert_pyramid", 300),
        json!({"op": "and", "args": [s("village", 400), {"op": "biome", "name": "cherry_grove", "within": 600}]}),
        json!({"op": "biome", "name": "mushroom_fields", "within": 1500}),
    ] {
        let cond = parse(&w, &[], &q).unwrap();
        let prog = gpu::program(&w, &cond, true);
        let n = 1u64 << 16;
        let t = std::time::Instant::now();
        let k = g.filter(&prog, Some(&clim), 0, 1 << 33, 0, n, None).unwrap().len();
        let gpu_rate = n as f64 / t.elapsed().as_secs_f64();
        let m = 1u64 << 12;
        let t = std::time::Instant::now();
        let c = (0..m).into_par_iter().filter(|&i| Eval::new(&w, (i + (1 << 33)) as i64, &[]).pre(&cond) && Eval::new(&w, (i + (1 << 33)) as i64, &[]).eval(&cond)).count();
        let cpu_rate = m as f64 / t.elapsed().as_secs_f64();
        eprintln!("{q}: GPU {gpu_rate:.0}/s ({k} of {n} pass), CPU pre+eval {cpu_rate:.0}/s ({c} of {m} match), x{:.1}", gpu_rate / cpu_rate);
    }
}
