//! Structure sets, placement and generation checks (levelgen/structure/*) for overworld types.
//! A structure "exists" in a chunk when its set places it there, the set's weighted pick reaches it,
//! and its findValidGenerationPoint succeeds (start point + biome at that point).

use crate::biome::{contains, BiomeSet, Biomes};
use crate::density::{f32_of, ns};
use crate::rng::{shuffle, JavaRandom, Rng};
use crate::world::{json_dir, BiomeCtx, Block, Heightmap, Seeded};
use flate2::read::GzDecoder;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Reduction {
    Default,
    Legacy1,
    Legacy2,
    Legacy3,
}

#[derive(Clone, Debug)]
pub enum Placement {
    Spread { spacing: i32, separation: i32, triangular: bool },
    Rings { distance: i32, spread: i32, count: i32, preferred: BiomeSet },
}

#[derive(Clone, Debug)]
pub struct Set {
    pub name: String,
    pub placement: Placement,
    salt: i32,
    frequency: f32,
    reduction: Reduction,
    exclusion: Option<(usize, i32)>,
    pub locate_offset: (i32, i32, i32),
    pub entries: Vec<(usize, i32)>,
    /// Some entry can generate in the overworld biome source (ChunkGeneratorStructureState filter).
    pub possible: bool,
}

#[derive(Clone, Debug)]
enum Anchor {
    Abs(i32),
    AboveBottom(i32),
    BelowTop(i32),
    Sea(i32),
}

#[derive(Clone, Debug)]
enum HeightProv {
    Const(Anchor),
    Uniform(Anchor, Anchor),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PortalPlacement {
    OnLandSurface,
    PartlyBuried,
    OnOceanFloor,
    InMountain,
    Underground,
    InNether,
}

impl PortalPlacement {
    /// The game's name (data/json "placement" values).
    fn name(self) -> &'static str {
        match self {
            PortalPlacement::OnLandSurface => "on_land_surface",
            PortalPlacement::PartlyBuried => "partly_buried",
            PortalPlacement::OnOceanFloor => "on_ocean_floor",
            PortalPlacement::InMountain => "in_mountain",
            PortalPlacement::Underground => "underground",
            PortalPlacement::InNether => "in_nether",
        }
    }
}

#[derive(Clone, Debug)]
struct PortalSetup {
    placement: PortalPlacement,
    air_pocket: f32,
    weight: f32,
}

/// Which piece generator an on-top-of-chunk structure runs (for piece details).
#[derive(Clone, Copy, Debug)]
enum Piece {
    SwampHut,
    Igloo,
    BuriedTreasure,
    Shipwreck { beached: bool },
    OceanRuin { large: f32, cluster: f32 },
}

