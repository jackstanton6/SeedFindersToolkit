//! Full chunks: NoiseBasedChunkGenerator.doFill (noise + aquifer) and MaterialSystem.buildSurface
//! (the material rules: surface blocks, terracotta bands, ice, bedrock, ore veins).

use crate::biome::{contains, BiomeSet};
use crate::blocks::State;
use crate::density::{f32_of, ns, parse, Ctx, Df, Volume, S};
use crate::noise::{NoiseStack, Simplex, SimplexStack};
use crate::rng::{AnyRng, JavaRandom, Positional, Rng};
use crate::world::{BiomeCtx, Block, Seeded, World};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub const WORLD_SURFACE_WG: usize = 0;
pub const OCEAN_FLOOR_WG: usize = 1;

pub const WORLD_SURFACE: usize = 2;
pub const OCEAN_FLOOR: usize = 3;
pub const MOTION_BLOCKING: usize = 4;
pub const MOTION_BLOCKING_NO_LEAVES: usize = 5;

/// A chunk's blocks (index ((y - min_y) * 16 + z) * 16 + x) and its heightmaps (first free y per
/// column, index z * 16 + x): the worldgen ones (WORLD_SURFACE_WG, OCEAN_FLOOR_WG) are kept up to
/// date until the chunk reaches TERRAIN, the final ones after it, primed on first use, as
/// ProtoChunk does (`heightmapsAfter` of its persisted status).
#[derive(Clone)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    pub min_y: i32,
    pub height: i32,
    pub blocks: Vec<State>,
    pub heights: [[i32; 256]; 6],
    pub primed: [bool; 6],
    /// The chunk is past TERRAIN: writes update the final heightmaps.
    pub terrain_done: bool,
    /// ProtoChunk.postProcessing: per section, packed positions in the order they were marked.
    pub post: Vec<Vec<u16>>,
}

impl Chunk {
    pub fn new(w: &World, x: i32, z: i32) -> Chunk {
        Chunk {
            x, z, min_y: w.min_y, height: w.height, blocks: vec![w.blocks.air; (16 * 16 * w.height) as usize],
            heights: [[w.min_y; 256]; 6], primed: [true, true, false, false, false, false], terrain_done: false,
            post: vec![vec![]; (w.height / 16) as usize],
        }
    }

    #[inline]
    fn idx(&self, x: i32, y: i32, z: i32) -> usize {
        (((y - self.min_y) * 16 + (z & 15)) * 16 + (x & 15)) as usize
    }

    /// The state at a block of this chunk (x, z world or local); void air outside the height.
    pub fn get(&self, w: &World, x: i32, y: i32, z: i32) -> State {
        if y < self.min_y || y >= self.min_y + self.height {
            return w.blocks.void_air;
        }
        self.blocks[self.idx(x, y, z)]
    }

    /// ProtoChunk.setBlockState: sets the block and updates the heightmaps of the chunk's status
    /// (priming missing final ones after the write).
    pub fn set(&mut self, m: &Materials, w: &World, x: i32, y: i32, z: i32, s: State) {
        if y < self.min_y || y >= self.min_y + self.height {
            return;
        }
        let i = self.idx(x, y, z);
        self.blocks[i] = s;
        let maps: &[usize] = if self.terrain_done { &[WORLD_SURFACE, OCEAN_FLOOR, MOTION_BLOCKING, MOTION_BLOCKING_NO_LEAVES] } else { &[WORLD_SURFACE_WG, OCEAN_FLOOR_WG] };
        let mut prime = vec![];
        for &hm in maps {
            if self.primed[hm] {
                self.update_height(m, w, hm, x & 15, y, z & 15, s);
            } else {
                prime.push(hm);
            }
        }
        self.prime(m, w, &prime);
    }

    /// A write that bypasses the heightmaps (LevelChunkSection.setBlockState, e.g. ore blobs).
    pub fn set_raw(&mut self, w: &World, x: i32, y: i32, z: i32, s: State) {
        if y >= w.min_y && y < w.min_y + w.height {
            let i = self.idx(x, y, z);
            self.blocks[i] = s;
        }
    }

    fn opaque(m: &Materials, w: &World, hm: usize, s: State) -> bool {
        match hm {
            WORLD_SURFACE_WG | WORLD_SURFACE => !w.blocks.is_air(s),
            OCEAN_FLOOR_WG | OCEAN_FLOOR => w.blocks.in_table(&m.motion, s),
            MOTION_BLOCKING => w.blocks.in_table(&m.motion, s) || w.blocks.has_fluid(s),
            _ => w.blocks.in_table(&m.motion_no_leaves, s) || w.blocks.has_fluid(s),
        }
    }

