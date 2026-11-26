//! GeodeFeature (literal port).

use crate::blocks::State;
use crate::feature::{add, rel, IntP, Level, Provider, P};
use crate::rng::{Rng, WorldgenXoro};
use crate::world::World;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub struct Geode {
    filling: Provider,
    inner: Provider,
    alternate: Provider,
    middle: Provider,
    outer: Provider,
    placements: Vec<State>,
    cannot_replace: Arc<Vec<bool>>,
    invalid: Arc<Vec<bool>>,
    /// filling, inner layer, middle layer, outer layer
    layers: [f64; 4],
    crack_chance: f64,
    base_crack: f64,
    crack_offset: i32,
    potential_chance: f64,
    alternate_chance: f64,
    require_alternate: bool,
    outer_wall: IntP,
    outer_wall_max: i32,
    points: IntP,
    point_offset: IntP,
    min_off: i32,
    max_off: i32,
    noise_mul: f64,
    invalid_threshold: i32,
}

impl Geode {
    pub fn parse(v: &Value, w: &World) -> Result<Geode, String> {
        let b = &v["blocks"];
        let prov = |k: &str| Provider::parse(&b[k], w);
        let f = |o: &Value, k: &str, d: f64| o[k].as_f64().unwrap_or(d);
        let ip = |k: &str, a: i32, c: i32| if v.get(k).is_some() { IntP::parse(&v[k]) } else { Ok(IntP::Uniform(a, c)) };
        let outer_wall = ip("outer_wall_distance", 4, 5)?;
        let outer_wall_max = match outer_wall {
            IntP::Uniform(_, m) | IntP::Const(m) => m,
            _ => 5,
        };
        Ok(Geode {
            filling: prov("filling_provider")?,
            inner: prov("inner_layer_provider")?,
            alternate: prov("alternate_inner_layer_provider")?,
            middle: prov("middle_layer_provider")?,
            outer: prov("outer_layer_provider")?,
            placements: b["inner_placements"].as_array().ok_or("inner_placements")?.iter().map(|x| w.blocks.parse(x)).collect::<Result<_, _>>()?,
            cannot_replace: crate::feature::block_set(w, &b["cannot_replace"])?,
            invalid: crate::feature::block_set(w, &b["invalid_blocks"])?,
            layers: [f(&v["layers"], "filling", 1.7), f(&v["layers"], "inner_layer", 2.2), f(&v["layers"], "middle_layer", 3.2), f(&v["layers"], "outer_layer", 4.2)],
            crack_chance: f(&v["crack"], "generate_crack_chance", 1.0),
            base_crack: f(&v["crack"], "base_crack_size", 2.0),
            crack_offset: v["crack"]["crack_point_offset"].as_i64().unwrap_or(2) as i32,
            potential_chance: v["use_potential_placements_chance"].as_f64().unwrap_or(0.35),
            alternate_chance: v["use_alternate_layer0_chance"].as_f64().unwrap_or(0.0),
            require_alternate: v["placements_require_layer0_alternate"].as_bool().unwrap_or(true),
            outer_wall,
            outer_wall_max,
            points: ip("distribution_points", 3, 4)?,
            point_offset: ip("point_offset", 1, 2)?,
            min_off: v["min_gen_offset"].as_i64().unwrap_or(-16) as i32,
            max_off: v["max_gen_offset"].as_i64().unwrap_or(16) as i32,
            noise_mul: v["noise_multiplier"].as_f64().unwrap_or(0.05),
            invalid_threshold: v["invalid_blocks_threshold"].as_i64().unwrap_or(0) as i32,
        })
    }
}