#[derive(Clone, Debug)]
enum Kind {
    Jigsaw { pool: String, height: HeightProv, project: Option<Heightmap>, start_jigsaw: Option<String>, padding: Option<(i32, i32)> },
    SinglePiece(i32, i32, i32), // ScatteredFeaturePiece width, height, depth
    OnTop(Heightmap, Piece),
    Monument,
    Mansion,
    RuinedPortal(Vec<PortalSetup>),
    Mineshaft,
    Stronghold,
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct Structure {
    pub name: String,
    kind: Kind,
    pub biomes: BiomeSet,
}

impl Structure {
    /// False where the start point is approximated (mineshaft depth needs its piece generator).
    pub fn exact(&self) -> bool {
        !matches!(self.kind, Kind::Mineshaft | Kind::Unsupported)
    }
}

#[derive(Clone, Debug)]
enum Element {
    Empty,
    Single(String),
    List(Vec<Element>),
    Feature,
}

struct Pool {
    elements: Vec<Element>,
    expanded: Vec<usize>,
}

struct Template {
    size: [i32; 3],
    markers: Vec<([i32; 3], String)>, // data markers (structure blocks in DATA mode), sorted by (y, x, z)
    jigsaws: Vec<([i32; 3], String, i32)>, // (pos, name, selection priority), sorted by (y, x, z)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub structure: usize,
    pub chunk: (i32, i32),
    pub pos: (i32, i32, i32),
    /// First piece's box [minX, minY, minZ, maxX, maxY, maxZ] where cheap to know (y before
    /// postProcess terrain fitting); None for structures whose layout needs the piece generator.
    pub bbox: Option<[i32; 6]>,
    /// Piece details: "basement", "large", "cluster", "ruins:6", "template:shipwreck/with_mast", ...
    pub tags: Vec<String>,
}

pub struct Structures {
    pub sets: Vec<Set>,
    pub structures: Vec<Structure>,
    by_name: HashMap<String, usize>,
    set_by_name: HashMap<String, usize>,
    pools: HashMap<String, Pool>,
    templates: HashMap<String, Template>,
    monument_surrounding: BiomeSet,
}

const PORTALS: [&str; 10] = [
    "ruined_portal/portal_1", "ruined_portal/portal_2", "ruined_portal/portal_3", "ruined_portal/portal_4", "ruined_portal/portal_5",
    "ruined_portal/portal_6", "ruined_portal/portal_7", "ruined_portal/portal_8", "ruined_portal/portal_9", "ruined_portal/portal_10",
];
const GIANT_PORTALS: [&str; 3] = ["ruined_portal/giant_portal_1", "ruined_portal/giant_portal_2", "ruined_portal/giant_portal_3"];

// ------------------------------------------------------------------------------------------------
// Loading
// ------------------------------------------------------------------------------------------------

fn anchor(v: &Value) -> Result<Anchor, String> {
    let get = |k: &str| v.get(k).and_then(Value::as_i64).map(|x| x as i32);
    if let Some(y) = get("absolute") { return Ok(Anchor::Abs(y)); }
    if let Some(y) = get("above_bottom") { return Ok(Anchor::AboveBottom(y)); }
    if let Some(y) = get("below_top") { return Ok(Anchor::BelowTop(y)); }
    if let Some(y) = get("relative_to_sea_level") { return Ok(Anchor::Sea(y)); }
    Err(format!("bad vertical anchor {v}"))
}

fn height_provider(v: &Value) -> Result<HeightProv, String> {
    match v.get("type").and_then(Value::as_str) {
        None => Ok(HeightProv::Const(anchor(v)?)),
        Some("minecraft:constant") => Ok(HeightProv::Const(anchor(&v["value"])?)),
        Some("minecraft:uniform") => Ok(HeightProv::Uniform(anchor(&v["min_inclusive"])?, anchor(&v["max_inclusive"])?)),
        Some(t) => Err(format!("unsupported height provider {t}")),
    }
}

fn heightmap(v: &Value) -> Option<Heightmap> {
    match v.as_str()? {
        "WORLD_SURFACE_WG" => Some(Heightmap::WorldSurfaceWg),
        "OCEAN_FLOOR_WG" => Some(Heightmap::OceanFloorWg),
        _ => None,
    }
}

fn kind(v: &Value) -> Result<Kind, String> {
    Ok(match v["type"].as_str().unwrap_or_default() {
        "minecraft:jigsaw" => Kind::Jigsaw {
            pool: ns(v["start_pool"].as_str().ok_or("jigsaw without start_pool")?),
            height: height_provider(&v["start_height"])?,
            project: v.get("project_start_to_heightmap").and_then(heightmap),
            start_jigsaw: v.get("start_jigsaw_name").and_then(Value::as_str).map(ns),
            padding: match v.get("dimension_padding") {
                Some(Value::Number(n)) => Some((n.as_i64().unwrap() as i32, n.as_i64().unwrap() as i32)),
                Some(o @ Value::Object(_)) => Some((o["bottom"].as_i64().unwrap_or(0) as i32, o["top"].as_i64().unwrap_or(0) as i32)),
                _ => None,
            },
        },
        "minecraft:desert_pyramid" => Kind::SinglePiece(21, 15, 21),
        "minecraft:jungle_temple" => Kind::SinglePiece(12, 10, 15),
        "minecraft:swamp_hut" => Kind::OnTop(Heightmap::WorldSurfaceWg, Piece::SwampHut),
        "minecraft:igloo" => Kind::OnTop(Heightmap::WorldSurfaceWg, Piece::Igloo),
        "minecraft:buried_treasure" => Kind::OnTop(Heightmap::OceanFloorWg, Piece::BuriedTreasure),
        "minecraft:ocean_ruin" => Kind::OnTop(Heightmap::OceanFloorWg, Piece::OceanRuin {
            large: f32_of(&v["large_probability"]),
            cluster: f32_of(&v["cluster_probability"]),
        }),
        "minecraft:shipwreck" => {
            let beached = v["is_beached"].as_bool().unwrap_or(false);
            Kind::OnTop(if beached { Heightmap::WorldSurfaceWg } else { Heightmap::OceanFloorWg }, Piece::Shipwreck { beached })
        }
        "minecraft:ocean_monument" => Kind::Monument,
        "minecraft:woodland_mansion" => Kind::Mansion,
        "minecraft:mineshaft" => Kind::Mineshaft,
        "minecraft:stronghold" => Kind::Stronghold,
        "minecraft:ruined_portal" => Kind::RuinedPortal(
            v["setups"].as_array().ok_or("ruined portal without setups")?.iter().map(|s| PortalSetup {
                placement: match s["placement"].as_str().unwrap_or_default() {
                    "partly_buried" => PortalPlacement::PartlyBuried,
                    "on_ocean_floor" => PortalPlacement::OnOceanFloor,
                    "in_mountain" => PortalPlacement::InMountain,
                    "underground" => PortalPlacement::Underground,
                    "in_nether" => PortalPlacement::InNether,
                    _ => PortalPlacement::OnLandSurface,
                },
                air_pocket: f32_of(&s["air_pocket_probability"]),
                weight: f32_of(&s["weight"]),
            }).collect(),
        ),
        _ => Kind::Unsupported, // nether/end types: out of scope for the overworld build
    })
}

fn element(v: &Value) -> Element {
    match v["element_type"].as_str().unwrap_or_default() {
        "minecraft:single_pool_element" | "minecraft:legacy_single_pool_element" => Element::Single(ns(v["location"].as_str().unwrap_or_default())),
        "minecraft:list_pool_element" => Element::List(v["elements"].as_array().map(|a| a.iter().map(element).collect()).unwrap_or_default()),
        "minecraft:feature_pool_element" => Element::Feature,
        _ => Element::Empty,
    }
}

/// Minimal NBT reader: just enough for template size, palette and jigsaw block entities.
#[derive(Debug)]
enum Nbt {
    Num(i64),
    Str(String),
    List(Vec<Nbt>),
    Compound(Vec<(String, Nbt)>),
    Other,
}

impl Nbt {
    fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(v) => v.iter().find(|(k, _)| k == key).map(|(_, x)| x),
            _ => None,
        }
    }
    fn list(&self) -> &[Nbt] {
        match self { Nbt::List(v) => v, _ => &[] }
    }
    fn int(&self) -> i32 {
        match self { Nbt::Num(n) => *n as i32, _ => 0 }
    }
    fn str(&self) -> &str {
        match self { Nbt::Str(s) => s, _ => "" }
    }
}

