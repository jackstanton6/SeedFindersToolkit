# Query language

A query is a Python-style boolean expression. It is parsed (never executed) into the condition tree
the engine evaluates. The GUI tree editor and the text box are two views of the same query.

```python
village(within=300) and (monument(within=800) or biome("mushroom_fields", within=500)) and not mansion(within=2000)
```

- Combine with `and`, `or`, `not` and parentheses.
- Arguments must be literals: numbers, strings, lists, tuples, `True`/`False`.
- Coordinates are block coordinates; `at=(x, z)` defaults to `(0, 0)`.
- Any call that is not one of the condition names below is shorthand for `structure(name, ...)`:
  `village(within=300)` is `structure("village", within=300)`.

## Conditions

### structure(name, within=1000, at=(0,0), min=1, max=None)

At least `min` (and at most `max`) structures whose locate position is within `within` blocks of
`at` actually generate. "Generates" means placement, the set's weighted variant pick and the
start's biome/terrain check all pass, exactly as in the game.

How `name` resolves:
- a bare name with a plural structure set means **any variant** of that set: `village`, `ruined_portal`,
  `shipwreck`, `mineshaft`, `stronghold`, `pillager_outpost`, `desert_pyramid`, `jungle_temple`,
  `igloo`, `swamp_hut`, `ocean_ruin`, `buried_treasure`, `trial_chamber`;
- otherwise a structure: `village_plains`, `ruined_portal_desert`, `monument`, `mansion`,
  `ancient_city`, `trail_ruins`, `jungle_pyramid`, `abandoned_camp_cherry_grove`;
- otherwise a set name: `abandoned_camp`, `ocean_monuments`, `woodland_mansions`;
- `minecraft:` forces the exact structure: `"minecraft:ruined_portal"` is only the standard
  (non-desert, non-jungle, ...) ruined portal.

The GUI's reference list shows every structure and its set.

Examples: `swamp_hut(within=300, min=2)`, `structure("village_snowy", within=500)`,
`ruined_portal(within=100, max=0)` (no ruined portal near spawn).

### near(a, b, dist=128, within=1000, at=(0,0))

Some generated `a` within `within` of `at` has a generated `b` (a different start) within `dist`
blocks of it, measured between start positions.

```python
near("village", "pillager_outpost", dist=150, within=800)
near("desert_pyramid", "village_desert", dist=200)
near("monument", "monument", dist=600, within=3000)       # two monuments close together
```

### biome(name, within=0, at=(0,0), y=64, step=16, fraction=0)

Biome samples on a `step`-block grid inside the disc of radius `within` around `at`, at height `y`.
With `fraction=0`, one matching sample is enough. With `fraction=0.3`, at least 30% of the samples
must match.

`name` is a biome, a list of biomes, or a tag with `#`: `"#is_ocean"`, `"#is_jungle"`,
`"#is_badlands"`, `"#has_structure/village_plains"`, and so on (the vanilla biome tags).

Biomes are 3D. Cave biomes (lush caves, dripstone caves, deep dark, sulfur caves) need a low `y`:

```python
biome("lush_caves", within=200, y=-20)
biome("cherry_grove", within=300, fraction=0.25)
biome(["jagged_peaks", "frozen_peaks", "stony_peaks"], within=500)
```

Cost grows with (within / step)². Use a bigger `step` for large areas.

### climate(param, at=(0,0), y=64, min=-inf, max=inf)

A raw climate parameter at a block position: `temperature`, `humidity`, `continentalness`,
`erosion`, `depth` or `weirdness`. Useful for things biome names can't express: very inland spawns
(`continentalness` > 0.8), flat land (`erosion` > 0.6), peaks (`weirdness`).

### height(at=(0,0), min, max, heightmap="surface")

Terrain height from the game's `getBaseHeight`. `heightmap="surface"` (`WORLD_SURFACE_WG`, water
counts as surface) or `"ocean_floor"` (`OCEAN_FLOOR_WG`, solid blocks only). Exact including
aquifers. Each call costs about 0.5 ms, so put it after cheap conditions.

### native(plugin, filter, **params)

A filter exported by a native (Rust/C) plugin. It runs inside the engine's worker threads at full
speed. Example: `native("example_rust", "spawn_island", radius=96)`. See [Plugins](Plugins.md).

### py(name, **params)

A Python plugin condition, `"plugin.condition"`. These run in Python on the engine's matches, so
they must be top-level `and` terms. Example: `py("example_python.seed_contains", text="777")`.

## Making queries fast

1. Structures first. Their placement pre-check runs on 48 bits before any biome work, and in
   structure-seed mode it prunes 65,536 seeds at a time.
2. Small radii. The candidate count grows with radius².
3. `near` with a small `within` on the rarer structure as `a`.
4. Large-radius `biome` and `height` last; `and` stops at the first failing term.
5. Use the estimate. A projected match rate below ~1e-6 is usually a job for structure-seed mode.
