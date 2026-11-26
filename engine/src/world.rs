//! A loaded version (`World`, from data/<version>/) and a per-seed view (`Seeded`, = RandomState):
//! climate/biomes on both evaluation paths, and getBaseHeight/getBaseColumn with the aquifer.

use crate::biome::{quantize, Biomes, Climate};
use crate::density::{f32_of, parse, Caches, Compiler, Ctx, Df, Volume, S};
use crate::noise::NormalNoise;
use crate::rng::{lcg_next, obfuscate_seed, JavaRandom, Positional, Xoroshiro};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Every *.json under `dir`, keyed "minecraft:<relative/path>" like a registry id.
pub fn json_dir(dir: &Path) -> Result<Vec<(String, Value)>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Value)>) -> Result<(), String> {
        for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            if p.is_dir() {
                walk(root, &p, out)?;
            } else if p.extension().is_some_and(|x| x == "json") {
                let rel = p.strip_prefix(root).unwrap().with_extension("");
                let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                let v = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
                out.push((format!("minecraft:{}", rel.to_string_lossy().replace('\\', "/")), v));
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Climate.Sampler order: temperature, humidity, continentalness, erosion, depth, weirdness.
const CLIMATE: [&str; 6] = ["temperature", "vegetation", "continents", "erosion", "depth", "ridges"];
const AQUIFER: [&str; 6] = ["barrier", "fluid_level_floodedness", "fluid_level_spread", "lava", "exclusion", "surface_level"];

pub struct World {
    pub version: String,
    pub dir: PathBuf,
    pub defs: HashMap<String, Df>,
    pub noises: HashMap<String, NormalNoise>,
    pub sea_level: i32,
    pub min_y: i32,
    pub height: i32,
    legacy_random: bool,
    climate_dfs: Vec<Df>,
    /// NoiseGeneratorSettings.spawnTarget: per target point, (climate sampler index, min, max).
    pub spawn_target: Vec<Vec<(usize, i64, i64)>>,
    final_density: Df,
    aquifer_dfs: Option<Vec<Df>>,
    /// NoiseRouter.chunkSurfaceLevel (MaterialSystem's preliminary surface).
    pub chunk_surface_level: Option<Df>,
    pub default_block: String,
    pub blocks: crate::blocks::Blocks,
    /// The overworld material rule tree (seed-independent), parsed on first use.
    pub materials: std::sync::OnceLock<Result<crate::chunk::Materials, String>>,
    pub material_rule: Value,
    pub biomes: Biomes,
    pub climate: Climate,
    pub structures: crate::structure::Structures,
    /// Chest loot tables; Err (with the reason) when the data folder predates loot extraction.
    pub loot: Result<crate::loot::Loot, String>,
    /// Rewritten router functions (climate, final density, aquifer, in that order) and their
    /// caches: the seed-independent part of compiling, done once.
    rewritten: std::sync::OnceLock<(Vec<Df>, Caches)>,
}

impl World {
    pub fn load(dir: &Path) -> Result<World, String> {
        let wg = dir.join("minecraft/worldgen");
        // Functions using unported (End/datapack-only) types are skipped; loading fails below only
        // if the overworld router actually reaches one.
        let mut defs = HashMap::new();
        let mut skipped = HashMap::new();
        for (id, v) in json_dir(&wg.join("density_function"))? {
            match parse(&v) {
                Ok(df) => { defs.insert(id, df); }
                Err(e) => { skipped.insert(id, e); }
            }
        }
        let noises = json_dir(&wg.join("noise"))?.into_iter().map(|(id, v)| { let n = NormalNoise::from_json(&id, &v); (id, n) }).collect();
        let settings_path = wg.join("noise_settings/overworld.json");
        let s: Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).map_err(|e| format!("{}: {e}", settings_path.display()))?)
            .map_err(|e| e.to_string())?;
        let router = &s["noise_router"];
        let climate_dfs: Vec<Df> = CLIMATE.iter().map(|k| parse(&router[*k])).collect::<Result<_, _>>()?;
        let aquifer_dfs: Option<Vec<Df>> = match s.get("aquifers") {
            Some(a) if a.is_object() => Some(AQUIFER.iter().map(|k| parse(&a[*k])).collect::<Result<_, _>>()?),
            _ => None,
        };
        let final_density = parse(&router["final_density"])?;
        let chunk_surface_level = router.get("chunk_surface_level").map(parse).transpose()?;
        // spawn target functions are the router's climate functions (by reference)
        let mut spawn_target = vec![];
        for point in s["spawn_target"].as_array().into_iter().flatten() {
            let mut params = vec![];
            for (name, range) in point.as_object().ok_or("spawn_target point must be an object")? {
                let i = CLIMATE.iter().position(|k| router[*k].as_str() == Some(name)).ok_or(format!("spawn_target function {name} is not a climate function"))?;
                params.push((i, quantize(f32_of(&range[0])), quantize(f32_of(&range[1]))));
            }
            spawn_target.push(params);
        }
        let mut todo: Vec<String> = vec![];
        for df in climate_dfs.iter().chain([&final_density]).chain(aquifer_dfs.iter().flatten()) {
            df.refs(&mut todo);
        }
        let mut seen = std::collections::HashSet::new();
        while let Some(n) = todo.pop() {
            if seen.insert(n.clone()) {
                match defs.get(&n) {
                    Some(df) => df.refs(&mut todo),
                    None => return Err(skipped.get(&n).map_or(format!("missing density function {n}"), |e| format!("{n}: {e}"))),
                }
            }
        }
        let biomes = Biomes::load(dir)?;
        let climate = Climate::load(&dir.join("reports/biome_parameters/minecraft/overworld.json"), &biomes)?;
        let mut structures = crate::structure::Structures::load(dir, &biomes)?;
        structures.mark_possible(climate.possible());
        Ok(World {
            loot: crate::loot::Loot::load(dir),
            version: std::fs::read_to_string(dir.join("version.txt")).unwrap_or_default().trim().to_string(),
            dir: dir.to_path_buf(),
            defs,
            noises,
            sea_level: s["sea_level"].as_i64().unwrap() as i32,
            min_y: s["noise"]["min_y"].as_i64().unwrap() as i32,
            height: s["noise"]["height"].as_i64().unwrap() as i32,
            legacy_random: s["legacy_random_source"].as_bool().unwrap_or(false),
            climate_dfs,
            spawn_target,
            final_density,
            chunk_surface_level,
            default_block: s["default_block"].as_str().unwrap_or("minecraft:stone").to_string(),
            blocks: crate::blocks::Blocks::load(dir)?,
            materials: std::sync::OnceLock::new(),
            material_rule: s["material_rule"].clone(),
            aquifer_dfs,
            biomes,
            climate,
            structures,
            rewritten: std::sync::OnceLock::new(),
        })
    }

    fn rewritten(&self) -> &(Vec<Df>, Caches) {
        self.rewritten.get_or_init(|| {
            // same order as compiling them one by one, so cache ids match
            let mut c = Compiler::new(&self.defs, &self.noises, Positional::Legacy { seed: 0 });
            let roots = self.climate_dfs.iter().chain([&self.final_density]).chain(self.aquifer_dfs.iter().flatten()).map(|df| c.rewrite(df)).collect();
            (roots, c.caches)
        })
    }
}

