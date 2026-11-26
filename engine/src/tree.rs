//! TreeFeature (literal port): trunk placers, foliage placers, the leaf distance pass, and a
//! java.util.HashSet<BlockPos> simulation (the leaf pass takes elements in its iteration order).

use crate::feature::{rel, IntP, Level, Provider, P};
use crate::rng::{Rng, WorldgenXoro};
use crate::world::World;
use serde_json::Value;

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

pub enum Trunk {
    Straight,
    /// not ported yet: the tree is not placed
    Other(String),
}

pub enum Foliage {
    Blob { height: i32 },
    Other(String),
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
    pub decorated: bool,
}

impl Tree {
    pub fn parse(v: &Value, w: &World) -> Result<Tree, String> {
        let tp = &v["trunk_placer"];
        let fp = &v["foliage_placer"];
        let ms = &v["minimum_size"];
        let i = |o: &Value, k: &str, d: i64| o[k].as_i64().unwrap_or(d) as i32;
        let three = ms["type"].as_str() == Some("minecraft:three_layers_feature_size");
        Ok(Tree {
            trunk_provider: Provider::parse(&v["trunk_provider"], w)?,
            foliage_provider: Provider::parse(&v["foliage_provider"], w)?,
            below_trunk: Provider::parse(&v["below_trunk_provider"], w)?,
            trunk: match tp["type"].as_str().unwrap_or_default() {
                "minecraft:straight_trunk_placer" => Trunk::Straight,
                t => Trunk::Other(t.to_string()),
            },
            base_height: i(tp, "base_height", 0),
            rand_a: i(tp, "height_rand_a", 0),
            rand_b: i(tp, "height_rand_b", 0),
            foliage: match fp["type"].as_str().unwrap_or_default() {
                "minecraft:blob_foliage_placer" => Foliage::Blob { height: i(fp, "height", 0) },
                t => Foliage::Other(t.to_string()),
            },
            radius: IntP::parse(&fp["radius"])?,
            offset: IntP::parse(&fp["offset"])?,
            size: if three {
                (i(ms, "limit", 1), i(ms, "upper_limit", 1), i(ms, "lower_size", 0), i(ms, "middle_size", 1), i(ms, "upper_size", 1), true)
            } else {
                (i(ms, "limit", 1), 0, i(ms, "lower_size", 0), 0, i(ms, "upper_size", 1), false)
            },
            min_clipped: ms["min_clipped_height"].as_i64().map(|n| n as i32),
            ignore_vines: v["ignore_vines"].as_bool().unwrap_or(false),
            decorated: v["decorators"].as_array().is_some_and(|a| !a.is_empty()),
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
struct Sets {
    trunks: JSet,
    foliage: JSet,
}

fn valid_tree_pos(l: &Level, p: P) -> bool {
    let s = l.get(p);
    l.w.blocks.is_air(s) || l.w.blocks.is(s, "minecraft:replaceable_by_trees")
}

fn is_free(l: &Level, p: P) -> bool {
    valid_tree_pos(l, p) || l.w.blocks.is(l.get(p), "minecraft:logs")
}

fn place_log(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, p: P) -> bool {
    if valid_tree_pos(l, p) {
        let s = t.trunk_provider.state(l, r, p);
        sets.trunks.insert(p);
        l.set_flags(p, s, 19);
        true
    } else {
        false
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

/// FoliagePlacer.placeLeavesRow
#[allow(clippy::too_many_arguments)]
fn leaves_row(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, sets: &mut Sets, o: P, radius: i32, y: i32, double: bool) {
    let off = double as i32;
    for dx in -radius..=radius + off {
        for dz in -radius..=radius + off {
            let (mdx, mdz) = if double { (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs())) } else { (dx.abs(), dz.abs()) };
            if !skip(&t.foliage, r, mdx, y, mdz, radius) {
                try_place_leaf(l, r, t, sets, (o.0 + dx, o.1 + y, o.2 + dz));
            }
        }
    }
}

fn skip(f: &Foliage, r: &mut WorldgenXoro, dx: i32, y: i32, dz: i32, radius: i32) -> bool {
    match f {
        Foliage::Blob { .. } => dx == radius && dz == radius && (r.next_bound(2) == 0 || y == 0),
        Foliage::Other(_) => true,
    }
}

/// TreeFeature.place
pub fn tree(l: &mut Level, r: &mut WorldgenXoro, t: &Tree, o: P) -> bool {
    if matches!(t.trunk, Trunk::Other(_)) || matches!(t.foliage, Foliage::Other(_)) {
        // ponytail: trunk and foliage placers other than straight/blob are being ported
        return false;
    }
    let mut sets = Sets { trunks: JSet::default(), foliage: JSet::default() };
    // doPlace
    let tree_height = t.base_height + r.next_bound(t.rand_a + 1) + r.next_bound(t.rand_b + 1);
    let foliage_height = match t.foliage {
        Foliage::Blob { height } => height,
        Foliage::Other(_) => 0,
    };
    let trunk_height = tree_height - foliage_height;
    let _ = trunk_height;
    let leaf_radius = t.radius.sample(r);
    let trunk_origin = o;
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
                if !is_free(l, p) || (!t.ignore_vines && l.name(p) == "minecraft:vine") {
                    clipped = y - 2;
                    break 'outer;
                }
            }
        }
    }
    if !(clipped >= tree_height || t.min_clipped.is_some_and(|m| clipped >= m)) {
        return false;
    }
    // trunk
    let attachments: Vec<(P, i32, bool)> = match t.trunk {
        Trunk::Straight => {
            let below = rel(trunk_origin, 0);
            if let Some(s) = t.below_trunk.optional(l, r, below) {
                sets.trunks.insert(below);
                l.set_flags(below, s, 19);
            }
            for y in 0..clipped {
                place_log(l, r, t, &mut sets, (trunk_origin.0, trunk_origin.1 + y, trunk_origin.2));
            }
            vec![((trunk_origin.0, trunk_origin.1 + clipped, trunk_origin.2), 0, false)]
        }
        Trunk::Other(_) => vec![],
    };
    // foliage
    for (pos, radius_offset, double) in attachments {
        let offset = t.offset.sample(r);
        match t.foliage {
            Foliage::Blob { .. } => {
                let mut yo = offset;
                while yo >= offset - foliage_height {
                    let radius = (leaf_radius + radius_offset - 1 - yo / 2).max(0);
                    leaves_row(l, r, t, &mut sets, pos, radius, yo, double);
                    yo -= 1;
                }
            }
            Foliage::Other(_) => {}
        }
    }
    if sets.trunks.is_empty() && sets.foliage.is_empty() {
        return false;
    }
    // ponytail: tree decorators (vines, beehives, cocoa...) are being ported
    update_leaves(l, &sets);
    true
}

/// TreeFeature.updateLeaves: leaf distances from the logs, nearest first.
fn update_leaves(l: &mut Level, sets: &Sets) {
    let all: Vec<P> = sets.trunks.iter().chain(sets.foliage.iter()).copied().collect();
    let (mut lo, mut hi) = (all[0], all[0]);
    for p in &all {
        lo = (lo.0.min(p.0), lo.1.min(p.1), lo.2.min(p.2));
        hi = (hi.0.max(p.0), hi.1.max(p.1), hi.2.max(p.2));
    }
    let inside = |p: P| p.0 >= lo.0 && p.0 <= hi.0 && p.1 >= lo.1 && p.1 <= hi.1 && p.2 >= lo.2 && p.2 <= hi.2;
    let mut filled: std::collections::HashSet<P> = std::collections::HashSet::new();
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
