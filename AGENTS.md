# AGENTS.md: working on Seed Finders Toolkit

Guide for AI agents and contributors. Read this before changing anything under `engine/`.

## What this is

A Minecraft Java seed finder. `engine/` is a Rust port of the game's world generation (bit-exact,
verified against the real game). `sft/` is the Python package: query DSL, plugin loader, CLI and
PySide6 GUI. Scope today: Java 26.3, overworld.

## Layout

```
engine/src/
  rng.rs        Java LCG, Xoroshiro128++, WorldgenXoro (decoration random), positional factories, MD5 seeding
  noise.rs      26.x float Perlin / smeared Perlin, NoiseStack, NormalNoise, BlendedNoise fbm
  density.rs    density functions: JSON -> rewrite (inline, cache dedupe, axis slicing) -> samplers;
                both evaluation paths (sample = sampleValue, sample_volume = sampleVolume); Interval
  biome.rs      Climate parameter list + 19-ary R-tree, biome registry, tags
  world.rs      World (data/<ver>), Seeded (= RandomState): biomes, getBaseHeight, columns, aquifer
  structure.rs  structure sets, placement, findGenerationPoint per type, jigsaw starts, NBT templates,
                piece details and chest loot seeds
  blocks.rs     block states with the game's ids (reports/blocks.json), properties, block tags
  chunk.rs      full chunks: doFill (noise + aquifer), MaterialSystem.buildSurface (material rules,
                bands, extensions, ore veins, bedrock), heightmaps, per-seed chunk cache, bedrock_floor
  carver.rs     cave and canyon carvers, the carving mask, applyCarvingMask
  query.rs      conditions: numeric metrics, expressions, objectives, anywhere; 48-bit pre-check +
                full eval (Eval); single-seed scan
  search.rs     threaded search runner (modes, stats, ranked top-N), native plugin ABI
  crack.rs      structure-seed cracking (low-bit lifting + brute force)
  gpu.rs        CUDA driver API (loaded at runtime), query -> GPU program lowering, filter/crack launches,
                the climate program (compiled climate samplers, noise layers, R-tree) for the device
  gpu.cu        CUDA kernels: literal ports of JavaRandom, grid_chunk, frequency_ok, potential, slime,
                Xoroshiro, Perlin/NoiseStack, the point-path samplers and RTree.search; stage 1 is a
                thread per seed (placement), stage 2 a block per survivor (biomes, Perlin tables in
                shared memory)
engine/build.rs compiles gpu.cu to PTX when nvcc exists (cfg sft_cuda); otherwise the GPU is unavailable
  loot.rs       LootTable.fill over the vanilla loot JSON
  lib.rs        PyO3 bindings (feature "python") -> sft._engine
engine/tests/oracle.rs   parity tests against the real game (ignored by default)
engine/tests/gpu.rs      GPU kernel vs CPU parity: pre-check, biomes, superset of matches (ignored; needs CUDA)
oracle/Oracle.java       ground-truth dumper that runs the actual 26.3 server classes
tests/loot_oracle.py     loot ground truth from the dedicated server (EULA) and its region files
tests/export_oracle.py   `export --mca` read back by the dedicated server
sft/            __init__ (World cache, Job, export), query, plugins, data, __main__ (CLI), worldfile
                (Anvil/.schem writers), gui/
sft/gui/        app (main window), blocks (block editor + palette), wizard ("I want..."), help (help
                window + tour), mapview, chunkview (chunk inspector)
plugins/        example_rust (native cdylib), example_python
sft/docs/       user documentation (shipped in the package: the help window shows it)
```

## Commands

```sh
pip install -e .                                   # build engine + install (maturin, release)
python -m sft data 26.3                            # one-time: data/26.3 + cache/datagen-26.3
cd engine && cargo test                            # unit tests
python tests/run_oracle.py 26.3                    # regenerate ground truth (needs Java)
python tests/run_oracle.py 26.3 structures slime
python tests/loot_oracle.py fill                   # dedicated server; loot tables
python tests/loot_oracle.py world 0 12345 35467 59104   # real worlds; chest seeds, portal obsidian
python tests/export_oracle.py 12345                # export --mca read back by the dedicated server
cd engine && cargo test --release -- --ignored     # parity suite: must stay at zero mismatches
python tests/test_query.py                         # DSL tests
python tests/test_engine.py                        # query errors and hints, match details, stop/resume, max results
python tests/test_gui.py                           # blocks <-> text, wizard, drag and drop (offscreen)
cd engine && cargo test --release --test gpu -- --ignored   # GPU parity (CUDA device)
python -m sft                                      # GUI
```