/// Heightmap.Types used by worldgen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Heightmap {
    WorldSurfaceWg,
    OceanFloorWg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    Air,
    Stone,
    Water,
    Lava,
}

impl Heightmap {
    pub fn test(self, b: Block) -> bool {
        match self {
            Heightmap::WorldSurfaceWg => b != Block::Air,
            Heightmap::OceanFloorWg => b == Block::Stone,
        }
    }
}

/// Point-path biome lookups keep their own sampler context and R-tree warm start.
pub struct BiomeCtx {
    pub ctx: Ctx,
    pub last: Option<usize>,
    /// Climate targets of chunks (volume path), for block-level lookups.
    pub chunks: HashMap<(i32, i32), Vec<[i64; 7]>>,
}

impl BiomeCtx {
    pub fn cached() -> Self {
        BiomeCtx { ctx: Ctx::cached(), last: None, chunks: HashMap::new() }
    }

    pub fn uncached() -> Self {
        BiomeCtx { ctx: Ctx::uncached(), last: None, chunks: HashMap::new() }
    }
}

struct Terrain {
    final_density: Arc<S>,
    aquifer: Option<Vec<Arc<S>>>,
    aquifer_random: Positional,
}

/// RandomState for one seed. Samplers are compiled on first use, so biome-only searches never
/// pay for terrain.
pub struct Seeded<'w> {
    pub w: &'w World,
    pub seed: i64,
    comp: Compiler<'w>,
    climate: Vec<Arc<S>>,
    terrain: Option<Terrain>,
}

impl<'w> Seeded<'w> {
    pub fn new(w: &'w World, seed: i64) -> Self {
        let random = if w.legacy_random { JavaRandom::new(seed).fork_positional() } else { Xoroshiro::new(seed).fork_positional() };
        let (roots, caches) = w.rewritten();
        let mut comp = Compiler::with_caches(&w.defs, &w.noises, random, caches.clone());
        let climate = roots[..w.climate_dfs.len()].iter().map(|df| comp.compile_rewritten(df)).collect();
        Seeded { w, seed, comp, climate, terrain: None }
    }

