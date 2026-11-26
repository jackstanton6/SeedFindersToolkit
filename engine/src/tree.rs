//! TreeFeature (literal port): trunk, foliage and root placers, tree decorators, the leaf distance
//! pass, and a java.util.HashSet<BlockPos> simulation (the leaf pass takes elements in its iteration order).

use crate::blocks::State;
use crate::carver::{mcos, msin};
use crate::chunk::MOTION_BLOCKING_NO_LEAVES;
use crate::density::f32_of;
use crate::feature::{block_set, rel, Ctx, Feature, IntP, Level, Provider, Registry, HORIZONTAL, P, STEP};
use crate::rng::{Rng, WorldgenXoro};
use crate::world::World;
use serde_json::Value;
use std::sync::Arc;

/// java.util.HashSet<BlockPos>: buckets in index order, insertion order within a bucket, the
/// table grown from 16 at load 0.75 (and when a bucket reaches 9 while the table is under 64).
// ponytail: tree bins (tables of 64+ with 9+ entries in one bucket) keep insertion order here
pub struct JSet {
    buckets: Vec<Vec<P>>,
    size: usize,
}

fn jhash(p: P) -> u32 {
    let h = p.1.wrapping_add(p.2.wrapping_mul(31)).wrapping_mul(31).wrapping_add(p.0) as u32;
    h ^ (h >> 16)
}

impl Default for JSet {
    fn default() -> Self {
        JSet { buckets: vec![], size: 0 }
    }
}

impl JSet {
    fn resize(&mut self) {
        let cap = if self.buckets.is_empty() { 16 } else { self.buckets.len() * 2 };
        let old = std::mem::take(&mut self.buckets);
        self.buckets = vec![vec![]; cap];
        for b in old {
            for p in b {
                self.buckets[jhash(p) as usize & (cap - 1)].push(p);
            }
        }
    }

    pub fn contains(&self, p: P) -> bool {
        !self.buckets.is_empty() && self.buckets[jhash(p) as usize & (self.buckets.len() - 1)].contains(&p)
    }

    pub fn insert(&mut self, p: P) -> bool {
        if self.buckets.is_empty() {
            self.resize();
        }
        let i = jhash(p) as usize & (self.buckets.len() - 1);
        if self.buckets[i].contains(&p) {
            return false;
        }
        self.buckets[i].push(p);
        if self.buckets[i].len() >= 9 && self.buckets.len() < 64 {
            self.resize();
        }
        self.size += 1;
        if self.size as f64 > self.buckets.len() as f64 * 0.75 {
            self.resize();
        }
        true
    }

    pub fn remove(&mut self, p: P) {
        if self.buckets.is_empty() {
            return;
        }
        let i = jhash(p) as usize & (self.buckets.len() - 1);
        if let Some(k) = self.buckets[i].iter().position(|&q| q == p) {
            self.buckets[i].remove(k);
            self.size -= 1;
        }
    }

