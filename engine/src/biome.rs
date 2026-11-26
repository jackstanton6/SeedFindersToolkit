//! Climate (biome/Climate.java): quantized parameter points and the R-tree nearest search,
//! plus the biome registry and biome tags.

use crate::density::{f32_of, ns};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

/// Climate.quantizeCoord: (long)(coord * 10000.0F)
pub fn quantize(v: f32) -> i64 {
    (v * 10000.0f32) as i64
}

#[derive(Clone, Copy, Debug)]
struct Param {
    min: i64,
    max: i64,
}

impl Param {
    fn dist(&self, t: i64) -> i64 {
        let (above, below) = (t - self.max, self.min - t);
        if above > 0 { above } else { below.max(0) }
    }
    fn center(&self) -> i64 {
        (self.min + self.max) / 2
    }
}

type Space = [Param; 7];

fn dist(space: &Space, t: &[i64; 7]) -> i64 {
    space.iter().zip(t).map(|(p, &v)| { let d = p.dist(v); d * d }).sum()
}

fn span<'a>(spaces: impl Iterator<Item = &'a Space>) -> Space {
    let mut out: Option<Space> = None;
    for s in spaces {
        out = Some(match out {
            None => *s,
            Some(o) => std::array::from_fn(|d| Param { min: o[d].min.min(s[d].min), max: o[d].max.max(s[d].max) }),
        });
    }
    out.expect("SubTree needs at least one child")
}