    /// Heightmap.primeHeightmaps
    fn prime(&mut self, m: &Materials, w: &World, maps: &[usize]) {
        if maps.is_empty() {
            return;
        }
        let top_section = (0..self.blocks.len()).rev().find(|&i| self.blocks[i] != w.blocks.air).map_or(self.min_y, |i| self.min_y + (i / 4096 * 16) as i32 + 16);
        for &hm in maps {
            self.heights[hm] = [self.min_y; 256];
            self.primed[hm] = true;
        }
        for x in 0..16 {
            for z in 0..16 {
                let mut left: Vec<usize> = maps.to_vec();
                for y in (self.min_y..top_section).rev() {
                    let st = self.blocks[self.idx(x, y, z)];
                    if st != w.blocks.air {
                        let mut k = 0;
                        while k < left.len() {
                            if Self::opaque(m, w, left[k], st) {
                                self.heights[left[k]][(z * 16 + x) as usize] = y + 1;
                                left.remove(k);
                            } else {
                                k += 1;
                            }
                        }
                        if left.is_empty() {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Heightmap.update
    #[allow(clippy::too_many_arguments)]
    fn update_height(&mut self, m: &Materials, w: &World, hm: usize, x: i32, y: i32, z: i32, s: State) {
        let k = (z * 16 + x) as usize;
        let first = self.heights[hm][k];
        if y <= first - 2 {
            return;
        }
        if Self::opaque(m, w, hm, s) {
            if y >= first {
                self.heights[hm][k] = y + 1;
            }
        } else if first - 1 == y {
            for yy in (self.min_y..y).rev() {
                if Self::opaque(m, w, hm, self.blocks[self.idx(x, yy, z)]) {
                    self.heights[hm][k] = yy + 1;
                    return;
                }
            }
            self.heights[hm][k] = self.min_y;
        }
    }

    /// ProtoChunk.markPosForPostProcessing
    pub fn mark(&mut self, x: i32, y: i32, z: i32) {
        let sec = ((y - self.min_y) >> 4) as usize;
        if let Some(list) = self.post.get_mut(sec) {
            list.push(((x & 15) | (y & 15) << 4 | (z & 15) << 8) as u16);
        }
    }

    /// ChunkAccess.getHeight: the highest occupied y (first free - 1). Final heightmaps must be
    /// primed (see `height_mut`).
    pub fn height(&self, hm: usize, x: i32, z: i32) -> i32 {
        self.heights[hm][((z & 15) * 16 + (x & 15)) as usize] - 1
    }

    /// ChunkAccess.getHeight, priming the heightmap first if needed.
    pub fn height_mut(&mut self, m: &Materials, w: &World, hm: usize, x: i32, z: i32) -> i32 {
        if !self.primed[hm] {
            self.prime(m, w, &[hm]);
        }
        self.height(hm, x, z)
    }
}

// ------------------------------------------------------------------------------------------------
// Material rules (seed-independent tree)
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
}

impl Anchor {
    fn parse(v: &Value) -> Result<Anchor, String> {
        let i = |k: &str| v[k].as_i64().map(|n| n as i32);
        i("absolute").map(Anchor::Absolute).or(i("above_bottom").map(Anchor::AboveBottom)).or(i("below_top").map(Anchor::BelowTop)).ok_or(format!("bad vertical anchor {v}"))
    }

    /// VerticalAnchor.resolveY in the overworld's WorldGenerationContext.
    fn y(self, min_y: i32, height: i32) -> i32 {
        match self {
            Anchor::Absolute(y) => y,
            Anchor::AboveBottom(o) => min_y + o,
            Anchor::BelowTop(o) => height - 1 + min_y - o,
        }
    }
}

#[derive(Debug)]
enum Cond {
    Biome(BiomeSet),
    /// Noise slot, min, max, 3d.
    Noise(usize, f64, f64, bool),
    /// Random slot, true at and below, false at and above.
    Gradient(usize, Anchor, Anchor),
    YAbove { anchor: Anchor, mul: i32, add_stone_depth: bool },
    Water { offset: i32, mul: i32, add_stone_depth: bool },
    StoneDepth { offset: i32, add_surface_depth: bool, secondary: i32, ceiling: bool },
    Temperature,
    Steep,
    Hole,
    AbovePreliminarySurface,
    Not(Box<Cond>),
}

#[derive(Debug)]
enum Rule {
    Seq(Vec<Rule>),
    If(Cond, Box<Rule>),
    Block(State),
    Bandlands,
    /// ore, raw ore, filler, raw ore chance, density slots (density, richness, gap).
    OreVein { ore: State, raw: State, filler: State, chance: f32, dfs: [usize; 3] },
}

/// The parsed overworld material rules and what they need per seed.
pub struct Materials {
    rule: Rule,
    noises: Vec<String>,
    randoms: Vec<String>,
    dfs: Vec<Df>,
    /// blocks_motion_in_heightmap, per block (OCEAN_FLOOR heightmaps).
    pub motion: Vec<bool>,
    /// blocks_motion_in_heightmap_no_leaves
    pub motion_no_leaves: Vec<bool>,
    /// The uncarvable block tag, per block.
    pub uncarvable: Vec<bool>,
    // MaterialSystem's fixed blocks
    white: State, orange: State, terracotta: State, yellow: State, brown: State, red: State, light_gray: State,
    packed_ice: State, snow_block: State, default: State,
    eroded_badlands: Option<u16>,
    frozen_oceans: BiomeSet,
    // Biome's static temperature noises
    temperature_noise: Simplex,
    frozen_noise: SimplexStack,
    pub info_noise: Simplex,
}

struct Parser<'a> {
    w: &'a World,
    noises: Vec<String>,
    randoms: Vec<String>,
    dfs: Vec<Df>,
}

impl Parser<'_> {
    fn slot(v: &mut Vec<String>, name: &str) -> usize {
        v.iter().position(|n| n == name).unwrap_or_else(|| {
            v.push(name.to_string());
            v.len() - 1
        })
    }

    fn file(&self, dir: &str, name: &str) -> Result<Value, String> {
        let n = ns(name);
        let path = self.w.dir.join("minecraft/worldgen").join(dir).join(format!("{}.json", n.trim_start_matches("minecraft:")));
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| e.to_string())
    }

    fn rule(&mut self, v: &Value) -> Result<Rule, String> {
        if let Some(name) = v.as_str() {
            let f = self.file("material_rule", name)?;
            return self.rule(&f);
        }
        let b = |p: &Self, k: &str| p.w.blocks.parse(&v[k]);
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:sequence" => Rule::Seq(v["sequence"].as_array().ok_or("sequence")?.iter().map(|r| self.rule(r)).collect::<Result<_, _>>()?),
            "minecraft:condition" => Rule::If(self.cond(&v["if_true"])?, Box::new(self.rule(&v["then_run"])?)),
            "minecraft:block" => Rule::Block(b(self, "result_state")?),
            "minecraft:bandlands" => Rule::Bandlands,
            "minecraft:ore_vein" => {
                let mut df = |k: &str| -> Result<usize, String> {
                    self.dfs.push(parse(&v[k])?);
                    Ok(self.dfs.len() - 1)
                };
                let dfs = [df("density")?, df("richness")?, df("filler_gap")?];
                Rule::OreVein { ore: b(self, "ore_block")?, raw: b(self, "raw_ore_block")?, filler: b(self, "filler_block")?, chance: f32_of(&v["raw_ore_chance"]), dfs }
            }
            t => return Err(format!("material rule type {t} is not ported")),
        })
    }