fn read_nbt(b: &[u8]) -> Option<Nbt> {
    fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        if b.len() < n { return None; }
        let (h, t) = b.split_at(n);
        *b = t;
        Some(h)
    }
    fn be(b: &mut &[u8], n: usize) -> Option<i64> {
        let mut v: i64 = 0;
        for &x in take(b, n)? { v = (v << 8) | x as i64; }
        // sign-extend
        let shift = 64 - 8 * n as u32;
        Some((v << shift) >> shift)
    }
    fn string(b: &mut &[u8]) -> Option<String> {
        let n = be(b, 2)? as u16 as usize;
        Some(String::from_utf8_lossy(take(b, n)?).into_owned())
    }
    fn payload(b: &mut &[u8], t: u8) -> Option<Nbt> {
        Some(match t {
            1 => Nbt::Num(be(b, 1)?),
            2 => Nbt::Num(be(b, 2)?),
            3 => Nbt::Num(be(b, 4)?),
            4 => Nbt::Num(be(b, 8)?),
            5 => { take(b, 4)?; Nbt::Other }
            6 => { take(b, 8)?; Nbt::Other }
            7 => { let n = be(b, 4)? as usize; take(b, n)?; Nbt::Other }
            8 => Nbt::Str(string(b)?),
            9 => {
                let et = take(b, 1)?[0];
                let n = be(b, 4)?.max(0) as usize;
                Nbt::List((0..n).map(|_| payload(b, et)).collect::<Option<_>>()?)
            }
            10 => {
                let mut v = vec![];
                loop {
                    let t = take(b, 1)?[0];
                    if t == 0 { break; }
                    let k = string(b)?;
                    v.push((k, payload(b, t)?));
                }
                Nbt::Compound(v)
            }
            11 => { let n = be(b, 4)? as usize; Nbt::List((0..n).map(|_| be(b, 4).map(Nbt::Num)).collect::<Option<_>>()?) }
            12 => { let n = be(b, 4)? as usize; take(b, 8 * n)?; Nbt::Other }
            _ => return None,
        })
    }
    let mut s = b;
    let t = take(&mut s, 1)?[0];
    string(&mut s)?;
    payload(&mut s, t)
}

fn load_template(dir: &Path, location: &str) -> Result<Template, String> {
    let path = dir.join("minecraft/structure").join(format!("{}.nbt", location.trim_start_matches("minecraft:")));
    let gz = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut raw = vec![];
    GzDecoder::new(&gz[..]).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let root = read_nbt(&raw).ok_or_else(|| format!("bad nbt {}", path.display()))?;
    let size = root.get("size").map(|s| s.list()).unwrap_or(&[]);
    let size = [size[0].int(), size[1].int(), size[2].int()];
    let palette = match root.get("palette") {
        Some(p) => p.list(),
        None => root.get("palettes").and_then(|p| p.list().first()).map(|p| p.list()).unwrap_or(&[]),
    };
    let mut jigsaws = vec![];
    let mut markers = vec![];
    for blk in root.get("blocks").map(|b| b.list()).unwrap_or(&[]) {
        let state = blk.get("state").map_or(0, Nbt::int) as usize;
        let name = palette.get(state).and_then(|p| p.get("id").or(p.get("Name"))).map_or("", Nbt::str);
        if let (true, Some(nbt)) = (name == "minecraft:structure_block", blk.get("nbt")) {
            if nbt.get("mode").map_or("", Nbt::str) == "DATA" {
                let p = blk.get("pos").map(|p| p.list()).unwrap_or(&[]);
                markers.push(([p[0].int(), p[1].int(), p[2].int()], nbt.get("metadata").map_or("", Nbt::str).to_string()));
            }
        }
        // 26.x palettes use "id"; older templates use "Name"
        let is_jigsaw = palette.get(state).and_then(|p| p.get("id").or(p.get("Name"))).is_some_and(|n| n.str() == "minecraft:jigsaw");
        if let (true, Some(nbt)) = (is_jigsaw, blk.get("nbt")) {
            let p = blk.get("pos").map(|p| p.list()).unwrap_or(&[]);
            jigsaws.push((
                [p[0].int(), p[1].int(), p[2].int()],
                nbt.get("name").map_or("minecraft:empty".into(), |n| ns(n.str())),
                nbt.get("selection_priority").map_or(0, Nbt::int),
            ));
        }
    }
    jigsaws.sort_by_key(|(p, ..)| (p[1], p[0], p[2])); // StructureTemplate.buildInfoList order
    markers.sort_by_key(|(p, _)| (p[1], p[0], p[2]));
    Ok(Template { size, jigsaws, markers })
}

