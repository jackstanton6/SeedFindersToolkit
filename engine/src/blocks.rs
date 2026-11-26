//! Block states as the game numbers them (Block.BLOCK_STATE_REGISTRY ids, from the data generator's
//! reports/blocks.json), their properties, and block tags.

use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// A block state id.
pub type State = u16;

pub struct Blocks {
    /// Per block: name, first state id, property names and their values (in state id order:
    /// the last property varies fastest).
    names: Vec<String>,
    first: Vec<State>,
    props: Vec<Vec<(String, Vec<String>)>>,
    default: Vec<State>,
    by_name: HashMap<String, usize>,
    /// State id -> block index.
    block_of: Vec<u16>,
    tags: HashMap<String, Vec<String>>,
    pub air: State,
    pub cave_air: State,
    pub void_air: State,
    pub stone: State,
    pub water: State,
    pub lava: State,
}

impl Blocks {
    pub fn load(data: &Path) -> Result<Blocks, String> {
        let path = data.join("reports/blocks.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e} (run `python -m sft data <version>` again)", path.display()))?)
            .map_err(|e| e.to_string())?;
        let mut b = Blocks {
            names: vec![], first: vec![], props: vec![], default: vec![], by_name: HashMap::new(), block_of: vec![], tags: HashMap::new(),
            air: 0, cave_air: 0, void_air: 0, stone: 0, water: 0, lava: 0,
        };
        let mut blocks: Vec<(&String, &Value)> = v.as_object().ok_or("blocks.json")?.iter().collect();
        blocks.sort_by_key(|(_, d)| d["states"].as_array().map_or(0, |s| s.iter().filter_map(|x| x["id"].as_u64()).min().unwrap_or(0)));
        for (name, d) in blocks {
            let i = b.names.len();
            let props: Vec<(String, Vec<String>)> = d["properties"]
                .as_object()
                .map(|o| o.iter().map(|(k, vs)| (k.clone(), vs.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect())).collect())
                .unwrap_or_default();
            let states = d["states"].as_array().ok_or("blocks.json states")?;
            let first = states.iter().filter_map(|s| s["id"].as_u64()).min().unwrap() as State;
            let default = states.iter().find(|s| s["default"].as_bool() == Some(true)).and_then(|s| s["id"].as_u64()).unwrap_or(first as u64) as State;
            let last = states.iter().filter_map(|s| s["id"].as_u64()).max().unwrap() as usize;
            if b.block_of.len() <= last {
                b.block_of.resize(last + 1, 0);
            }
            for id in first as usize..=last {
                b.block_of[id] = i as u16;
            }
            b.by_name.insert(name.clone(), i);
            b.names.push(name.clone());
            b.first.push(first);
            b.props.push(props);
            b.default.push(default);
        }
        // serde_json keeps object keys sorted (no preserve_order), and the game also orders a
        // block's properties by name, so `props` is in state id order
        for (id, v) in crate::world::json_dir(&data.join("minecraft/tags/block"))? {
            let vals = v["values"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().or_else(|| x["id"].as_str()).map(String::from)).collect()).unwrap_or_default();
            b.tags.insert(id, vals);
        }
        let get = |b: &Blocks, n: &str| b.state(n).ok_or(format!("blocks.json has no {n}"));
        b.air = get(&b, "minecraft:air")?;
        b.cave_air = get(&b, "minecraft:cave_air")?;
        b.void_air = get(&b, "minecraft:void_air")?;
        b.stone = get(&b, "minecraft:stone")?;
        b.water = get(&b, "minecraft:water")?;
        b.lava = get(&b, "minecraft:lava")?;
        Ok(b)
    }

    /// The default state of a block ("minecraft:stone" or "stone").
    pub fn state(&self, name: &str) -> Option<State> {
        self.by_name.get(&crate::density::ns(name)).map(|&i| self.default[i])
    }

    /// A state written as JSON: a name, or {"Name": ..., "Properties": {...}} (BlockState.CODEC).
    pub fn parse(&self, v: &Value) -> Result<State, String> {
        let name = v.as_str().or_else(|| v["Name"].as_str()).ok_or(format!("not a block state: {v}"))?;
        let mut s = self.state(name).ok_or(format!("unknown block {name}"))?;
        if let Some(p) = v["Properties"].as_object() {
            for (k, val) in p {
                s = self.with(s, k, val.as_str().unwrap_or_default()).ok_or(format!("{name} has no {k}={val}"))?;
            }
        }
        Ok(s)
    }

    fn block(&self, s: State) -> usize {
        self.block_of[s as usize] as usize
    }

    pub fn name(&self, s: State) -> &str {
        &self.names[self.block(s)]
    }

    /// The value of property `k`, if the block has it.
    pub fn get(&self, s: State, k: &str) -> Option<&str> {
        let b = self.block(s);
        let mut rest = (s - self.first[b]) as usize;
        let props = &self.props[b];
        let mut out = None;
        for (name, vals) in props.iter().rev() {
            let i = rest % vals.len();
            rest /= vals.len();
            if name == k {
                out = Some(vals[i].as_str());
            }
        }
        out
    }

    /// `s` with property `k` set to `v`.
    pub fn with(&self, s: State, k: &str, v: &str) -> Option<State> {
        let b = self.block(s);
        let (mut stride, mut rest) = (1isize, (s - self.first[b]) as usize);
        for (name, vals) in self.props[b].iter().rev() {
            let i = rest % vals.len();
            rest /= vals.len();
            if name == k {
                let j = vals.iter().position(|x| x == v)?;
                return Some((s as isize + (j as isize - i as isize) * stride) as State);
            }
            stride *= vals.len() as isize;
        }
        None
    }

    /// BlockStateParser.serialize: "minecraft:grass_block[snowy=false]".
    pub fn to_string(&self, s: State) -> String {
        let b = self.block(s);
        let props = &self.props[b];
        if props.is_empty() {
            return self.names[b].clone();
        }
        let vals: Vec<String> = props.iter().map(|(k, _)| format!("{k}={}", self.get(s, k).unwrap())).collect();
        format!("{}[{}]", self.names[b], vals.join(","))
    }

    /// BlockState.isAir
    pub fn is_air(&self, s: State) -> bool {
        s == self.air || s == self.cave_air || s == self.void_air
    }

    /// !getFluidState().isEmpty(): water, lava (any level), bubble columns and waterlogged blocks.
    pub fn has_fluid(&self, s: State) -> bool {
        let n = self.name(s);
        n == "minecraft:water" || n == "minecraft:lava" || n == "minecraft:bubble_column" || self.get(s, "waterlogged") == Some("true")
    }

    /// Whether the state's block is in a block tag ("minecraft:dirt", without '#').
    pub fn is(&self, s: State, tag: &str) -> bool {
        self.in_tag(self.name(s), tag, 0)
    }

    fn in_tag(&self, block: &str, tag: &str, depth: usize) -> bool {
        let Some(vals) = self.tags.get(tag) else { return false };
        depth < 16
            && vals.iter().any(|v| match v.strip_prefix('#') {
                Some(t) => self.in_tag(block, &crate::density::ns(t), depth + 1),
                None => crate::density::ns(v) == block,
            })
    }

    /// Every block in a tag, as a per-block table (index = block index) for fast checks.
    pub fn tag_table(&self, tag: &str) -> Vec<bool> {
        self.names.iter().map(|n| self.in_tag(n, tag, 0)).collect()
    }

    /// A per-block table lookup for a state.
    pub fn in_table(&self, table: &[bool], s: State) -> bool {
        table[self.block(s)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reported_state_round_trips() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/26.3");
        if !dir.join("reports/blocks.json").exists() {
            eprintln!("skipped: needs data/26.3");
            return;
        }
        let b = Blocks::load(&dir).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("reports/blocks.json")).unwrap()).unwrap();
        let mut n = 0;
        for (name, d) in v.as_object().unwrap() {
            for st in d["states"].as_array().unwrap() {
                let mut s = b.state(name).unwrap();
                for (k, val) in st["properties"].as_object().into_iter().flatten() {
                    s = b.with(s, k, val.as_str().unwrap()).unwrap();
                    assert_eq!(b.get(s, k), val.as_str());
                }
                assert_eq!(s as u64, st["id"].as_u64().unwrap(), "{name} {}", st["properties"]);
                n += 1;
            }
        }
        assert!(n > 30000);
        assert_eq!(b.to_string(b.state("grass_block").unwrap()), "minecraft:grass_block[snowy=false]");
    }
}
