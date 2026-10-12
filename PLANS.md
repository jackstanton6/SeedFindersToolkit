# PLANS.md: scenario roadmap

What the toolkit should answer next, chosen with the project owner on 2026-10-10 (53 of 64 proposed
scenarios). The goal: if someone asks "where could this structure or series of structures
generate?", the app answers it.

Tiers say what each scenario depends on:
- **Now:** the verified engine already computes everything needed; only query/search work is left.
- **Small port:** a small, cheap piece of game logic must be ported first.
- **M2 / M3 / M4:** waits on that milestone (see AGENTS.md).

## Definition of done (every item)

- Anything that touches game logic gets an oracle dump mode in `oracle/Oracle.java` and a parity test
  in `engine/tests/oracle.rs` with zero mismatches.
- New query conditions follow the AGENTS.md checklist: Rust `Cond` + `parse` + `pre` (when a 48-bit
  pre-check exists) + `eval`, the DSL entry in `sft/query.py`, a GUI template, and a section in
  `docs/wiki/Query-Language.md`.
- An example query in the docs, and one runnable check for any non-trivial logic.

## Tier 1: six building blocks (Now)

Most of the 53 scenarios reduce to these.

### 1. Clusters: `cluster(names, count, dist, within, at)`

N generated structures (one name or a list) all within `dist` of a common point, with that point
anywhere within `within` of `at` (relocatable). Witch-hut clusters also check the AFK-sphere geometry
(every hut's spawning space within 128 blocks of one standing point).

Covers: quad/triple witch huts, monument clusters, village clusters, village type mixes (list of
names), ocean ruin cities, shipwreck graveyards, relocatable patterns.

### 2. Relations: `chain([a, b, c], dist)`, overlap, exact coordinates

- `chain`: an ordered series, each start within `dist` of the previous; also N in a rough line.
- Overlap: starts whose areas intersect (start-area level now; exact piece boxes need M3).
- Exact coordinates: `structure("village", chunk=(0, 0))` / `at=(x, z), within=0`. Solved by
  enumerating only the one grid cell, so a 48-bit sweep stays very fast.
- Stronghold helpers: closest ring-1 stronghold, stronghold under a village or in a biome, all three
  ring-1 strongholds within X.
- Structure bingo: `bingo(names=None, within, min_types)` counts distinct structure types.

Covers: chains and series, overlapping structures, exact target coordinates, stronghold scenarios,
structure bingo, ancient city access.

### 3. Biome grid analysis

Sample biomes on a grid (point path, chosen `y` and `step`), then:
- connected components by flood fill (area, bounding box, isolation from other land);
- distinct-biome counts and category coverage (all oceans, all cave biomes, ...);
- adjacency (biome A touching B, border length);
- a land/ocean mask with shape metrics (area, circularity, elongation, bays/lagoons as enclosed
  water, island isolation).

Conditions: `biome_area`, `biome_count`, `biome_border`, `island`, `landform`.

Covers: biome size and area, diversity and bingo, adjacency and borders, landmass shape, landform
shapes, mob-safe zones (mushroom-fields islands), ocean features (warm-ocean area, trench next to a
monument), scenic biome combos, big landmarks.

### 4. Height and column analysis

Height grid (`getBaseHeight`, both heightmaps) plus column scans (`getBaseColumn`):
- max/min height in an area, slope and cliff detection (height delta over distance);
- overhangs and floating terrain (solid, air, solid in one column);
- underground voids and aquifer lakes (air, water or lava runs below the surface), under a point,
  village or spawn.

Conditions: `peak`, `trench`, `cliff`, `overhang`, `void`, `lake`. Height-of-structure checks make
"village on a mountain/plateau" possible.

Covers: extreme heights, cliffs and overhangs, underground voids and lakes, village in unusual
spots, cave biome under spawn (3D biome at a chosen y under a point).

### 5. Numeric scores

Conditions can return numbers (count, distance, area, height) as well as true/false.
- `score(expr)`: a weighted sum, with the top N seeds kept live in the results table.
- Record hunts: `maximize(metric)` / `minimize(metric)`.
- Rarity calculator: match probability with a confidence interval and the expected time to the
  first hit, from the calibration sample (extends the existing estimate).
- Farm-site finder: presets that score cluster geometry, ocean depth and distance to spawn.

Covers: weighted scoring, record hunts, rarity calculator, farm-site finder.

### 6. Single-seed scan mode and map overlays

- World scan: run any pattern over ±R in one seed and list every occurrence nearest-first, with map
  markers.
- Placement heatmaps: biome-valid zones per structure, region grid cells, stronghold ring bands,
  attempted-but-failed starts.
- Placement statistics: sample many seeds for per-region probability, expected count and
  nearest-distance distributions.
- Seed cracking: observations (structure coordinates, then biomes or heights at known spots) become
  a structure-seed search (48-bit, placement only) followed by the sister expansion.
- Sister seed explorer: rank a seed's 65,535 sisters by a biome or terrain query.

Covers: world scan, placement heatmaps, placement statistics, seed cracking, sister seed explorer.

## Tier 2: small ports

- **Slime chunks:** the seed-only slime chunk formula, with `slime_cluster(count, area)`.
- **Cheap piece details** (from each structure's piece code, oracle-verified): igloo basement,
  shipwreck type (full, bow/stern, with treasure or map chest, beached), ocean ruins large and
  clustered.

Covers: slime chunk clusters, cheap piece details, and the piece-level parts of shipwreck graveyards
and ocean ruin cities.

## Tier 3: M2 (spawn and surface)

- Exact spawn point (`NoiseSpawnFinder`), then `at="spawn"` for every condition.
- Surface materials (material rules): terracotta bands, ice and glaciers, red sand, snow cover.
- Ore veins (density-function based), bedrock patterns (positional random).

Covers: exact spawn, spawn-anchored conditions, unique spawns, surface materials, ore veins, bedrock
patterns.

## Tier 4: M3 (pieces, features, loot)

- Full structure layouts: village buildings, job sites, beds, bells, golems; mansion rooms;
  mineshaft depth (also makes mineshafts exact).
- Loot tables: enchanted golden apples, buried-treasure diamonds, ruined portal obsidian plus flint
  and steel, shipwreck maps, enchanted books, trial vaults.
- Features: tallest cactus, sugar cane and bamboo, giant trees, ores, dungeons and spawners, fossils,
  geodes, desert wells, icebergs, lava pools.
- Speedrun (RSG/FSG) filters assembled from the above plus tier 1 (overworld parts can ship earlier).

Covers: full layouts, village contents, chest loot, tall plants and trees, ores, rare features,
speedrun filters.

## Tier 5: M4 and later

Nether (biomes, fortresses, bastion types), End (cities with ships, gateways, pillar heights;
pillars are a cheap seed-only calculation and can come early), arbitrary block patterns, older
versions (1.18 to 1.21.x).

## Declined (not planned)

Start-piece variant filters, flat build land, multi-target (Pareto) tradeoffs, text seed spaces,
observation import, portal-linked queries, raid/outpost setups, buried treasure hunts, trial chamber
hunts, mineshaft/dungeon hunts, cave systems. Several of these still fall out of the building blocks
for free (flat land from height analysis, buried treasure and trial chambers from clusters and
coordinates, cave systems from biome and column analysis). They just won't get dedicated presets.

## Order

1. Tier 1 blocks in order 1 to 6 (each block unlocks a group of scenarios).
2. Tier 2 small ports.
3. M2, then M3, then M4.

## Current batch (requested 2026-10-10)

Scope: all of Tier 1, Tier 2 small ports, speedrun filters (overworld parts) and chest loot. The
project owner accepted the Minecraft EULA so the official server can be the ground truth for loot.

Progress:
- [x] `Hit` carries `bbox` (first piece box, y before terrain fitting) and `tags`. Done for scattered
  features (desert pyramid, jungle temple, swamp hut: orientation = first nextInt(4)), monument
  (58x58), jigsaw start piece, and ruined portal (template box + tags template/placement/giant/
  mirrored/air_pocket). Igloo `basement` + `ladders:N`, shipwreck `template:` + `chest:<marker>`
  (from template DATA markers), ocean ruin `large`/`cluster`/`ruins:N` (addClusterRuins simulated).
  Oracle-verified (327 piece rows, 4 seeds).
- [x] Conditions rewritten around a numeric core in `engine/src/query.rs` (design below); the runner
  and native ABI stay in `search.rs`. Native filters' return value is their metric.
- [x] Python DSL: comparisons (lowered to metric bounds when possible), arithmetic, `maximize`/
  `minimize`, `anywhere`; GUI condition menu by category, presets menu, score column, live top-N.
- [x] Single-seed scan (`World.scan`, CLI `scan`, GUI Tools), attempts overlay with stronghold ring
  bands, placement statistics, rarity (Wilson 95% interval, time to first match).
- [x] Seed cracking (`engine/src/crack.rs`, CLI `crack`, GUI Tools): low-bit lifting, high-bit brute
  force, sister expansion as a list search with the observations. `search --sisters` explorer.
- [x] Slime chunks, oracle-verified (80k chunks).
- [x] Chest loot: `loot.rs` (LootTable.fill over the vanilla JSON; 432 server-unpacked chests over
  18 tables match) and chest seeds for buried treasure, shipwrecks, ruined portals, igloo
  basements and desert pyramids (138 real-world chests match; beached wrecks flagged inexact).
  The 26.x decoration random is a `WorldgenRandom` over Xoroshiro; template containers take a
  `nextLong` in (y, x, z) order per chunk, then loot data markers. Oracle: `tests/loot_oracle.py`
  drives the dedicated server (fill: explicit seeds + unpack; world: real generation, region files).
  Conditions `loot(...)`, `portal_frame`, `portal_obsidian`; CLI `loot`; `World.chests`/`World.loot`.
- [x] Speedrun presets (overworld): village + portal, near-complete portal frames (obsidian after
  BlockAgeProcessor, 40 real portals match), frame + chest obsidian, flint and steel, treasure
  diamonds, shipwreck iron, temple + village, close stronghold. Nether parts wait for M4.
- [x] Docs: Query-Language, Search-Modes, GUI, Accuracy, README, AGENTS.
- [ ] Left from this batch: chest seeds for ocean ruins (cluster piece list) and jungle temples
  (per-block stone randomness before the chests); exact frame definitions for broken portal
  templates instead of best-frame detection.

Design of the condition core (search.rs):
- `Cond::{And, Or, Not, Metric{m, min, max}, Cmp{expr, min, max}, Objective{expr, maximize},
  Anywhere{cond, at, within, step, anchor}, Native}`. Every metric has a numeric value; as a
  condition it passes when `min <= value <= max` (defaults per metric).
- Metrics: structure (count), nearest (distance), near (count of a with b within dist), cluster
  (largest set covered by a circle of radius dist whose centre is within `within`; `afk=True` uses
  boxes), chain (longest prefix), overlap (overlapping pairs), bingo (distinct sets), biome
  (fraction), biome_area / biome_count / biome_border / island (flood fill on a biome grid), climate,
  height, peak / trench / cliff / overhang / void / lake (height grid and column scans), slime (max
  slime chunks within radius of a point).
- Structure selectors accept `biome=` (biome at the start position) and `tags=` (all must match).
- JSON expressions: `{"op":"num","value"}`, `{"op":"add|sub|mul|div","a","b"}`, `{"op":"neg","a"}`,
  any metric node as a leaf. Objectives keep a top-N in Rust (threshold check before pushing).