impl Structures {
    pub fn load(dir: &Path, biomes: &Biomes) -> Result<Structures, String> {
        let wg = dir.join("minecraft/worldgen");
        let mut structures = vec![];
        let mut by_name = HashMap::new();
        for (id, v) in json_dir(&wg.join("structure"))? {
            by_name.insert(id.clone(), structures.len());
            structures.push(Structure { kind: kind(&v)?, biomes: biomes.set(&v["biomes"]), name: id });
        }
        let raw = json_dir(&wg.join("structure_set"))?;
        let set_by_name: HashMap<String, usize> = raw.iter().enumerate().map(|(i, (id, _))| (id.clone(), i)).collect();
        let mut sets = vec![];
        for (id, v) in &raw {
            let p = &v["placement"];
            let int = |k: &str| p.get(k).and_then(Value::as_i64).unwrap_or(0) as i32;
            let placement = match p["type"].as_str().unwrap_or_default() {
                "minecraft:concentric_rings" => Placement::Rings {
                    distance: int("distance"), spread: int("spread"), count: int("count"), preferred: biomes.set(&p["preferred_biomes"]),
                },
                _ => Placement::Spread {
                    spacing: int("spacing"), separation: int("separation"),
                    triangular: p.get("spread_type").and_then(Value::as_str) == Some("triangular"),
                },
            };
            let off = p.get("locate_offset").and_then(Value::as_array);
            let off_at = |i: usize| off.and_then(|a| a.get(i)).and_then(Value::as_i64).unwrap_or(0) as i32;
            sets.push(Set {
                name: id.clone(),
                placement,
                salt: int("salt"),
                frequency: p.get("frequency").map_or(1.0, f32_of),
                reduction: match p.get("frequency_reduction_method").and_then(Value::as_str) {
                    Some("legacy_type_1") => Reduction::Legacy1,
                    Some("legacy_type_2") => Reduction::Legacy2,
                    Some("legacy_type_3") => Reduction::Legacy3,
                    _ => Reduction::Default,
                },
                exclusion: p.get("exclusion_zone").map(|e| (set_by_name[&ns(e["other_set"].as_str().unwrap())], e["chunk_count"].as_i64().unwrap() as i32)),
                locate_offset: (off_at(0), off_at(1), off_at(2)),
                entries: v["structures"].as_array().unwrap().iter()
                    .map(|e| (by_name[&ns(e["structure"].as_str().unwrap())], e["weight"].as_i64().unwrap() as i32))
                    .collect(),
                possible: true,
            });
        }
        let mut pools = HashMap::new();
        for (id, v) in json_dir(&wg.join("template_pool"))? {
            let mut elements = vec![];
            let mut expanded = vec![];
            for e in v["elements"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                expanded.extend(std::iter::repeat_n(elements.len(), e["weight"].as_i64().unwrap_or(1) as usize));
                elements.push(element(&e["element"]));
            }
            pools.insert(id, Pool { elements, expanded });
        }
        // Templates are only needed for start pieces and ruined portals; load those up front.
        let mut wanted: Vec<String> = PORTALS.iter().chain(GIANT_PORTALS.iter()).map(|p| format!("minecraft:{p}")).collect();
        wanted.extend(SHIPWRECK_OCEAN.iter().chain(SHIPWRECK_BEACHED.iter()).map(|t| format!("minecraft:shipwreck/{t}")));
        fn collect(e: &Element, out: &mut Vec<String>) {
            match e {
                Element::Single(l) => out.push(l.clone()),
                Element::List(v) => v.iter().for_each(|x| collect(x, out)),
                _ => {}
            }
        }
        for s in &structures {
            if let Kind::Jigsaw { pool, .. } = &s.kind {
                if let Some(p) = pools.get(pool) {
                    p.elements.iter().for_each(|e| collect(e, &mut wanted));
                }
            }
        }
        let mut templates = HashMap::new();
        for loc in wanted {
            if !templates.contains_key(&loc) {
                templates.insert(loc.clone(), load_template(dir, &loc)?);
            }
        }
        Ok(Structures {
            sets, structures, by_name, set_by_name, pools, templates,
            monument_surrounding: biomes.tag("minecraft:required_ocean_monument_surrounding"),
        })
    }

    /// Drops sets that cannot generate with the given biome source (ChunkGeneratorStructureState).
    pub fn mark_possible(&mut self, possible_biomes: BiomeSet) {
        for set in &mut self.sets {
            set.possible = set.entries.iter().any(|(s, _)| self.structures[*s].biomes & possible_biomes != 0);
        }
    }

    pub fn structure_id(&self, name: &str) -> Option<usize> {
        self.by_name.get(&ns(name)).copied()
    }

    /// Resolves a user-facing name to (set, only this structure). A bare name prefers its plural
    /// set ("ruined_portal" = any ruined portal); "minecraft:ruined_portal" means that variant only.
    pub fn resolve(&self, name: &str) -> Option<(usize, Option<usize>)> {
        let n = ns(name);
        let set_of = |st: usize| self.sets.iter().position(|s| s.entries.iter().any(|e| e.0 == st));
        if !name.contains(':') {
            for plural in [format!("{n}s"), format!("{n}es")] {
                if let Some(&set) = self.set_by_name.get(&plural) {
                    return Some((set, None));
                }
            }
        }
        if let Some(&st) = self.by_name.get(&n) {
            return Some((set_of(st)?, Some(st)));
        }
        self.set_by_name.get(&n).map(|&set| (set, None))
    }

    // --------------------------------------------------------------------------------------------
    // Placement
    // --------------------------------------------------------------------------------------------

    /// RandomSpreadStructurePlacement.getPotentialStructureChunk for spacing-grid cell (gx, gz).
    /// Depends only on the low 48 bits of the seed.
    pub fn grid_chunk(&self, set: usize, seed: i64, gx: i32, gz: i32) -> Option<(i32, i32)> {
        let s = &self.sets[set];
        let Placement::Spread { spacing, separation, triangular } = s.placement else { return None };
        let mut r = JavaRandom::new(0);
        r.set_large_feature_with_salt(seed, gx, gz, s.salt);
        let limit = spacing - separation;
        let mut spread = || if triangular { (r.next_bound(limit) + r.next_bound(limit)) / 2 } else { r.next_bound(limit) };
        let dx = spread();
        let dz = spread();
        Some((gx * spacing + dx, gz * spacing + dz))
    }

    fn frequency_ok(&self, s: &Set, seed: i64, cx: i32, cz: i32) -> bool {
        if s.frequency >= 1.0 {
            return true;
        }
        let mut r = JavaRandom::new(0);
        match s.reduction {
            // vanilla passes (salt, x, z) into the (x, z, salt) slots here
            Reduction::Default => { r.set_large_feature_with_salt(seed, s.salt, cx, cz); r.next_float() < s.frequency }
            Reduction::Legacy1 => {
                let (a, b) = (cx >> 4, cz >> 4);
                r.set_seed((a ^ (b << 4)) as i64 ^ seed);
                r.next_int();
                r.next_bound((1.0f32 / s.frequency) as i32) == 0
            }
            Reduction::Legacy2 => { r.set_large_feature_with_salt(seed, cx, cz, 10387320); r.next_float() < s.frequency }
            Reduction::Legacy3 => { r.set_large_feature_seed(seed, cx, cz); r.next_double() < s.frequency as f64 }
        }
    }