    /// The compiled climate samplers (temperature, humidity, continentalness, erosion, depth,
    /// weirdness) and the compiler holding their noises: the GPU port walks them.
    pub fn climate_samplers(&self) -> (&[Arc<S>], &Compiler<'w>) {
        (&self.climate, &self.comp)
    }

    /// Whether this world forks its randoms with Xoroshiro (all 26.x overworlds) rather than JavaRandom.
    pub fn xoroshiro(&self) -> bool {
        !self.w.legacy_random
    }

    /// Raw climate parameters at a block position (point path), as floats.
    pub fn climate_at(&self, ctx: &mut Ctx, x: i32, y: i32, z: i32) -> [f32; 6] {
        std::array::from_fn(|i| self.climate[i].sample(ctx, x, y, z))
    }

    /// BiomeResolver from createResolver (point path): structure checks and findBiomeHorizontal.
    pub fn biome(&self, bc: &mut BiomeCtx, qx: i32, qy: i32, qz: i32) -> u16 {
        let c = self.climate_at(&mut bc.ctx, qx << 2, qy << 2, qz << 2);
        let t = [quantize(c[0]), quantize(c[1]), quantize(c[2]), quantize(c[3]), quantize(c[4]), quantize(c[5]), 0];
        self.w.climate.search(&t, &mut bc.last)
    }

    /// createResolverForChunk (volume path): what chunks actually store. Index order is the
    /// volume's (y fastest, then x, then z).
    #[allow(clippy::too_many_arguments)]
    pub fn biomes_volume(&self, bc: &mut BiomeCtx, qx: i32, qy: i32, qz: i32, sx: i32, sy: i32, sz: i32) -> Vec<u16> {
        let targets = self.climate_volume(bc, qx, qy, qz, sx, sy, sz);
        targets.iter().map(|t| self.w.climate.search(t, &mut bc.last)).collect()
    }

    /// The climate targets of a volume path region (what createResolverForChunk samples eagerly).
    #[allow(clippy::too_many_arguments)]
    fn climate_volume(&self, bc: &mut BiomeCtx, qx: i32, qy: i32, qz: i32, sx: i32, sy: i32, sz: i32) -> Vec<[i64; 7]> {
        let vol = Volume::stepped(sx, sy, sz, qx << 2, qy << 2, qz << 2, 4, 4, 4);
        let bufs: Vec<Vec<f32>> = self
            .climate
            .iter()
            .map(|s| {
                let mut b = vec![0.0; vol.size()];
                s.sample_volume(&mut bc.ctx, &mut b, &vol);
                b
            })
            .collect();
        (0..vol.size()).map(|i| std::array::from_fn(|k| if k < 6 { quantize(bufs[k][i]) } else { 0 })).collect()
    }

    /// NoiseSpawnFinder.findSpawnPosition (block x, z; y is 0): the climate point closest to the
    /// spawn targets, biased towards the origin. None when the settings have no spawn target.
    pub fn spawn_origin(&self) -> Option<(i32, i32)> {
        if self.w.spawn_target.is_empty() {
            return None;
        }
        // getOrigin: one sampler context with caches for the whole search
        let mut ctx = Ctx::cached();
        let mut best = self.spawn_fitness(&mut ctx, 0, 0);
        for ring in spawn_rings() {
            let origin = best.0;
            for &(dx, dz) in ring {
                let c = self.spawn_fitness(&mut ctx, origin.0 + dx, origin.1 + dz);
                if c.1 < best.1 {
                    best = c;
                }
            }
        }
        Some(best.0)
    }

    /// NoiseSpawnFinder.getSpawnPositionAndFitness
    fn spawn_fitness(&self, ctx: &mut Ctx, x: i32, z: i32) -> ((i32, i32), i64) {
        let (qx, qz) = ((x >> 2) << 2, (z >> 2) << 2);
        let mut min = i64::MAX;
        for point in &self.w.spawn_target {
            let mut f = 0i64;
            for &(i, lo, hi) in point {
                let v = quantize(self.climate[i].sample(ctx, qx, 0, qz));
                let d = if v - hi > 0 { v - hi } else { (lo - v).max(0) };
                f += d * d;
            }
            min = min.min(f);
        }
        let bias = (x as i64) * (x as i64) + (z as i64) * (z as i64);
        ((x, z), min * 2048 * 2048 + bias)
    }

