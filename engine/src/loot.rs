//! Chest loot: LootTable.fill (26.3) interpreted straight from the vanilla loot table JSON.
//!
//! The random is LegacyRandomSource(loot table seed). Entries, conditions and functions follow the
//! game's order of random calls; functions that never touch the random only record what they set.

use crate::biome::{contains, Biomes};
use crate::density::{f32_of, ns};
use crate::rng::{shuffle, JavaRandom, Rng};
use crate::world::json_dir;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// One stack in a chest slot. `extra` holds component-like details ("damage:12", "potion:...").
#[derive(Clone, Debug, PartialEq)]
pub struct Stack {
    pub item: String,
    pub count: i32,
    pub enchants: Vec<(String, i32)>,
    pub extra: Vec<String>,
}

impl Stack {
    pub fn to_json(&self) -> Value {
        let mut v = json!({"item": self.item, "count": self.count});
        if !self.enchants.is_empty() {
            v["enchantments"] = self.enchants.iter().map(|(e, l)| json!([e, l])).collect();
        }
        if !self.extra.is_empty() {
            v["extra"] = json!(self.extra);
        }
        v
    }
}

struct Ench {
    name: String,
    weight: i32,
    max_level: i32,
    min_cost: (i32, i32),
    max_cost: (i32, i32),
    supported: Vec<String>,
    primary: Option<Vec<String>>,
    exclusive: Vec<String>,
}

/// Mob effects whose stew duration is not multiplied by 20 (InstantenousMobEffect).
const INSTANT: [&str; 3] = ["minecraft:instant_health", "minecraft:instant_damage", "minecraft:saturation"];
const AIR: &str = "minecraft:air";

pub struct Loot {
    pub tables: HashMap<String, Value>,
    enchants: Vec<Ench>,                // registry order (sorted ids)
    tags: HashMap<String, Vec<String>>, // "item:<id>", "enchantment:<id>", "instrument:<id>", flattened
    items: HashMap<String, [i32; 3]>,   // max stack size, enchantability, max damage
}

fn cost(v: &Value) -> (i32, i32) {
    (v["base"].as_i64().unwrap_or(0) as i32, v["per_level_above_first"].as_i64().unwrap_or(0) as i32)
}