    /// StructurePlacement.isStructureChunk for random-spread sets (rings: see `stronghold_chunks`).
    pub fn is_structure_chunk(&self, set: usize, seed: i64, cx: i32, cz: i32) -> bool {
        let s = &self.sets[set];
        let Placement::Spread { spacing, .. } = s.placement else { return false };
        if self.grid_chunk(set, seed, cx.div_euclid(spacing), cz.div_euclid(spacing)) != Some((cx, cz)) {
            return false;
        }
        if !self.frequency_ok(s, seed, cx, cz) {
            return false;
        }
        match s.exclusion {
            None => true,
            Some((other, r)) => !(cx - r..=cx + r).any(|x| (cz - r..=cz + r).any(|z| self.is_structure_chunk(other, seed, x, z))),
        }
    }

    /// ChunkGeneratorStructureState.generateRingPositions, first `n` positions only.
    pub fn stronghold_chunks(&self, s: &Seeded, set: usize, n: usize) -> Vec<(i32, i32)> {
        let Placement::Rings { distance, spread, count, preferred } = self.sets[set].placement else { return vec![] };
        let mut r = JavaRandom::new(s.seed);
        let mut angle = r.next_double() * std::f64::consts::PI * 2.0;
        let (mut pos_in_circle, mut circle, mut spread) = (0, 0, spread);
        let mut out = vec![];
        for i in 0..count.min(n as i32) {
            let dist = (4 * distance + distance * circle * 6) as f64 + (r.next_double() - 0.5) * (distance as f64 * 2.5);
            let ix = java_round(angle.cos() * dist) as i32;
            let iz = java_round(angle.sin() * dist) as i32;
            let mut fork = r.fork();
            let found = find_biome_horizontal(s, (ix << 4) + 8, 0, (iz << 4) + 8, 112, preferred, &mut fork);
            out.push(found.map_or((ix, iz), |(x, z)| (x >> 4, z >> 4)));
            angle += std::f64::consts::PI * 2.0 / spread as f64;
            pos_in_circle += 1;
            if pos_in_circle == spread {
                circle += 1;
                pos_in_circle = 0;
                spread += 2 * spread / (circle + 1);
                spread = spread.min(count - i);
                angle += r.next_double() * std::f64::consts::PI * 2.0;
            }
        }
        out
    }

    // --------------------------------------------------------------------------------------------
    // Generation
    // --------------------------------------------------------------------------------------------

    /// ChunkGenerator.createStructures for one set at a placement chunk: weighted pick order,
    /// first structure whose generation point is valid wins.
    pub fn generate_set(&self, s: &mut Seeded, bc: &mut BiomeCtx, set: usize, cx: i32, cz: i32) -> Option<Hit> {
        let entries = &self.sets[set].entries;
        if entries.len() == 1 {
            return self.generate(s, bc, entries[0].0, cx, cz);
        }
        let mut options = entries.clone();
        let mut r = JavaRandom::new(0);
        r.set_large_feature_seed(s.seed, cx, cz);
        let mut total: i32 = options.iter().map(|o| o.1).sum();
        while !options.is_empty() {
            let mut choice = r.next_bound(total);
            let mut index = 0;
            for o in &options {
                choice -= o.1;
                if choice < 0 {
                    break;
                }
                index += 1;
            }
            let (st, w) = options[index];
            if let Some(hit) = self.generate(s, bc, st, cx, cz) {
                return Some(hit);
            }
            options.remove(index);
            total -= w;
        }
        None
    }

    /// Structure.findValidGenerationPoint, then the cheap start of the piece generator (first piece
    /// box, details), which continues on the same context random.
    pub fn generate(&self, s: &mut Seeded, bc: &mut BiomeCtx, st: usize, cx: i32, cz: i32) -> Option<Hit> {
        let mut r = JavaRandom::new(0);
        r.set_large_feature_seed(s.seed, cx, cz);
        let (p, mut bbox, mut tags) = self.generation_point(s, bc, &mut r, st, cx, cz)?;
        if !contains(self.structures[st].biomes, s.biome(bc, p.0 >> 2, p.1 >> 2, p.2 >> 2)) {
            return None;
        }
        self.piece_details(&mut r, st, cx, cz, &mut bbox, &mut tags);
        Some(Hit { structure: st, chunk: (cx, cz), pos: p, bbox, tags })
    }