    /// ChunkGenerator.getOrigin: the chunk setInitialSpawn starts from ((0, 0) without targets).
    pub fn spawn_chunk(&self) -> (i32, i32) {
        self.spawn_origin().map_or((0, 0), |(x, z)| (x >> 4, z >> 4))
    }

    /// The biome a chunk stores at a quart (ChunkAccess.getNoiseBiome: y clamped to the world).
    /// Each chunk's 4x96x4 climate volume is sampled once with its own cached context; the R-tree
    /// search runs per lookup, as in createResolverForChunk.
    pub fn chunk_biome(&self, bc: &mut BiomeCtx, qx: i32, qy: i32, qz: i32) -> u16 {
        let (cx, cz) = (qx >> 2, qz >> 2);
        let t = bc.chunks.entry((cx, cz)).or_insert_with(|| self.climate_volume(&mut BiomeCtx::cached(), cx * 4, -16, cz * 4, 4, 96, 4));
        // volume order: y fastest, then x, then z
        let t = t[(((qz & 3) * 4 + (qx & 3)) * 96 + (qy.clamp(-16, 79) + 16)) as usize];
        // ponytail: the R-tree warm start is thread-wide in the game, so where two biomes tie (rare)
        // the stored biome depends on generation order; we follow this context's lookup order.
        self.w.climate.search(&t, &mut bc.last)
    }

    /// BiomeManager.getBiome: the block-level (F3) biome, zoomed from the stored chunk biomes with
    /// the obfuscated seed.
    pub fn block_biome(&self, bc: &mut BiomeCtx, x: i32, y: i32, z: i32) -> u16 {
        let zoom = obfuscate_seed(self.seed);
        let (ax, ay, az) = (x - 2, y - 2, z - 2);
        let (px, py, pz) = (ax >> 2, ay >> 2, az >> 2);
        let (fx, fy, fz) = ((ax & 3) as f64 / 4.0, (ay & 3) as f64 / 4.0, (az & 3) as f64 / 4.0);
        let mut min_i = 0;
        let mut min_d = f64::INFINITY;
        for i in 0..8 {
            let (xe, ye, ze) = (i & 4 == 0, i & 2 == 0, i & 1 == 0);
            let (cx, cy, cz) = (if xe { px } else { px + 1 }, if ye { py } else { py + 1 }, if ze { pz } else { pz + 1 });
            let (dx, dy, dz) = (if xe { fx } else { fx - 1.0 }, if ye { fy } else { fy - 1.0 }, if ze { fz } else { fz - 1.0 });
            let d = fiddled_distance(zoom, cx, cy, cz, dx, dy, dz);
            if min_d > d {
                min_i = i;
                min_d = d;
            }
        }
        let bx = if min_i & 4 == 0 { px } else { px + 1 };
        let by = if min_i & 2 == 0 { py } else { py + 1 };
        let bz = if min_i & 1 == 0 { pz } else { pz + 1 };
        self.chunk_biome(bc, bx, by, bz)
    }

    /// Compiles the terrain samplers (final density + aquifer) once; afterwards height queries
    /// work through `&self`, so one Seeded can be shared by worker threads.
    pub fn prepare_terrain(&mut self) {
        if self.terrain.is_none() {
            let (roots, _) = self.w.rewritten();
            let k = self.w.climate_dfs.len();
            let final_density = self.comp.compile_rewritten(&roots[k]);
            let aquifer = self.w.aquifer_dfs.as_ref().map(|_| roots[k + 1..].iter().map(|d| self.comp.compile_rewritten(d)).collect());
            let aquifer_random = self.comp.random.from_hash("minecraft:aquifer").fork_positional();
            self.terrain = Some(Terrain { final_density, aquifer, aquifer_random });
        }
    }

    /// NoiseBasedChunkGenerator.iterateNoiseColumn: walks the column top-down through `visit`
    /// until it returns true (y + 1 is then the heightmap value).
    fn iterate_column(&self, x: i32, z: i32, mut visit: impl FnMut(i32, Block) -> bool) -> Option<i32> {
        let (min_y, height, sea) = (self.w.min_y, self.w.height, self.w.sea_level);
        let t = self.terrain.as_ref().expect("prepare_terrain() first");
        let vol = Volume::new(1, height, 1, x, min_y, z);
        let mut ctx = Ctx::cached();
        let mut aq = t.aquifer.as_ref().map(|s| Aquifer::new(s, t.aquifer_random, &vol, sea, &mut ctx));
        let mut buf = vec![0.0f32; vol.size()];
        t.final_density.sample_volume(&mut ctx, &mut buf, &vol);
        for iy in (0..vol.sy).rev() {
            let y = vol.by(iy);
            let d = buf[iy as usize] as f64;
            let b = match aq.as_mut() {
                Some(a) => a.substance(&mut ctx, x, y, z, d),
                None => if d > 0.0 { Block::Stone } else { global_fluid(sea, y).at(y) },
            };
            if visit(y, b) {
                return Some(y + 1);
            }
        }
        None
    }