impl Loot {
    pub fn load(dir: &Path) -> Result<Loot, String> {
        let m = dir.join("minecraft");
        let tables = json_dir(&m.join("loot_table"))?.into_iter().collect();
        let mut raw: HashMap<String, Vec<Value>> = HashMap::new();
        for kind in ["item", "enchantment", "instrument"] {
            let d = m.join("tags").join(kind);
            if d.exists() {
                for (id, v) in json_dir(&d)? {
                    raw.insert(format!("{kind}:{id}"), v["values"].as_array().cloned().unwrap_or_default());
                }
            }
        }
        // TagLoader: entries in file order, nested tags expanded in place, first occurrence wins
        fn flatten(raw: &HashMap<String, Vec<Value>>, key: &str, out: &mut Vec<String>, depth: u32) {
            let kind = key.split(':').next().unwrap();
            for e in raw.get(key).map_or(&[][..], |v| v) {
                let id = e.as_str().or_else(|| e["id"].as_str()).unwrap_or_default();
                match id.strip_prefix('#') {
                    Some(t) if depth < 16 => flatten(raw, &format!("{kind}:{}", ns(t)), out, depth + 1),
                    Some(_) => {}
                    None => {
                        let id = ns(id);
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                }
            }
        }
        let tags = raw
            .keys()
            .map(|k| {
                let mut out = vec![];
                flatten(&raw, k, &mut out, 0);
                (k.clone(), out)
            })
            .collect();
        let text = std::fs::read_to_string(dir.join("items.json")).map_err(|e| format!("items.json: {e} (rerun `python -m sft data`)"))?;
        let items_json: HashMap<String, Vec<i32>> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let items = items_json.into_iter().map(|(k, v)| (k, [v[0], v[1], v.get(2).copied().unwrap_or(0)])).collect();
        let mut loot = Loot { tables, enchants: vec![], tags, items };
        let enchants = json_dir(&m.join("enchantment"))?
            .into_iter()
            .map(|(name, v)| Ench {
                weight: v["weight"].as_i64().unwrap_or(1) as i32,
                max_level: v["max_level"].as_i64().unwrap_or(1) as i32,
                min_cost: cost(&v["min_cost"]),
                max_cost: cost(&v["max_cost"]),
                supported: loot.holders("item", &v["supported_items"]),
                primary: v.get("primary_items").map(|p| loot.holders("item", p)),
                exclusive: loot.holders("enchantment", &v["exclusive_set"]),
                name,
            })
            .collect();
        loot.enchants = enchants;
        Ok(loot)
    }

    /// A HolderSet in JSON: "#tag", "id" or a list of ids.
    fn holders(&self, kind: &str, v: &Value) -> Vec<String> {
        match v {
            Value::String(s) => match s.strip_prefix('#') {
                Some(t) => self.tags.get(&format!("{kind}:{}", ns(t))).cloned().unwrap_or_default(),
                None => vec![ns(s)],
            },
            Value::Array(a) => a.iter().filter_map(Value::as_str).map(ns).collect(),
            _ => vec![],
        }
    }

    fn item(&self, id: &str) -> [i32; 3] {
        if id == AIR {
            return [64, 0, 0];
        }
        self.items.get(id).copied().unwrap_or([64, 0, 0])
    }

    /// LootTable.fill into an empty 27-slot chest. `biome` (at the chest) feeds location_check.
    pub fn fill(&self, table: &str, seed: i64, biome: Option<(u16, &Biomes)>) -> Result<Vec<Option<Stack>>, String> {
        let t = self.tables.get(table).ok_or(format!("unknown loot table '{table}'"))?;
        let mut cx = Cx { l: self, r: JavaRandom::new(seed), biome, chain: vec![], out: vec![] };
        cx.table(t);
        let Cx { mut r, mut out, .. } = cx;
        let mut slots: Vec<usize> = (0..27).collect();
        shuffle(&mut slots, &mut r);
        shuffle_and_split(&mut out, slots.len(), &mut r);
        let mut chest = vec![None; 27];
        for s in out {
            let Some(slot) = slots.pop() else { break };
            chest[slot] = (s.count > 0).then_some(s);
        }
        Ok(chest)
    }
}

/// Mth.nextInt(random, min, max)
fn between(r: &mut impl Rng, min: i32, max: i32) -> i32 {
    if min >= max { min } else { r.next_bound(max - min + 1) + min }
}

/// LootTable.shuffleAndSplitItems
fn shuffle_and_split(items: &mut Vec<Stack>, slots: usize, r: &mut impl Rng) {
    let mut split = vec![];
    let mut keep = vec![];
    for s in items.drain(..) {
        if s.count <= 0 {
            continue;
        }
        if s.count > 1 { split.push(s) } else { keep.push(s) }
    }
    while slots as i64 - keep.len() as i64 - split.len() as i64 > 0 && !split.is_empty() {
        let mut s: Stack = split.remove(between(r, 0, split.len() as i32 - 1) as usize);
        let n = between(r, 1, s.count / 2);
        s.count -= n;
        let copy = Stack { count: n, ..s.clone() };
        if s.count > 1 && r.next_bool() { split.push(s) } else { keep.push(s) }
        if copy.count > 1 && r.next_bool() { split.push(copy) } else { keep.push(copy) }
    }
    keep.append(&mut split);
    shuffle(&mut keep, r);
    *items = keep;
}

/// One fill: the loot context random plus the modifier chain of the entry being generated.
struct Cx<'a> {
    l: &'a Loot,
    r: JavaRandom,
    biome: Option<(u16, &'a Biomes)>,
    chain: Vec<&'a Value>, // modifiers, outermost (table) first
    out: Vec<Stack>,
}

impl<'a> Cx<'a> {
    fn table(&mut self, t: &'a Value) {
        self.chain.push(&t["modifier"]);
        for pool in t["pools"].as_array().map_or(&[][..], |v| v) {
            if !self.test(&pool["condition"]) {
                continue;
            }
            self.chain.push(&pool["modifier"]);
            let rolls = self.int(&pool["rolls"]);
            let _bonus = self.float(&pool["bonus_rolls"]); // times luck (0), but the provider still runs
            for _ in 0..rolls {
                self.roll(pool);
            }
            self.chain.pop();
        }
        self.chain.pop();
    }