    fn cond(&mut self, v: &Value) -> Result<Cond, String> {
        if let Some(name) = v.as_str() {
            let f = self.file("material_condition", name)?;
            return self.cond(&f);
        }
        let int = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
        let flag = |k: &str| v[k].as_bool().unwrap_or(false);
        Ok(match v["type"].as_str().unwrap_or_default() {
            "minecraft:biome" => Cond::Biome(self.w.biomes.set(&v["biome_is"])),
            "minecraft:noise_threshold" => {
                let n = Self::slot(&mut self.noises, &ns(v["noise"].as_str().unwrap_or_default()));
                Cond::Noise(n, v["min_threshold"].as_f64().unwrap_or(0.0), v["max_threshold"].as_f64().unwrap_or(f64::MAX), flag("is_3d"))
            }
            "minecraft:vertical_gradient" => Cond::Gradient(
                Self::slot(&mut self.randoms, &ns(v["random_name"].as_str().unwrap_or_default())),
                Anchor::parse(&v["true_at_and_below"])?,
                Anchor::parse(&v["false_at_and_above"])?,
            ),
            "minecraft:y_above" => Cond::YAbove { anchor: Anchor::parse(&v["anchor"])?, mul: int("surface_depth_multiplier"), add_stone_depth: flag("add_stone_depth") },
            "minecraft:water" => Cond::Water { offset: int("offset"), mul: int("surface_depth_multiplier"), add_stone_depth: flag("add_stone_depth") },
            "minecraft:stone_depth" => Cond::StoneDepth {
                offset: int("offset"),
                add_surface_depth: flag("add_surface_depth"),
                secondary: int("secondary_depth_range"),
                ceiling: v["surface_type"].as_str() == Some("ceiling"),
            },
            "minecraft:temperature" => Cond::Temperature,
            "minecraft:steep" => Cond::Steep,
            "minecraft:hole" => Cond::Hole,
            "minecraft:above_preliminary_surface" => Cond::AbovePreliminarySurface,
            "minecraft:not" => Cond::Not(Box::new(self.cond(&v["invert"])?)),
            t => return Err(format!("material condition type {t} is not ported")),
        })
    }
}

impl Materials {
    pub fn load(w: &World) -> Result<Materials, String> {
        let mut p = Parser { w, noises: vec![], randoms: vec![], dfs: vec![] };
        let rule = p.rule(&w.material_rule)?;
        let bl = &w.blocks;
        let st = |n: &str| bl.state(n).ok_or(format!("no block {n}"));
        // Biome's static noises: WorldgenRandom(LegacyRandomSource(seed)) behaves as java.util.Random here
        let temperature_noise = Simplex::new(&mut JavaRandom::new(1234), true);
        let mut r = JavaRandom::new(3456);
        let frozen_noise = SimplexStack(vec![
            (Simplex::new(&mut r, true), 1.0, 0.14285715),
            (Simplex::new(&mut r, true), 0.5, 0.2857143),
            (Simplex::new(&mut r, true), 0.25, 0.5714286),
        ]);
        let info_noise = Simplex::new(&mut JavaRandom::new(2345), true);
        Ok(Materials {
            rule,
            noises: p.noises,
            randoms: p.randoms,
            dfs: p.dfs,
            motion: bl.tag_table("minecraft:blocks_motion_in_heightmap"),
            motion_no_leaves: bl.tag_table("minecraft:blocks_motion_in_heightmap_no_leaves"),
            uncarvable: bl.tag_table("minecraft:uncarvable"),
            white: st("white_terracotta")?, orange: st("orange_terracotta")?, terracotta: st("terracotta")?, yellow: st("yellow_terracotta")?,
            brown: st("brown_terracotta")?, red: st("red_terracotta")?, light_gray: st("light_gray_terracotta")?,
            packed_ice: st("packed_ice")?, snow_block: st("snow_block")?, default: st(&w.default_block)?,
            eroded_badlands: w.biomes.id("eroded_badlands"),
            frozen_oceans: w.biomes.set(&serde_json::json!(["minecraft:frozen_ocean", "minecraft:deep_frozen_ocean"])),
            temperature_noise,
            frozen_noise,
            info_noise,
        })
    }

    /// Biome.getHeightAdjustedTemperature (the per-thread cache only memoises).
    pub fn temperature(&self, w: &World, biome: u16, x: i32, y: i32, z: i32) -> f32 {
        let (base, frozen) = w.biomes.climate[biome as usize];
        let mut t = base;
        if frozen {
            // TemperatureModifier.FROZEN
            let large = (self.frozen_noise.get2(x as f64 * 0.05, z as f64 * 0.05) * 7.0) as f64;
            let edge = self.info_noise.get2(x as f64 * 0.2, z as f64 * 0.2) as f64;
            if large + edge < 0.3 && (self.info_noise.get2(x as f64 * 0.09, z as f64 * 0.09) as f64) < 0.8 {
                t = 0.2;
            }
        }
        let snow = w.sea_level + 17;
        if y > snow {
            let v = self.temperature_noise.get2((x as f32 / 8.0) as f64, (z as f32 / 8.0) as f64) * 8.0;
            t - (v + y as f32 - snow as f32) * 0.05 / 40.0
        } else {
            t
        }
    }
}

// ------------------------------------------------------------------------------------------------
// MaterialSystem for one seed
// ------------------------------------------------------------------------------------------------

