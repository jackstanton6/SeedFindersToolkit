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
        let mut bc = BiomeCtx::uncached();
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
fn block_biomes_match() {
    let w = world();
    for f in fixtures("blockbiomes") {
        let seed = f["seed"].as_i64().unwrap();
        let s = Seeded::new(w, seed);
        let mut bc = BiomeCtx::cached();
        let mut bad = 0;
        let pts = f["points"].as_array().unwrap();
        for p in pts {
            let (x, y, z) = (p[0].as_i64().unwrap() as i32, p[1].as_i64().unwrap() as i32, p[2].as_i64().unwrap() as i32);
            let got = w.biomes.name(s.block_biome(&mut bc, x, y, z));
            if got != p[3].as_str().unwrap() {
                if bad < 10 {
                    eprintln!("seed {seed} ({x},{y},{z}) got {got} want {}", p[3]);
                }
                bad += 1;
            }
        }
        assert_eq!(bad, 0, "seed {seed}: {bad}/{} block biome mismatches", pts.len());
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

/// Chest contents from the real server (tests/loot_oracle.py fill): chests placed with explicit
/// seeds and unpacked. Maps are compared by item only.
#[test]
#[ignore]
fn loot_fill_matches() {
    let w = world();
    let loot = w.loot.as_ref().expect("loot data");
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(root().join("cache/oracle/loot-fill.json")).expect("run tests/loot_oracle.py fill")).unwrap();
    let mut bad = 0;
    for c in &cases {
        let (table, seed) = (c["table"].as_str().unwrap(), c["seed"].as_i64().unwrap());
        let got: Vec<Value> = loot
            .fill(table, seed, None)
            .unwrap()
            .into_iter()
            .map(|s| {
                s.map_or(Value::Null, |mut s| {
                    s.enchants.sort();
                    // defaults are not saved: damage 0, the goat horn's own instrument
                    s.extra.retain(|e| !e.starts_with("map:") && e != "damage:0" && e != "instrument:minecraft:ponder_goat_horn");
                    s.extra.sort();
                    s.to_json()
                })
            })
            .collect();
        let want = c["slots"].as_array().unwrap();
        if got != *want {
            if bad < 3 {
                let diff: Vec<_> = (0..got.len().min(want.len())).filter(|&i| got[i] != want[i]).map(|i| format!("slot {i}: got {} want {}", got[i], want[i])).collect();
                eprintln!("{table} seed {seed}:\n  {}", diff.join("\n  "));
            }
            bad += 1;
        }
    }
    assert_eq!(bad, 0, "{bad}/{} chests differ", cases.len());
}

/// Structure chests in real worlds (tests/loot_oracle.py world): every chest the engine predicts
/// must exist at its (x, z) with the same loot table and LootTableSeed. y is not compared.
#[test]
#[ignore]
fn structure_chests_match() {
    let w = world();
    let (mut checked, mut bad, mut inexact, mut inexact_ok) = (0, 0, 0, 0);
    let mut by_name = std::collections::BTreeMap::new();
    for f in fixtures("loot-world") {
        let seed = f["seed"].as_i64().unwrap();
        let r = f["radius"].as_i64().unwrap() as i32;
        let real: Vec<(i32, i32, String, i64)> = f["chests"].as_array().unwrap().iter().map(|c| {
            (c[0].as_i64().unwrap() as i32, c[2].as_i64().unwrap() as i32, c[4].as_str().unwrap().to_string(), c[5].as_i64().unwrap())
        }).collect();
        let mut ev = sft_engine::query::Eval::new(w, seed, &[]);
        for name in ["buried_treasure", "ruined_portal", "shipwreck", "igloo", "desert_pyramid", "ocean_ruin", "jungle_temple"] {
            let (set, only) = w.structures.resolve(name).unwrap();
            for h in ev.instances(set, only, (0, 0), r) {
                for c in &h.chests {
                    // same column and table (suspicious sand/gravel can share the column)
                    let same: Vec<_> = real.iter().filter(|x| (x.0, x.1) == (c.pos.0, c.pos.2) && x.2 == c.table).collect();
                    let found = same.first();
                    let ok = same.iter().any(|x| x.3 == c.seed);
                    if !c.exact {
                        inexact += 1;
                        inexact_ok += ok as i32;
                        continue;
                    }
                    *by_name.entry(name).or_insert(0) += 1;
                    if !ok {
                        if bad < 10 {
                            eprintln!("seed {seed} {} at {:?} tags {:?}: engine {} {} real {found:?}", w.structures.structures[h.structure].name, c.pos, h.tags, c.table, c.seed);
                        }
                        bad += 1;
                    }
                    checked += 1;
                }
            }
        }
    }
    eprintln!("{checked} chests checked {by_name:?}; {inexact_ok}/{inexact} inexact (beached) ones match too");
    assert!(checked > 0);
    assert_eq!(bad, 0, "{bad}/{checked} chest seeds differ");
}

/// Ruined portal obsidian after BlockAgeProcessor (some turns crying): plain obsidian inside each
/// portal box. Boxes holding more obsidian than the template (another structure overlaps) are skipped.
#[test]
#[ignore]
fn portal_obsidian_matches() {
    let w = world();
    let (mut checked, mut bad, mut skipped, mut giant) = (0, 0, 0, 0);
    for f in fixtures("loot-world") {
        let seed = f["seed"].as_i64().unwrap();
        let real: Vec<Vec<i64>> = f["obsidian"].as_array().unwrap().iter().map(|o| {
            o.as_array().unwrap().iter().map(|v| v.as_i64().unwrap_or(v.as_bool().unwrap_or(false) as i64)).collect()
        }).collect();
        let mut ev = sft_engine::query::Eval::new(w, seed, &[]);
        let (set, only) = w.structures.resolve("ruined_portal").unwrap();
        for h in ev.instances(set, only, (0, 0), f["radius"].as_i64().unwrap() as i32) {
            let b = h.bbox.unwrap();
            let inside = |o: &&Vec<i64>| (b[0]..=b[3]).contains(&(o[0] as i32)) && (b[1]..=b[4]).contains(&(o[1] as i32)) && (b[2]..=b[5]).contains(&(o[2] as i32));
            let want = real.iter().filter(inside).filter(|o| o[3] == 0).count();
            let got = h.tags.iter().find_map(|t| t.strip_prefix("obsidian:")).and_then(|v| v.parse::<usize>().ok()).unwrap();
            let template = h.tags.iter().find_map(|t| t.strip_prefix("template:")).unwrap();
            let size = [("portal_1", 11), ("portal_2", 11), ("portal_3", 11), ("portal_4", 11), ("portal_5", 15), ("portal_6", 16), ("portal_7", 12),
                        ("portal_8", 17), ("portal_9", 12), ("portal_10", 13), ("giant_portal_1", 31), ("giant_portal_2", 29), ("giant_portal_3", 25)];
            let total = size.iter().find(|s| template.ends_with(&format!("/{}", s.0))).unwrap().1;
            if real.iter().filter(inside).count() > total {
                skipped += 1;
                continue;
            }
            // frame completeness from the real blocks (world coordinates, so this also checks the placement transform)
            let plain: Vec<[i32; 3]> = real.iter().filter(inside).filter(|o| o[3] == 0).map(|o| [o[0] as i32, o[1] as i32, o[2] as i32]).collect();
            let frame = h.tags.iter().find_map(|t| t.strip_prefix("frame:")).map(|v| v.parse::<usize>().unwrap());
            let want_frame = frame.map(|_| sft_engine::structure::frame_value(&sft_engine::structure::portal_frames(&plain), |_| true));
            if got != want || frame != want_frame {
                if bad < 10 {
                    eprintln!("seed {seed} portal {:?} {:?}: engine obsidian {got} frame {frame:?}, real {want} {want_frame:?}", h.pos, h.tags);
                }
                bad += 1;
            }
            checked += 1;
            giant += template.contains("giant") as usize;
        }
    }
    eprintln!("{checked} portals checked ({giant} giant), {skipped} skipped (foreign obsidian in the box)");
    assert!(checked > 0);
    assert_eq!(bad, 0, "{bad}/{checked} portals differ");
}

#[test]
#[ignore]
fn slime_chunks_match() {
    for f in fixtures("slime") {
        let seed = f["seed"].as_i64().unwrap();
        for c in f["chunks"].as_array().unwrap() {
            let (x, z) = (c[0].as_i64().unwrap() as i32, c[1].as_i64().unwrap() as i32);
            assert_eq!(sft_engine::query::is_slime_chunk(seed, x, z), c[2].as_bool().unwrap(), "seed {seed} chunk ({x},{z})");
        }
    }
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