    /// LootPool.addRandomItem
    fn roll(&mut self, pool: &'a Value) {
        let mut valid = vec![];
        let mut total = 0;
        for e in pool["entries"].as_array().map_or(&[][..], |v| v) {
            if !self.test(&e["condition"]) {
                continue;
            }
            let w = (e["weight"].as_i64().unwrap_or(1) as i32).max(0); // weight + quality * luck(0)
            if w > 0 {
                valid.push(e);
                total += w;
            }
        }
        if total == 0 || valid.is_empty() {
            return;
        }
        let pick = if valid.len() == 1 {
            valid[0]
        } else {
            let mut i = self.r.next_bound(total);
            *valid
                .iter()
                .find(|e| {
                    i -= e["weight"].as_i64().unwrap_or(1) as i32;
                    i < 0
                })
                .unwrap()
        };
        self.chain.push(&pick["modifier"]);
        match pick["type"].as_str().unwrap_or_default() {
            "minecraft:item" => {
                let item = ns(pick["name"].as_str().unwrap_or_default());
                self.emit(Stack { item, count: 1, enchants: vec![], extra: vec![] });
            }
            "minecraft:loot_table" => {
                let l = self.l;
                let t = match &pick["value"] {
                    Value::String(s) => l.tables.get(&ns(s)),
                    inline @ Value::Object(_) => Some(inline),
                    _ => None,
                };
                if let Some(t) = t {
                    self.table(t);
                }
            }
            _ => {} // empty
        }
        self.chain.pop();
    }

    /// An item leaves its entry: entry, pool, then table modifiers, then the stack splitter.
    fn emit(&mut self, mut s: Stack) {
        for i in (0..self.chain.len()).rev() {
            s = self.apply(self.chain[i], s);
        }
        let max = self.l.item(&s.item)[0].max(1);
        if s.count < max {
            self.out.push(s);
        } else {
            let mut n = s.count;
            while n > 0 {
                self.out.push(Stack { count: n.min(max), ..s.clone() });
                n -= max;
            }
        }
    }

    fn int(&mut self, v: &Value) -> i32 {
        match v {
            Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64) as i32,
            Value::Object(o) => match o.get("type").and_then(Value::as_str).unwrap_or("minecraft:constant") {
                "minecraft:uniform" => {
                    let a = self.int(&v["min"]);
                    let b = self.int(&v["max"]);
                    between(&mut self.r, a, b)
                }
                "minecraft:binomial" => {
                    let n = self.int(&v["n"]);
                    let p = self.float(&v["p"]);
                    (0..n).filter(|_| self.r.next_float() < p).count() as i32
                }
                _ => self.int(&v["value"]),
            },
            _ => 0,
        }
    }

    fn float(&mut self, v: &Value) -> f32 {
        match v {
            Value::Number(_) => f32_of(v),
            Value::Object(o) => match o.get("type").and_then(Value::as_str).unwrap_or("minecraft:constant") {
                "minecraft:uniform" => {
                    let a = self.float(&v["min"]);
                    let b = self.float(&v["max"]);
                    if a >= b { a } else { self.r.next_float() * (b - a) + a }
                }
                _ => self.float(&v["value"]),
            },
            _ => 0.0,
        }
    }

    fn test(&mut self, c: &Value) -> bool {
        match c {
            Value::Null => true,
            Value::Array(a) => a.iter().all(|c| self.test(c)),
            _ => match c["type"].as_str().unwrap_or_default() {
                "minecraft:random_chance" => {
                    let p = self.float(&c["chance"]);
                    self.r.next_float() < p
                }
                "minecraft:inverted" => !self.test(&c["term"]),
                "minecraft:all_of" => c["terms"].as_array().is_some_and(|t| t.iter().all(|c| self.test(c))),
                "minecraft:any_of" => c["terms"].as_array().is_some_and(|t| t.iter().any(|c| self.test(c))),
                // ponytail: only the biome part of location_check, at noise-biome resolution
                "minecraft:location_check" => match (self.biome, c["predicate"].get("biomes")) {
                    (Some((b, biomes)), Some(set)) => contains(biomes.set(set), b),
                    (_, None) => true,
                    _ => false,
                },
                _ => true,
            },
        }
    }