/// GeodeFeature.NOISE_PARAMETERS = NormalNoise.createParity(-4, 1.0), created per seed.
fn geode_noise(seed: i64) -> Arc<crate::noise::NoiseStack> {
    static CACHE: Mutex<Option<(i64, Arc<crate::noise::NoiseStack>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap();
    if let Some((s, n)) = c.as_ref() {
        if *s == seed {
            return n.clone();
        }
    }
    let params = crate::noise::NormalNoise::from_json("", &serde_json::json!({"base_octave": -4, "base_amplitude": 1.0, "normalize": "parity"}));
    let n = Arc::new(params.create(&mut crate::rng::AnyRng::J(crate::rng::JavaRandom::new(seed))));
    *c = Some((seed, n.clone()));
    n
}

/// Vec3i.distSqr
fn dist_sqr(a: P, b: P) -> f64 {
    let (dx, dy, dz) = ((a.0 - b.0) as f64, (a.1 - b.1) as f64, (a.2 - b.2) as f64);
    dx * dx + dy * dy + dz * dz
}

/// Feature.safeSetBlock
fn safe_set(l: &mut Level, p: P, s: State, cannot: &[bool]) {
    if !l.w.blocks.in_table(cannot, l.get(p)) {
        l.set(p, s);
    }
}

/// GeodeFeature.place
pub fn geode(l: &mut Level, r: &mut WorldgenXoro, g: &Geode, o: P) -> bool {
    let mut points: Vec<(P, i32)> = vec![];
    let n = g.points.sample(r);
    let noise = geode_noise(l.s.seed);
    let mut cracks: Vec<P> = vec![];
    let adj = n as f64 / g.outer_wall_max as f64;
    let inner_air = 1.0 / g.layers[0].sqrt();
    let innermost = 1.0 / (g.layers[1] + adj).sqrt();
    let inner_crust = 1.0 / (g.layers[2] + adj).sqrt();
    let outer_crust = 1.0 / (g.layers[3] + adj).sqrt();
    let crack_size = 1.0 / (g.base_crack + r.next_double() / 2.0 + if n > 3 { adj } else { 0.0 }).sqrt();
    let crack = (r.next_float() as f64) < g.crack_chance;
    let mut invalid = 0;
    for _ in 0..n {
        let x = g.outer_wall.sample(r);
        let y = g.outer_wall.sample(r);
        let z = g.outer_wall.sample(r);
        let p = add(o, (x, y, z));
        let s = l.get(p);
        if l.w.blocks.is_air(s) || l.w.blocks.in_table(&g.invalid, s) {
            invalid += 1;
            if invalid > g.invalid_threshold {
                return false;
            }
        }
        points.push((p, g.point_offset.sample(r)));
    }
    if crack {
        let c = n * 2 + 1;
        let (dx, dz) = match r.next_bound(4) {
            0 => (c, 0),
            1 => (0, c),
            2 => (c, c),
            _ => (0, 0),
        };
        for dy in [7, 5, 1] {
            cracks.push(add(o, (dx, dy, dz)));
        }
    }
    let mut potential = vec![];
    // BlockPos.betweenClosed: x fastest, then y, then z
    for z in o.2 + g.min_off..=o.2 + g.max_off {
        for y in o.1 + g.min_off..=o.1 + g.max_off {
            for x in o.0 + g.min_off..=o.0 + g.max_off {
                let p = (x, y, z);
                let noff = noise.get(x as f64, y as f64, z as f64) as f64 * g.noise_mul;
                let mut shell = 0.0;
                for &(q, off) in &points {
                    shell += 1.0 / (dist_sqr(p, q) + off as f64).sqrt() + noff;
                }
                if shell < outer_crust {
                    continue;
                }
                if shell >= inner_air {
                    let s = g.filling.state(l, r, p);
                    safe_set(l, p, s, &g.cannot_replace);
                    continue;
                }
                let mut crack_sum = 0.0;
                for &q in &cracks {
                    crack_sum += 1.0 / (dist_sqr(p, q) + g.crack_offset as f64).sqrt() + noff;
                }
                if crack && crack_sum >= crack_size {
                    let air = l.w.blocks.air;
                    safe_set(l, p, air, &g.cannot_replace);
                } else if shell >= innermost {
                    let alt = (r.next_float() as f64) < g.alternate_chance;
                    let s = if alt { g.alternate.state(l, r, p) } else { g.inner.state(l, r, p) };
                    safe_set(l, p, s, &g.cannot_replace);
                    if (!g.require_alternate || alt) && (r.next_float() as f64) < g.potential_chance {
                        potential.push(p);
                    }
                } else if shell >= inner_crust {
                    let s = g.middle.state(l, r, p);
                    safe_set(l, p, s, &g.cannot_replace);
                } else if shell >= outer_crust {
                    let s = g.outer.state(l, r, p);
                    safe_set(l, p, s, &g.cannot_replace);
                }
            }
        }
    }
    let names = ["down", "up", "north", "south", "west", "east"];
    for c in potential {
        let mut s = g.placements[r.next_bound(g.placements.len() as i32) as usize];
        for (d, name) in names.iter().enumerate() {
            let bl = &l.w.blocks;
            if bl.get(s, "facing").is_some() {
                s = bl.with(s, "facing", name).unwrap_or(s);
            }
            let q = rel(c, d);
            let ps = l.get(q);
            if bl.get(s, "waterlogged").is_some() {
                let src = bl.fluid(ps).2;
                s = bl.with(s, "waterlogged", if src { "true" } else { "false" }).unwrap_or(s);
            }
            // BuddingAmethystBlock.canClusterGrowAtState
            if bl.is_air(ps) || (bl.name(ps) == "minecraft:water" && bl.fluid(ps).1 == 8) {
                safe_set(l, q, s, &g.cannot_replace);
                break;
            }
        }
    }
    true
}