    pub fn first(&self) -> Option<P> {
        self.buckets.iter().find_map(|b| b.first().copied())
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &P> {
        self.buckets.iter().flatten()
    }
}


/// Trunk placers (TrunkPlacer subclasses).
pub enum Trunk {
    Straight,
    Fancy,
    Forking,
    Giant,
    MegaJungle,
    DarkOak,
    Bending { min_leaves: i32, bend: IntP },
    Poplar { above: IntP, branches: IntP },
    /// branch count, horizontal length, start offset from top (uniform), end offset from top
    Cherry { count: IntP, length: IntP, start: (i32, i32), end: IntP },
    UpwardsBranching { steps: IntP, chance: f32, length: IntP, through: Arc<Vec<bool>> },
}

/// Foliage placers (FoliagePlacer subclasses) with their own fields.
pub enum Foliage {
    Blob(i32),
    Fancy(i32),
    Bush(i32),
    /// MegaJungleFoliagePlacer ("jungle_foliage_placer")
    Jungle(i32),
    Acacia,
    DarkOak,
    Cherry { height: IntP, wide_hole: f32, corner_hole: f32, hang: f32, hang_ext: f32 },
    MegaPine(IntP),
    Pine(IntP),
    Spruce(IntP),
    Poplar { height: IntP, side_hole: f32 },
    RandomSpread { height: IntP, attempts: i32 },
}

/// MangroveRootPlacer
pub struct Roots {
    offset: IntP,
    provider: Provider,
    above: Option<(Provider, f32)>,
    through: Arc<Vec<bool>>,
    muddy_in: Arc<Vec<bool>>,
    muddy: Provider,
    width: i32,
    length: i32,
    skew: f32,
}

pub enum Decorator {
    TrunkVine,
    LeaveVine(f32),
    Cocoa(f32),
    Beehive(f32),
    CreakingHeart(f32),
    ShelfMushroom(f32),
    AlterGround(Provider),
    AttachedToLeaves { chance: f32, xz: i32, y: i32, provider: Provider, empty: i32, dirs: Vec<usize> },
    AttachedToLogs { chance: f32, provider: Provider, dirs: Vec<usize> },
    PlaceOnGround { tries: i32, radius: i32, height: i32, provider: Provider },
    PaleMoss { leaves: f32, trunk: f32, ground: f32, patch: Arc<Feature> },
}

pub struct Tree {
    trunk_provider: Provider,
    foliage_provider: Provider,
    below_trunk: Provider,
    trunk: Trunk,
    base_height: i32,
    rand_a: i32,
    rand_b: i32,
    foliage: Foliage,
    radius: IntP,
    offset: IntP,
    /// two/three layers feature size: (limit, upper limit, lower, middle, upper), min clipped
    size: (i32, i32, i32, i32, i32, bool),
    min_clipped: Option<i32>,
    ignore_vines: bool,
    roots: Option<Roots>,
    decorators: Vec<Decorator>,
}

fn dirs(v: &Value) -> Result<Vec<usize>, String> {
    v.as_array().ok_or("directions")?.iter().map(|d| crate::feature::direction(d.as_str().unwrap_or_default())).collect()
}

impl Tree {
    pub fn parse(v: &Value, w: &World, reg: &Registry) -> Result<Tree, String> {
        let tp = &v["trunk_placer"];
        let fp = &v["foliage_placer"];
        let ms = &v["minimum_size"];
        let i = |o: &Value, k: &str, d: i64| o[k].as_i64().unwrap_or(d) as i32;
        let f = |o: &Value, k: &str| f32_of(&o[k]);
        let ip = |o: &Value, k: &str| IntP::parse(&o[k]);
        let three = ms["type"].as_str() == Some("minecraft:three_layers_feature_size");
        let trunk = match tp["type"].as_str().unwrap_or_default() {
            "minecraft:straight_trunk_placer" => Trunk::Straight,
            "minecraft:fancy_trunk_placer" => Trunk::Fancy,
            "minecraft:forking_trunk_placer" => Trunk::Forking,
            "minecraft:giant_trunk_placer" => Trunk::Giant,
            "minecraft:mega_jungle_trunk_placer" => Trunk::MegaJungle,
            "minecraft:dark_oak_trunk_placer" => Trunk::DarkOak,
            "minecraft:bending_trunk_placer" => Trunk::Bending { min_leaves: i(tp, "min_height_for_leaves", 1), bend: ip(tp, "bend_length")? },
            "minecraft:poplar_trunk_placer" => Trunk::Poplar { above: ip(tp, "trunk_height_above_branches")?, branches: ip(tp, "branch_amount")? },
            "minecraft:cherry_trunk_placer" => {
                let s = &tp["branch_start_offset_from_top"];
                Trunk::Cherry {
                    count: ip(tp, "branch_count")?,
                    length: ip(tp, "branch_horizontal_length")?,
                    start: (i(s, "min_inclusive", 0), i(s, "max_inclusive", 0)),
                    end: ip(tp, "branch_end_offset_from_top")?,
                }
            }
            "minecraft:upwards_branching_trunk_placer" => Trunk::UpwardsBranching {
                steps: ip(tp, "extra_branch_steps")?,
                chance: f(tp, "place_branch_per_log_probability"),
                length: ip(tp, "extra_branch_length")?,
                through: block_set(w, &tp["can_grow_through"])?,
            },
            t => return Err(format!("trunk placer {t} is not ported")),
        };
        let foliage = match fp["type"].as_str().unwrap_or_default() {
            "minecraft:blob_foliage_placer" => Foliage::Blob(i(fp, "height", 0)),
            "minecraft:fancy_foliage_placer" => Foliage::Fancy(i(fp, "height", 0)),
            "minecraft:bush_foliage_placer" => Foliage::Bush(i(fp, "height", 0)),
            "minecraft:jungle_foliage_placer" => Foliage::Jungle(i(fp, "height", 0)),
            "minecraft:acacia_foliage_placer" => Foliage::Acacia,
            "minecraft:dark_oak_foliage_placer" => Foliage::DarkOak,
            "minecraft:cherry_foliage_placer" => Foliage::Cherry {
                height: ip(fp, "height")?,
                wide_hole: f(fp, "wide_bottom_layer_hole_chance"),
                corner_hole: f(fp, "corner_hole_chance"),
                hang: f(fp, "hanging_leaves_chance"),
                hang_ext: f(fp, "hanging_leaves_extension_chance"),
            },
            "minecraft:mega_pine_foliage_placer" => Foliage::MegaPine(ip(fp, "crown_height")?),
            "minecraft:pine_foliage_placer" => Foliage::Pine(ip(fp, "height")?),
            "minecraft:spruce_foliage_placer" => Foliage::Spruce(ip(fp, "trunk_height")?),
            "minecraft:poplar_foliage_placer" => Foliage::Poplar { height: ip(fp, "height")?, side_hole: f(fp, "side_hole_chance") },
            "minecraft:random_spread_foliage_placer" => Foliage::RandomSpread { height: ip(fp, "foliage_height")?, attempts: i(fp, "leaf_placement_attempts", 128) },
            t => return Err(format!("foliage placer {t} is not ported")),
        };
        let roots = match v.get("root_placer") {
            Some(rp) if !rp.is_null() => {
                if rp["type"].as_str() != Some("minecraft:mangrove_root_placer") {
                    return Err(format!("root placer {} is not ported", rp["type"]));
                }
                let m = &rp["mangrove_root_placement"];
                let a = &rp["above_root_placement"];
                Some(Roots {
                    offset: ip(rp, "trunk_offset_y")?,
                    provider: Provider::parse(&rp["root_provider"], w)?,
                    above: if a.is_object() { Some((Provider::parse(&a["above_root_provider"], w)?, f(a, "above_root_placement_chance"))) } else { None },
                    through: block_set(w, &m["can_grow_through"])?,
                    muddy_in: block_set(w, &m["muddy_roots_in"])?,
                    muddy: Provider::parse(&m["muddy_roots_provider"], w)?,
                    width: i(m, "max_root_width", 8),
                    length: i(m, "max_root_length", 15),
                    skew: f(m, "random_skew_chance"),
                })
            }
            _ => None,
        };
        let mut decorators = vec![];
        for d in v["decorators"].as_array().map(|a| a.as_slice()).unwrap_or_default() {
            decorators.push(match d["type"].as_str().unwrap_or_default() {
                "minecraft:trunk_vine" => Decorator::TrunkVine,
                "minecraft:leave_vine" => Decorator::LeaveVine(f(d, "probability")),
                "minecraft:cocoa" => Decorator::Cocoa(f(d, "probability")),
                "minecraft:beehive" => Decorator::Beehive(f(d, "probability")),
                "minecraft:creaking_heart" => Decorator::CreakingHeart(f(d, "probability")),
                "minecraft:shelf_mushroom" => Decorator::ShelfMushroom(f(d, "probability")),
                "minecraft:alter_ground" => Decorator::AlterGround(Provider::parse(&d["provider"], w)?),
                "minecraft:attached_to_leaves" => Decorator::AttachedToLeaves {
                    chance: f(d, "probability"),
                    xz: i(d, "exclusion_radius_xz", 0),
                    y: i(d, "exclusion_radius_y", 0),
                    provider: Provider::parse(&d["block_provider"], w)?,
                    empty: i(d, "required_empty_blocks", 1),
                    dirs: dirs(&d["directions"])?,
                },
                "minecraft:attached_to_logs" => Decorator::AttachedToLogs { chance: f(d, "probability"), provider: Provider::parse(&d["block_provider"], w)?, dirs: dirs(&d["directions"])? },
                "minecraft:place_on_ground" => Decorator::PlaceOnGround {
                    tries: i(d, "tries", 128),
                    radius: i(d, "radius", 2),
                    height: i(d, "height", 1),
                    provider: Provider::parse(&d["block_state_provider"], w)?,
                },
                "minecraft:pale_moss" => Decorator::PaleMoss {
                    leaves: f(d, "leaves_probability"),
                    trunk: f(d, "trunk_probability"),
                    ground: f(d, "ground_probability"),
                    patch: reg.feature(w, &Value::from("minecraft:pale_moss_patch"))?,
                },
                t => return Err(format!("tree decorator {t} is not ported")),
            });
        }
        Ok(Tree {
            trunk_provider: Provider::parse(&v["trunk_provider"], w)?,
            foliage_provider: Provider::parse(&v["foliage_provider"], w)?,
            below_trunk: Provider::parse(&v["below_trunk_provider"], w)?,
            trunk,
            base_height: i(tp, "base_height", 0),
            rand_a: i(tp, "height_rand_a", 0),
            rand_b: i(tp, "height_rand_b", 0),
            foliage,
            radius: ip(fp, "radius")?,
            offset: ip(fp, "offset")?,
            size: if three {
                (i(ms, "limit", 1), i(ms, "upper_limit", 1), i(ms, "lower_size", 0), i(ms, "middle_size", 1), i(ms, "upper_size", 1), true)
            } else {
                (i(ms, "limit", 1), 0, i(ms, "lower_size", 0), 0, i(ms, "upper_size", 1), false)
            },
            min_clipped: ms["min_clipped_height"].as_i64().map(|n| n as i32),
            ignore_vines: v["ignore_vines"].as_bool().unwrap_or(false),
            roots,
            decorators,
        })
    }

    /// FeatureSize.getSizeAtHeight
    fn size_at(&self, tree_height: i32, y: i32) -> i32 {
        let (limit, upper_limit, lower, middle, upper, three) = self.size;
        if three {
            if y < limit { lower } else if y >= tree_height - upper_limit { upper } else { middle }
        } else if y < limit {
            lower
        } else {
            upper
        }
    }
}

/// The tree's write sets (TreeFeature.place's setters).
#[derive(Default)]
struct Sets {
    roots: JSet,
    trunks: JSet,
    foliage: JSet,
    decorations: JSet,
}

/// FoliagePlacer.FoliageAttachment (the height offset is always 0 in the vanilla placers).
struct Att {
    pos: P,
    radius_offset: i32,
    double: bool,
}

fn att(pos: P, radius_offset: i32, double: bool) -> Att {
    Att { pos, radius_offset, double }
}

fn up(p: P, n: i32) -> P {
    (p.0, p.1 + n, p.2)
}

/// BlockPos.relative(direction, n)
fn mv(p: P, d: usize, n: i32) -> P {
    (p.0 + STEP[d].0 * n, p.1 + STEP[d].1 * n, p.2 + STEP[d].2 * n)
}

fn manhattan(a: P, b: P) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs()
}

/// Direction.getClockWise for horizontal directions.
fn clockwise(d: usize) -> usize {
    match d {
        2 => 5,
        5 => 3,
        3 => 4,
        _ => 2,
    }
}

fn axis(d: usize) -> &'static str {
    ["y", "y", "z", "z", "x", "x"][d]
}

const DIR_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

/// Util.shuffle
fn shuffle<T>(v: &mut [T], r: &mut WorldgenXoro) {
    for i in (2..=v.len()).rev() {
        let j = r.next_bound(i as i32) as usize;
        v.swap(i - 1, j);
    }
}

/// Direction.Plane.HORIZONTAL.getRandomDirection
fn random_horizontal(r: &mut WorldgenXoro) -> usize {
    HORIZONTAL[r.next_bound(4) as usize]
}

fn valid_tree_pos(l: &Level, p: P) -> bool {
    let s = l.get(p);
    l.w.blocks.is_air(s) || l.w.blocks.is(s, "minecraft:replaceable_by_trees")
}