    fn apply(&mut self, f: &Value, mut s: Stack) -> Stack {
        match f {
            Value::Null => return s,
            Value::Array(a) => {
                for f in a {
                    s = self.apply(f, s);
                }
                return s;
            }
            _ => {}
        }
        if !self.test(&f["condition"]) {
            return s;
        }
        let l = self.l;
        let item = if s.count > 0 { s.item.clone() } else { AIR.to_string() }; // an empty stack is air
        match f["type"].as_str().unwrap_or_default() {
            "minecraft:sequence" => return self.apply(&f["functions"], s),
            "minecraft:set_count" => {
                let base = if f["add"].as_bool().unwrap_or(false) { s.count } else { 0 };
                s.count = base + self.int(&f["count"]);
            }
            "minecraft:set_damage" => {
                let max = l.item(&item)[2];
                if max > 0 {
                    let dmg = s.extra.iter().find_map(|e| e.strip_prefix("damage:")).and_then(|d| d.parse::<i32>().ok()).unwrap_or(0);
                    let base = if f["add"].as_bool().unwrap_or(false) { 1.0 - dmg as f32 / max as f32 } else { 0.0 };
                    let pct = 1.0 - (self.float(&f["damage"]) + base).clamp(0.0, 1.0);
                    s.extra.retain(|e| !e.starts_with("damage:"));
                    s.extra.push(format!("damage:{}", (pct * max as f32).floor() as i32));
                }
            }
            "minecraft:enchant_randomly" => {
                let book = item == "minecraft:book";
                let check = !book && f["only_compatible"].as_bool().unwrap_or(true);
                let pool: Vec<&Ench> = self.options(&f["options"]).into_iter().filter(|e| !check || e.supported.contains(&item)).collect();
                if !pool.is_empty() {
                    let e = pool[self.r.next_bound(pool.len() as i32) as usize];
                    let level = between(&mut self.r, 1, e.max_level);
                    if book {
                        s.item = "minecraft:enchanted_book".into();
                    }
                    enchant(&mut s, &e.name, level);
                }
            }
            "minecraft:enchant_with_levels" => {
                let cost = self.int(&f["levels"]);
                let opts = self.options(&f["options"]);
                self.enchant_with_levels(&mut s, &item, cost, &opts);
            }
            "minecraft:set_enchantments" => {
                if item == "minecraft:book" {
                    s.item = "minecraft:enchanted_book".into();
                }
                let add = f["add"].as_bool().unwrap_or(false);
                for (e, lv) in f["enchantments"].as_object().into_iter().flatten() {
                    let e = ns(e);
                    let old = s.enchants.iter().find(|(n, _)| *n == e).map_or(0, |x| x.1);
                    let v = (if add { old } else { 0 } + self.int(lv)).clamp(0, 255);
                    s.enchants.retain(|(n, _)| *n != e);
                    if v > 0 {
                        s.enchants.push((e, v));
                    }
                }
            }
            "minecraft:set_stew_effect" => {
                let effects = f["effects"].as_array().map_or(&[][..], |v| v);
                if item == "minecraft:suspicious_stew" && !effects.is_empty() {
                    let e = &effects[self.r.next_bound(effects.len() as i32) as usize];
                    let id = ns(e["type"].as_str().unwrap_or_default());
                    let mut d = self.int(&e["duration"]);
                    if !INSTANT.contains(&id.as_str()) {
                        d *= 20;
                    }
                    s.extra.push(format!("stew:{id}:{d}"));
                }
            }
            "minecraft:set_instrument" => {
                let opts = l.holders("instrument", &f["options"]);
                if !opts.is_empty() {
                    s.extra.push(format!("instrument:{}", opts[self.r.next_bound(opts.len() as i32) as usize]));
                }
            }
            "minecraft:set_ominous_bottle_amplifier" => {
                let a = self.int(&f["amplifier"]).clamp(0, 4);
                s.extra.push(format!("amplifier:{a}"));
            }
            "minecraft:set_potion" => s.extra.push(format!("potion:{}", ns(f["id"].as_str().unwrap_or_default()))),
            "minecraft:exploration_map" => s.extra.push(format!("map:{}", f["destination"].as_str().unwrap_or("#minecraft:on_treasure_maps"))),
            // ponytail: filters only guard exploration maps (map_id present); assume the map was made
            "minecraft:filtered" => return self.apply(&f["on_pass"], s),
            "minecraft:discard" => s.count = 0,
            _ => {} // set_name, set_lore, ...: no random use, not tracked
        }
        s
    }