    #[allow(clippy::type_complexity)]
    fn generation_point(&self, s: &mut Seeded, bc: &mut BiomeCtx, r: &mut JavaRandom, st: usize, cx: i32, cz: i32) -> Option<((i32, i32, i32), Option<[i32; 6]>, Vec<String>)> {
        let structure = &self.structures[st];
        let (min_y, max_y, sea) = (s.w.min_y, s.w.min_y + s.w.height - 1, s.w.sea_level);
        let (mx, mz) = (cx * 16 + 8, cz * 16 + 8);
        let on_top = |s: &mut Seeded, bc: &mut BiomeCtx, hm| {
            column_ok(s, bc, structure.biomes, mx, mz, min_y - 1, max_y).then(|| (mx, s.base_height(mx, mz, hm) - 1, mz))
        };
        match &structure.kind {
            Kind::OnTop(hm, _) => on_top(s, bc, *hm).map(|p| (p, None, vec![])),
            Kind::SinglePiece(w, _, d) => {
                if !column_ok(s, bc, structure.biomes, mx, mz, min_y - 1, max_y) || lowest_y(s, cx * 16, cz * 16, *w, *d) < sea {
                    return None;
                }
                Some(((mx, s.base_height(mx, mz, Heightmap::WorldSurfaceWg) - 1, mz), None, vec![]))
            }
            Kind::Monument => {
                let (x, z) = (cx * 16 + 9, cz * 16 + 9);
                // getBiomesWithin(x, seaLevel, z, 29): z, x, y order; every quart must allow a monument
                for qz in (z - 29) >> 2..=(z + 29) >> 2 {
                    for qx in (x - 29) >> 2..=(x + 29) >> 2 {
                        for qy in (sea - 29) >> 2..=(sea + 29) >> 2 {
                            if !contains(self.monument_surrounding, s.biome(bc, qx, qy, qz)) {
                                return None;
                            }
                        }
                    }
                }
                on_top(s, bc, Heightmap::OceanFloorWg).map(|p| (p, None, vec![]))
            }
            Kind::Mansion => {
                let (x, z) = (cx * 16 + 7, cz * 16 + 7);
                if !column_ok(s, bc, structure.biomes, x, z, min_y - 1, max_y) {
                    return None;
                }
                let (ox, oz) = match r.next_bound(4) { 0 => (5, 5), 1 => (-5, 5), 2 => (-5, -5), _ => (5, -5) };
                let y = lowest_y(s, x, z, ox, oz);
                (y >= 60).then_some(((x, y, z), None, vec![]))
            }
            Kind::Stronghold => Some(((cx * 16, 0, cz * 16), None, vec![])),
            // ponytail: depth comes from the mineshaft piece layout (MineshaftPieces, M3); y=50 start for now
            Kind::Mineshaft => Some(((mx, 50, cz * 16), None, vec![])),
            Kind::RuinedPortal(setups) => self.portal_point(s, r, setups, cx, cz),
            Kind::Jigsaw { pool, height, project, start_jigsaw, padding } => {
                let y = match height {
                    HeightProv::Const(a) => resolve(a, s),
                    HeightProv::Uniform(lo, hi) => {
                        let (lo, hi) = (resolve(lo, s), resolve(hi, s));
                        if lo > hi { lo } else { r.between_inclusive(lo, hi) }
                    }
                };
                let pos = [cx * 16, y, cz * 16];
                let rot = r.next_bound(4);
                // ponytail: pool aliases are ignored; no vanilla start pool is an alias target
                let pool = self.pools.get(pool)?;
                if pool.expanded.is_empty() {
                    return None;
                }
                let el = &pool.elements[pool.expanded[r.next_bound(pool.expanded.len() as i32) as usize]];
                if matches!(el, Element::Empty) {
                    return None;
                }
                let anchored = match start_jigsaw {
                    Some(name) => self.named_jigsaw(el, name, pos, rot, r)?,
                    None => pos,
                };
                let local = [anchored[0] - pos[0], anchored[1] - pos[1], anchored[2] - pos[2]];
                let adjusted = [pos[0] - local[0], pos[1] - local[1], pos[2] - local[2]];
                let mut b = self.element_box(el, adjusted, rot)?;
                let (cxm, czm) = ((b[3] + b[0]) / 2, (b[5] + b[2]) / 2);
                let bottom = match project {
                    Some(hm) => {
                        if !column_ok(s, bc, structure.biomes, cxm, czm, min_y, max_y) {
                            return None;
                        }
                        pos[1] + s.base_height(cxm, czm, *hm)
                    }
                    None => adjusted[1],
                };
                let dy = bottom - (b[1] + 1);
                b[1] += dy;
                b[4] += dy;
                if let Some((lo, hi)) = padding {
                    if b[1] < min_y + lo || b[4] > max_y - hi {
                        return None;
                    }
                }
                Some(((cxm, bottom + local[1], czm), Some(b), vec![]))
            }
            Kind::Unsupported => None,
        }
    }

    /// The first random calls of each simple piece generator (generatePieces), for boxes and details.
    fn piece_details(&self, r: &mut JavaRandom, st: usize, cx: i32, cz: i32, bbox: &mut Option<[i32; 6]>, tags: &mut Vec<String>) {
        let (x0, z0) = (cx * 16, cz * 16);
        match &self.structures[st].kind {
            Kind::SinglePiece(w, h, d) => *bbox = Some(scattered_box(x0, z0, r.next_bound(4), *w, *h, *d)),
            Kind::OnTop(_, Piece::SwampHut) => *bbox = Some(scattered_box(x0, z0, r.next_bound(4), 7, 7, 9)),
            Kind::Monument => *bbox = Some([x0 - 29, 39, z0 - 29, x0 + 28, 61, z0 + 28]),
            Kind::OnTop(_, Piece::Igloo) => {
                r.next_bound(4); // rotation
                if r.next_double() < 0.5 {
                    tags.push("basement".into());
                    tags.push(format!("ladders:{}", r.next_bound(8) + 4));
                }
            }
            Kind::OnTop(_, Piece::Shipwreck { beached }) => {
                r.next_bound(4); // rotation
                let list: &[&str] = if *beached { &SHIPWRECK_BEACHED } else { &SHIPWRECK_OCEAN };
                let t = list[r.next_bound(list.len() as i32) as usize];
                tags.push(format!("template:shipwreck/{t}"));
                for (_, m) in self.templates.get(&format!("minecraft:shipwreck/{t}")).map_or(&[][..], |t| &t.markers) {
                    tags.push(format!("chest:{m}"));
                }
            }
            Kind::OnTop(_, Piece::OceanRuin { large, cluster }) => {
                let rot = r.next_bound(4);
                let is_large = r.next_float() <= *large;
                r.next_bound(if is_large { 4 } else { 8 }); // template
                if is_large {
                    tags.push("large".into());
                    if r.next_float() <= *cluster {
                        tags.push("cluster".into());
                        tags.push(format!("ruins:{}", 1 + cluster_ruins(r, rot, x0, z0)));
                    }
                }
            }
            _ => {}
        }
    }

    fn named_jigsaw(&self, el: &Element, name: &str, pos: [i32; 3], rot: i32, r: &mut JavaRandom) -> Option<[i32; 3]> {
        let Element::Single(loc) = el else { return None };
        let t = self.templates.get(loc)?;
        let mut list: Vec<([i32; 3], &str, i32)> = t.jigsaws.iter().map(|(p, n, prio)| {
            let q = transform(*p, false, rot, (0, 0));
            ([q[0] + pos[0], q[1] + pos[1], q[2] + pos[2]], n.as_str(), *prio)
        }).collect();
        shuffle(&mut list, r);
        list.sort_by(|a, b| b.2.cmp(&a.2)); // stable: highest selection priority first
        list.iter().find(|j| j.1 == name).map(|j| j.0)
    }