/// TrunkPlacer.validTreePos (upwards branching also grows through its block set)
fn trunk_valid(l: &Level, t: &Tree, p: P) -> bool {
    valid_tree_pos(l, p) || matches!(&t.trunk, Trunk::UpwardsBranching { through, .. } if l.w.blocks.in_table(through, l.get(p)))
}

fn is_free(l: &Level, t: &Tree, p: P) -> bool {
    trunk_valid(l, t, p) || l.w.blocks.is(l.get(p), "minecraft:logs")
}

fn air_or_leaves(l: &Level, p: P) -> bool {
    let s = l.get(p);
    l.w.blocks.is_air(s) || l.w.blocks.is(s, "minecraft:leaves")
}

/// TrunkPlacer.placeLog, with the log turned along an axis (trySetValue(AXIS, ...)) if given.
fn place_log(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, p: P, ax: Option<&str>) -> bool {
    if trunk_valid(l, t, p) {
        let mut s = t.trunk_provider.state(l, r, p);
        if let Some(a) = ax {
            s = l.w.blocks.with(s, "axis", a).unwrap_or(s);
        }
        sets.trunks.insert(p);
        l.set_flags(p, s, 19);
        true
    } else {
        false
    }
}

fn place_log_if_free(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, p: P) {
    if is_free(l, t, p) {
        place_log(l, r, t, sets, p, None);
    }
}

fn below_trunk(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, p: P) {
    if let Some(s) = t.below_trunk.optional(l, r, p) {
        sets.trunks.insert(p);
        l.set_flags(p, s, 19);
    }
}

fn try_place_leaf(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, p: P) -> bool {
    let b = &l.w.blocks;
    let persistent = b.get(l.get(p), "persistent") == Some("true");
    if persistent || !valid_tree_pos(l, p) {
        return false;
    }
    let mut s = t.foliage_provider.state(l, r, p);
    let b = &l.w.blocks;
    if b.get(s, "waterlogged").is_some() {
        let (ty, _, source, _) = b.fluid(l.get(p));
        s = b.with(s, "waterlogged", if ty == 1 && source { "true" } else { "false" }).unwrap_or(s);
    }
    sets.foliage.insert(p);
    l.set_flags(p, s, 19);
    true
}

/// FoliagePlacer.shouldSkipLocation (distances already made absolute)
#[allow(clippy::too_many_arguments)]
fn skip(f: &Foliage, r: &mut WorldgenXoro, dx: i32, y: i32, dz: i32, radius: i32, double: bool) -> bool {
    let corner = dx == radius && dz == radius;
    match f {
        Foliage::Blob(_) => corner && (r.next_bound(2) == 0 || y == 0),
        Foliage::Fancy(_) => (dx as f32 + 0.5) * (dx as f32 + 0.5) + (dz as f32 + 0.5) * (dz as f32 + 0.5) > (radius * radius) as f32,
        Foliage::Bush(_) => corner && r.next_bound(2) == 0,
        Foliage::Acacia => if y == 0 { (dx > 1 || dz > 1) && dx != 0 && dz != 0 } else { corner && radius > 0 },
        Foliage::Cherry { wide_hole, corner_hole, .. } => {
            if y == -1 && (dx == radius || dz == radius) && r.next_float() < *wide_hole {
                return true;
            }
            if radius > 2 {
                corner || (dx + dz > radius * 2 - 2 && r.next_float() < *corner_hole)
            } else {
                corner && r.next_float() < *corner_hole
            }
        }
        Foliage::DarkOak => {
            if y == -1 && !double {
                corner
            } else {
                y == 1 && dx + dz > radius * 2 - 2
            }
        }
        Foliage::Jungle(_) | Foliage::MegaPine(_) => dx + dz >= 7 || dx * dx + dz * dz > radius * radius,
        Foliage::Pine(_) | Foliage::Spruce(_) => corner && radius > 0,
        Foliage::RandomSpread { .. } | Foliage::Poplar { .. } => false,
    }
}

/// FoliagePlacer.placeLeavesRow (shouldSkipLocationSigned)
#[allow(clippy::too_many_arguments)]
fn leaves_row(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, o: P, radius: i32, y: i32, double: bool) {
    let off = double as i32;
    for dx in -radius..=radius + off {
        for dz in -radius..=radius + off {
            // DarkOakFoliagePlacer.shouldSkipLocationSigned
            let dark_corner = matches!(t.foliage, Foliage::DarkOak)
                && !(y != 0 || !double || (dx != -radius && dx < radius) || (dz != -radius && dz < radius));
            let (mdx, mdz) = if double { (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs())) } else { (dx.abs(), dz.abs()) };
            if !dark_corner && !skip(&t.foliage, r, mdx, y, mdz, radius, double) {
                try_place_leaf(l, r, t, sets, (o.0 + dx, o.1 + y, o.2 + dz));
            }
        }
    }
}

/// FoliagePlacer.placeLeavesRowWithHangingLeavesBelow
#[allow(clippy::too_many_arguments)]
fn leaves_row_hanging(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, o: P, radius: i32, y: i32, double: bool, chance: f32, ext: f32) {
    leaves_row(l, r, t, sets, o, radius, y, double);
    let off = double as i32;
    let log = rel(o, 0);
    for &along in &HORIZONTAL {
        let to_edge = clockwise(along);
        let to_edge_n = if to_edge == 5 || to_edge == 3 { radius + off } else { radius };
        let mut p = mv(mv(up(o, y - 1), to_edge, to_edge_n), along, -radius);
        let mut k = -radius;
        while k < radius + off {
            let above = sets.foliage.contains(rel(p, 1));
            if above && try_extension(l, r, t, sets, chance, log, p) {
                try_extension(l, r, t, sets, ext, log, rel(p, 0));
            }
            k += 1;
            p = rel(p, along);
        }
    }
}

fn try_extension(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, chance: f32, log: P, p: P) -> bool {
    if manhattan(p, log) >= 7 || r.next_float() > chance {
        return false;
    }
    try_place_leaf(l, r, t, sets, p)
}

/// FoliagePlacer.foliageHeight
fn foliage_height(f: &Foliage, r: &mut WorldgenXoro, tree_height: i32) -> i32 {
    match f {
        Foliage::Blob(h) | Foliage::Fancy(h) | Foliage::Bush(h) | Foliage::Jungle(h) => *h,
        Foliage::Acacia => 0,
        Foliage::DarkOak => 4,
        Foliage::Cherry { height, .. } | Foliage::Pine(height) | Foliage::Poplar { height, .. } | Foliage::RandomSpread { height, .. } => height.sample(r),
        Foliage::MegaPine(crown) => crown.sample(r),
        Foliage::Spruce(trunk) => 4.max(tree_height - trunk.sample(r)),
    }
}

