# Query language

A query is a Python-style expression. It is parsed (never executed) into the condition tree the
engine evaluates. The GUI tree editor and the text box are two views of the same query.

```python
village(within=300) and (monument(within=800) or biome("mushroom_fields", within=500)) and not mansion(within=2000)
nearest("stronghold") < 1400 and cluster("swamp_hut", count=3, dist=128, within=8000, afk=True)
maximize(island(biomes=["mushroom_fields"], within=3000))
```

- Combine with `and`, `or`, `not` and parentheses.
- Arguments must be literals: numbers, strings, lists, tuples, `True`/`False`.
- Coordinates are block coordinates; `at=(x, z)` defaults to `(0, 0)`.
- Any call that is not one of the condition names below is shorthand for `structure(name, ...)`:
  `village(within=300)` is `structure("village", within=300)`.

## Numbers, comparisons and scores

Every condition is also a number: a count, a distance, an area, a height. On its own it passes on
a default range (usually "at least 1"). Compare it to choose the range yourself:

```python
nearest("village") < 400                    # distance to the nearest village start
300 <= height(at=(0, 0)) <= 320             # chained comparisons work
village(within=500) + 2 * pillager_outpost(within=500) >= 3
biome_count(within=2000, step=32) >= 40
```

`min=` and `max=` arguments do the same thing (`village(within=500, min=2)`). Arithmetic (`+ - * /`,
unary `-`) combines numbers.

**Record hunts:** `maximize(expr)` and `minimize(expr)` rank seeds instead of filtering them. The
search keeps the best `max` seeds (100 by default) and the results table shows a score column,
updated live. Several objectives add up, minimize subtracts. Combine with filters:

```python
village(within=300) and minimize(nearest("stronghold", within=3000))
maximize(biome_area("mushroom_fields", window=1500))
maximize(slime(radius=128, within=500))
```

**Anywhere:** `anywhere(cond, within=1000, step=64, anchor=None)` asks whether `cond` holds with its
coordinates moved to some point within `within` of `at`, on a `step` grid or at the starts of the
`anchor` structure. The first place found is reported as an `anchor` detail.

```python
anywhere(slime(radius=24) >= 6, within=800, step=32)
anywhere(biome("cherry_grove", within=64, fraction=0.8), anchor="village", within=2000)
```

## Structures

### structure(name, within=1000, at=(0,0), min=1, max=inf, biome=None, tags=[])

The number of structures whose locate position is within `within` blocks of `at` and that actually
generate. "Generate" means placement, the set's weighted variant pick and the start's biome and
terrain check all pass, exactly as in the game.

How `name` resolves:
- a bare name with a plural structure set means **any variant** of that set: `village`, `ruined_portal`,
  `shipwreck`, `mineshaft`, `stronghold`, `pillager_outpost`, `desert_pyramid`, `jungle_temple`,
  `igloo`, `swamp_hut`, `ocean_ruin`, `buried_treasure`, `trial_chamber`;
- otherwise a structure: `village_plains`, `ruined_portal_desert`, `monument`, `mansion`,
  `ancient_city`, `trail_ruins`, `jungle_pyramid`, `abandoned_camp_cherry_grove`;
- otherwise a set name: `abandoned_camp`, `ocean_monuments`, `woodland_mansions`;
- `minecraft:` forces the exact structure: `"minecraft:ruined_portal"` is only the standard
  (non-desert, non-jungle, ...) ruined portal.

Every structure condition takes two filters:
- `biome=` keeps starts whose start position is in that biome (a name, list or `#tag`):
  `stronghold(within=3000, biome="#is_ocean")`.
- `tags=` keeps starts whose piece details include every listed tag (prefix match). Tags come from
  each structure's own piece generator, as listed in the table below.

| Structure | Tags |
| --- | --- |
| igloo | `basement`, `ladders:N` |
| shipwreck | `template:shipwreck/<variant>`, `chest:map_chest`, `chest:treasure_chest`, `chest:supply_chest` |
| ocean ruin | `large`, `cluster`, `ruins:N` |
| ruined portal | `template:ruined_portal/<name>`, `placement:<on_land_surface/partly_buried/on_ocean_floor/in_mountain/underground>`, `giant`, `mirrored`, `air_pocket`, `rotation:N`, `obsidian:N`, `frame:N` |

```python
igloo(within=800, tags=["basement"])
shipwreck(within=300, tags=["template:shipwreck/rightsideup_full", "chest:treasure"])
ruined_portal(within=200, tags=["placement:on_land_surface"])
```

### nearest(name, within=3000, at=(0,0))

Distance from `at` to the nearest generated start (its locate position). Infinity if none within
`within`. Passes by default when one exists.

### near(a, b, dist=128, within=1000, at=(0,0))

How many generated `a` within `within` of `at` have a generated `b` (a different start) within
`dist` blocks, measured between start positions.

```python
near("village", "pillager_outpost", dist=150, within=800)
near("stronghold", "village", dist=96, within=3000)     # stronghold under a village
```

### cluster(names, count=2, dist=128, within=1000, at=(0,0), afk=False)

The most structures (one name or a list) that fit in a circle of radius `dist` whose centre lies
within `within` of `at`. `afk=True` uses each structure's box corners instead of its start, so all
of every hut is inside the circle (witch-hut and monument farms). The best centre is exact.

```python
cluster("swamp_hut", count=4, dist=128, within=20000, afk=True)      # quad witch huts
cluster(["village", "pillager_outpost"], count=3, dist=300)
```

### chain(names, dist=256, within=1000, at=(0,0))

The longest prefix of the series `names[0] -> names[1] -> ...` where each start is within `dist`
of the previous one. Passes when the whole chain exists.

### overlap(a, b, within=1000, at=(0,0), margin=0)

