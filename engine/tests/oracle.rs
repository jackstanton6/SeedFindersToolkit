//! Parity against the real game (fixtures from tests/run_oracle.py). Ignored by default because
//! they need data/26.3 and cache/oracle; run with `cargo test --release -- --ignored`.

use serde_json::Value;
use sft_engine::density::{f32_of, Ctx};
use sft_engine::world::{BiomeCtx, Heightmap, Seeded, World};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn world() -> &'static World {
    static W: OnceLock<World> = OnceLock::new();
    W.get_or_init(|| World::load(&root().join("data/26.3")).expect("run `python -m sft data 26.3` first"))
}

fn fixtures(mode: &str) -> Vec<Value> {
    let dir = root().join("cache/oracle");
    let mut out = vec![];
    for e in std::fs::read_dir(&dir).expect("run tests/run_oracle.py first") {
        let p = e.unwrap().path();
        if p.file_name().unwrap().to_string_lossy().starts_with(&format!("{mode}-")) {
            out.push(serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap());
        }
    }
    assert!(!out.is_empty(), "no {mode} fixtures");
    out
}

#[test]
#[ignore]
fn climate_and_point_biomes_match() {
    let w = world();
    for f in fixtures("climate") {
        let seed = f["seed"].as_i64().unwrap();
        let s = Seeded::new(w, seed);
        let mut bc = BiomeCtx { ctx: Ctx::uncached(), last: None };
        let (mut bad_vals, mut bad_biomes, mut n) = (0, 0, 0);
        for p in f["points"].as_array().unwrap() {
            let p = p.as_array().unwrap();
            let (qx, qy, qz) = (p[0].as_i64().unwrap() as i32, p[1].as_i64().unwrap() as i32, p[2].as_i64().unwrap() as i32);
            let got = s.climate_at(&mut Ctx::uncached(), qx << 2, qy << 2, qz << 2);
            let want: Vec<f32> = (3..9).map(|i| f32_of(&p[i])).collect();
            if got.iter().zip(&want).any(|(a, b)| a.to_bits() != b.to_bits()) {
                if bad_vals < 5 {
                    eprintln!("seed {seed} q({qx},{qy},{qz}) climate got {got:?} want {want:?}");
                }
                bad_vals += 1;
            }
            let biome = w.biomes.name(s.biome(&mut bc, qx, qy, qz));
            if biome != p[9].as_str().unwrap() {
                if bad_biomes < 5 {
                    eprintln!("seed {seed} q({qx},{qy},{qz}) biome got {biome} want {}", p[9]);
                }
                bad_biomes += 1;
            }
            n += 1;
        }
        assert_eq!((bad_vals, bad_biomes), (0, 0), "seed {seed}: {bad_vals}/{n} climate, {bad_biomes}/{n} biome mismatches");
    }
}

#[test]
#[ignore]
fn chunk_biomes_match() {
    let w = world();
    for f in fixtures("chunkbiomes") {
        let seed = f["seed"].as_i64().unwrap();
        let s = Seeded::new(w, seed);
        for c in f["chunks"].as_array().unwrap() {
            let (cx, cz) = (c[0].as_i64().unwrap() as i32, c[1].as_i64().unwrap() as i32);
            let got = s.biomes_volume(&mut BiomeCtx::cached(), cx * 4, -16, cz * 4, 4, 96, 4);
            let want = c[2].as_array().unwrap();
            // oracle order: z, x, y loops == volume index order
            for (i, b) in want.iter().enumerate() {
                assert_eq!(w.biomes.name(got[i]), b.as_str().unwrap(), "seed {seed} chunk ({cx},{cz}) index {i}");
            }
        }
    }
}