/// RTree comparator: dimension `d`, then the following dimensions cyclically.
fn cmp_dim(a: &Space, b: &Space, d: usize, absolute: bool) -> Ordering {
    (0..7)
        .map(|k| (d + k) % 7)
        .map(|k| if absolute { a[k].center().abs().cmp(&b[k].center().abs()) } else { a[k].center().cmp(&b[k].center()) })
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

enum Node {
    Leaf(usize),
    Sub(Space, Vec<Node>),
}

/// Climate.ParameterList<Biome> with its RTree.
pub struct Climate {
    leaves: Vec<(Space, u16)>,
    root: Node,
}

const PER_NODE: usize = 19;

impl Climate {
    /// Loads the data generator's biome_parameters report. Report floats are unquantize(q),
    /// so q is recovered by rounding (truncating could be off by one).
    pub fn load(path: &Path, biomes: &Biomes) -> Result<Climate, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let q = |x: &Value| (f32_of(x) as f64 * 10000.0).round() as i64;
        let param = |x: &Value| match x {
            Value::Array(a) => Param { min: q(&a[0]), max: q(&a[1]) },
            _ => Param { min: q(x), max: q(x) },
        };
        let mut leaves = vec![];
        for e in v["biomes"].as_array().ok_or("biome_parameters: no biomes")? {
            let p = &e["parameters"];
            let off = q(&p["offset"]);
            let space = [
                param(&p["temperature"]), param(&p["humidity"]), param(&p["continentalness"]),
                param(&p["erosion"]), param(&p["depth"]), param(&p["weirdness"]), Param { min: off, max: off },
            ];
            let name = e["biome"].as_str().unwrap();
            leaves.push((space, biomes.id(name).ok_or_else(|| format!("unknown biome {name}"))?));
        }
        let root = build(&leaves, (0..leaves.len()).map(Node::Leaf).collect());
        Ok(Climate { leaves, root })
    }

    /// RTree.search with the per-thread `lastResult` warm start (`last` is the caller's slot).
    /// Number of R-tree leaves (valid warm starts).
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    pub fn search(&self, t: &[i64; 7], last: &mut Option<usize>) -> u16 {
        let leaf = self.search_node(&self.root, t, *last);
        *last = Some(leaf);
        self.leaves[leaf].1
    }

    fn search_node(&self, n: &Node, t: &[i64; 7], cand: Option<usize>) -> usize {
        match n {
            Node::Leaf(i) => *i,
            Node::Sub(_, kids) => {
                let mut min = cand.map_or(i64::MAX, |c| dist(&self.leaves[c].0, t));
                let mut best = cand;
                for k in kids {
                    let cd = dist(&space_of(&self.leaves, k), t);
                    if min > cd {
                        let leaf = self.search_node(k, t, best);
                        let ld = if matches!(k, Node::Leaf(_)) { cd } else { dist(&self.leaves[leaf].0, t) };
                        if min > ld {
                            min = ld;
                            best = Some(leaf);
                        }
                    }
                }
                best.unwrap()
            }
        }
    }

    /// The R-tree for the GPU, flattened: nodes [leaf index or -1, first kid, kid count, space],
    /// kid node ids, spaces (7 x [min, max] each) and the biome of each leaf (leaf i has space i).
    pub fn flat(&self) -> (Vec<[i32; 4]>, Vec<i32>, Vec<i64>, Vec<u16>) {
        fn walk(c: &Climate, n: &Node, nodes: &mut Vec<[i32; 4]>, kids: &mut Vec<i32>, spaces: &mut Vec<i64>) -> i32 {
            let id = nodes.len() as i32;
            match n {
                Node::Leaf(i) => nodes.push([*i as i32, 0, 0, *i as i32]),
                Node::Sub(s, ks) => {
                    nodes.push([-1, 0, ks.len() as i32, (spaces.len() / 14) as i32]);
                    spaces.extend(s.iter().flat_map(|p| [p.min, p.max]));
                    let ids: Vec<i32> = ks.iter().map(|k| walk(c, k, nodes, kids, spaces)).collect();
                    nodes[id as usize][1] = kids.len() as i32;
                    kids.extend(ids);
                }
            }
            id
        }
        // leaf spaces first, so leaf i's space is space i
        let mut spaces: Vec<i64> = self.leaves.iter().flat_map(|(s, _)| s.iter().flat_map(|p| [p.min, p.max])).collect();
        let (mut nodes, mut kids) = (vec![], vec![]);
        walk(self, &self.root, &mut nodes, &mut kids, &mut spaces);
        (nodes, kids, spaces, self.leaves.iter().map(|l| l.1).collect())
    }

    /// BiomeSource.possibleBiomes
    pub fn possible(&self) -> BiomeSet {
        self.leaves.iter().fold(0, |acc, (_, b)| acc | 1u128 << b)
    }

    /// findValueBruteForce, used by tests to cross-check the tree.
    pub fn search_brute(&self, t: &[i64; 7]) -> u16 {
        let mut best = (i64::MAX, 0u16);
        for (s, b) in &self.leaves {
            let d = dist(s, t);
            if d < best.0 {
                best = (d, *b);
            }
        }
        best.1
    }
}

fn space_of(leaves: &[(Space, u16)], n: &Node) -> Space {
    match n {
        Node::Leaf(i) => leaves[*i].0,
        Node::Sub(s, _) => *s,
    }
}

fn subtree(leaves: &[(Space, u16)], kids: Vec<Node>) -> Node {
    let s = span(kids.iter().map(|k| space_of(leaves, k)).collect::<Vec<_>>().iter());
    Node::Sub(s, kids)
}

