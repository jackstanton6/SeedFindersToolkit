# AGENTS.md: working on Seed Finders Toolkit

Guide for AI agents and contributors. Read this before changing anything under `engine/`.

## What this is

A Minecraft Java seed finder. `engine/` is a Rust port of the game's world generation (bit-exact,
verified against the real game). `sft/` is the Python package: query DSL, plugin loader, CLI and
PySide6 GUI. Scope today: Java 26.3, overworld.

## Layout

```
engine/src/
  rng.rs        Java LCG, Xoroshiro128++, positional factories, MD5 seeding
  noise.rs      26.x float Perlin / smeared Perlin, NoiseStack, NormalNoise, BlendedNoise fbm
  density.rs    density functions: JSON -> rewrite (inline, cache dedupe, axis slicing) -> samplers;
                both evaluation paths (sample = sampleValue, sample_volume = sampleVolume); Interval
  biome.rs      Climate parameter list + 19-ary R-tree, biome registry, tags
  world.rs      World (data/<ver>), Seeded (= RandomState): biomes, getBaseHeight, columns, aquifer
  structure.rs  structure sets, placement, findGenerationPoint per type, jigsaw starts, NBT templates
  search.rs     condition tree, 48-bit pre-check + full eval, native plugin ABI, threaded search
  lib.rs        PyO3 bindings (feature "python") -> sft._engine
engine/tests/oracle.rs   parity tests against the real game (ignored by default)
oracle/Oracle.java       ground-truth dumper that runs the actual 26.3 server classes
sft/            __init__ (World cache, Job, export), query, plugins, data, __main__ (CLI), gui/
plugins/        example_rust (native cdylib), example_python
docs/wiki/      user documentation
```

## Commands

```sh
pip install -e .                                   # build engine + install (maturin, release)
python -m sft data 26.3                            # one-time: data/26.3 + cache/datagen-26.3
cd engine && cargo test                            # unit tests
python tests/run_oracle.py 26.3                    # regenerate ground truth (needs Java)
python tests/run_oracle.py 26.3 structures
cd engine && cargo test --release -- --ignored     # parity suite: must stay at zero mismatches
python tests/test_query.py                         # DSL tests
python -m sft                                      # GUI
```

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
- New query conditions need the Rust `Cond` variant + `parse` + `pre` (if a 48-bit pre-check exists)
  + `eval`, a DSL entry in `sft/query.py` (`CALLS`/`POSITIONAL`), a GUI template in `app.py`
  `TEMPLATES`, and a section in `docs/wiki/Query-Language.md`.
- The native plugin ABI (`SftHost`, `ABI_VERSION`) is a public contract. Append fields only, and
  bump `ABI_VERSION` on any change.
- Mojang code and data never go into git: `client-refcode/`, `decompiled/`, `data/`, `cache/` and
  `tools/` are ignored.

## Where things stand (milestones)

- **M1 (done):** RNG/noise/density, biomes (both paths), heights + aquifer, all overworld structure
  placement and start validity (oracle-verified), query DSL, threaded search with structure-seed
  mode, PyO3 bindings, GUI, plugins, docs.
- **M2:** spawn point (`NoiseSpawnFinder`), surface blocks (material rules), block-level biomes
  (`BiomeManager` zoom).
- **M3:** structure pieces (mineshaft depth exact, village layouts, loot), features and decoration
  (trees, sugar cane, cacti, ores, geodes).
- **M4:** carvers and full block-level generation; optional CUDA brute-force stage; Nether/End.
- **Versions:** see docs/wiki/Versions.md.

## Decompiled reference

`decompiled/` (gitignored) holds Vineflower output for the relevant 26.3 packages, from
`client-refcode/`. Regenerate with:
`java -jar tools/vineflower.jar -s --folder "--included-classes=net\.minecraft\.(world\.level|util|core|data\.worldgen|data\.registries|world\.phys|tags|resources|server\.level|SharedConstants).*" client-refcode decompiled`