CI (`.github/workflows/ci.yml`, Windows + Linux) runs only what needs no Mojang data or GPU: `cargo
test`, the Python tests (their search parts skip without `data/26.3`) and a CLI start. The parity
suites stay local.

After any engine change, run the parity suite. A change that makes it fail is wrong, even if the
new behaviour "looks" better.

## Rules for the world-generation port

1. **Port literally.** Same operation order, same float vs double types, same integer division and
   shift semantics. Java `float` is Rust `f32`. Do not "simplify" arithmetic: `(a * b) * c` is not
   `a * (b * c)` in floating point. Rust never fuses multiply-add on its own; keep it that way (no
   `mul_add`).
2. **Keep both density paths.** The game evaluates density functions point-by-point
   (`sampleValue`) or over volumes (`sampleVolume`), and the two are not always bit-identical
   (interpolation, non-power-of-two noise scales). Callers must use the path the game uses:
   structure biome checks use the point path, chunk biomes and `couldStructureExistInColumn` use the
   volume path, and heights use the volume path.
3. **Caches are semantic.** `Ctx` reproduces `SamplerContext` because a cached volume can be returned
   for a later point query. Don't drop cache layers or reorder sampling in contexts the game shares.
4. **Java helpers have Java semantics.** Use `jmin`/`jmax` (NaN and -0 behaviour), `div_euclid`/
   `rem_euclid` for `floorDiv`/`floorMod`, `>>` on `i32` for arithmetic shifts, the Kahan `java_sum`
   for `DoubleStream.sum`, and `java_round` for `Math.round`.
5. **Floats from JSON are parsed from the literal text** (`f32_of`): the game uses
   `Float.parseFloat`, and serde_json is built with `arbitrary_precision` for this.
6. **Mark deliberate approximations** with a `ponytail:` comment naming the ceiling and the upgrade
   path, and surface them to users (e.g. `Structure::exact()` drives the "~approx" flags).

## Rules for everything else

- Prefer the stdlib and existing helpers. The DSL is parsed with `ast`, plugins use `tomllib`, and
  there is no numpy.
- New query conditions are metrics: a `Metric` variant in `query.rs` + its `parse` arm (with default
  min/max) + `shift` + `pre` (if a 48-bit pre-check exists) + `value`, a DSL entry in `sft/query.py`
  (`POSITIONAL`), a GUI template in `app.py` `TEMPLATES`, and a section in
  `sft/docs/Query-Language.md`.
- The native plugin ABI (`SftHost`, `ABI_VERSION`) is a public contract. Append fields only, and
  bump `ABI_VERSION` on any change.
- Mojang code and data never go into git: `client-refcode/`, `decompiled/`, `data/`, `cache/` and
  `tools/` are ignored.

## Where things stand (milestones)

- **M1 (done):** RNG/noise/density, biomes (both paths), heights + aquifer, all overworld structure
  placement and start validity (oracle-verified), query DSL, threaded search with structure-seed
  mode, PyO3 bindings, GUI, plugins, docs.
- **Tier 1 batch (done, see PLANS.md):** numeric condition core with scores and record hunts,
  clusters/chains/overlaps/bingo, biome-grid and island analysis, terrain scans, slime chunks,
  single-seed scan, cracking, placement stats, rarity, chest loot for seven structure types, ruined
  portal completeness, speedrun presets (all oracle-verified).
- **M2 (in progress, see PLANS.md "M2 batch"):** block-level biomes, spawn chunk (CPU + GPU), full
  TERRAIN-status chunks (noise, material rules, carvers; oracle-verified block for block), block
  conditions, bedrock cracking, export, map block layers. Left: structure pieces and the
  beardifier, decoration features, the exact spawn block.
- **M3:** structure pieces (mineshaft depth exact, village layouts, loot), features and decoration
  (trees, sugar cane, cacti, ores, geodes).
- **GPU biomes (done):** climate and biomes on the device, lowered `biome(...)` and structure start
  columns (whole seeds only), two-stage filter; 3.5-7x on biome-heavy queries.
- **GPU batch (done):** CUDA seed-only pre-filter and crack brute force, block query builder and
  wizard, in-app help and tour, jungle temple loot, search progress/estimate fixes.
- **M4:** carvers and full block-level generation; Nether/End.
- **Versions:** see sft/docs/Versions.md.

## Decompiled reference

`decompiled/` (gitignored) holds Vineflower output for the relevant 26.3 packages, from
`client-refcode/`. Regenerate with:
`java -jar tools/vineflower.jar -s --folder "--included-classes=net\.minecraft\.(world\.level|util|core|data\.worldgen|data\.registries|world\.phys|tags|resources|server\.level|SharedConstants).*" client-refcode decompiled`