/// MaterialSystem + the per-seed parts of MaterialRuleContext (noises, randoms, samplers).
pub struct Surface {
    noises: Vec<Arc<NoiseStack>>,
    randoms: Vec<Positional>,
    dfs: Vec<Arc<S>>,
    preliminary: Option<Arc<S>>,
    noise_random: Positional,
    ore_random: Positional,
    surface: Arc<NoiseStack>,
    surface_secondary: Arc<NoiseStack>,
    clay_offset: Arc<NoiseStack>,
    badlands_pillar: Arc<NoiseStack>,
    badlands_pillar_roof: Arc<NoiseStack>,
    badlands_surface: Arc<NoiseStack>,
    iceberg_pillar: Arc<NoiseStack>,
    iceberg_pillar_roof: Arc<NoiseStack>,
    iceberg_surface: Arc<NoiseStack>,
    bands: Vec<State>,
}

/// MaterialRuleContext's per-position state.
struct Ctx2<'a> {
    w: &'a World,
    m: &'a Materials,
    sf: &'a Surface,
    bc: &'a mut BiomeCtx,
    s: &'a Seeded<'a>,
    ctx: Ctx,
    x: i32,
    y: i32,
    z: i32,
    grad_x: i32,
    grad_z: i32,
    surface_depth: i32,
    secondary: Option<f64>,
    min_surface: Option<i32>,
    water_height: i32,
    stone_below: i32,
    stone_above: i32,
    biome: Option<u16>,
    /// Prefilled ore vein densities over the chunk (getDensitiesInChunk(prefill = true)).
    prefilled: Vec<Option<Vec<f32>>>,
    prelim: Option<Vec<f32>>,
    vol: Volume,
    /// Biomes zoomed over the point-path resolver (topMaterial in applyCarvingMask).
    point_biomes: bool,
}

impl Surface {
    pub fn new(m: &Materials, s: &mut Seeded) -> Surface {
        let random = s.random();
        let mut noise = |n: &str| s.noise(n);
        let noises = m.noises.iter().map(|n| noise(n)).collect();
        let (surface, surface_secondary, clay_offset) = (noise("minecraft:surface"), noise("minecraft:surface_secondary"), noise("minecraft:clay_bands_offset"));
        let (badlands_pillar, badlands_pillar_roof, badlands_surface) = (noise("minecraft:badlands_pillar"), noise("minecraft:badlands_pillar_roof"), noise("minecraft:badlands_surface"));
        let (iceberg_pillar, iceberg_pillar_roof, iceberg_surface) = (noise("minecraft:iceberg_pillar"), noise("minecraft:iceberg_pillar_roof"), noise("minecraft:iceberg_surface"));
        let dfs = m.dfs.iter().map(|d| s.sampler_of(d)).collect();
        let preliminary = s.w.chunk_surface_level.clone().map(|d| s.sampler_of(&d));
        let randoms = m.randoms.iter().map(|n| random.from_hash(n).fork_positional()).collect();
        let bands = generate_bands(m, &mut random.from_hash("minecraft:clay_bands"));
        Surface {
            noises, randoms, dfs, preliminary, noise_random: random,
            ore_random: random.from_hash("minecraft:ore").fork_positional(),
            surface, surface_secondary, clay_offset, badlands_pillar, badlands_pillar_roof, badlands_surface,
            iceberg_pillar, iceberg_pillar_roof, iceberg_surface, bands,
        }
    }

    /// MaterialSystem.getSurfaceDepth
    fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let n = self.surface.get(x as f64, 0.0, z as f64) as f64;
        (n * 2.75 + 3.0 + self.noise_random.at(x, 0, z).next_double() * 0.25) as i32
    }

    /// MaterialSystem.getBand
    fn band(&self, x: i32, y: i32, z: i32) -> State {
        let off = java_round_f(self.clay_offset.get(x as f64, 0.0, z as f64) * 4.0);
        let n = self.bands.len() as i32;
        self.bands[((y + off + n) % n) as usize]
    }
}

/// Math.round(float)
pub fn java_round_f(v: f32) -> i32 {
    (v + 0.5).floor() as i32
}

/// MaterialSystem.generateBands
fn generate_bands(m: &Materials, r: &mut AnyRng) -> Vec<State> {
    let mut b = vec![m.terracotta; 192];
    let mut i = 0usize;
    while i < b.len() {
        i += r.next_bound(5) as usize + 1;
        if i < b.len() {
            b[i] = m.orange;
        }
        i += 1;
    }
    make_bands(r, &mut b, 1, m.yellow);
    make_bands(r, &mut b, 2, m.brown);
    make_bands(r, &mut b, 1, m.red);
    let white_count = r.between_inclusive(9, 15);
    let (mut i, mut start) = (0, 0usize);
    while i < white_count && start < b.len() {
        b[start] = m.white;
        if start >= 2 && r.next_bool() {
            b[start - 1] = m.light_gray;
        }
        if start + 1 < b.len() && r.next_bool() {
            b[start + 1] = m.light_gray;
        }
        i += 1;
        start += r.next_bound(16) as usize + 4;
    }
    b
}

fn make_bands(r: &mut AnyRng, b: &mut [State], base: i32, s: State) {
    let count = r.between_inclusive(6, 15);
    for _ in 0..count {
        let width = base + r.next_bound(3);
        let start = r.next_bound(b.len() as i32) as usize;
        let mut p = 0;
        while start + p < b.len() && (p as i32) < width {
            b[start + p] = s;
            p += 1;
        }
    }
}