/// FoliagePlacer.createFoliage
#[allow(clippy::too_many_arguments)]
fn create_foliage(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, a: &Att, fh: i32, lr: i32, offset: i32) {
    let (pos, ro, dbl) = (a.pos, a.radius_offset, a.double);
    match &t.foliage {
        Foliage::Blob(_) => {
            for yo in (offset - fh..=offset).rev() {
                leaves_row(l, r, t, sets, pos, (lr + ro - 1 - yo / 2).max(0), yo, dbl);
            }
        }
        Foliage::Fancy(_) => {
            for yo in (offset - fh..=offset).rev() {
                let radius = lr + if yo != offset && yo != offset - fh { 1 } else { 0 };
                leaves_row(l, r, t, sets, pos, radius, yo, dbl);
            }
        }
        Foliage::Bush(_) => {
            for yo in (offset - fh..=offset).rev() {
                leaves_row(l, r, t, sets, pos, lr + ro - 1 - yo, yo, dbl);
            }
        }
        Foliage::Jungle(_) => {
            let lh = if dbl { fh } else { 1 + r.next_bound(2) };
            for yo in (offset - lh..=offset).rev() {
                leaves_row(l, r, t, sets, pos, lr + ro + 1 - yo, yo, dbl);
            }
        }
        Foliage::Acacia => {
            let fp = up(pos, offset);
            leaves_row(l, r, t, sets, fp, lr + ro, -1 - fh, dbl);
            leaves_row(l, r, t, sets, fp, lr - 1, -fh, dbl);
            leaves_row(l, r, t, sets, fp, lr + ro - 1, 0, dbl);
        }
        Foliage::DarkOak => {
            let fp = up(pos, offset);
            if dbl {
                leaves_row(l, r, t, sets, fp, lr + 2, -1, dbl);
                leaves_row(l, r, t, sets, fp, lr + 3, 0, dbl);
                leaves_row(l, r, t, sets, fp, lr + 2, 1, dbl);
                if r.next_bool() {
                    leaves_row(l, r, t, sets, fp, lr, 2, dbl);
                }
            } else {
                leaves_row(l, r, t, sets, fp, lr + 2, -1, dbl);
                leaves_row(l, r, t, sets, fp, lr + 1, 0, dbl);
            }
        }
        Foliage::Cherry { hang, hang_ext, .. } => {
            let fp = up(pos, offset);
            let cr = lr + ro - 1;
            leaves_row(l, r, t, sets, fp, cr - 2, fh - 3, dbl);
            leaves_row(l, r, t, sets, fp, cr - 1, fh - 4, dbl);
            for y in (0..=fh - 5).rev() {
                leaves_row(l, r, t, sets, fp, cr, y, dbl);
            }
            leaves_row_hanging(l, r, t, sets, fp, cr, -1, dbl, *hang, *hang_ext);
            leaves_row_hanging(l, r, t, sets, fp, cr - 1, -2, dbl, *hang, *hang_ext);
        }
        Foliage::MegaPine(_) => {
            let mut prev = 0;
            for yy in pos.1 - fh + offset..=pos.1 + offset {
                let yo = pos.1 - yy;
                let smooth = lr + ro + (yo as f32 / fh as f32 * 3.5f32).floor() as i32;
                let jagged = if yo > 0 && smooth == prev && (yy & 1) == 0 { smooth + 1 } else { smooth };
                leaves_row(l, r, t, sets, (pos.0, yy, pos.2), jagged, 0, dbl);
                prev = smooth;
            }
        }
        Foliage::Pine(_) => {
            let mut cr = 0;
            for yo in (offset - fh..=offset).rev() {
                leaves_row(l, r, t, sets, pos, cr, yo, dbl);
                if cr >= 1 && yo == offset - fh + 1 {
                    cr -= 1;
                } else if cr < lr + ro {
                    cr += 1;
                }
            }
        }
        Foliage::Spruce(_) => {
            let mut cr = r.next_bound(2);
            let (mut max, mut min) = (1, 0);
            for yo in (-fh..=offset).rev() {
                leaves_row(l, r, t, sets, pos, cr, yo, dbl);
                if cr >= max {
                    cr = min;
                    min = 1;
                    max = (max + 1).min(lr + ro);
                } else {
                    cr += 1;
                }
            }
        }
        Foliage::Poplar { side_hole, .. } => poplar_foliage(l, r, t, sets, up(pos, offset), lr + ro - 1, fh, dbl, *side_hole),
        Foliage::RandomSpread { attempts, .. } => {
            for _ in 0..*attempts {
                let dx = r.next_bound(lr) - r.next_bound(lr);
                let dy = r.next_bound(fh) - r.next_bound(fh);
                let dz = r.next_bound(lr) - r.next_bound(lr);
                try_place_leaf(l, r, t, sets, (pos.0 + dx, pos.1 + dy, pos.2 + dz));
            }
        }
    }
}

// --- PoplarFoliagePlacer ---------------------------------------------------------------------

fn poplar_partial(fh: i32, y: i32) -> bool {
    fh - 1 == y || fh - 2 == y
}

fn poplar_corner_cut(dx: i32, dz: i32, radius: i32, partial: bool, flip: bool) -> i32 {
    let small = if flip { (dx > 0 && dz > 0) || (dz < 0 && dx < 0) } else { (dx > 0 && dz < 0) || (dz > 0 && dx < 0) };
    if small {
        radius - 1
    } else if partial {
        radius + 1
    } else {
        radius
    }
}

fn rhombus(radius: i32, adx: i32, adz: i32, cut: i32, extra: i32) -> bool {
    adx + adz <= radius * 2 - (cut + extra)
}

