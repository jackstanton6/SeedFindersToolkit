//! VegetationPatchFeature and WaterloggedVegetationPatchFeature (literal ports), and Java
//! HashSet<BlockPos> iteration order (the order vegetation is placed in).

use crate::feature::{place_nested, rel, Ctx, IntP, Level, PlacedFeature, Provider, P};
use crate::rng::{Rng, WorldgenXoro};
use crate::world::World;
use serde_json::Value;
use std::sync::Arc;

pub struct Patch {
    replaceable: Arc<Vec<bool>>,
    ground: Provider,
    vegetation: Arc<PlacedFeature>,
    /// CaveSurface direction: down (floor) or up (ceiling)
    inwards: usize,
    depth: IntP,
    extra_bottom: f32,
    vertical_range: i32,
    vegetation_chance: f32,
    xz_radius: IntP,
    extra_edge: f32,
    waterlogged: bool,
}

impl Patch {
    pub fn parse(v: &Value, w: &World, reg: &crate::feature::Registry, waterlogged: bool) -> Result<Patch, String> {
        let f = |k: &str| crate::density::f32_of(&v[k]);
        Ok(Patch {
            replaceable: crate::feature::block_set(w, &v["replaceable"])?,
            ground: Provider::parse(&v["ground_state"], w)?,
            vegetation: reg.placed(w, &v["vegetation_feature"])?,
            inwards: if v["surface"].as_str() == Some("ceiling") { 1 } else { 0 },
            depth: IntP::parse(&v["depth"])?,
            extra_bottom: f("extra_bottom_block_chance"),
            vertical_range: v["vertical_range"].as_i64().unwrap_or(0) as i32,
            vegetation_chance: f("vegetation_chance"),
            xz_radius: IntP::parse(&v["xz_radius"])?,
            extra_edge: f("extra_edge_column_chance"),
            waterlogged,
        })
    }
}

/// The iteration order of a java.util.HashSet<BlockPos> holding `items` (in insertion order):
/// by bucket (hash ^ hash >>> 16, table grown from 16 at load 0.75), then insertion order.
pub fn java_hash_order(items: &[P]) -> Vec<P> {
    let mut uniq: Vec<P> = vec![];
    for &p in items {
        if !uniq.contains(&p) {
            uniq.push(p);
        }
    }
    let mut cap = 16usize;
    while uniq.len() as f64 > cap as f64 * 0.75 {
        cap *= 2;
    }
    let bucket = |p: &P| {
        let h = (p.1.wrapping_add(p.2.wrapping_mul(31))).wrapping_mul(31).wrapping_add(p.0);
        ((h ^ ((h as u32) >> 16) as i32) as u32 as usize) & (cap - 1)
    };
    let mut idx: Vec<(usize, usize)> = uniq.iter().enumerate().map(|(i, p)| (bucket(p), i)).collect();
    idx.sort();
    idx.into_iter().map(|(_, i)| uniq[i]).collect()
}

fn ground_patch(l: &mut Level, r: &mut WorldgenXoro, g: &Patch, o: P, xr: i32, zr: i32) -> Vec<P> {
    let b = &l.w.blocks;
    let (inwards, outwards) = (g.inwards, g.inwards ^ 1);
    let mut surface: Vec<P> = vec![];
    for dx in -xr..=xr {
        let x_edge = dx == -xr || dx == xr;
        for dz in -zr..=zr {
            let z_edge = dz == -zr || dz == zr;
            let edge = x_edge || z_edge;
            let corner = x_edge && z_edge;
            let edge_not_corner = edge && !corner;
            if corner || (edge_not_corner && (g.extra_edge == 0.0 || r.next_float() > g.extra_edge)) {
                continue;
            }
            let mut p = (o.0 + dx, o.1, o.2 + dz);
            let mut off = 0;
            while b.is_air(l.get(p)) && off < g.vertical_range {
                p = rel(p, inwards);
                off += 1;
            }
            let mut off = 0;
            while !b.is_air(l.get(p)) && off < g.vertical_range {
                p = rel(p, outwards);
                off += 1;
            }
            let below = rel(p, inwards);
            let bs = l.get(below);
            if b.is_air(l.get(p)) && b.face_sturdy(bs, outwards) {
                let extra = g.extra_bottom > 0.0 && r.next_float() < g.extra_bottom;
                let depth = g.depth.sample(r) + extra as i32;
                // placeGround
                let mut q = below;
                let mut placed = true;
                for i in 0..depth {
                    let s = g.ground.state(l, r, q);
                    let bq = l.get(q);
                    if l.w.blocks.name(s) != l.w.blocks.name(bq) {
                        if !l.w.blocks.in_table(&g.replaceable, bq) {
                            placed = i != 0;
                            break;
                        }
                        l.set(q, s);
                        q = rel(q, inwards);
                    }
                }
                if placed {
                    surface.push(below);
                }
            }
        }
    }
    if !g.waterlogged {
        return surface;
    }
    // WaterloggedVegetationPatchFeature: covered positions become water
    let set = surface.clone();
    let exposed_dir = |l: &Level, p: P, d: usize| !l.w.blocks.face_sturdy(l.get(rel(p, d)), d ^ 1);
    let mut water: Vec<P> = vec![];
    for p in java_hash_order(&set) {
        let exposed = [2usize, 5, 3, 4, 0].iter().any(|&d| exposed_dir(l, p, d));
        if !exposed {
            water.push(p);
        }
    }
    let w = l.w.blocks.water;
    for p in java_hash_order(&water) {
        l.set(p, w);
    }
    water
}

/// VegetationPatchFeature.place
pub fn patch(cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, g: &Patch, o: P) -> bool {
    let xr = g.xz_radius.sample(r) + 1;
    let zr = g.xz_radius.sample(r) + 1;
    let surface = ground_patch(l, r, g, o, xr, zr);
    for p in java_hash_order(&surface) {
        if g.vegetation_chance > 0.0 && r.next_float() < g.vegetation_chance {
            let at = rel(p, g.inwards ^ 1);
            if g.waterlogged {
                // placeVegetation(pos.below()) then waterlog what was placed at pos
                if place_nested(cx, l, r, &g.vegetation, rel(at, 0)) {
                    let placed = l.get(p);
                    if l.w.blocks.get(placed, "waterlogged") == Some("false") {
                        let s = l.w.blocks.with(placed, "waterlogged", "true").unwrap();
                        l.set(p, s);
                    }
                }
            } else {
                place_nested(cx, l, r, &g.vegetation, at);
            }
        }
    }
    !surface.is_empty()
}