#[test]
#[ignore]
fn structures_match() {
    use sft_engine::structure::Placement;
    let w = world();
    let st = &w.structures;
    for f in fixtures("structures") {
        let seed = f["seed"].as_i64().unwrap();
        let r = f["radius"].as_i64().unwrap() as i32;
        let mut s = Seeded::new(w, seed);
        // oracle rows: [set, cx, cz] or [set, cx, cz, structure, x, y, z]
        let mut want: Vec<String> = f["starts"].as_array().unwrap().iter().map(|v| v.to_string()).collect();
        let mut got = vec![];
        let rings: Vec<(i32, i32)> = f["rings"].as_array().unwrap().iter().map(|c| (c[0].as_i64().unwrap() as i32, c[1].as_i64().unwrap() as i32)).collect();
        for (i, set) in st.sets.iter().enumerate().filter(|(_, s)| s.possible) {
            let ring_chunks = match set.placement {
                Placement::Rings { count, .. } => {
                    let ours = st.stronghold_chunks(&s, i, count as usize);
                    assert_eq!(ours, rings, "seed {seed}: stronghold ring positions");
                    ours
                }
                Placement::Spread { .. } => vec![],
            };
            for cx in -r..=r {
                for cz in -r..=r {
                    let placed = match set.placement {
                        Placement::Spread { .. } => st.is_structure_chunk(i, seed, cx, cz),
                        Placement::Rings { .. } => ring_chunks.contains(&(cx, cz)),
                    };
                    if !placed {
                        continue;
                    }
                    let mut bc = BiomeCtx::cached();
                    let row = match st.generate_set(&mut s, &mut bc, i, cx, cz) {
                        Some(h) => serde_json::json!([set.name, cx, cz, st.structures[h.structure].name, h.pos.0, h.pos.1, h.pos.2]),
                        None => serde_json::json!([set.name, cx, cz]),
                    };
                    got.push(row.to_string());
                }
            }
        }
        want.sort();
        got.sort();
        let approx = |row: &String| row.contains("mineshaft");
        let missing: Vec<_> = want.iter().filter(|x| !got.contains(x) && !approx(x)).collect();
        let extra: Vec<_> = got.iter().filter(|x| !want.contains(x) && !approx(x)).collect();
        let approx_bad = want.iter().filter(|x| approx(x) && !got.contains(x)).count();
        eprintln!("seed {seed}: {} starts, mineshaft (approximate) mismatches: {approx_bad}", want.len());
        assert!(missing.is_empty() && extra.is_empty(), "seed {seed}\n  oracle-only: {missing:?}\n  engine-only: {extra:?}");
    }
}

#[test]
#[ignore]
fn piece_details_match() {
    let w = world();
    let st = &w.structures;
    let mut checked = 0;
    for f in fixtures("structures") {
        let seed = f["seed"].as_i64().unwrap();
        let mut s = Seeded::new(w, seed);
        for p in f["pieces"].as_array().unwrap() {
            let set_name = p[0].as_str().unwrap();
            let (cx, cz) = (p[1].as_i64().unwrap() as i32, p[2].as_i64().unwrap() as i32);
            let want_box: Vec<i32> = p[3].as_array().unwrap().iter().map(|v| v.as_i64().unwrap() as i32).collect();
            let (count, template) = (p[4].as_i64().unwrap() as i32, p[5].as_str().unwrap());
            let set = st.sets.iter().position(|x| x.name == set_name).unwrap();
            let h = st.generate_set(&mut s, &mut BiomeCtx::cached(), set, cx, cz).expect("oracle hit must generate");
            let name = &st.structures[h.structure].name;
            let tag = |k: &str| h.tags.iter().find_map(|t| t.strip_prefix(k).map(str::to_string));
            let ctx = format!("seed {seed} {name} chunk ({cx},{cz}) tags {:?}", h.tags);
            if let Some(b) = h.bbox {
                assert_eq!(b.to_vec(), want_box, "box: {ctx}");
            }
            if name.contains("igloo") {
                assert_eq!(h.tags.contains(&"basement".to_string()), count > 1, "{ctx}");
                if count > 1 {
                    assert_eq!(tag("ladders:"), Some((count - 1).to_string()), "{ctx}");
                }
            }
            if name.contains("shipwreck") || name.contains("ruined_portal") {
                assert_eq!(tag("template:").map(|t| format!("minecraft:{t}")), Some(template.to_string()), "{ctx}");
            }
            if name.contains("ocean_ruin") {
                assert_eq!(h.tags.contains(&"large".to_string()), template.contains("big"), "{ctx}");
                let ruins = if name.contains("cold") { count / 3 } else { count };
                assert_eq!(tag("ruins:").unwrap_or("1".into()), ruins.to_string(), "{ctx}");
            }
            checked += 1;
        }
    }
    eprintln!("{checked} piece rows checked");
}

#[test]
#[ignore]
fn heights_match() {
    let w = world();
    for f in fixtures("heights") {
        let seed = f["seed"].as_i64().unwrap();
        let mut s = Seeded::new(w, seed);
        let mut bad = 0;
        let all = f["heights"].as_array().unwrap();
        for h in all {
            let v: Vec<i32> = h.as_array().unwrap().iter().map(|x| x.as_i64().unwrap() as i32).collect();
            let ws = s.base_height(v[0], v[1], Heightmap::WorldSurfaceWg);
            let of = s.base_height(v[0], v[1], Heightmap::OceanFloorWg);
            if (ws, of) != (v[2], v[3]) {
                if bad < 10 {
                    eprintln!("seed {seed} ({},{}) got ws={ws} of={of} want ws={} of={}", v[0], v[1], v[2], v[3]);
                }
                bad += 1;
            }
        }
        assert_eq!(bad, 0, "seed {seed}: {bad}/{} height mismatches", all.len());
    }
}