/// RTree.build. Java sorts one list in place for d = 0..6 and keeps the cheapest bucketing;
/// sorting an index permutation the same way reproduces every intermediate order exactly.
fn build(leaves: &[(Space, u16)], mut children: Vec<Node>) -> Node {
    if children.len() == 1 {
        return children.pop().unwrap();
    }
    let spaces: Vec<Space> = children.iter().map(|n| space_of(leaves, n)).collect();
    let mut order: Vec<usize> = (0..children.len()).collect();
    let mut slots: Vec<Option<Node>> = children.into_iter().map(Some).collect();
    if slots.len() <= PER_NODE {
        order.sort_by_key(|&i| spaces[i].iter().map(|p| p.center().abs()).sum::<i64>());
        return subtree(leaves, order.iter().map(|&i| slots[i].take().unwrap()).collect());
    }
    let n = order.len() as f64;
    let expected = (PER_NODE as f64).powf(((n - 0.01).ln() / (PER_NODE as f64).ln()).floor()) as usize;
    let mut best: Option<(i64, usize, Vec<usize>)> = None;
    for d in 0..7 {
        order.sort_by(|&a, &b| cmp_dim(&spaces[a], &spaces[b], d, false));
        let total: i64 = order
            .chunks(expected)
            .map(|c| span(c.iter().map(|&i| &spaces[i])).iter().map(|p| (p.max - p.min).abs()).sum::<i64>())
            .sum();
        if best.as_ref().is_none_or(|(c, ..)| *c > total) {
            best = Some((total, d, order.clone()));
        }
    }
    let (_, dim, best_order) = best.unwrap();
    let mut buckets: Vec<(Space, Vec<Node>)> = best_order
        .chunks(expected)
        .map(|c| (span(c.iter().map(|&i| &spaces[i])), c.iter().map(|&i| slots[i].take().unwrap()).collect()))
        .collect();
    buckets.sort_by(|a, b| cmp_dim(&a.0, &b.0, dim, true));
    subtree(leaves, buckets.into_iter().map(|(_, kids)| build(leaves, kids)).collect())
}

/// Biome registry: ids are positions in the sorted list of biome files.
pub struct Biomes {
    pub names: Vec<String>,
    by_name: HashMap<String, u16>,
    tags: HashMap<String, Vec<Value>>,
    /// Biome.ClimateSettings per biome: base temperature and whether the modifier is "frozen".
    pub climate: Vec<(f32, bool)>,
    /// The biome JSON (generation settings: features, carvers), by id.
    pub json: Vec<Value>,
}

/// Bitset over biome ids (fits the ~65 overworld/nether/end biomes).
pub type BiomeSet = u128;

impl Biomes {
    pub fn load(data: &Path) -> Result<Biomes, String> {
        let dir = data.join("minecraft/worldgen/biome");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(|n| format!("minecraft:{n}")))
            .collect();
        names.sort();
        assert!(names.len() <= 128, "more than 128 biomes");
        let by_name = names.iter().enumerate().map(|(i, n)| (n.clone(), i as u16)).collect();
        let mut tags = HashMap::new();
        let tag_dir = data.join("minecraft/tags/worldgen/biome");
        for (id, v) in crate::world::json_dir(&tag_dir)? {
            tags.insert(id, v["values"].as_array().cloned().unwrap_or_default());
        }
        let mut climate = vec![];
        let mut json = vec![];
        for n in &names {
            let path = dir.join(format!("{}.json", n.trim_start_matches("minecraft:")));
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| e.to_string())?;
            climate.push((f32_of(&v["temperature"]), v["temperature_modifier"].as_str() == Some("frozen")));
            json.push(v);
        }
        Ok(Biomes { names, by_name, tags, climate, json })
    }

    pub fn id(&self, name: &str) -> Option<u16> {
        self.by_name.get(&ns(name)).copied()
    }

    pub fn name(&self, id: u16) -> &str {
        &self.names[id as usize]
    }

    /// Resolves a HolderSet<Biome> as written in JSON: "#tag", "id", or a list of ids.
    pub fn set(&self, v: &Value) -> BiomeSet {
        match v {
            Value::String(s) => match s.strip_prefix('#') {
                Some(tag) => self.tag(&ns(tag)),
                None => self.id(s).map_or(0, |i| 1u128 << i),
            },
            Value::Array(a) => a.iter().fold(0, |acc, x| acc | self.set(x)),
            Value::Object(o) => o.get("id").map_or(0, |x| self.set(x)),
            _ => 0,
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.contains_key(tag)
    }

    pub fn tag_names(&self) -> impl Iterator<Item = &str> {
        self.tags.keys().map(String::as_str)
    }

    pub fn tag(&self, tag: &str) -> BiomeSet {
        self.tags.get(tag).map_or(0, |vals| vals.iter().fold(0, |acc, x| acc | self.set(x)))
    }
}

pub fn contains(set: BiomeSet, biome: u16) -> bool {
    set >> biome & 1 != 0
}