    fn element_box(&self, el: &Element, pos: [i32; 3], rot: i32) -> Option<[i32; 6]> {
        match el {
            Element::Single(loc) => self.templates.get(loc).map(|t| template_box(pos, rot, (0, 0), false, t.size)),
            Element::List(v) => v.iter().filter(|e| !matches!(e, Element::Empty)).filter_map(|e| self.element_box(e, pos, rot)).reduce(|a, b| {
                [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2]), a[3].max(b[3]), a[4].max(b[4]), a[5].max(b[5])]
            }),
            Element::Feature => Some([pos[0], pos[1], pos[2], pos[0], pos[1], pos[2]]),
            Element::Empty => None,
        }
    }

    #[allow(clippy::type_complexity)]
    fn portal_point(&self, s: &mut Seeded, r: &mut JavaRandom, setups: &[PortalSetup], cx: i32, cz: i32) -> Option<((i32, i32, i32), Option<[i32; 6]>, Vec<String>)> {
        let setup = if setups.len() > 1 {
            let total: f32 = setups.iter().fold(0.0, |t, s| t + s.weight);
            let mut pick = r.next_float();
            setups.iter().find(|s| { pick -= s.weight / total; pick < 0.0 })?
        } else {
            &setups[0]
        };
        let air = match setup.air_pocket { 0.0 => false, 1.0 => true, p => r.next_float() < p };
        let giant = r.next_float() < 0.05;
        let name = if giant { GIANT_PORTALS[r.next_bound(3) as usize] } else { PORTALS[r.next_bound(10) as usize] };
        let size = self.templates.get(&format!("minecraft:{name}"))?.size;
        let rot = r.next_bound(4);
        let mirror = r.next_float() >= 0.5;
        let b = template_box([cx * 16, 0, cz * 16], rot, (size[0] / 2, size[2] / 2), mirror, size);
        let center = (b[0] + (b[3] - b[0] + 1) / 2, b[2] + (b[5] - b[2] + 1) / 2);
        let hm = if setup.placement == PortalPlacement::OnOceanFloor { Heightmap::OceanFloorWg } else { Heightmap::WorldSurfaceWg };
        let surface = s.base_height(center.0, center.1, hm) - 1;
        let y_span = b[4] - b[1] + 1;
        let min_y = s.w.min_y + 15;
        let new_y = match setup.placement {
            PortalPlacement::InNether => {
                if air { r.between_inclusive(32, 100) } else if r.next_float() < 0.5 { r.between_inclusive(27, 29) } else { r.between_inclusive(29, 100) }
            }
            PortalPlacement::InMountain => within(r, 70, surface - y_span),
            PortalPlacement::Underground => within(r, min_y, surface - y_span),
            PortalPlacement::PartlyBuried => surface - y_span + r.between_inclusive(2, 8),
            _ => surface,
        };
        let columns: Vec<Vec<Block>> = [(b[0], b[2]), (b[3], b[2]), (b[0], b[5]), (b[3], b[5])].iter().map(|&(x, z)| s.column(x, z)).collect();
        let at = |col: &Vec<Block>, y: i32| col.get((y - s.w.min_y) as usize).copied().filter(|_| y >= s.w.min_y).unwrap_or(Block::Air);
        let mut tags = vec![format!("template:{name}"), format!("placement:{}", setup.placement.name())];
        tags.extend([(giant, "giant"), (mirror, "mirrored"), (air, "air_pocket")].iter().filter(|t| t.0).map(|t| t.1.to_string()));
        let done = |y: i32| Some(((cx * 16, y, cz * 16), Some([b[0], b[1] + y, b[2], b[3], b[4] + y, b[5]]), tags.clone()));
        let mut y = new_y;
        while y > min_y {
            let mut solid = 0;
            for c in &columns {
                if hm.test(at(c, y)) {
                    solid += 1;
                    if solid == 3 {
                        return done(y);
                    }
                }
            }
            y -= 1;
        }
        done(y)
    }
}

// ------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------

const SHIPWRECK_BEACHED: [&str; 11] = [
    "with_mast", "sideways_full", "sideways_fronthalf", "sideways_backhalf", "rightsideup_full", "rightsideup_fronthalf",
    "rightsideup_backhalf", "with_mast_degraded", "rightsideup_full_degraded", "rightsideup_fronthalf_degraded", "rightsideup_backhalf_degraded",
];
const SHIPWRECK_OCEAN: [&str; 20] = [
    "with_mast", "upsidedown_full", "upsidedown_fronthalf", "upsidedown_backhalf", "sideways_full", "sideways_fronthalf",
    "sideways_backhalf", "rightsideup_full", "rightsideup_fronthalf", "rightsideup_backhalf", "with_mast_degraded",
    "upsidedown_full_degraded", "upsidedown_fronthalf_degraded", "upsidedown_backhalf_degraded", "sideways_full_degraded",
    "sideways_fronthalf_degraded", "sideways_backhalf_degraded", "rightsideup_full_degraded", "rightsideup_fronthalf_degraded",
    "rightsideup_backhalf_degraded",
];

/// ScatteredFeaturePiece box at y=64 (before terrain fitting); direction index NORTH, EAST, SOUTH, WEST.
fn scattered_box(x: i32, z: i32, dir: i32, w: i32, h: i32, d: i32) -> [i32; 6] {
    if dir % 2 == 0 { [x, 64, z, x + w - 1, 64 + h - 1, z + d - 1] } else { [x, 64, z, x + d - 1, 64 + h - 1, z + w - 1] }
}