#[allow(clippy::too_many_arguments)]
fn poplar_row(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, o: P, radius: i32, y: i32, dbl: bool, fh: i32, flip: bool, side_hole: f32) {
    let off = dbl as i32;
    for dx in -radius..=radius + off {
        for dz in -radius..=radius + off {
            let partial = poplar_partial(fh, y);
            let cut = poplar_corner_cut(dx, dz, radius, partial, flip);
            let (adx, adz) = (dx.abs(), dz.abs());
            let skip = if partial && (adx == radius || adz == radius) {
                true
            } else {
                let extra = if r.next_float() <= side_hole { 1 } else { 0 };
                !rhombus(radius, adx, adz, cut, extra)
            };
            if !skip {
                try_place_leaf(l, r, t, sets, (o.0 + dx, o.1 + y, o.2 + dz));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poplar_foliage(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, fp: P, cr: i32, fh: i32, dbl: bool, side_hole: f32) {
    let flip = r.next_bool();
    poplar_row(l, r, t, sets, fp, cr - 2, fh - 1, dbl, fh, flip, side_hole);
    poplar_row(l, r, t, sets, fp, cr - 1, fh - 2, dbl, fh, flip, side_hole);
    poplar_row(l, r, t, sets, fp, cr - 1, fh - 3, dbl, fh, flip, side_hole);
    for y in (1..=fh - 4).rev() {
        poplar_row(l, r, t, sets, fp, cr, y, dbl, fh, flip, side_hole);
    }
    // replaceLeavesWithLog
    let y = fh - 4;
    let off = dbl as i32;
    for dx in -cr..=cr + off {
        for dz in -cr..=cr + off {
            let (adx, adz) = (dx.abs(), dz.abs());
            let cut = poplar_corner_cut(dx, dz, cr, poplar_partial(fh, y), flip);
            if rhombus(cr, adx, adz, cut, 2) && ((adz == 0 && cr - adx >= 4) || (adx == 0 && cr - adz >= 4)) {
                let p = (fp.0 + dx, fp.1 + y, fp.2 + dz);
                let leaf = t.foliage_provider.state(l, r, p);
                if l.get(p) == leaf {
                    let s = t.trunk_provider.state(l, r, p);
                    let s = l.w.blocks.with(s, "axis", if adz == 0 { "x" } else { "z" }).unwrap_or(s);
                    sets.foliage.insert(p);
                    l.set_flags(p, s, 19);
                }
            }
        }
    }
    poplar_row(l, r, t, sets, fp, cr - 1, 0, dbl, fh, flip, side_hole);
    poplar_row(l, r, t, sets, fp, (cr - 2).clamp(1, 2), -1, dbl, fh, flip, side_hole);
}

// --- TreeFeature -----------------------------------------------------------------------------

/// TreeFeature.place
pub fn tree(cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, t: &Tree, o: P) -> bool {
    let mut sets = Sets::default();
    // doPlace
    let tree_height = t.base_height + r.next_bound(t.rand_a + 1) + r.next_bound(t.rand_b + 1);
    let foliage_height = foliage_height(&t.foliage, r, tree_height);
    let trunk_height = tree_height - foliage_height;
    let mut leaf_radius = t.radius.sample(r);
    if let Foliage::Pine(_) = t.foliage {
        leaf_radius += r.next_bound((trunk_height + 1).max(1));
    }
    let trunk_origin = match &t.roots {
        Some(rp) => up(o, rp.offset.sample(r)),
        None => o,
    };
    let min_y = o.1.min(trunk_origin.1);
    let max_y = o.1.max(trunk_origin.1) + tree_height + 1;
    let (wmin, wmax) = (l.w.min_y, l.w.min_y + l.w.height - 1);
    if !(min_y >= wmin + 1 && max_y <= wmax + 1) {
        return false;
    }
    // getMaxFreeTreeHeight
    let mut clipped = tree_height;
    'outer: for y in 0..=tree_height + 1 {
        let rad = t.size_at(tree_height, y);
        for x in -rad..=rad {
            for z in -rad..=rad {
                let p = (trunk_origin.0 + x, trunk_origin.1 + y, trunk_origin.2 + z);
                if !is_free(l, t, p) || (!t.ignore_vines && l.name(p) == "minecraft:vine") {
                    clipped = y - 2;
                    break 'outer;
                }
            }
        }
    }
    if !(clipped >= tree_height || t.min_clipped.is_some_and(|m| clipped >= m)) {
        return false;
    }
    if let Some(rp) = &t.roots {
        if !place_roots(l, r, rp, &mut sets, o, trunk_origin) {
            return false;
        }
    }
    let attachments = place_trunk(l, r, t, &mut sets, clipped, trunk_origin);
    for a in &attachments {
        let offset = t.offset.sample(r);
        create_foliage(l, r, t, &mut sets, a, foliage_height, leaf_radius, offset);
    }
    if sets.trunks.is_empty() && sets.foliage.is_empty() {
        return false;
    }
    if !t.decorators.is_empty() {
        decorate(cx, l, r, t, &mut sets);
    }
    // ponytail: StructureTemplate.updateShapeAtEdge (neighbour shape updates around the tree) is not run
    update_leaves(l, &sets);
    true
}

/// TrunkPlacer.placeTrunk
fn place_trunk(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, th: i32, o: P) -> Vec<Att> {
    let mut atts = vec![];
    match &t.trunk {
        Trunk::Straight => {
            below_trunk(l, r, t, sets, rel(o, 0));
            for y in 0..th {
                place_log(l, r, t, sets, up(o, y), None);
            }
            atts.push(att(up(o, th), 0, false));
        }
        Trunk::Fancy => return fancy_trunk(l, r, t, sets, th, o),
        Trunk::Forking => {
            below_trunk(l, r, t, sets, rel(o, 0));
            let lean = random_horizontal(r);
            let lean_h = th - r.next_bound(4) - 1;
            let mut lean_steps = 3 - r.next_bound(3);
            let (mut tx, mut tz) = (o.0, o.2);
            let mut ey = None;
            for yo in 0..th {
                let yy = o.1 + yo;
                if yo >= lean_h && lean_steps > 0 {
                    tx += STEP[lean].0;
                    tz += STEP[lean].2;
                    lean_steps -= 1;
                }
                if place_log(l, r, t, sets, (tx, yy, tz), None) {
                    ey = Some(yy + 1);
                }
            }
            if let Some(e) = ey {
                atts.push(att((tx, e, tz), 1, false));
            }
            let (mut tx, mut tz) = (o.0, o.2);
            let branch = random_horizontal(r);
            if branch != lean {
                let branch_pos = lean_h - r.next_bound(2) - 1;
                let mut steps = 1 + r.next_bound(3);
                let mut ey = None;
                let mut yo = branch_pos;
                while yo < th && steps > 0 {
                    if yo >= 1 {
                        let yy = o.1 + yo;
                        tx += STEP[branch].0;
                        tz += STEP[branch].2;
                        if place_log(l, r, t, sets, (tx, yy, tz), None) {
                            ey = Some(yy + 1);
                        }
                    }
                    yo += 1;
                    steps -= 1;
                }
                if let Some(e) = ey {
                    atts.push(att((tx, e, tz), 0, false));
                }
            }
        }
        Trunk::Giant | Trunk::MegaJungle => {
            let below = rel(o, 0);
            for p in [below, rel(below, 5), rel(below, 3), rel(rel(below, 3), 5)] {
                below_trunk(l, r, t, sets, p);
            }
            for hh in 0..th {
                place_log_if_free(l, r, t, sets, (o.0, o.1 + hh, o.2));
                if hh < th - 1 {
                    place_log_if_free(l, r, t, sets, (o.0 + 1, o.1 + hh, o.2));
                    place_log_if_free(l, r, t, sets, (o.0 + 1, o.1 + hh, o.2 + 1));
                    place_log_if_free(l, r, t, sets, (o.0, o.1 + hh, o.2 + 1));
                }
            }
            atts.push(att(up(o, th), 0, true));
            if let Trunk::MegaJungle = t.trunk {
                let mut bh = th - 2 - r.next_bound(4);
                while bh > th / 2 {
                    let angle = r.next_float() * (std::f64::consts::PI * 2.0) as f32;
                    let (mut bx, mut bz) = (0, 0);
                    for b in 0..5 {
                        bx = (1.5f32 + mcos(angle as f64) * b as f32) as i32;
                        bz = (1.5f32 + msin(angle as f64) * b as f32) as i32;
                        place_log(l, r, t, sets, (o.0 + bx, o.1 + bh - 3 + b / 2, o.2 + bz), None);
                    }
                    atts.push(att((o.0 + bx, o.1 + bh, o.2 + bz), -2, false));
                    bh -= 2 + r.next_bound(4);
                }
            }
        }
        Trunk::DarkOak => {
            let below = rel(o, 0);
            for p in [below, rel(below, 5), rel(below, 3), rel(rel(below, 3), 5)] {
                below_trunk(l, r, t, sets, p);
            }
            let lean = random_horizontal(r);
            let lean_h = th - r.next_bound(4);
            let mut lean_steps = 2 - r.next_bound(3);
            let (x, y, z) = o;
            let (mut tx, mut tz) = (x, z);
            let ey = y + th - 1;
            for dy in 0..th {
                if dy >= lean_h && lean_steps > 0 {
                    tx += STEP[lean].0;
                    tz += STEP[lean].2;
                    lean_steps -= 1;
                }
                let p = (tx, y + dy, tz);
                if air_or_leaves(l, p) {
                    place_log(l, r, t, sets, p, None);
                    place_log(l, r, t, sets, rel(p, 5), None);
                    place_log(l, r, t, sets, rel(p, 3), None);
                    place_log(l, r, t, sets, rel(rel(p, 5), 3), None);
                }
            }
            atts.push(att((tx, ey, tz), 0, true));
            for ox in -1..=2 {
                for oz in -1..=2 {
                    if (ox < 0 || ox > 1 || oz < 0 || oz > 1) && r.next_bound(3) <= 0 {
                        let len = r.next_bound(3) + 2;
                        for by in 0..len {
                            place_log(l, r, t, sets, (x + ox, ey - by - 1, z + oz), None);
                        }
                        atts.push(att((x + ox, ey, z + oz), 0, false));
                    }
                }
            }
        }
        Trunk::Bending { min_leaves, bend } => {
            let dir = random_horizontal(r);
            let log_h = th - 1;
            let mut p = o;
            below_trunk(l, r, t, sets, rel(p, 0));
            for i in 0..=log_h {
                if i + 1 >= log_h + r.next_bound(2) {
                    p = rel(p, dir);
                }
                if valid_tree_pos(l, p) {
                    place_log(l, r, t, sets, p, None);
                }
                if i >= *min_leaves {
                    atts.push(att(p, 0, false));
                }
                p = rel(p, 1);
            }
            let len = bend.sample(r);
            for _ in 0..=len {
                if valid_tree_pos(l, p) {
                    place_log(l, r, t, sets, p, None);
                }
                atts.push(att(p, 0, false));
                p = rel(p, dir);
            }
        }
        Trunk::Poplar { above, branches } => {
            below_trunk(l, r, t, sets, rel(o, 0));
            let up_to = th - above.sample(r);
            for y in 0..th {
                let p = up(o, y);
                place_log(l, r, t, sets, p, None);
                let mut dirs: Vec<usize> = (0..6).collect();
                shuffle(&mut dirs, r);
                dirs.retain(|&d| d >= 2);
                if up_to - 1 == y {
                    let n = branches.sample(r);
                    for &d in dirs.iter().take(n.max(0) as usize) {
                        place_log(l, r, t, sets, rel(p, d), Some(axis(d)));
                    }
                }
            }
            atts.push(att(up(o, up_to), 0, false));
        }
        Trunk::Cherry { count, length, start, end } => {
            below_trunk(l, r, t, sets, rel(o, 0));
            let first = (th - 1 + r.between_inclusive(start.0, start.1)).max(0);
            let mut second = (th - 1 + r.between_inclusive(start.0, start.1 - 1)).max(0);
            if second >= first {
                second += 1;
            }
            let n = count.sample(r);
            let (middle, both) = (n == 3, n >= 2);
            let trunk_h = if middle { th } else if both { first.max(second) + 1 } else { first + 1 };
            for y in 0..trunk_h {
                place_log(l, r, t, sets, up(o, y), None);
            }
            if middle {
                atts.push(att(up(o, trunk_h), 0, false));
            }
            let dir = random_horizontal(r);
            let a = cherry_branch(l, r, t, sets, th, o, length, end, dir, axis(dir), first, first < trunk_h - 1);
            atts.push(a);
            if both {
                let a = cherry_branch(l, r, t, sets, th, o, length, end, dir ^ 1, axis(dir), second, second < trunk_h - 1);
                atts.push(a);
            }
        }
        Trunk::UpwardsBranching { steps, chance, length, .. } => {
            for hp in 0..th {
                let cur = o.1 + hp;
                let p = (o.0, cur, o.2);
                if place_log(l, r, t, sets, p, None) && hp < th - 1 && r.next_float() < *chance {
                    let dir = random_horizontal(r);
                    let blen = length.sample(r);
                    let bpos = (blen - length.sample(r) - 1).max(0);
                    let bsteps = steps.sample(r);
                    upwards_branch(l, r, t, sets, th, &mut atts, p, cur, dir, bpos, bsteps);
                }
                if hp == th - 1 {
                    atts.push(att((o.0, cur + 1, o.2), 0, false));
                }
            }
        }
    }
    atts
}

/// CherryTrunkPlacer.generateBranch
#[allow(clippy::too_many_arguments)]
fn cherry_branch(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, th: i32, o: P, length: &IntP, end: &IntP, dir: usize, ax: &str, off: i32, middle_up: bool) -> Att {
    let mut p = up(o, off);
    let end_off = th - 1 + end.sample(r);
    let extend = middle_up || end_off < off;
    let dist = length.sample(r) + extend as i32;
    let end_pos = up(mv(o, dir, dist), end_off);
    for _ in 0..if extend { 2 } else { 1 } {
        p = rel(p, dir);
        place_log(l, r, t, sets, p, Some(ax));
    }
    let vertical = if end_pos.1 > p.1 { 1 } else { 0 };
    loop {
        let d = manhattan(p, end_pos);
        if d == 0 {
            return att(up(end_pos, 1), 0, false);
        }
        let chance = (end_pos.1 - p.1).abs() as f32 / d as f32;
        let grow_up = r.next_float() < chance;
        p = rel(p, if grow_up { vertical } else { dir });
        place_log(l, r, t, sets, p, if grow_up { None } else { Some(ax) });
    }
}

/// UpwardsBranchingTrunkPlacer.placeBranch
#[allow(clippy::too_many_arguments)]
fn upwards_branch(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, th: i32, atts: &mut Vec<Att>, log: P, cur: i32, dir: usize, bpos: i32, mut bsteps: i32) {
    let mut along = cur + bpos;
    let (mut lx, mut lz) = (log.0, log.2);
    let mut i = bpos;
    while i < th && bsteps > 0 {
        if i >= 1 {
            let ph = cur + i;
            lx += STEP[dir].0;
            lz += STEP[dir].2;
            along = ph;
            if place_log(l, r, t, sets, (lx, ph, lz), None) {
                along += 1;
            }
            atts.push(att((lx, ph, lz), 0, false));
        }
        i += 1;
        bsteps -= 1;
    }
    if along - cur > 1 {
        atts.push(att((lx, along, lz), 0, false));
        atts.push(att((lx, along - 2, lz), 0, false));
    }
}

/// FancyTrunkPlacer.makeLimb
#[allow(clippy::too_many_arguments)]
fn make_limb(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, start: P, end: P, place: bool) -> bool {
    if !place && start == end {
        return true;
    }
    let d = (end.0 - start.0, end.1 - start.1, end.2 - start.2);
    let steps = d.0.abs().max(d.1.abs().max(d.2.abs()));
    let (dx, dy, dz) = (d.0 as f32 / steps as f32, d.1 as f32 / steps as f32, d.2 as f32 / steps as f32);
    for i in 0..=steps {
        let p = (start.0 + (0.5f32 + i as f32 * dx).floor() as i32, start.1 + (0.5f32 + i as f32 * dy).floor() as i32, start.2 + (0.5f32 + i as f32 * dz).floor() as i32);
        if place {
            let (xd, zd) = ((p.0 - start.0).abs(), (p.2 - start.2).abs());
            let m = xd.max(zd);
            let ax = if m > 0 { if xd == m { "x" } else { "z" } } else { "y" };
            place_log(l, r, t, sets, p, Some(ax));
        } else if !is_free(l, t, p) {
            return false;
        }
    }
    true
}

/// FancyTrunkPlacer.treeShape
fn tree_shape(height: i32, y: i32) -> f32 {
    if (y as f32) < height as f32 * 0.3 {
        return -1.0;
    }
    let radius = height as f32 / 2.0;
    let adjacent = radius - y as f32;
    let mut distance = ((radius * radius - adjacent * adjacent) as f64).sqrt() as f32;
    if adjacent == 0.0 {
        distance = radius;
    } else if adjacent.abs() >= radius {
        return 0.0;
    }
    distance * 0.5
}

/// FancyTrunkPlacer.placeTrunk
fn fancy_trunk(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, tree_height: i32, o: P) -> Vec<Att> {
    let height = tree_height + 2;
    let trunk_height = (height as f64 * 0.618).floor() as i32;
    below_trunk(l, r, t, sets, rel(o, 0));
    let clusters = 1.min((1.382 + (1.0 * height as f64 / 13.0).powf(2.0)).floor() as i32);
    let trunk_top = o.1 + trunk_height;
    let mut rel_y = height - 5;
    // (attachment pos, branch base y)
    let mut coords: Vec<(P, i32)> = vec![(up(o, rel_y), trunk_top)];
    while rel_y >= 0 {
        let shape = tree_shape(height, rel_y);
        if shape >= 0.0 {
            for _ in 0..clusters {
                let radius = 1.0 * shape as f64 * (r.next_float() as f64 + 0.328);
                let angle = (r.next_float() * 2.0) as f64 * std::f64::consts::PI;
                let x = radius * angle.sin() + 0.5;
                let z = radius * angle.cos() + 0.5;
                let start = (o.0 + x.floor() as i32, o.1 + rel_y - 1, o.2 + z.floor() as i32);
                let end = up(start, 5);
                if make_limb(l, r, t, sets, start, end, false) {
                    let (dx, dz) = (o.0 - start.0, o.2 - start.2);
                    let branch_height = start.1 as f64 - ((dx * dx + dz * dz) as f64).sqrt() * 0.381;
                    let branch_top = if branch_height > trunk_top as f64 { trunk_top } else { branch_height as i32 };
                    let base = (o.0, branch_top, o.2);
                    if make_limb(l, r, t, sets, base, start, false) {
                        coords.push((start, base.1));
                    }
                }
            }
        }
        rel_y -= 1;
    }
    make_limb(l, r, t, sets, o, up(o, trunk_height), true);
    // makeBranches
    let trim = |y: i32| y as f64 >= height as f64 * 0.2;
    for &(pos, base_y) in &coords {
        let base = (o.0, base_y, o.2);
        if base != pos && trim(base_y - o.1) {
            make_limb(l, r, t, sets, base, pos, true);
        }
    }
    coords.into_iter().filter(|&(_, b)| trim(b - o.1)).map(|(p, _)| att(p, 0, false)).collect()
}

// --- MangroveRootPlacer ----------------------------------------------------------------------

fn can_root(l: &Level, rp: &Roots, p: P) -> bool {
    valid_tree_pos(l, p) || l.w.blocks.in_table(&rp.through, l.get(p))
}

/// RootPlacer.getPotentiallyWaterloggedState
fn waterlogged(l: &Level, p: P, s: State) -> State {
    let b = &l.w.blocks;
    if b.get(s, "waterlogged").is_some() {
        b.with(s, "waterlogged", if b.fluid(l.get(p)).0 == 1 { "true" } else { "false" }).unwrap_or(s)
    } else {
        s
    }
}

fn place_roots(l: &mut Level, r: &mut WorldgenXoro, rp: &Roots, sets: &mut Sets, o: P, trunk_origin: P) -> bool {
    let mut col = o;
    while col.1 < trunk_origin.1 {
        if !can_root(l, rp, col) {
            return false;
        }
        col = rel(col, 1);
    }
    let mut positions = vec![rel(trunk_origin, 0)];
    for &d in &HORIZONTAL {
        let p = rel(trunk_origin, d);
        let mut list = vec![];
        if !simulate_roots(l, r, rp, p, d, trunk_origin, &mut list, 0) {
            return false;
        }
        positions.extend(list);
        positions.push(p);
    }
    for p in positions {
        place_root(l, r, rp, sets, p);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn simulate_roots(l: &Level, r: &mut WorldgenXoro, rp: &Roots, p: P, d: usize, origin: P, list: &mut Vec<P>, layer: i32) -> bool {
    if layer == rp.length || list.len() as i32 > rp.length {
        return false;
    }
    // potentialRootPositions
    let below = rel(p, 0);
    let next = rel(p, d);
    let width = manhattan(p, origin);
    let candidates = if width > rp.width - 3 && width <= rp.width {
        if r.next_float() < rp.skew { vec![below, rel(next, 0)] } else { vec![below] }
    } else if width > rp.width || r.next_float() < rp.skew {
        vec![below]
    } else if r.next_bool() {
        vec![next]
    } else {
        vec![below]
    };
    for q in candidates {
        if can_root(l, rp, q) {
            list.push(q);
            if !simulate_roots(l, r, rp, q, d, origin, list, layer + 1) {
                return false;
            }
        }
    }
    true
}

fn place_root(l: &mut Level, r: &mut WorldgenXoro, rp: &Roots, sets: &mut Sets, p: P) {
    if l.w.blocks.in_table(&rp.muddy_in, l.get(p)) {
        let s = rp.muddy.state(l, r, p);
        let s = waterlogged(l, p, s);
        sets.roots.insert(p);
        l.set_flags(p, s, 19);
    } else if can_root(l, rp, p) {
        let s = rp.provider.state(l, r, p);
        let s = waterlogged(l, p, s);
        sets.roots.insert(p);
        l.set_flags(p, s, 19);
        if let Some((provider, chance)) = &rp.above {
            let a = rel(p, 1);
            if r.next_float() < *chance && l.is_air(a) {
                let s = provider.state(l, r, a);
                let s = waterlogged(l, a, s);
                sets.roots.insert(a);
                l.set_flags(a, s, 19);
            }
        }
    }
}

// --- tree decorators -------------------------------------------------------------------------

fn deco_set(l: &mut Level, sets: &mut Sets, p: P, s: State) {
    sets.decorations.insert(p);
    l.set_flags(p, s, 19);
}

fn block_with(l: &Level, name: &str, props: &[(&str, &str)]) -> State {
    let b = &l.w.blocks;
    props.iter().fold(b.state(name).unwrap(), |s, (k, v)| b.with(s, k, v).unwrap_or(s))
}

/// TreeFeature.getLowestTrunkOrRootOfTree
fn lowest(logs: &[P], roots: &[P]) -> Vec<P> {
    if roots.is_empty() {
        logs.to_vec()
    } else if !logs.is_empty() && roots[0].1 == logs[0].1 {
        logs.iter().chain(roots).copied().collect()
    } else {
        roots.to_vec()
    }
}

fn place_vine(l: &mut Level, sets: &mut Sets, p: P, face: &str) {
    let s = block_with(l, "minecraft:vine", &[(face, "true")]);
    deco_set(l, sets, p, s);
}

/// TreeDecorator.place for each decorator, over the y-sorted logs, leaves and roots.
fn decorate(cx: &mut Ctx, l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets) {
    let sorted = |s: &JSet| {
        let mut v: Vec<P> = s.iter().copied().collect();
        v.sort_by_key(|p| p.1);
        v
    };
    let (logs, leaves, roots) = (sorted(&sets.trunks), sorted(&sets.foliage), sorted(&sets.roots));
    // west face east, east face west, north face south, south face north
    const SIDES: [(usize, &str); 4] = [(4, "east"), (5, "west"), (2, "south"), (3, "north")];
    for d in &t.decorators {
        match d {
            Decorator::TrunkVine => {
                for &p in &logs {
                    for (dir, face) in SIDES {
                        if r.next_bound(3) > 0 {
                            let q = rel(p, dir);
                            if l.is_air(q) {
                                place_vine(l, sets, q, face);
                            }
                        }
                    }
                }
            }
            Decorator::LeaveVine(chance) => {
                for &p in &leaves {
                    for (dir, face) in SIDES {
                        if r.next_float() < *chance {
                            let q = rel(p, dir);
                            if l.is_air(q) {
                                // addHangingVine
                                place_vine(l, sets, q, face);
                                let mut below = rel(q, 0);
                                let mut n = 4;
                                while l.is_air(below) && n > 0 {
                                    place_vine(l, sets, below, face);
                                    below = rel(below, 0);
                                    n -= 1;
                                }
                            }
                        }
                    }
                }
            }
            Decorator::Cocoa(chance) => {
                if r.next_float() >= *chance || logs.is_empty() {
                    continue;
                }
                let tree_y = logs[0].1;
                for &p in logs.iter().filter(|p| p.1 - tree_y <= 2) {
                    for &dir in &HORIZONTAL {
                        if r.next_float() <= 0.25 {
                            let op = STEP[dir ^ 1];
                            let q = (p.0 + op.0, p.1, p.2 + op.2);
                            if l.is_air(q) {
                                let age = r.next_bound(3).to_string();
                                let s = block_with(l, "minecraft:cocoa", &[("age", &age), ("facing", DIR_NAMES[dir])]);
                                deco_set(l, sets, q, s);
                            }
                        }
                    }
                }
            }
            Decorator::Beehive(chance) => {
                if logs.is_empty() || r.next_float() >= *chance {
                    continue;
                }
                let hive_y = if !leaves.is_empty() { (leaves[0].1 - 1).max(logs[0].1 + 1) } else { (logs[0].1 + 1 + r.next_bound(3)).min(logs[logs.len() - 1].1) };
                // SPAWN_DIRECTIONS: the horizontal directions but north
                let mut spots: Vec<P> = logs.iter().filter(|p| p.1 == hive_y).flat_map(|&p| [5usize, 3, 4].map(|d| rel(p, d))).collect();
                if spots.is_empty() {
                    continue;
                }
                shuffle(&mut spots, r);
                if let Some(&q) = spots.iter().find(|&&q| l.is_air(q) && l.is_air(rel(q, 3))) {
                    let s = block_with(l, "minecraft:bee_nest", &[("facing", "south")]);
                    deco_set(l, sets, q, s);
                    // the bees (Occupant.create(random.nextInt(599)))
                    let bees = 2 + r.next_bound(2);
                    for _ in 0..bees {
                        r.next_bound(599);
                    }
                }
            }
            Decorator::CreakingHeart(chance) => {
                if logs.is_empty() || r.next_float() >= *chance {
                    continue;
                }
                let mut spots = logs.clone();
                shuffle(&mut spots, r);
                if let Some(&q) = spots.iter().find(|&&q| (0..6).all(|d| l.w.blocks.is(l.get(rel(q, d)), "minecraft:logs"))) {
                    let s = block_with(l, "minecraft:creaking_heart", &[("creaking_heart_state", "dormant"), ("natural", "true")]);
                    deco_set(l, sets, q, s);
                }
            }
            Decorator::ShelfMushroom(chance) => {
                if r.next_float() >= *chance || logs.is_empty() {
                    continue;
                }
                shelf_mushrooms(l, r, sets, &logs);
            }
            Decorator::AlterGround(provider) => {
                let positions = lowest(&logs, &roots);
                if positions.is_empty() {
                    continue;
                }
                let min_y = positions[0].1;
                for &p in positions.iter().filter(|p| p.1 == min_y) {
                    for c in [(-1, -1), (2, -1), (-1, 2), (2, 2)] {
                        alter_circle(l, r, sets, provider, (p.0 + c.0, p.1, p.2 + c.1));
                    }
                    for _ in 0..5 {
                        let k = r.next_bound(64);
                        let (xx, zz) = (k % 8, k / 8);
                        if xx == 0 || xx == 7 || zz == 0 || zz == 7 {
                            alter_circle(l, r, sets, provider, (p.0 - 3 + xx, p.1, p.2 - 3 + zz));
                        }
                    }
                }
            }
            Decorator::AttachedToLeaves { chance, xz, y, provider, empty, dirs } => {
                let mut blacklist: std::collections::HashSet<P> = std::collections::HashSet::new();
                let mut shuffled = leaves.clone();
                shuffle(&mut shuffled, r);
                for leaf in shuffled {
                    let d = dirs[r.next_bound(dirs.len() as i32) as usize];
                    let q = rel(leaf, d);
                    if !blacklist.contains(&q) && r.next_float() < *chance && (1..=*empty).all(|i| l.is_air(mv(leaf, d, i))) {
                        for bx in -xz..=*xz {
                            for by in -y..=*y {
                                for bz in -xz..=*xz {
                                    blacklist.insert((q.0 + bx, q.1 + by, q.2 + bz));
                                }
                            }
                        }
                        let s = provider.state(l, r, q);
                        deco_set(l, sets, q, s);
                    }
                }
            }
            Decorator::AttachedToLogs { chance, provider, dirs } => {
                let mut shuffled = logs.clone();
                shuffle(&mut shuffled, r);
                for p in shuffled {
                    let d = dirs[r.next_bound(dirs.len() as i32) as usize];
                    let q = rel(p, d);
                    if r.next_float() <= *chance && l.is_air(q) {
                        let s = provider.state(l, r, q);
                        deco_set(l, sets, q, s);
                    }
                }
            }
            Decorator::PlaceOnGround { tries, radius, height, provider } => {
                let positions = lowest(&logs, &roots);
                if positions.is_empty() {
                    continue;
                }
                let o = positions[0];
                let (mut x0, mut x1, mut z0, mut z1) = (o.0, o.0, o.2, o.2);
                for p in positions.iter().filter(|p| p.1 == o.1) {
                    x0 = x0.min(p.0);
                    x1 = x1.max(p.0);
                    z0 = z0.min(p.2);
                    z1 = z1.max(p.2);
                }
                for _ in 0..*tries {
                    let x = r.between_inclusive(x0 - radius, x1 + radius);
                    let y = r.between_inclusive(o.1 - height, o.1 + height);
                    let z = r.between_inclusive(z0 - radius, z1 + radius);
                    let (p, a) = ((x, y, z), (x, y + 1, z));
                    if (l.is_air(a) || l.name(a) == "minecraft:vine")
                        && l.w.blocks.is_solid_render(l.get(p))
                        && l.height(MOTION_BLOCKING_NO_LEAVES, x, z) <= a.1
                    {
                        let s = provider.state(l, r, a);
                        deco_set(l, sets, a, s);
                    }
                }
            }
            Decorator::PaleMoss { leaves: lp, trunk, ground, patch } => {
                let mut shuffled = logs.clone();
                shuffle(&mut shuffled, r);
                if shuffled.is_empty() {
                    continue;
                }
                let origin = *shuffled.iter().min_by_key(|p| p.1).unwrap();
                if r.next_float() < *ground {
                    crate::feature::place_feature(cx, l, r, patch, rel(origin, 1));
                }
                for (list, chance) in [(&logs, *trunk), (&leaves, *lp)] {
                    for &p in list {
                        if r.next_float() < chance {
                            let down = rel(p, 0);
                            if l.is_air(down) {
                                // addMossHanger
                                let mut q = down;
                                while l.is_air(rel(q, 0)) && r.next_float() >= 0.5 {
                                    let s = block_with(l, "minecraft:pale_hanging_moss", &[("tip", "false")]);
                                    deco_set(l, sets, q, s);
                                    q = rel(q, 0);
                                }
                                let s = block_with(l, "minecraft:pale_hanging_moss", &[("tip", "true")]);
                                deco_set(l, sets, q, s);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// AlterGroundDecorator.placeCircle
fn alter_circle(l: &mut Level, r: &mut WorldgenXoro, sets: &mut Sets, provider: &Provider, c: P) {
    for xx in -2i32..=2 {
        for zz in -2i32..=2 {
            if xx.abs() != 2 || zz.abs() != 2 {
                let p = (c.0 + xx, c.1, c.2 + zz);
                for dy in (-3..=2).rev() {
                    let q = up(p, dy);
                    if let Some(s) = provider.optional(l, r, q) {
                        deco_set(l, sets, q, s);
                        break;
                    }
                    if !l.is_air(q) && dy < 0 {
                        break;
                    }
                }
            }
        }
    }
}

/// ShelfMushroomDecorator (after the probability roll)
fn shelf_mushrooms(l: &mut Level, r: &mut WorldgenXoro, sets: &mut Sets, logs: &[P]) {
    let water = |l: &Level, p: P| l.name(p) == "minecraft:water";
    let replaceable = |l: &Level, p: P| l.w.blocks.can_be_replaced(l.get(p)) && !(water(l, p) || [5, 4, 2, 3].iter().any(|&d| water(l, rel(p, d))));
    let shelf_at = |l: &Level, p: P| l.name(p) == "minecraft:shelf_mushroom";
    let adjacent = |l: &Level, p: P| HORIZONTAL.iter().any(|&d| shelf_at(l, rel(p, d)));
    let mut place = |l: &mut Level, r: &mut WorldgenXoro, p: P, d: usize| {
        let age = r.next_bound(2).to_string();
        let s = block_with(l, "minecraft:shelf_mushroom", &[("age", &age), ("facing", DIR_NAMES[d])]);
        deco_set(l, sets, p, s);
    };
    let (first, last) = (logs[0], logs[logs.len() - 1]);
    if first.1 == last.1 {
        // a fallen log: both sides across it
        let dirs = if first.0 != last.0 { [2, 3] } else { [5, 4] };
        for &p in logs {
            for d in dirs {
                if r.next_float() <= 0.25 {
                    let q = (p.0 + STEP[d].0, p.1, p.2 + STEP[d].2);
                    if replaceable(l, q) && !adjacent(l, q) && !adjacent(l, p) {
                        place(l, r, q, d);
                    }
                }
            }
        }
    } else {
        let d0 = random_horizontal(r);
        let dirs = [d0, clockwise(d0)];
        let base = first.1;
        for &p in logs {
            if (1..=4).contains(&(p.1 - base)) {
                for d in dirs {
                    if r.next_float() <= 0.25 {
                        let q = (p.0 + STEP[d].0, p.1, p.2 + STEP[d].2);
                        if replaceable(l, q) && !shelf_at(l, rel(q, 0)) {
                            place(l, r, q, d);
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// TreeFeature.updateLeaves: leaf distances from the logs, nearest first.
fn update_leaves(l: &mut Level, sets: &Sets) {
    let all: Vec<P> = sets.roots.iter().chain(sets.trunks.iter()).chain(sets.foliage.iter()).chain(sets.decorations.iter()).copied().collect();
    let (mut lo, mut hi) = (all[0], all[0]);
    for p in &all {
        lo = (lo.0.min(p.0), lo.1.min(p.1), lo.2.min(p.2));
        hi = (hi.0.max(p.0), hi.1.max(p.1), hi.2.max(p.2));
    }
    let inside = |p: P| p.0 >= lo.0 && p.0 <= hi.0 && p.1 >= lo.1 && p.1 <= hi.1 && p.2 >= lo.2 && p.2 <= hi.2;
    let mut filled: std::collections::HashSet<P> = sets.decorations.iter().chain(sets.roots.iter()).copied().collect();
    let mut to_check: Vec<JSet> = (0..7).map(|_| JSet::default()).collect();
    for p in sets.trunks.iter() {
        to_check[0].insert(*p);
    }
    let mut smallest = 0usize;
    loop {
        while smallest >= 7 || !to_check[smallest].is_empty() {
            if smallest >= 7 {
                return;
            }
            let p = to_check[smallest].first().unwrap();
            to_check[smallest].remove(p);
            if !inside(p) {
                continue;
            }
            if smallest != 0 {
                let s = l.get(p);
                if let Some(s2) = l.w.blocks.with(s, "distance", &smallest.to_string()) {
                    l.set_flags(p, s2, 19);
                }
            }
            filled.insert(p);
            for d in 0..6 {
                let q = rel(p, d);
                if !inside(q) || filled.contains(&q) {
                    continue;
                }
                let cs = l.get(q);
                let b = &l.w.blocks;
                let dist = if b.is(cs, "minecraft:prevents_nearby_leaf_decay") {
                    Some(0)
                } else {
                    b.get(cs, "distance").and_then(|v| v.parse::<usize>().ok())
                };
                if let Some(d0) = dist {
                    let nd = d0.min(smallest + 1);
                    if nd < 7 {
                        to_check[nd].insert(q);
                        smallest = smallest.min(nd);
                    }
                }
            }
        }
        smallest += 1;
    }
}