    /// NoiseBasedChunkGenerator.doFill's substances for a chunk (no structures, so no beardifier):
    /// index ((y - min_y) * 16 + z) * 16 + x. Stone stands for the default block.
    pub fn noise_chunk(&self, cx: i32, cz: i32) -> Vec<Block> {
        let (min_y, height, sea) = (self.w.min_y, self.w.height, self.w.sea_level);
        let t = self.terrain.as_ref().expect("prepare_terrain() first");
        let vol = Volume::new(16, height, 16, cx * 16, min_y, cz * 16);
        let mut ctx = Ctx::cached();
        let mut aq = t.aquifer.as_ref().map(|s| Aquifer::new(s, t.aquifer_random, &vol, sea, &mut ctx));
        let mut buf = vec![0.0f32; vol.size()];
        t.final_density.sample_volume(&mut ctx, &mut buf, &vol);
        let mut out = vec![Block::Air; vol.size()];
        // doFill's order: z, x, then y from the top
        for iz in 0..16 {
            for ix in 0..16 {
                let (x, z) = (vol.bx(ix), vol.bz(iz));
                for iy in (0..vol.sy).rev() {
                    let y = vol.by(iy);
                    let d = buf[vol.idx(ix, iy, iz)] as f64;
                    out[((iy * 16 + iz) * 16 + ix) as usize] = match aq.as_mut() {
                        Some(a) => a.substance(&mut ctx, x, y, z, d),
                        None => if d > 0.0 { Block::Stone } else { global_fluid(sea, y).at(y) },
                    };
                }
            }
        }
        out
    }

    /// A named density function compiled for this seed (cachingSamplers().get(function)).
    pub fn sampler(&mut self, name: &str) -> Arc<S> {
        self.comp.compile_ref(name)
    }

    /// A density function compiled for this seed.
    pub fn sampler_of(&mut self, df: &Df) -> Arc<S> {
        self.comp.compile_root(df)
    }

    /// RandomState.getOrCreateNoise
    pub fn noise(&mut self, name: &str) -> Arc<crate::noise::NoiseStack> {
        self.comp.noise(name)
    }

    /// RandomState's positional random (Xoroshiro(seed).forkPositional()).
    pub fn random(&self) -> Positional {
        self.comp.random
    }

    /// ChunkGenerator.getBaseHeight (getFirstFreeHeight; getFirstOccupiedHeight is this - 1).
    pub fn base_height(&mut self, x: i32, z: i32, hm: Heightmap) -> i32 {
        self.prepare_terrain();
        self.height_at(x, z, hm)
    }

    /// base_height for a prepared Seeded (see `prepare_terrain`).
    pub fn height_at(&self, x: i32, z: i32, hm: Heightmap) -> i32 {
        self.iterate_column(x, z, |_, b| hm.test(b)).unwrap_or(self.w.min_y)
    }

    /// getBaseColumn, bottom-up (index 0 = min_y).
    pub fn column(&mut self, x: i32, z: i32) -> Vec<Block> {
        self.prepare_terrain();
        let mut col = vec![];
        self.iterate_column(x, z, |_, b| {
            col.push(b);
            false
        });
        col.reverse();
        col
    }
}

// ------------------------------------------------------------------------------------------------
// Aquifer.NoiseBasedAquifer (substance only; fluid-tick scheduling flags are not needed here)
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
struct Fluid {
    level: i32,
    ty: Block,
}

impl Fluid {
    fn at(&self, y: i32) -> Block {
        if y < self.level { self.ty } else { Block::Air }
    }
}

const WAY_BELOW_MIN_Y: i32 = -32512;
const SURFACE_OFFSETS: [(i32, i32); 13] =
    [(0, 0), (-2, -1), (-1, -1), (0, -1), (1, -1), (-3, 0), (-2, 0), (-1, 0), (1, 0), (-2, 1), (-1, 1), (0, 1), (1, 1)];

fn global_fluid(sea: i32, y: i32) -> Fluid {
    if y < (-54).min(sea) { Fluid { level: -54, ty: Block::Lava } } else { Fluid { level: sea, ty: Block::Water } }
}