impl Ctx2<'_> {
    fn biome(&mut self) -> u16 {
        if self.biome.is_none() {
            self.biome = Some(if self.point_biomes {
                self.s.block_biome_point(self.bc, self.x, self.y, self.z)
            } else {
                self.s.block_biome(self.bc, self.x, self.y, self.z)
            });
        }
        self.biome.unwrap()
    }

    fn secondary(&mut self) -> f64 {
        if self.secondary.is_none() {
            self.secondary = Some(self.sf.surface_secondary.get(self.x as f64, 0.0, self.z as f64) as f64);
        }
        self.secondary.unwrap()
    }

    /// MaterialRuleContext.getMinSurfaceLevel
    fn min_surface(&mut self) -> i32 {
        if self.min_surface.is_none() {
            let level = match (&self.sf.preliminary, self.vol.index_of_block(self.x, self.vol.y0, self.z)) {
                (Some(f), Some(_)) => {
                    if self.prelim.is_none() {
                        let v = Volume::new(self.vol.sx, 1, self.vol.sz, self.vol.x0, 0, self.vol.z0);
                        let mut buf = vec![0.0; v.size()];
                        f.sample_volume(&mut self.ctx, &mut buf, &v);
                        self.prelim = Some(buf);
                    }
                    let v = Volume::new(self.vol.sx, 1, self.vol.sz, self.vol.x0, 0, self.vol.z0);
                    self.prelim.as_ref().unwrap()[v.idx(self.x - v.x0, 0, self.z - v.z0)]
                }
                (Some(f), None) => f.sample(&mut self.ctx, self.x, 0, self.z),
                (None, _) => 0.0,
            };
            self.min_surface = Some(level.floor() as i32 + self.surface_depth - 8);
        }
        self.min_surface.unwrap()
    }

    fn density(&mut self, slot: usize, prefill: bool) -> f32 {
        let f = self.sf.dfs[slot].clone();
        if !prefill {
            return f.sample(&mut self.ctx, self.x, self.y, self.z);
        }
        if self.prefilled[slot].is_none() {
            let mut buf = vec![0.0; self.vol.size()];
            f.sample_volume(&mut self.ctx, &mut buf, &self.vol);
            self.prefilled[slot] = Some(buf);
        }
        match self.vol.index_of_block(self.x, self.y, self.z) {
            Some(i) => self.prefilled[slot].as_ref().unwrap()[i],
            None => f.sample(&mut self.ctx, self.x, self.y, self.z),
        }
    }

    fn test(&mut self, c: &Cond) -> bool {
        let (min_y, height) = (self.w.min_y, self.w.height);
        match c {
            Cond::Biome(set) => contains(*set, self.biome()),
            Cond::Noise(n, min, max, d3) => {
                let y = if *d3 { self.y as f64 } else { 0.0 };
                let v = self.sf.noises[*n].get(self.x as f64, y, self.z as f64) as f64;
                v >= *min && v <= *max
            }
            Cond::Gradient(r, t, f) => {
                let (t, f) = (t.y(min_y, height), f.y(min_y, height));
                if self.y <= t {
                    return true;
                }
                if self.y >= f {
                    return false;
                }
                // Mth.map(blockY, trueAtAndBelow, falseAtAndAbove, 1, 0)
                let p = 1.0 + (self.y as f64 - t as f64) / (f as f64 - t as f64) * (0.0 - 1.0);
                (self.sf.randoms[*r].at(self.x, self.y, self.z).next_float() as f64) < p
            }
            Cond::YAbove { anchor, mul, add_stone_depth } => {
                self.y + if *add_stone_depth { self.stone_above } else { 0 } >= anchor.y(min_y, height) + self.surface_depth * mul
            }
            Cond::Water { offset, mul, add_stone_depth } => {
                self.water_height == i32::MIN
                    || self.y + if *add_stone_depth { self.stone_above } else { 0 } >= self.water_height + offset + self.surface_depth * mul
            }
            Cond::StoneDepth { offset, add_surface_depth, secondary, ceiling } => {
                let depth = if *ceiling { self.stone_below } else { self.stone_above };
                let sd = if *add_surface_depth { self.surface_depth } else { 0 };
                let sec = if *secondary == 0 { 0 } else { (0.0 + (self.secondary() - -1.0) / (1.0 - -1.0) * (*secondary as f64 - 0.0)) as i32 };
                depth <= 1 + offset + sd + sec
            }
            Cond::Temperature => {
                let b = self.biome();
                self.m.temperature(self.w, b, self.x, self.y, self.z) < 0.15
            }
            Cond::Steep => self.grad_x <= -4 || self.grad_z >= 4,
            Cond::Hole => self.surface_depth <= 0,
            Cond::AbovePreliminarySurface => self.y >= self.min_surface(),
            Cond::Not(c) => !self.test(c),
        }
    }

    fn apply(&mut self, r: &Rule) -> Option<State> {
        match r {
            Rule::Seq(v) => v.iter().find_map(|r| self.apply(r)),
            Rule::If(c, r) => if self.test(c) { self.apply(r) } else { None },
            Rule::Block(s) => Some(*s),
            Rule::Bandlands => Some(self.sf.band(self.x, self.y, self.z)),
            Rule::OreVein { ore, raw, filler, chance, dfs } => {
                let d = self.density(dfs[0], true);
                if d <= 0.0 {
                    return None;
                }
                let mut r = self.sf.ore_random.at(self.x, self.y, self.z);
                if r.next_float() > d {
                    return None;
                }
                let richness = self.density(dfs[1], true);
                if r.next_float() < richness && self.density(dfs[2], false) < 0.0 {
                    Some(if r.next_float() < *chance { *raw } else { *ore })
                } else {
                    Some(*filler)
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Generation
// ------------------------------------------------------------------------------------------------

impl World {
    pub fn materials(&self) -> Result<&Materials, String> {
        self.materials.get_or_init(|| Materials::load(self)).as_ref().map_err(Clone::clone)
    }

    pub fn carvers(&self) -> Result<&crate::carver::Carvers, String> {
        self.carvers.get_or_init(|| crate::carver::Carvers::load(self)).as_ref().map_err(Clone::clone)
    }
}

/// The chunk after doFill (no structures), buildSurface and (with `carvers`) generateCarvers: the
/// TERRAIN status. `s` must have its terrain prepared.
pub fn terrain_chunk(s: &Seeded, sf: &Surface, bc: &mut BiomeCtx, cx: i32, cz: i32, carvers: bool) -> Result<Chunk, String> {
    let w = s.w;
    let m = w.materials()?;
    let mut c = Chunk::new(w, cx, cz);
    // doFill: air is not written; heightmaps follow the blocks top-down
    let (fill, marks) = s.noise_chunk(cx, cz);
    for z in 0..16 {
        for x in 0..16 {
            for y in (w.min_y..w.min_y + w.height).rev() {
                let k = (((y - w.min_y) * 16 + z) * 16 + x) as usize;
                let st = match fill[k] {
                    Block::Air => continue,
                    Block::Stone => m.default,
                    Block::Water => w.blocks.water,
                    Block::Lava => w.blocks.lava,
                };
                c.set(m, w, cx * 16 + x, y, cz * 16 + z, st);
                if marks[k] && w.blocks.has_fluid(st) {
                    c.mark(x, y, z);
                }
            }
        }
    }
    build_surface(s, sf, bc, &mut c)?;
    if carvers {
        carve(s, sf, bc, &mut c)?;
    }
    c.terrain_done = true;
    Ok(c)
}

/// NoiseBasedChunkGenerator.generateCarvers + applyCarvingMask
fn carve(s: &Seeded, sf: &Surface, bc: &mut BiomeCtx, c: &mut Chunk) -> Result<(), String> {
    let w = s.w;
    let m = w.materials()?;
    let carvers = w.carvers()?;
    let mut point = BiomeCtx::uncached();
    let mask = carvers.mask(w, s.seed, c.x, c.z, |qx, qz| s.biome(&mut point, qx, 0, qz));
    if mask.is_empty() {
        return Ok(());
    }
    let (grass, mycelium, dirt) = (w.blocks.state("grass_block"), w.blocks.state("mycelium"), w.blocks.state("dirt"));
    let is = |st: State, b: Option<State>| b.is_some_and(|b| w.blocks.name(st) == w.blocks.name(b));
    s.with_aquifer(c.x, c.z, |sub| {
        let mut runs = vec![];
        mask.visit(|x, z, bottom, top| runs.push((x, z, bottom, top)));
        for (x, z, bottom, top) in runs {
            let mut has_grass = false;
            let (wx, wz) = (c.x * 16 + x, c.z * 16 + z);
            for y in (bottom..=top).rev() {
                let old = c.get(w, wx, y, wz);
                if w.blocks.in_table(&m.uncarvable, old) {
                    continue;
                }
                if is(old, grass) || is(old, mycelium) {
                    has_grass = true;
                }
                let (sub_block, schedule) = sub(wx, y, wz, 0.0);
                let st = match sub_block {
                    Block::Stone => continue, // null: nothing to place
                    Block::Air => w.blocks.air,
                    Block::Water => w.blocks.water,
                    Block::Lava => w.blocks.lava,
                };
                c.set(m, w, wx, y, wz, st);
                if schedule && w.blocks.has_fluid(st) {
                    c.mark(wx, y, wz);
                }
                if has_grass && is(c.get(w, wx, y - 1, wz), dirt) {
                    let under_fluid = w.blocks.has_fluid(st);
                    if let Some(top) = top_material(s, sf, bc, c, wx, y - 1, wz, under_fluid) {
                        c.set(m, w, wx, y - 1, wz, top);
                        if w.blocks.has_fluid(top) {
                            c.mark(wx, y - 1, wz);
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

/// MaterialSystem.topMaterial: the rules for one block (a 1x1x1 volume), with biomes zoomed over
/// the point-path resolver.
#[allow(clippy::too_many_arguments)]
fn top_material(s: &Seeded, sf: &Surface, bc: &mut BiomeCtx, c: &Chunk, x: i32, y: i32, z: i32, under_fluid: bool) -> Option<State> {
    let w = s.w;
    let m = w.materials().ok()?;
    let mut cx = Ctx2 {
        w, m, sf, bc, s, ctx: Ctx::cached(), x, y, z, grad_x: 0, grad_z: 0, surface_depth: 0, secondary: None, min_surface: None,
        water_height: 0, stone_below: 0, stone_above: 0, biome: None, prefilled: vec![None; sf.dfs.len()], prelim: None,
        vol: Volume::new(1, 1, 1, x, y, z), point_biomes: true,
    };
    let hm = |x: i32, z: i32| c.height(WORLD_SURFACE_WG, x, z);
    let (lx, lz) = (x & 15, z & 15);
    cx.grad_x = hm((lx + 1).min(15), lz) - hm((lx - 1).max(0), lz);
    cx.grad_z = hm(lx, (lz + 1).min(15)) - hm(lx, (lz - 1).max(0));
    cx.surface_depth = sf.surface_depth(x, z);
    cx.stone_above = 1;
    cx.stone_below = 1;
    cx.water_height = if under_fluid { y + 1 } else { i32::MIN };
    cx.apply(&m.rule)
}

/// MaterialSystem.buildSurface
fn build_surface(s: &Seeded, sf: &Surface, bc: &mut BiomeCtx, c: &mut Chunk) -> Result<(), String> {
    let w = s.w;
    let m = w.materials()?;
    let (min_y, max_y) = (w.min_y, w.min_y + w.height - 1);
    // the narrowed volume: up to the highest section holding a non-air block
    let top = (0..c.blocks.len()).rev().find(|&i| !w.blocks.is_air(c.blocks[i])).map_or(min_y, |i| min_y + (i / 256) as i32);
    let max_block_y = (top.div_euclid(16)) * 16 + 15;
    let vol = Volume::new(16, (max_block_y - min_y + 1).max(1), 16, c.x * 16, min_y, c.z * 16);
    let mut cx = Ctx2 {
        w, m, sf, bc, s, ctx: Ctx::cached(), x: 0, y: 0, z: 0, grad_x: 0, grad_z: 0, surface_depth: 0, secondary: None, min_surface: None,
        water_height: 0, stone_below: 0, stone_above: 0, biome: None, prefilled: vec![None; sf.dfs.len()], prelim: None, vol,
        point_biomes: false,
    };
    for lx in 0..16 {
        for lz in 0..16 {
            let (bx, bz) = (c.x * 16 + lx, c.z * 16 + lz);
            let starting = c.height(WORLD_SURFACE_WG, lx, lz) + 1;
            let surface_biome = s.block_biome(cx.bc, bx, starting, bz);
            if Some(surface_biome) == m.eroded_badlands {
                eroded_badlands(m, sf, w, c, bx, bz, starting);
            }
            let height = c.height(WORLD_SURFACE_WG, lx, lz) + 1;
            let hm = |c: &Chunk, x: i32, z: i32| c.height(WORLD_SURFACE_WG, x, z);
            cx.grad_x = hm(c, (lx + 1).min(15), lz) - hm(c, (lx - 1).max(0), lz);
            cx.grad_z = hm(c, lx, (lz + 1).min(15)) - hm(c, lx, (lz - 1).max(0));
            // updateXZ
            cx.x = bx;
            cx.z = bz;
            cx.surface_depth = sf.surface_depth(bx, bz);
            cx.secondary = None;
            cx.min_surface = None;
            let (mut stone_above, mut water_height, mut next_ceiling) = (0, i32::MIN, i32::MAX);
            let end_y = min_y;
            let mut y = height;
            while y >= end_y {
                let old = c.get(w, bx, y, bz);
                if w.blocks.is_air(old) {
                    stone_above = 0;
                    water_height = i32::MIN;
                } else if w.blocks.has_fluid(old) {
                    if water_height == i32::MIN {
                        water_height = y + 1;
                    }
                } else {
                    if next_ceiling >= y {
                        next_ceiling = -32512; // DimensionType.WAY_BELOW_MIN_Y
                        let mut ly = y - 1;
                        while ly >= end_y - 1 {
                            let st = c.get(w, bx, ly, bz);
                            if w.blocks.is_air(st) || w.blocks.has_fluid(st) {
                                next_ceiling = ly + 1;
                                break;
                            }
                            ly -= 1;
                        }
                    }
                    stone_above += 1;
                    // updateY
                    cx.stone_above = stone_above;
                    cx.stone_below = y - next_ceiling + 1;
                    cx.water_height = water_height;
                    cx.y = y;
                    cx.biome = None;
                    if y >= min_y && y <= max_y {
                        if let Some(st) = cx.apply(&m.rule) {
                            c.set(m, w, bx, y, bz, st);
                            if w.blocks.has_fluid(st) {
                                c.mark(bx, y, bz);
                            }
                        }
                    }
                }
                y -= 1;
            }
            if crate::biome::contains(m.frozen_oceans, surface_biome) {
                let min_surface = cx.min_surface();
                frozen_ocean(m, sf, w, c, min_surface, surface_biome, bx, bz, starting);
            }
        }
    }
    Ok(())
}

/// MaterialSystem.erodedBadlandsExtension
fn eroded_badlands(m: &Materials, sf: &Surface, w: &World, c: &mut Chunk, x: i32, z: i32, height: i32) {
    let buffer = (sf.badlands_surface.get(x as f64, 0.0, z as f64) as f64 * 8.25)
        .abs()
        .min((sf.badlands_pillar.get(x as f64 * 0.2, 0.0, z as f64 * 0.2) * 15.0) as f64);
    if buffer <= 0.0 {
        return;
    }
    let floor = (sf.badlands_pillar_roof.get(x as f64 * 0.75, 0.0, z as f64 * 0.75) as f64 * 1.5).abs();
    let top = 64.0 + (buffer * buffer * 2.5).min((floor * 50.0).ceil() + 24.0);
    let start = top.floor() as i32;
    if height > start {
        return;
    }
    let default_block = w.blocks.name(m.default).to_string();
    for y in (w.min_y..=start).rev() {
        let old = c.get(w, x, y, z);
        if w.blocks.name(old) == default_block {
            break;
        }
        if w.blocks.name(old) == "minecraft:water" {
            return;
        }
    }
    let mut y = start;
    while y >= w.min_y && w.blocks.is_air(c.get(w, x, y, z)) {
        c.set(m, w, x, y, z, m.default);
        y -= 1;
    }
}

/// MaterialSystem.frozenOceanExtension
#[allow(clippy::too_many_arguments)]
fn frozen_ocean(m: &Materials, sf: &Surface, w: &World, c: &mut Chunk, min_surface: i32, biome: u16, x: i32, z: i32, height: i32) {
    let sea = w.sea_level;
    let iceberg = (sf.iceberg_surface.get(x as f64, 0.0, z as f64) as f64 * 8.25)
        .abs()
        .min((sf.iceberg_pillar.get(x as f64 * 1.28, 0.0, z as f64 * 1.28) * 15.0) as f64);
    if iceberg <= 1.8 {
        return;
    }
    let roof = (sf.iceberg_pillar_roof.get(x as f64 * 1.17, 0.0, z as f64 * 1.17) as f64 * 1.5).abs();
    let mut top = (iceberg * iceberg * 1.2).min((roof * 40.0).ceil() + 14.0);
    if m.temperature(w, biome, x, sea, z) > 0.1 {
        top -= 2.0;
    }
    if top <= 2.0 {
        return;
    }
    let bottom = sea as f64 - top - 7.0;
    top += sea as f64;
    let mut r = sf.noise_random.at(x, 0, z);
    let max_snow_depth = 2 + r.next_bound(4);
    let min_snow_height = sea + 18 + r.next_bound(10);
    let mut snow_depth = 0;
    let mut y = height.max(top as i32 + 1);
    while y >= min_surface {
        let old = c.get(w, x, y, z);
        if (w.blocks.is_air(old) && y < top as i32 && r.next_double() > 0.01)
            || (w.blocks.name(old) == "minecraft:water" && y > bottom as i32 && y < sea && r.next_double() > 0.15)
        {
            if snow_depth <= max_snow_depth && y > min_snow_height {
                c.set(m, w, x, y, z, m.snow_block);
                snow_depth += 1;
            } else {
                c.set(m, w, x, y, z, m.packed_ice);
            }
        }
        y -= 1;
    }
}

/// How many observed floor blocks (x, y, z, is bedrock) the bedrock_floor rule reproduces for a
/// seed, from its vertical gradient alone (as if every block there were solid ground).
pub fn bedrock_floor(w: &World, seed: i64, blocks: &[(i32, i32, i32, bool)]) -> usize {
    let r = if w.xoroshiro_random() { crate::rng::Xoroshiro::new(seed).fork_positional() } else { JavaRandom::new(seed).fork_positional() };
    let f = r.from_hash("minecraft:bedrock_floor").fork_positional();
    // the overworld bedrock_floor rule: true at and below min_y, false at and above min_y + 5
    let (t, fa) = (w.min_y, w.min_y + 5);
    blocks
        .iter()
        .filter(|&&(x, y, z, want)| {
            let got = if y <= t {
                true
            } else if y >= fa {
                false
            } else {
                let p = 1.0 + (y as f64 - t as f64) / (fa as f64 - t as f64) * (0.0 - 1.0);
                (f.at(x, y, z).next_float() as f64) < p
            };
            got == want
        })
        .count()
}

/// Generated chunks of one seed, cached (per-seed, per-thread use).
pub struct Chunks<'w> {
    pub s: Seeded<'w>,
    sf: Option<Surface>,
    bc: BiomeCtx,
    cache: HashMap<(i32, i32), Arc<Chunk>>,
    surface_cache: HashMap<(i32, i32), Arc<Chunk>>,
    full_cache: HashMap<(i32, i32), Arc<Chunk>>,
}

impl<'w> Chunks<'w> {
    pub fn new(w: &'w World, seed: i64) -> Self {
        let mut s = Seeded::new(w, seed);
        s.prepare_terrain();
        Chunks { s, sf: None, bc: BiomeCtx::cached(), cache: HashMap::new(), surface_cache: HashMap::new(), full_cache: HashMap::new() }
    }

    pub fn terrain(&mut self, cx: i32, cz: i32) -> Result<Arc<Chunk>, String> {
        if let Some(c) = self.cache.get(&(cx, cz)) {
            return Ok(c.clone());
        }
        let c = Arc::new(self.generate(cx, cz, true)?);
        self.cache.insert((cx, cz), c.clone());
        Ok(c)
    }

    /// The chunk after the material rules (no carvers), cached separately.
    pub fn surface(&mut self, cx: i32, cz: i32) -> Result<Arc<Chunk>, String> {
        if let Some(c) = self.surface_cache.get(&(cx, cz)) {
            return Ok(c.clone());
        }
        let c = Arc::new(self.generate(cx, cz, false)?);
        self.surface_cache.insert((cx, cz), c.clone());
        Ok(c)
    }

    /// The FULL chunk: the TERRAIN chunk after postProcessGeneration (the one fluid tick), with
    /// its neighbours' TERRAIN chunks around it.
    // ponytail: neighbours are not post-processed first; the game finishes chunks in no fixed order,
    // so fluid one neighbour spreads across the border is missing
    pub fn full(&mut self, cx: i32, cz: i32) -> Result<Arc<Chunk>, String> {
        if let Some(c) = self.full_cache.get(&(cx, cz)) {
            return Ok(c.clone());
        }
        let w = self.s.w;
        // FEATURES: the 3x3 chunks around this one are decorated (each writes one chunk around
        // it), so the 5x5 TERRAIN chunks are needed
        // ponytail: the game decorates neighbours in no fixed order
        let mut terrain = HashMap::new();
        for dz in -2..=2 {
            for dx in -2..=2 {
                terrain.insert((cx + dx, cz + dz), (*self.terrain(cx + dx, cz + dz)?).clone());
            }
        }
        let seed = self.s.seed;
        let mut level = crate::feature::Level { w, m: w.materials()?, s: &self.s, chunks: terrain, centre: (cx, cz), bc: BiomeCtx::cached() };
        // x ascending, then z: of the orders tried this fits the server worlds best (overlapping
        // tree canopies depend on it: leaves are replaceable by trees)
        for (dx, dz) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (dx, dz))) {
            level.centre = (cx + dx, cz + dz);
            crate::feature::decorate(&mut level, seed)?;
        }
        let around: Vec<Chunk> = (-1..=1).flat_map(|dz| (-1..=1).map(move |dx| (dx, dz))).map(|(dx, dz)| level.chunks.remove(&(cx + dx, cz + dz)).unwrap()).collect();
        let mut r = crate::fluid::Region::new(w, w.materials()?, cx, cz, around);
        r.post_process();
        let c = Arc::new(r.chunks.swap_remove(4));
        self.full_cache.insert((cx, cz), c.clone());
        Ok(c)
    }

    /// A chunk without caching; `carvers` false stops after the material rules.
    pub fn generate(&mut self, cx: i32, cz: i32, carvers: bool) -> Result<Chunk, String> {
        if self.sf.is_none() {
            let m = self.s.w.materials()?;
            self.sf = Some(Surface::new(m, &mut self.s));
        }
        terrain_chunk(&self.s, self.sf.as_ref().unwrap(), &mut self.bc, cx, cz, carvers)
    }
}
