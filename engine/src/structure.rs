//! Structure sets, placement and generation checks (levelgen/structure/*) for overworld types.
//! A structure "exists" in a chunk when its set places it there, the set's weighted pick reaches it,
//! and its findValidGenerationPoint succeeds (start point + biome at that point).

use crate::biome::{contains, BiomeSet, Biomes};
use crate::density::{f32_of, ns};
use crate::rng::{pos_seed, shuffle, JavaRandom, Rng, WorldgenXoro};
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
    OceanRuin { large: f32, cluster: f32, warm: bool },
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
    /// (index, step) for ChunkGenerator.applyBiomeDecoration's feature seed: index among all
    /// registry structures (sorted ids) of the same generation step.
    decoration: (i32, i32),
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
    containers: Vec<([i32; 3], String)>, // blocks with NBT that are loot containers (+ LootTable), (y, x, z) order
    obsidian: Vec<[i32; 3]>,
    frames: Vec<(usize, Vec<usize>)>, // ruined portals: lightable frames reusing its obsidian (portal_frames), as indices into `obsidian`
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
    /// Loot chests with their LootTableSeed (buried treasure, shipwrecks, ruined portals, igloo
    /// basements, desert pyramids, ocean ruins).
    pub chests: Vec<Chest>,
}

/// A loot chest a structure places. The seed is exact unless `exact` is false; y is approximate
/// except for ruined portals (terrain fitting is not simulated).
#[derive(Clone, Debug, PartialEq)]
pub struct Chest {
    pub pos: (i32, i32, i32),
    pub table: String,
    pub seed: i64,
    pub exact: bool,
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
/// GenerationStep.Decoration, in ordinal order.
const STEPS: [&str; 11] = [
    "raw_generation", "lakes", "local_modifications", "underground_structures", "surface_structures", "strongholds",
    "underground_ores", "underground_decoration", "fluid_springs", "vegetal_decoration", "top_layer_modification",
];
/// Block entities that are RandomizableContainers (they take a LootTableSeed when a template places them).
const CONTAINERS: [&str; 8] = [
    "minecraft:chest", "minecraft:trapped_chest", "minecraft:barrel", "minecraft:dispenser", "minecraft:dropper",
    "minecraft:hopper", "minecraft:decorated_pot", "minecraft:crafter",
];

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
            warm: v["biome_temp"].as_str() != Some("cold"),
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
    let mut containers = vec![];
    let mut obsidian = vec![];
    for blk in root.get("blocks").map(|b| b.list()).unwrap_or(&[]) {
        let state = blk.get("state").map_or(0, Nbt::int) as usize;
        let name = palette.get(state).and_then(|p| p.get("id").or(p.get("Name"))).map_or("", Nbt::str);
        if name == "minecraft:obsidian" {
            let p = blk.get("pos").map(|p| p.list()).unwrap_or(&[]);
            obsidian.push([p[0].int(), p[1].int(), p[2].int()]);
        }
        if let (true, Some(nbt)) = (CONTAINERS.contains(&name) || name.ends_with("shulker_box"), blk.get("nbt")) {
            let p = blk.get("pos").map(|p| p.list()).unwrap_or(&[]);
            containers.push(([p[0].int(), p[1].int(), p[2].int()], nbt.get("LootTable").map_or("", Nbt::str).to_string()));
        }
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
    containers.sort_by_key(|(p, _)| (p[1], p[0], p[2]));
    let frames = if location.contains("ruined_portal/") { portal_frames(&obsidian) } else { vec![] };
    let index: HashMap<[i32; 3], usize> = obsidian.iter().enumerate().map(|(i, p)| (*p, i)).collect();
    let frames = frames.into_iter().map(|(len, on)| (len, on.iter().map(|p| index[p]).collect())).collect();
    Ok(Template { size, jigsaws, markers, containers, obsidian, frames })
}

impl Structures {
    pub fn load(dir: &Path, biomes: &Biomes) -> Result<Structures, String> {
        let wg = dir.join("minecraft/worldgen");
        let mut structures = vec![];
        let mut by_name = HashMap::new();
        for (id, v) in json_dir(&wg.join("structure"))? {
            by_name.insert(id.clone(), structures.len());
            let step = STEPS.iter().position(|s| Some(*s) == v["step"].as_str()).unwrap_or(4) as i32;
            structures.push(Structure { kind: kind(&v)?, biomes: biomes.set(&v["biomes"]), name: id, decoration: (0, step) });
        }
        let mut next = [0; STEPS.len()];
        for s in &mut structures {
            s.decoration.0 = next[s.decoration.1 as usize];
            next[s.decoration.1 as usize] += 1;
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
        wanted.push(IGLOO_LAB.into());
        for (prefix, n) in [("warm_", 8), ("brick_", 8), ("cracked_", 8), ("mossy_", 8)] {
            wanted.extend((1..=n).map(|i| format!("minecraft:underwater_ruin/{prefix}{i}")));
        }
        for i in [1, 2, 3, 8] {
            wanted.extend(["big_brick_", "big_cracked_", "big_mossy_"].map(|b| format!("minecraft:underwater_ruin/{b}{i}")));
        }
        wanted.extend((4..=7).map(|i| format!("minecraft:underwater_ruin/big_warm_{i}")));
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

    /// Every structure and structure set id (for "did you mean" hints).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().chain(self.set_by_name.keys()).map(String::as_str)
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
    /// Random-spread parameters for the GPU kernel (gpu.cu): spacing, separation, triangular, salt,
    /// frequency reduction (0 default, 1-3 legacy types, 4 none), frequency bits, locate offset x, z.
    /// Exclusion zones are left to the CPU.
    pub fn gpu_spread(&self, set: usize) -> Option<[i32; 8]> {
        let s = &self.sets[set];
        let Placement::Spread { spacing, separation, triangular } = s.placement else { return None };
        let reduction = match s.reduction {
            _ if s.frequency >= 1.0 => 4,
            Reduction::Default => 0,
            Reduction::Legacy1 => 1,
            Reduction::Legacy2 => 2,
            Reduction::Legacy3 => 3,
        };
        Some([spacing, separation, triangular as i32, s.salt, reduction, s.frequency.to_bits() as i32, s.locate_offset.0, s.locate_offset.2])
    }

    /// How far a start's position and box corners can sit from its locate position (blocks).
    /// Swamp huts and single-piece temples start at chunk + 8 with a box inside chunk + 0..size;
    /// other kinds get the general 128.
    pub fn reach(&self, set: usize) -> i32 {
        let s = &self.sets[set];
        let (ox, oz) = (s.locate_offset.0, s.locate_offset.2);
        let size = |st: usize| match self.structures[st].kind {
            Kind::SinglePiece(w, _, d) => Some(w.max(d) - 1),
            Kind::OnTop(_, Piece::SwampHut) => Some(8),
            _ => None,
        };
        // farthest of offsets 0..=m (box corners and the start at 8) from the locate offset
        let far = |m: i32, o: i32| o.abs().max((m.max(8) - o).abs()) as f64;
        s.entries.iter().map(|&(st, _)| size(st).map_or(128, |m| far(m, ox).hypot(far(m, oz)).ceil() as i32)).max().unwrap_or(128)
    }

    pub fn is_structure_chunk(&self, set: usize, seed: i64, cx: i32, cz: i32) -> bool {
        let Placement::Spread { spacing, .. } = self.sets[set].placement else { return false };
        self.grid_chunk(set, seed, cx.div_euclid(spacing), cz.div_euclid(spacing)) == Some((cx, cz)) && self.chunk_ok(set, seed, cx, cz)
    }

    /// The rest of isStructureChunk for a chunk `grid_chunk` returned: frequency and exclusion zone.
    pub fn chunk_ok(&self, set: usize, seed: i64, cx: i32, cz: i32) -> bool {
        let s = &self.sets[set];
        if !self.frequency_ok(s, seed, cx, cz) {
            return false;
        }
        let Some((other, r)) = s.exclusion else { return true };
        let Placement::Spread { spacing, .. } = self.sets[other].placement else { return true };
        // vanilla tests every chunk of the square; only the other set's grid chunks can pass, so
        // testing the grid cells that touch the square gives the same answer
        !((cx - r).div_euclid(spacing)..=(cx + r).div_euclid(spacing)).any(|gx| {
            ((cz - r).div_euclid(spacing)..=(cz + r).div_euclid(spacing)).any(|gz| {
                let c = self.grid_chunk(other, seed, gx, gz).unwrap();
                (c.0 - cx).abs() <= r && (c.1 - cz).abs() <= r && self.chunk_ok(other, seed, c.0, c.1)
            })
        })
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
        let mut chests = vec![];
        self.piece_details(&mut r, s.seed, st, (cx, cz), p.1, &mut bbox, &mut tags, &mut chests);
        Some(Hit { structure: st, chunk: (cx, cz), pos: p, bbox, tags, chests })
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

    /// The first random calls of each simple piece generator (generatePieces), for boxes, details
    /// and loot chests.
    #[allow(clippy::too_many_arguments)]
    fn piece_details(&self, r: &mut JavaRandom, world_seed: i64, st: usize, (cx, cz): (i32, i32), y: i32, bbox: &mut Option<[i32; 6]>, tags: &mut Vec<String>, chests: &mut Vec<Chest>) {
        let (x0, z0) = (cx * 16, cz * 16);
        match &self.structures[st].kind {
            Kind::SinglePiece(w, h, d) => {
                let dir = r.next_bound(4);
                let b = scattered_box(x0, z0, dir, *w, *h, *d);
                *bbox = Some(b);
                match self.structures[st].name.as_str() {
                    "minecraft:desert_pyramid" => *chests = self.pyramid_chests(world_seed, st, b, dir, y),
                    "minecraft:jungle_pyramid" => *chests = self.jungle_chests(world_seed, st, b, dir, y),
                    _ => {}
                }
            }
            Kind::OnTop(_, Piece::BuriedTreasure) => {
                let mut fr = self.feature_random(world_seed, st, (cx, cz));
                chests.push(Chest { pos: (x0 + 9, y, z0 + 9), table: "minecraft:chests/buried_treasure".into(), seed: fr.next_long(), exact: true });
            }
            Kind::OnTop(_, Piece::SwampHut) => *bbox = Some(scattered_box(x0, z0, r.next_bound(4), 7, 7, 9)),
            Kind::Monument => *bbox = Some([x0 - 29, 39, z0 - 29, x0 + 28, 61, z0 + 28]),
            Kind::OnTop(_, Piece::Igloo) => {
                let rot = r.next_bound(4);
                if r.next_double() < 0.5 {
                    let depth = r.next_bound(8) + 4;
                    tags.push("basement".into());
                    tags.push(format!("ladders:{depth}"));
                    // IglooPieces: the laboratory is the first piece, at start + (0, -3, -2), 3 blocks per ladder lower
                    let t = &self.templates[IGLOO_LAB];
                    let origin = [x0, 90 - 3 - depth * 3, z0 - 2];
                    let loot = |m: &str| (m == "chest").then_some(("minecraft:chests/igloo_chest", -1));
                    *chests = self.template_chests(world_seed, st, &[Placed { t, origin, mirror: false, rot, pivot: (3, 7), loot: &loot }], None, true);
                    chests.iter_mut().for_each(|c| c.pos.1 += y - 90);
                }
            }
            Kind::OnTop(_, Piece::Shipwreck { beached }) => {
                let rot = r.next_bound(4);
                let list: &[&str] = if *beached { &SHIPWRECK_BEACHED } else { &SHIPWRECK_OCEAN };
                let t = list[r.next_bound(list.len() as i32) as usize];
                tags.push(format!("template:shipwreck/{t}"));
                let tpl = &self.templates[&format!("minecraft:shipwreck/{t}")];
                for (_, m) in &tpl.markers {
                    tags.push(format!("chest:{m}"));
                }
                // ponytail: a beached wreck that fits a worldgen region draws its height (random) in the
                // first chunk the game happens to process, so its chest seeds are marked inexact
                let exact = !*beached || tpl.size[0] > 32 || tpl.size[2] > 32;
                let loot = |m: &str| match m {
                    "map_chest" => Some(("minecraft:chests/shipwreck_map", -1)),
                    "treasure_chest" => Some(("minecraft:chests/shipwreck_treasure", -1)),
                    "supply_chest" => Some(("minecraft:chests/shipwreck_supply", -1)),
                    _ => None,
                };
                let piece = Placed { t: tpl, origin: [x0, y, z0], mirror: false, rot, pivot: (4, 15), loot: &loot };
                *chests = self.template_chests(world_seed, st, &[piece], None, exact);
            }
            Kind::OnTop(_, Piece::OceanRuin { large, cluster, warm }) => {
                let rot = r.next_bound(4);
                let is_large = r.next_float() <= *large;
                let idx = r.next_bound(if is_large { 4 } else { 8 }); // template
                let mut ruins = vec![((x0, z0), rot, idx, is_large)];
                if is_large {
                    tags.push("large".into());
                    if r.next_float() <= *cluster {
                        ruins.extend(cluster_ruins(r, rot, x0, z0).into_iter().map(|(p, rot, idx)| (p, rot, idx, false)));
                        tags.push("cluster".into());
                        tags.push(format!("ruins:{}", ruins.len()));
                    }
                }
                // OceanRuinPieces.addPiece: a warm ruin is one template; a cold one is brick, cracked and
                // mossy pieces at the same place (each sets the chest again; the last one wins)
                let big = |m: &str| (m == "chest").then_some(("minecraft:chests/underwater_ruin_big", 0));
                let small = |m: &str| (m == "chest").then_some(("minecraft:chests/underwater_ruin_small", 0));
                let mut pieces = vec![];
                for ((px, pz), rot, idx, large) in ruins {
                    let names: Vec<String> = match (*warm, large) {
                        (true, true) => vec![format!("big_warm_{}", idx + 4)],
                        (true, false) => vec![format!("warm_{}", idx + 1)],
                        (false, true) => ["big_brick_", "big_cracked_", "big_mossy_"].iter().map(|b| format!("{b}{}", [1, 2, 3, 8][idx as usize])).collect(),
                        (false, false) => ["brick_", "cracked_", "mossy_"].iter().map(|b| format!("{b}{}", idx + 1)).collect(),
                    };
                    for n in names {
                        let t = &self.templates[&format!("minecraft:underwater_ruin/{n}")];
                        pieces.push(Placed { t, origin: [px, y, pz], mirror: false, rot, pivot: (0, 0), loot: if large { &big } else { &small } });
                    }
                }
                *chests = self.template_chests(world_seed, st, &pieces, None, true);
            }
            Kind::RuinedPortal(_) => {
                // RuinedPortalPiece places everything from the chunk holding its box centre
                let tag = |k: &str| tags.iter().find_map(|t| t.strip_prefix(k)).map(str::to_string);
                if let (Some(name), Some(rot), Some(b)) = (tag("template:"), tag("rotation:"), *bbox) {
                    let t = &self.templates[&format!("minecraft:{name}")];
                    let center = (b[0] + (b[3] - b[0] + 1) / 2, b[2] + (b[5] - b[2] + 1) / 2);
                    let mirror = tags.iter().any(|t| t == "mirrored");
                    let rot: i32 = rot.parse().unwrap_or(0);
                    let pivot = (t.size[0] / 2, t.size[2] / 2);
                    let piece = Placed { t, origin: [x0, b[1], z0], mirror, rot, pivot, loot: &|_| None };
                    *chests = self.template_chests(world_seed, st, &[piece], Some((center.0 >> 4, center.1 >> 4)), true);
                    // BlockAgeProcessor: each obsidian turns crying with LegacyRandom(Mth.getSeed(pos)) < 0.15.
                    // Crying obsidian is not counted: the netherrack spread after placement may overwrite it.
                    let plain = |p: &[i32; 3]| {
                        let q = transform(*p, mirror, rot, pivot);
                        JavaRandom::new(pos_seed(q[0] + x0, q[1] + b[1], q[2] + z0)).next_float() >= 0.15
                    };
                    // once per block: the frames share them
                    let plain: Vec<bool> = t.obsidian.iter().map(plain).collect();
                    tags.push(format!("obsidian:{}", plain.iter().filter(|p| **p).count()));
                    tags.push(format!("frame:{}", frame_value(&t.frames, |&i| plain[i])));
                }
            }
            _ => {}
        }
    }

    /// WorldgenRandom(Xoroshiro) as applyBiomeDecoration seeds it for this structure in `chunk`.
    fn feature_random(&self, world_seed: i64, st: usize, chunk: (i32, i32)) -> WorldgenXoro {
        let mut r = WorldgenXoro::new(0);
        let d = r.set_decoration_seed(world_seed, chunk.0 * 16, chunk.1 * 16);
        let (index, step) = self.structures[st].decoration;
        r.set_feature_seed(d, index, step);
        r
    }

    /// Loot chests of template pieces (in piece order). In each chunk, every piece touching it draws
    /// one nextLong per container placeInWorld places (template order), then one per loot data
    /// marker (whose chest is `dy` blocks from the marker; a later draw for the same chest wins).
    /// `only_chunk`: the whole piece is placed from that chunk (ruined portals).
    fn template_chests(&self, world_seed: i64, st: usize, pieces: &[Placed], only_chunk: Option<(i32, i32)>, exact: bool) -> Vec<Chest> {
        type Spots<'a> = Vec<([i32; 3], &'a str)>;
        let placed: Vec<(Spots, Spots)> = pieces
            .iter()
            .map(|pc| {
                let at = |p: [i32; 3]| {
                    let q = transform(p, pc.mirror, pc.rot, pc.pivot);
                    [q[0] + pc.origin[0], q[1] + pc.origin[1], q[2] + pc.origin[2]]
                };
                let containers = pc.t.containers.iter().map(|(p, l)| (at(*p), l.as_str())).collect();
                let markers = pc.t.markers.iter().filter_map(|(p, m)| (pc.loot)(m).map(|(l, dy)| {
                    let q = at(*p);
                    ([q[0], q[1] + dy, q[2]], l)
                })).collect();
                (containers, markers)
            })
            .collect();
        let chunk_of = |p: [i32; 3]| only_chunk.unwrap_or((p[0] >> 4, p[2] >> 4));
        let mut chunks: Vec<(i32, i32)> = placed.iter().flat_map(|(c, m)| c.iter().chain(m)).map(|(p, _)| chunk_of(*p)).collect();
        chunks.sort();
        chunks.dedup();
        let mut out: Vec<Chest> = vec![];
        for c in chunks {
            let mut r = self.feature_random(world_seed, st, c);
            for (containers, markers) in &placed {
                for (p, table) in containers.iter().filter(|(p, _)| chunk_of(*p) == c) {
                    let seed = r.next_long();
                    if !table.is_empty() {
                        out.push(Chest { pos: (p[0], p[1], p[2]), table: table.to_string(), seed, exact });
                    }
                }
                for (p, table) in markers.iter().filter(|(p, _)| chunk_of(*p) == c) {
                    let pos = (p[0], p[1], p[2]);
                    out.retain(|ch| ch.pos != pos);
                    out.push(Chest { pos, table: table.to_string(), seed: r.next_long(), exact });
                }
            }
        }
        out
    }

    /// DesertPyramidPiece: every chunk draws nextInt(3) (height offset) first, then one nextLong
    /// per chest it holds, in NORTH, EAST, SOUTH, WEST order. `dir` is the piece orientation.
    fn pyramid_chests(&self, world_seed: i64, st: usize, b: [i32; 6], dir: i32, y: i32) -> Vec<Chest> {
        let pos: Vec<(i32, i32)> = [(10, 8), (12, 10), (10, 12), (8, 10)].iter().map(|&(lx, lz)| scattered_xz(b, dir, lx, lz)).collect();
        let mut chunks: Vec<(i32, i32)> = pos.iter().map(|p| (p.0 >> 4, p.1 >> 4)).collect();
        chunks.sort();
        chunks.dedup();
        let mut out = vec![];
        for c in chunks {
            let mut r = self.feature_random(world_seed, st, c);
            r.next_bound(3);
            for p in pos.iter().filter(|p| (p.0 >> 4, p.1 >> 4) == c) {
                out.push(Chest { pos: (p.0, y - 11, p.1), table: "minecraft:chests/desert_pyramid".into(), seed: r.next_long(), exact: true });
            }
        }
        out
    }

    /// JungleTemplePiece.postProcess. Every generateBox with the moss-stone selector draws one nextFloat
    /// per block (skipAir is false, so the count is the same in every chunk and independent of
    /// terrain); dispensers and chests inside the generating chunk draw their loot seed in source order.
    fn jungle_chests(&self, world_seed: i64, st: usize, b: [i32; 6], dir: i32, y: i32) -> Vec<Chest> {
        let vol = |v: &[[i32; 6]]| v.iter().map(|q| (q[3] - q[0] + 1) * (q[4] - q[1] + 1) * (q[5] - q[2] + 1)).sum::<i32>();
        let mut boxes = vec![
            [0, -4, 0, 11, 0, 14], [2, 1, 2, 9, 2, 2], [2, 1, 12, 9, 2, 12], [2, 1, 3, 2, 2, 11], [9, 1, 3, 9, 2, 11],
            [1, 3, 1, 10, 6, 1], [1, 3, 13, 10, 6, 13], [1, 3, 2, 1, 6, 12], [10, 3, 2, 10, 6, 12], [2, 3, 2, 9, 3, 12],
            [2, 6, 2, 9, 6, 12], [3, 7, 3, 8, 7, 11], [4, 8, 4, 7, 8, 10],
        ];
        for z in [0, 14] {
            boxes.extend([[2, 4, z, 2, 5, z], [4, 4, z, 4, 5, z], [7, 4, z, 7, 5, z], [9, 4, z, 9, 5, z]]);
        }
        boxes.push([5, 6, 0, 6, 6, 0]);
        for x in [0, 11] {
            boxes.extend((2..=12).step_by(2).map(|z| [x, 4, z, x, 5, z]));
            boxes.extend([[x, 6, 5, x, 6, 5], [x, 6, 9, x, 6, 9]]);
        }
        boxes.extend([
            [2, 7, 2, 2, 9, 2], [9, 7, 2, 9, 9, 2], [2, 7, 12, 2, 9, 12], [9, 7, 12, 9, 9, 12], [4, 9, 4, 4, 9, 4], [7, 9, 4, 7, 9, 4],
            [4, 9, 10, 4, 9, 10], [7, 9, 10, 7, 9, 10], [5, 9, 7, 6, 9, 7], [4, 1, 9, 4, 1, 9], [7, 1, 9, 7, 1, 9], [4, 1, 10, 7, 2, 10],
            [5, 4, 5, 6, 4, 5],
        ]);
        boxes.extend((1..=13).step_by(2).map(|z| [1, -3, z, 1, -2, z]));
        boxes.extend((2..=12).step_by(2).map(|z| [1, -1, z, 3, -1, z]));
        boxes.extend([[2, -2, 1, 5, -2, 1], [7, -2, 1, 9, -2, 1], [6, -3, 1, 6, -3, 1], [6, -1, 1, 6, -1, 1]]);
        let (before, between) = (vol(&boxes), vol(&[[9, -1, 1, 9, -1, 5], [8, -3, 8, 8, -3, 10], [10, -3, 8, 10, -3, 10]]));
        // (local x, y, z, table): trap dispensers, main chest, then the hidden chest after `between` floats
        let containers = [
            (3, -2, 1, "minecraft:chests/jungle_temple_dispenser"),
            (9, -2, 3, "minecraft:chests/jungle_temple_dispenser"),
            (8, -3, 3, "minecraft:chests/jungle_temple"),
            (9, -3, 10, "minecraft:chests/jungle_temple"),
        ];
        let pos: Vec<(i32, i32)> = containers.iter().map(|c| scattered_xz(b, dir, c.0, c.2)).collect();
        let mut chunks: Vec<(i32, i32)> = pos.iter().map(|p| (p.0 >> 4, p.1 >> 4)).collect();
        chunks.sort();
        chunks.dedup();
        let mut out = vec![];
        for c in chunks {
            let mut r = self.feature_random(world_seed, st, c);
            for _ in 0..before {
                r.next_float();
            }
            for (i, (p, k)) in pos.iter().zip(&containers).enumerate() {
                if i == 3 {
                    for _ in 0..between {
                        r.next_float();
                    }
                }
                if (p.0 >> 4, p.1 >> 4) == c {
                    // ponytail: y assumes the box sits at the start height (updateAverageGroundHeight
                    // averages the first generated chunk); x, z and the seed are exact
                    out.push(Chest { pos: (p.0, y + k.1, p.1), table: k.3.into(), seed: r.next_long(), exact: true });
                }
            }
        }
        out
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
        let mut tags = vec![format!("template:{name}"), format!("placement:{}", setup.placement.name()), format!("rotation:{rot}")];
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

const IGLOO_LAB: &str = "minecraft:igloo/bottom";
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

/// Lightable portal frames that reuse `obsidian`: vertical (lying frames cannot be lit), 4..23 wide
/// and 5..23 tall, corners not needed, missing at most 9 blocks. Returns (frame blocks, obsidian on it).
/// A 4x5 frame anywhere misses at most 10, so these are the only frames that can beat building one.
pub fn portal_frames(obsidian: &[[i32; 3]]) -> Vec<(usize, Vec<[i32; 3]>)> {
    let lo = |a: usize| obsidian.iter().map(|p| p[a]).min().unwrap_or(0);
    let hi = |a: usize| obsidian.iter().map(|p| p[a]).max().unwrap_or(0);
    let mut out = vec![];
    for wa in [0, 2] {
        let fa = 2 - wa;
        let mut planes: Vec<i32> = obsidian.iter().map(|p| p[fa]).collect();
        planes.sort();
        planes.dedup();
        for w in 4..=23 {
            for h in 5..=23 {
                let len = 2 * (w - 2 + h - 2) as usize;
                if len > 9 + obsidian.len() {
                    continue;
                }
                for &c in &planes {
                    if len - obsidian.iter().filter(|p| p[fa] == c).count().min(len) > 9 {
                        continue;
                    }
                    for bw in lo(wa) - w + 1..=hi(wa) {
                        for bh in lo(1) - h + 1..=hi(1) {
                            let on: Vec<[i32; 3]> = obsidian
                                .iter()
                                .filter(|p| {
                                    let (i, j) = (p[wa] - bw, p[1] - bh);
                                    let (ei, ej) = (i == 0 || i == w - 1, j == 0 || j == h - 1);
                                    p[fa] == c && (0..w).contains(&i) && (0..h).contains(&j) && (ei || ej) && !(ei && ej)
                                })
                                .copied()
                                .collect();
                            if len - on.len() <= 9 {
                                out.push((len, on));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// `portal_frame` value: 10 minus the blocks the most complete frame still misses (10 = lightable).
pub fn frame_value<T>(frames: &[(usize, Vec<T>)], plain: impl Fn(&T) -> bool) -> usize {
    10usize.saturating_sub(frames.iter().map(|(len, on)| len - on.iter().filter(|p| plain(p)).count()).min().unwrap_or(10))
}

/// ScatteredFeaturePiece box at y=64 (before terrain fitting); direction index NORTH, EAST, SOUTH, WEST.
/// ScatteredFeaturePiece getWorldX/getWorldZ for local (x, z) in box `b` facing `dir` (N, E, S, W).
fn scattered_xz(b: [i32; 6], dir: i32, lx: i32, lz: i32) -> (i32, i32) {
    match dir {
        0 => (b[0] + lx, b[5] - lz),
        2 => (b[0] + lx, b[2] + lz),
        3 => (b[3] - lz, b[2] + lx),
        _ => (b[0] + lz, b[2] + lx),
    }
}

fn scattered_box(x: i32, z: i32, dir: i32, w: i32, h: i32, d: i32) -> [i32; 6] {
    if dir % 2 == 0 { [x, 64, z, x + w - 1, 64 + h - 1, z + d - 1] } else { [x, 64, z, x + d - 1, 64 + h - 1, z + w - 1] }
}

/// A template piece as placed, and what its data markers make: (loot table, chest y offset).
struct Placed<'a> {
    t: &'a Template,
    origin: [i32; 3],
    mirror: bool,
    rot: i32,
    pivot: (i32, i32),
    loot: &'a dyn Fn(&str) -> Option<(&'static str, i32)>,
}

/// OceanRuinPieces.addClusterRuins: the small ruins a cluster adds, ((x, z), rotation, template index).
fn cluster_ruins(r: &mut JavaRandom, rot: i32, x: i32, z: i32) -> Vec<((i32, i32), i32, i32)> {
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
    let mut added = vec![];
    for _ in 0..count {
        if positions.is_empty() {
            continue;
        }
        let (px, pz) = positions.remove(r.next_bound(positions.len() as i32) as usize);
        let rot = r.next_bound(4);
        let c = transform([5, 0, 6], false, rot, (0, 0));
        let (ax, az, cx2, cz2) = (px.min(px + c[0]), pz.min(pz + c[2]), px.max(px + c[0]), pz.max(pz + c[2]));
        if !(cx2 >= parent[0] && ax <= parent[2] && cz2 >= parent[1] && az <= parent[3]) {
            added.push(((px, pz), rot, r.next_bound(8))); // small ruin template
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame blocks (no corners) of a w x h frame in the z-y plane at x=0, origin (0, 0, 0).
    fn frame(w: i32, h: i32) -> Vec<[i32; 3]> {
        (0..w).flat_map(|i| (0..h).map(move |j| [0, j, i])).filter(|p| (p[2] == 0 || p[2] == w - 1) != (p[1] == 0 || p[1] == h - 1)).collect()
    }

    #[test]
    fn frame_value_counts_missing_blocks() {
        let all = |_: &[i32; 3]| true;
        assert_eq!(frame_value(&portal_frames(&frame(4, 5)), all), 10);
        // a 5x5 frame with one block gone is one short, not judged as a 4x5 frame
        let mut f = frame(5, 5);
        f.pop();
        assert_eq!(frame_value(&portal_frames(&f), all), 9);
        // crying obsidian counts as missing
        assert_eq!(frame_value(&portal_frames(&frame(4, 5)), |p| p[1] != 0), 8);
        // lying frames cannot be lit; only a vertical frame through one of its edges remains
        let lying: Vec<[i32; 3]> = frame(4, 5).iter().map(|p| [p[1], 0, p[2]]).collect();
        assert_eq!(frame_value(&portal_frames(&lying), all), 2);
        assert_eq!(frame_value(&portal_frames(&[]), all), 0);
    }
}