fn grid_xz(b: i32) -> i32 { b >> 4 }
fn grid_y(b: i32) -> i32 { b.div_euclid(12) }
fn from_grid_xz(g: i32, off: i32) -> i32 { (g << 4) + off }
fn from_grid_y(g: i32, off: i32) -> i32 { g * 12 + off }
fn similarity(d1: i32, d2: i32) -> f64 { 1.0 - (d2 - d1) as f64 / 25.0 }
fn lerp(a: f64, p0: f64, p1: f64) -> f64 { p0 + a * (p1 - p0) }
fn inverse_lerp(v: f64, a: f64, b: f64) -> f64 { (v - a) / (b - a) }
fn clamped_map(v: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    let f = inverse_lerp(v, a, b);
    if f < 0.0 { c } else if f > 1.0 { d } else { lerp(f, c, d) }
}
/// Mth.clamp(double): value < min ? min : Math.min(value, max)
fn clamp_java(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo { lo } else if v.is_nan() { v } else { v.min(hi) }
}

struct Aquifer<'a> {
    s: &'a [Arc<S>], // barrier, floodedness, spread, lava, exclusion, surface_level
    random: Positional,
    sea: i32,
    status: Vec<Option<Fluid>>,
    location: Vec<Option<(i32, i32, i32)>>,
    surface: HashMap<(i32, i32), i32>,
    skip_above: i32,
    min_g: (i32, i32, i32),
    size_x: i32,
    size_z: i32,
}

impl<'a> Aquifer<'a> {
    fn new(s: &'a [Arc<S>], random: Positional, vol: &Volume, sea: i32, ctx: &mut Ctx) -> Self {
        let min_gx = grid_xz(vol.x0 - 5);
        let max_gx = grid_xz(vol.max_x() - 5) + 1;
        let min_gy = grid_y(vol.y0 + 1) - 1;
        let max_gy = grid_y(vol.max_y() + 1) + 1;
        let min_gz = grid_xz(vol.z0 - 5);
        let max_gz = grid_xz(vol.max_z() - 5) + 1;
        let (size_x, size_y, size_z) = (max_gx - min_gx + 1, max_gy - min_gy + 1, max_gz - min_gz + 1);
        let total = (size_x * size_y * size_z) as usize;
        let mut a = Aquifer {
            s, random, sea,
            status: vec![None; total],
            location: vec![None; total],
            surface: HashMap::new(),
            skip_above: 0,
            min_g: (min_gx, min_gy, min_gz),
            size_x, size_z,
        };
        let max_surface = a.max_surface(ctx, from_grid_xz(min_gx, 0), from_grid_xz(min_gz, 0), from_grid_xz(max_gx, 9), from_grid_xz(max_gz, 9)) + 8;
        a.skip_above = from_grid_y(grid_y(max_surface + 12) + 1, 11) - 1;
        a
    }

    fn surface_level(&mut self, ctx: &mut Ctx, x: i32, z: i32) -> i32 {
        let (qx, qz) = ((x >> 2) << 2, (z >> 2) << 2);
        if let Some(&v) = self.surface.get(&(qx, qz)) {
            return v;
        }
        let v = self.s[5].sample(ctx, qx, 0, qz).floor() as i32;
        self.surface.insert((qx, qz), v);
        v
    }

    fn max_surface(&mut self, ctx: &mut Ctx, x0: i32, z0: i32, x1: i32, z1: i32) -> i32 {
        let (qx0, qx1, qz0, qz1) = (x0 >> 2, x1 >> 2, z0 >> 2, z1 >> 2);
        let vol = Volume::stepped(qx1 - qx0 + 1, 1, qz1 - qz0 + 1, qx0 << 2, 0, qz0 << 2, 4, 1, 4);
        let mut buf = vec![0.0; vol.size()];
        self.s[5].sample_volume(ctx, &mut buf, &vol);
        let mut max = i32::MIN;
        for iz in 0..vol.sz {
            for ix in 0..vol.sx {
                let v = buf[vol.idx(ix, 0, iz)].floor() as i32;
                self.surface.insert((vol.bx(ix), vol.bz(iz)), v);
                max = max.max(v);
            }
        }
        max
    }

    fn index(&self, gx: i32, gy: i32, gz: i32) -> usize {
        (((gy - self.min_g.1) * self.size_z + (gz - self.min_g.2)) * self.size_x + (gx - self.min_g.0)) as usize
    }

    fn status_at(&mut self, ctx: &mut Ctx, i: usize) -> Fluid {
        if let Some(f) = self.status[i] {
            return f;
        }
        let (x, y, z) = self.location[i].unwrap();
        let f = self.compute_fluid(ctx, x, y, z);
        self.status[i] = Some(f);
        f
    }