How many pairs of `a` and `b` have intersecting first-piece boxes (grown by `margin`). Structures
without a cheap box use their start position.

### bingo(names=all sets, within=2000, at=(0,0))

How many of the listed structures (default: every overworld set) have a start within `within`.

## Biomes

All biome conditions sample biomes on a `step`-block grid at height `y`. Biomes are 3D, so cave
biomes need a low `y`. Cost grows with (radius / step)²; use a bigger `step` for large areas.
`name` is a biome, a list of biomes, or a `#tag` (`"#is_ocean"`, `"#is_jungle"`, ...).

### biome(name, within=0, at=(0,0), y=64, step=16, fraction=0)

The fraction of samples in the disc that match. With `fraction=0` one matching sample is enough.

```python
biome("lush_caves", within=200, y=-20)
biome("cherry_grove", within=300, fraction=0.25)
```

### biome_area(name, within=0, window=512, at=(0,0), y=64, step=16)

Area in blocks² of the largest connected patch of `name` that touches the disc. The patch is cut
off at `window` blocks beyond the disc, so raise `window` for huge biomes.

### biome_count(among=None, within=0, at=(0,0), y=64, step=16)

How many different biomes appear (optionally only those in `among`). `min="all"` means every
possible one of `among`: `biome_count(among="#is_ocean", within=3000, step=64, min="all")`.

### biome_border(a, b, within=0, at=(0,0), y=64, step=16)

Length in blocks of the border where `a` touches `b`.

### island(biomes=None, within=0, window=1024, at=(0,0), y=63, step=16, min_circularity=0, max_elongation=inf, lagoon=False)

Area of the largest island touching the disc: land surrounded by ocean that fits inside the
window. Land is anything not `#is_ocean`, or `biomes` (which must then border only ocean).
Shape filters: circularity (4πA/P², 1 is a disc), elongation (bounding-box aspect ratio) and
`lagoon=True` (encloses a lake of its own).

```python
island(biomes=["mushroom_fields"], within=1500)
island(within=2000, min_circularity=0.6, lagoon=True)
```

## Terrain

### climate(param, at=(0,0), y=64)

A raw climate parameter at a block position: `temperature`, `humidity`, `continentalness`,
`erosion`, `depth` or `weirdness`. For example, very inland spawns (`continentalness` > 0.8) or flat
land (`erosion` > 0.6).

### height(at=(0,0), heightmap="surface")

Terrain height from the game's `getBaseHeight`: `"surface"` (`WORLD_SURFACE_WG`, water counts) or
`"ocean_floor"` (`OCEAN_FLOOR_WG`, solid blocks only). Exact including aquifers, about 0.5 ms each.

### peak / trench / cliff / overhang / void / lake (within=0, at=(0,0), step=32)

Height-grid and column scans over the disc:
- `peak`: the highest surface;
- `trench`: the lowest ocean floor;
- `cliff`: the biggest height step between neighbouring samples;
- `overhang`: the longest air gap under solid ground above sea level;
- `void`: the longest air run below sea level;
- `lake(fluid="lava"|"water")`: the longest fluid run under the surface (aquifer lakes).

```python
peak(within=500, step=32) > 280
cliff(within=300, step=8) >= 40
lake("lava", within=64, step=16) >= 6
```

## Slime chunks

### slime(radius=128, within=0, at=(0,0))

The most slime chunks (chunk centres within `radius`) around any point within `within` of `at`.
Uses the game's seed-only formula, so it also works as a 48-bit pre-check.

## Loot

### loot(name, item, enchant=None, within=1000, at=(0,0), total=False)

How many of `item` are in the chests of a structure: the best single structure, or the sum over
all of them with `total=True`. `enchant` counts only items carrying that enchantment. Chests are
generated exactly as the game does (chest seeds and contents), for buried treasure, shipwrecks,
ruined portals, igloo basements and desert pyramids. Match details include each counted chest with
its full contents.

```python
loot("buried_treasure", "diamond", within=400) >= 2
loot("ruined_portal", "flint_and_steel", within=200)
loot("desert_pyramid", "enchanted_golden_apple", within=2000)
loot("shipwreck", "enchanted_book", enchant="mending", within=1500)
```

### portal_frame(within=1000, at=(0,0)) / portal_obsidian(...)

For the best ruined portal in range: `portal_frame` is how many of the 10 frame blocks (a 4×5 frame
without corners) are plain obsidian; 10 is a lightable frame. `portal_obsidian` is all of the
portal's plain obsidian. Crying obsidian, which the game rolls per block, is excluded. Giant
portals have no frame value.

```python
portal_frame(within=150) + loot("ruined_portal", "obsidian", within=150) >= 10   # completable from the chest
```

## Plugins

### native(plugin, filter, **params)

A filter exported by a native (Rust/C) plugin. It runs inside the engine's worker threads at full
speed; its return value is its number (> 0 passes by default). See [Plugins](Plugins.md).

### py(name, **params)

A Python plugin condition, `"plugin.condition"`. These run in Python on the engine's matches, so
they must be top-level `and` terms. Example: `py("example_python.seed_contains", text="777")`.

## Making queries fast

1. Structures first. Their placement pre-check runs on 48 bits before any biome work, and in
   structure-seed mode it prunes 65,536 seeds at a time. `structure`, `near`, `cluster`, `bingo`,
   `loot`, `portal_*` and `slime` all have pre-checks.
2. Small radii. The candidate count grows with radius².
3. `near` with a small `within` on the rarer structure as `a`.
4. Large-radius biome, island and terrain conditions last; `and` stops at the first failing term.
5. Use the estimate. Its match rate and "first match in" tell you before you start whether a query
   needs structure-seed mode, a smaller radius or a different approach.