    fn options(&self, v: &Value) -> Vec<&'a Ench> {
        let l = self.l;
        if v.is_null() {
            return l.enchants.iter().collect();
        }
        l.holders("enchantment", v).iter().filter_map(|n| l.enchants.iter().find(|e| e.name == *n)).collect()
    }

    /// EnchantmentHelper.enchantItem / selectEnchantment
    fn enchant_with_levels(&mut self, s: &mut Stack, item: &str, mut cost: i32, opts: &[&'a Ench]) {
        let enchantability = self.l.item(item)[1];
        let book = item == "minecraft:book";
        let mut picked: Vec<(&Ench, i32)> = vec![];
        if enchantability > 0 {
            cost += 1 + self.r.next_bound(enchantability / 4 + 1) + self.r.next_bound(enchantability / 4 + 1);
            let span = (self.r.next_float() + self.r.next_float() - 1.0) * 0.15;
            let c = cost as f32;
            cost = ((c + c * span) as f64 + 0.5).floor().clamp(1.0, i32::MAX as f64) as i32; // Math.round
            // getAvailableEnchantmentResults: highest level whose cost window holds `cost`
            let mut avail: Vec<(&Ench, i32)> = opts
                .iter()
                .filter(|e| book || (e.supported.iter().any(|x| x == item) && e.primary.as_ref().is_none_or(|p| p.iter().any(|x| x == item))))
                .filter_map(|e| {
                    (1..=e.max_level)
                        .rev()
                        .find(|&lv| {
                            let (lo, hi) = (e.min_cost.0 + e.min_cost.1 * (lv - 1), e.max_cost.0 + e.max_cost.1 * (lv - 1));
                            cost >= lo && cost <= hi
                        })
                        .map(|lv| (*e, lv))
                })
                .collect();
            if !avail.is_empty() {
                if let Some(x) = weighted(&mut self.r, &avail) {
                    picked.push(x);
                }
                while self.r.next_bound(50) <= cost {
                    if let Some(&(last, _)) = picked.last() {
                        avail.retain(|(e, _)| compatible(last, e));
                    }
                    if avail.is_empty() {
                        break;
                    }
                    if let Some(x) = weighted(&mut self.r, &avail) {
                        picked.push(x);
                    }
                    cost /= 2;
                }
            }
        }
        if book {
            s.item = "minecraft:enchanted_book".into();
        }
        for (e, lv) in picked {
            enchant(s, &e.name, lv);
        }
    }
}

/// Enchantment.areCompatible
fn compatible(a: &Ench, b: &Ench) -> bool {
    a.name != b.name && !a.exclusive.contains(&b.name) && !b.exclusive.contains(&a.name)
}

/// WeightedRandom.getRandomItem
fn weighted<'e>(r: &mut impl Rng, v: &[(&'e Ench, i32)]) -> Option<(&'e Ench, i32)> {
    let total: i32 = v.iter().map(|(e, _)| e.weight).sum();
    if total == 0 {
        return None;
    }
    let mut i = r.next_bound(total);
    v.iter()
        .find(|(e, _)| {
            i -= e.weight;
            i < 0
        })
        .copied()
}

/// ItemStack.enchant: keeps the higher level when already present.
fn enchant(s: &mut Stack, name: &str, level: i32) {
    match s.enchants.iter_mut().find(|(n, _)| n == name) {
        Some(e) => e.1 = e.1.max(level),
        None => s.enchants.push((name.to_string(), level)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_items() {
        let mut r = JavaRandom::new(1);
        let one = |item: &str, count| Stack { item: item.into(), count, enchants: vec![], extra: vec![] };
        let mut v = vec![one("a", 20), one("b", 1)];
        shuffle_and_split(&mut v, 27, &mut r);
        assert_eq!(v.iter().map(|s| s.count).sum::<i32>(), 21);
        assert!(v.len() > 2 && v.len() <= 27);
    }
}