    /// computeSubstance: Stone = "null" (default block), otherwise the fluid/air state.
    fn substance(&mut self, ctx: &mut Ctx, x: i32, y: i32, z: i32, density: f64) -> Block {
        use crate::rng::Rng;
        if density > 0.0 {
            return Block::Stone;
        }
        let global = global_fluid(self.sea, y);
        if y > self.skip_above {
            return global.at(y);
        }
        if global.at(y) == Block::Lava {
            return Block::Lava;
        }
        let (xa, ya, za) = (grid_xz(x - 5), grid_y(y + 1), grid_xz(z - 5));
        let mut d = [i32::MAX; 4];
        let mut idx = [0usize; 4];
        for x1 in 0..=1 {
            for y1 in -1..=1 {
                for z1 in 0..=1 {
                    let (gx, gy, gz) = (xa + x1, ya + y1, za + z1);
                    let i = self.index(gx, gy, gz);
                    let loc = match self.location[i] {
                        Some(l) => l,
                        None => {
                            let mut r = self.random.at(gx, gy, gz);
                            let lx = from_grid_xz(gx, r.next_bound(10));
                            let ly = from_grid_y(gy, r.next_bound(9));
                            let l = (lx, ly, from_grid_xz(gz, r.next_bound(10)));
                            self.location[i] = Some(l);
                            l
                        }
                    };
                    let (dx, dy, dz) = (loc.0 - x, loc.1 - y, loc.2 - z);
                    let nd = dx * dx + dy * dy + dz * dz;
                    if d[0] >= nd {
                        d = [nd, d[0], d[1], d[2]];
                        idx = [i, idx[0], idx[1], idx[2]];
                    } else if d[1] >= nd {
                        d = [d[0], nd, d[1], d[2]];
                        idx = [idx[0], i, idx[1], idx[2]];
                    } else if d[2] >= nd {
                        d = [d[0], d[1], nd, d[2]];
                        idx = [idx[0], idx[1], i, idx[2]];
                    } else if d[3] >= nd {
                        d[3] = nd;
                        idx[3] = i;
                    }
                }
            }
        }
        let s1 = self.status_at(ctx, idx[0]);
        let sim12 = similarity(d[0], d[1]);
        let fluid = s1.at(y);
        if sim12 <= 0.0 {
            return fluid;
        }
        if fluid == Block::Water && global_fluid(self.sea, y - 1).at(y - 1) == Block::Lava {
            return fluid;
        }
        let mut barrier = f64::NAN;
        let s2 = self.status_at(ctx, idx[1]);
        if density + sim12 * self.pressure(ctx, x, y, z, &mut barrier, s1, s2) > 0.0 {
            return Block::Stone;
        }
        let s3 = self.status_at(ctx, idx[2]);
        let sim13 = similarity(d[0], d[2]);
        if sim13 > 0.0 && density + sim12 * sim13 * self.pressure(ctx, x, y, z, &mut barrier, s1, s3) > 0.0 {
            return Block::Stone;
        }
        let sim23 = similarity(d[1], d[2]);
        if sim23 > 0.0 && density + sim12 * sim23 * self.pressure(ctx, x, y, z, &mut barrier, s2, s3) > 0.0 {
            return Block::Stone;
        }
        fluid
    }

    #[allow(clippy::too_many_arguments)]
    fn pressure(&self, ctx: &mut Ctx, x: i32, y: i32, z: i32, barrier: &mut f64, a: Fluid, b: Fluid) -> f64 {
        let (ta, tb) = (a.at(y), b.at(y));
        if (ta == Block::Lava && tb == Block::Water) || (ta == Block::Water && tb == Block::Lava) {
            return 2.0;
        }
        let diff = (a.level - b.level).abs();
        if diff == 0 {
            return 0.0;
        }
        let avg = 0.5 * (a.level + b.level) as f64;
        let above = y as f64 + 0.5 - avg;
        let edge = diff as f64 / 2.0 - above.abs();
        let gradient = if above > 0.0 {
            let c = 0.0 + edge;
            if c > 0.0 { c / 1.5 } else { c / 2.5 }
        } else {
            let c = 3.0 + edge;
            if c > 0.0 { c / 3.0 } else { c / 10.0 }
        };
        let noise = if !(gradient < -2.0) && !(gradient > 2.0) {
            if barrier.is_nan() {
                *barrier = self.s[0].sample(ctx, x, y, z) as f64;
            }
            *barrier
        } else {
            0.0
        };
        2.0 * (noise + gradient)
    }