/// OceanRuinPieces.addClusterRuins: how many extra ruins the cluster adds.
fn cluster_ruins(r: &mut JavaRandom, rot: i32, x: i32, z: i32) -> i32 {
    let ni = |r: &mut JavaRandom, a: i32, b: i32| if a >= b { a } else { r.next_bound(b - a + 1) + a };
    let corner = transform([15, 0, 15], false, rot, (0, 0));
    let parent = [x.min(x + corner[0]), z.min(z + corner[2]), x.max(x + corner[0]), z.max(z + corner[2])];
    let (bx, bz) = (parent[0], parent[1]);
    let offs: [(i32, i32, i32, i32, i32); 8] = [(-16, 1, 8, 16, 1), (-16, 1, 8, 0, 1), (-16, 1, 8, -16, 4), (0, 1, 7, 16, 1), (0, 1, 7, -16, 4), (16, 1, 7, 16, 3), (16, 1, 7, 0, 1), (16, 1, 7, -16, 4)];
    let zmax = [7, 7, 8, 7, 6, 8, 7, 8];
    let mut positions: Vec<(i32, i32)> = offs.iter().zip(zmax).map(|(&(dx, a, b, dz, c), zm)| {
        let px = bx + dx + ni(r, a, b);
        (px, bz + dz + ni(r, c, zm))
    }).collect();
    let count = ni(r, 4, 8);
    let mut added = 0;
    for _ in 0..count {
        if positions.is_empty() {
            continue;
        }
        let (px, pz) = positions.remove(r.next_bound(positions.len() as i32) as usize);
        let c = transform([5, 0, 6], false, r.next_bound(4), (0, 0));
        let (ax, az, cx2, cz2) = (px.min(px + c[0]), pz.min(pz + c[2]), px.max(px + c[0]), pz.max(pz + c[2]));
        if !(cx2 >= parent[0] && ax <= parent[2] && cz2 >= parent[1] && az <= parent[3]) {
            r.next_bound(8); // small ruin template
            added += 1;
        }
    }
    added
}

fn resolve(a: &Anchor, s: &Seeded) -> i32 {
    match *a {
        Anchor::Abs(y) => y,
        Anchor::AboveBottom(o) => s.w.min_y + o,
        Anchor::BelowTop(o) => s.w.height - 1 + s.w.min_y - o,
        Anchor::Sea(o) => s.w.sea_level + o,
    }
}

fn within(r: &mut JavaRandom, min: i32, max: i32) -> i32 {
    if min < max { r.between_inclusive(min, max) } else { max }
}

/// Math.round(double): nearest, ties toward positive infinity.
fn java_round(x: f64) -> i64 {
    let f = x.floor();
    (if x - f >= 0.5 { f + 1.0 } else { f }) as i64
}

/// StructureTemplate.transform (mirror: FRONT_BACK when true)
fn transform(p: [i32; 3], mirror: bool, rot: i32, pivot: (i32, i32)) -> [i32; 3] {
    let (x, y, z) = (if mirror { -p[0] } else { p[0] }, p[1], p[2]);
    let (px, pz) = pivot;
    match rot {
        3 => [px - pz + z, y, px + pz - x],
        1 => [px + pz - z, y, pz - px + x],
        2 => [px + px - x, y, pz + pz - z],
        _ => [x, y, z],
    }
}

/// StructureTemplate.getBoundingBox -> [minX, minY, minZ, maxX, maxY, maxZ]
fn template_box(pos: [i32; 3], rot: i32, pivot: (i32, i32), mirror: bool, size: [i32; 3]) -> [i32; 6] {
    let a = transform([0, 0, 0], mirror, rot, pivot);
    let b = transform([size[0] - 1, size[1] - 1, size[2] - 1], mirror, rot, pivot);
    [
        a[0].min(b[0]) + pos[0], a[1].min(b[1]) + pos[1], a[2].min(b[2]) + pos[2],
        a[0].max(b[0]) + pos[0], a[1].max(b[1]) + pos[1], a[2].max(b[2]) + pos[2],
    ]
}

/// GenerationContext.couldStructureExistInColumn (volume-path biome column)
fn column_ok(s: &Seeded, bc: &mut BiomeCtx, biomes: BiomeSet, x: i32, z: i32, min_y: i32, max_y: i32) -> bool {
    let (qy0, qy1) = (min_y >> 2, max_y >> 2);
    s.biomes_volume(bc, x >> 2, qy0, z >> 2, 1, qy1 - qy0 + 1, 1).into_iter().any(|b| contains(biomes, b))
}

/// Structure.getLowestY over the four corners (getFirstOccupiedHeight, WORLD_SURFACE_WG)
fn lowest_y(s: &mut Seeded, x: i32, z: i32, sx: i32, sz: i32) -> i32 {
    [(x, z), (x, z + sz), (x + sx, z), (x + sx, z + sz)]
        .iter()
        .map(|&(a, b)| s.base_height(a, b, Heightmap::WorldSurfaceWg) - 1)
        .min()
        .unwrap()
}

/// BiomeSource.findBiomeHorizontal (skipSteps 1, not closest-first), with a fresh caching resolver.
fn find_biome_horizontal(s: &Seeded, x: i32, y: i32, z: i32, radius: i32, allowed: BiomeSet, r: &mut JavaRandom) -> Option<(i32, i32)> {
    let (cx, cz, rad, qy) = (x >> 2, z >> 2, radius >> 2, y >> 2);
    let mut bc = BiomeCtx::cached();
    let mut result = None;
    let mut found = 0;
    for dz in -rad..=rad {
        for dx in -rad..=rad {
            if contains(allowed, s.biome(&mut bc, cx + dx, qy, cz + dz)) {
                if result.is_none() || r.next_bound(found + 1) == 0 {
                    result = Some(((cx + dx) << 2, (cz + dz) << 2));
                }
                found += 1;
            }
        }
    }
    result
}