    fn compute_fluid(&mut self, ctx: &mut Ctx, x: i32, y: i32, z: i32) -> Fluid {
        let global = global_fluid(self.sea, y);
        let mut lowest = i32::MAX;
        let (top, bottom) = (y + 12, y - 12);
        let mut center_under = false;
        for (ox, oz) in SURFACE_OFFSETS {
            let (sx, sz) = (x + (ox << 4), z + (oz << 4));
            let level = self.surface_level(ctx, sx, sz);
            let adjusted = level + 8;
            let start = ox == 0 && oz == 0;
            if start && bottom > adjusted {
                return global;
            }
            let pokes = top > adjusted;
            if pokes || start {
                let at_surface = global_fluid(self.sea, adjusted);
                if at_surface.at(adjusted) != Block::Air {
                    if start {
                        center_under = true;
                    }
                    if pokes {
                        return at_surface;
                    }
                }
            }
            lowest = lowest.min(level);
        }
        let level = self.fluid_surface_level(ctx, x, y, z, global, lowest, center_under);
        Fluid { level, ty: self.fluid_type(ctx, x, y, z, global, level) }
    }

    #[allow(clippy::too_many_arguments)]
    fn fluid_surface_level(&mut self, ctx: &mut Ctx, x: i32, y: i32, z: i32, global: Fluid, lowest: i32, center_under: bool) -> i32 {
        let (partial, full) = if self.s[4].sample(ctx, x, y, z) as f64 > 0.0 {
            (-1.0, -1.0)
        } else {
            let below = lowest + 8 - y;
            let factor = if center_under { clamped_map(below as f64, 0.0, 64.0, 1.0, 0.0) } else { 0.0 };
            let noise = clamp_java(self.s[1].sample(ctx, x, y, z) as f64, -1.0, 1.0);
            let full_t = lerp(inverse_lerp(factor, 1.0, 0.0), -0.3, 0.8);
            let part_t = lerp(inverse_lerp(factor, 1.0, 0.0), -0.8, 0.4);
            (noise - part_t, noise - full_t)
        };
        if full > 0.0 {
            global.level
        } else if partial > 0.0 {
            let (cx, cy, cz) = (x.div_euclid(16), y.div_euclid(40), z.div_euclid(16));
            let spread = (self.s[2].sample(ctx, cx, cy, cz) * 10.0f32) as f64;
            let q = (spread / 3.0).floor() as i32 * 3;
            lowest.min(cy * 40 + 20 + q)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fluid_type(&mut self, ctx: &mut Ctx, x: i32, y: i32, z: i32, global: Fluid, level: i32) -> Block {
        if level <= -10 && level != WAY_BELOW_MIN_Y && global.ty != Block::Lava {
            let lava = self.s[3].sample(ctx, x.div_euclid(64), y.div_euclid(40), z.div_euclid(64)) as f64;
            if lava.abs() > 0.3 {
                return Block::Lava;
            }
        }
        global.ty
    }
}

/// BiomeManager.getFiddledDistance
fn fiddled_distance(seed: i64, x: i32, y: i32, z: i32, dx: f64, dy: f64, dz: f64) -> f64 {
    let mut r = seed;
    for c in [x, y, z, x, y, z] {
        r = lcg_next(r, c as i64);
    }
    let fx = fiddle(r);
    r = lcg_next(r, seed);
    let fy = fiddle(r);
    r = lcg_next(r, seed);
    let fz = fiddle(r);
    (dz + fz) * (dz + fz) + (dy + fy) * (dy + fy) + (dx + fx) * (dx + fx)
}

/// BiomeManager.getFiddle
fn fiddle(r: i64) -> f64 {
    ((r >> 24).rem_euclid(1024) as f64 / 1024.0 - 0.5) * 0.9
}

/// NoiseSpawnFinder.radialSearch's candidates as offsets from the search origin, for its two calls
/// (2048 by 512, then 512 by 32). They don't depend on the seed (the GPU gets the same list).
pub fn spawn_rings() -> &'static [Vec<(i32, i32)>; 2] {
    static RINGS: std::sync::OnceLock<[Vec<(i32, i32)>; 2]> = std::sync::OnceLock::new();
    RINGS.get_or_init(|| {
        [(2048.0f32, 512.0f32), (512.0, 32.0)].map(|(max_radius, inc)| {
            let (mut angle, mut radius, mut v) = (0.0f32, inc, vec![]);
            while radius <= max_radius {
                // (int)(Math.sin(angle) * radius): float angle and radius widened to double
                v.push((((angle as f64).sin() * radius as f64) as i32, ((angle as f64).cos() * radius as f64) as i32));
                angle += inc / radius;
                if angle as f64 > std::f64::consts::PI * 2.0 {
                    angle = 0.0;
                    radius += inc;
                }
            }
            v
        })
    })
}
