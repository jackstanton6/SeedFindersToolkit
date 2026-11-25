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
  `sft/docs/Query-Language.md`.
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
  basements, desert pyramids and ocean ruins incl. clusters (242 real-world chests match; beached
  wrecks flagged inexact).
  The 26.x decoration random is a `WorldgenRandom` over Xoroshiro; template containers take a
  `nextLong` in (y, x, z) order per chunk, then loot data markers. Oracle: `tests/loot_oracle.py`
  drives the dedicated server (fill: explicit seeds + unpack; world: real generation, region files).
  Conditions `loot(...)`, `portal_frame`, `portal_obsidian`; CLI `loot`; `World.chests`/`World.loot`.
- [x] Speedrun presets (overworld): village + portal, near-complete portal frames (obsidian after
  BlockAgeProcessor, 40 real portals match), frame + chest obsidian, flint and steel, treasure
  diamonds, shipwreck iron, temple + village, close stronghold. Nether parts wait for M4.
- [x] Docs: Query-Language, Search-Modes, GUI, Accuracy, README, AGENTS.
- [x] Portal frames: exact per seed instead of best-4x5 detection. `portal_frame` = 10 minus the
  blocks the most complete upright frame (any size, 4x5 to 23x23) still misses; 40 real portals
  (3 giant) match when recomputed from their world blocks.

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

## Current batch (requested 2026-10-12)

Scope: jungle temple chests, CUDA (seed-only pre-filter + cracking), a block-based query builder with
an "I want..." wizard (replaces the tree editor), in-app help and a first-start tour, and a UX audit
(searches that used all CPU without showing progress).

Progress:
- [x] Stalls: Start ran a hidden 2048-unit calibration first (in structure-seed mode up to
  ~10^8 full evaluations, not cancellable); progress flushed per block of up to 256 seeds; the map
  rendered on the GUI thread. Now: Start starts at once with live rarity; Estimate runs the real
  search for 3 s (Stop works, natives included); stats flush after every full evaluation and
  sister expansion shows fractional progress; engine panics stop the search with the unit and
  message; map renders in a thread; result rows are inserted in batches; max results defaults to
  10k; a hint when structure-seed mode cannot help.
- [x] Jungle temple chest seeds: a constant number of moss-stone nextFloat draws (every block of
  every box, any chunk) before each container; chests + trap dispensers (9-slot fill), 8 real ones match.
- [x] CUDA: seed-only pre-filter (structure placement incl. all frequency reductions, slime) on the
  GPU, CPU verifies survivors; GPU cracking brute force; GUI checkbox with a "CUDA compatible"
  status; CLI `--gpu`. Kernel parity: identical seed sets to the CPU pre-check. Gains are large
  only when the seed-only part is what limits a search (slime 8x, crack); biome checks stay on the
  CPU, as agreed.
- [x] Block query builder (nested blocks, sentence-style fields, palette, drag and drop, undo) +
  "I want..." wizard (10 goals); text box and a plain-English summary stay in sync.
- [x] Help viewer (wiki pages in the app, F1, per-block help) and an optional first-start tour.

## Performance batch (requested 2026-10-12)

Scope: faster searches and real GPU use without losing accuracy. Every change keeps the matched seed
set identical (oracle parity at zero mismatches, CPU vs GPU result sets equal, resume equal).

Progress:
- [x] Clusters: `max_cover` buckets points (same candidates and members as all pairs, early stop
  in the pre-check); per-structure slack (`Structures::reach`: 12 blocks for swamp huts, size-based
  for temples, 128 otherwise) instead of 128 for every start; the full check only generates starts
  that have `count - 1` placement chunks close enough, and sister seeds reuse those chunks
  (thread-local, keyed by the low 48 bits). Quad witch huts pre-check 97 ms -> 4 ms, pass rate
  1.0 -> 1e-3, sister check 144 ms -> 0.4 ms.
- [x] GPU cluster atom (pair distances, and for count <= 4 a pairwise check of the neighbours):
  a superset of the CPU pre-check; 2.3M structure seeds/s on the quad hut preset.
- [x] GPU filters the next batch while the CPU checks this one; in structure-seed mode each
  survivor's 65536 sisters are split into 16 parts so every thread works on it.
- [x] `and` children ordered at search start by measured cost / (1 - pass rate); only speed and the
  order of match details change.
- [x] The density-function rewrite (inline, cache dedupe, slicing) runs once per World; a seed only
  compiles samplers and noises. `Seeded::new` 0.18 -> 0.04 ms, with terrain 0.59 -> 0.19 ms.
- Skipped: lazy noise creation (all noises together cost 0.1 ms per seed).

Measured (RTX 5070 Ti, 24 threads): quad witch huts (structure seeds) ~0 -> 760/s CPU, 890/s with
the GPU; village + monument (range) 16.2k -> 21.2k seeds/s.

## Bug-fix batch (requested 2026-11-26)

Scope: every bug in `docs/KNOWN-BUGS.md`, plus CI and a non-editable install. Work on master, one
commit per item: reproduce with a failing test, fix, remove the KNOWN-BUGS entry. Parity suite and
all test files pass before the last commit.

- [x] 0. Seed bits panel (uncommitted): run `test_gui.py`, check a headless screenshot, commit on its own.
- [x] 1. Estimate then Resume: `Main.estimate` resets `resume_unit` (and the results table).
- [x] 2. CLI `--start-unit`: the Job knows its start unit; percent and ETA count only the remaining
  range. Possible duplicates near the resume point (at most a block per thread) are documented.
- [x] 3. Crack memory: sisters of cracked structure seeds are expanded and checked in the engine
  (threaded, streamed to results), not as Python lists. Above 1,024 structure seeds, warn with the
  expected sister-check time and suggest more observations; the crack keeps running.
- [x] 4. Crack runs as a normal Job: progress, ETA, Stop, Ctrl-C.
- [x] 5. Loot: a query using loot is refused at parse when the tables did not load (message names
  `python -m sft data 26.3`); a fill error stops the search with the unit and message.
- [x] 6. Plugins everywhere: `PyWorld.parse` gets the loaded native libraries; `check`, the GUI
  re-check and `scan` evaluate `py(...)` terms, so they agree with a search.
- [x] 7. CLI: `crack --max N` stops after N seeds; an empty `--seeds-file` is an error; `scan` with
  only `py(...)` terms runs them.
- [x] 8. `island()` details carry their condition, so two island conditions both show.
- [x] 9. `query._literal` turns any literal error into a `QueryError`; the example markdown exporter
  handles details without a `name`.
- [x] CI (GitHub Actions, Windows + Linux): `cargo test`, build without CUDA, pip install,
  `test_query.py`, `test_gui.py` offscreen. Tests that need `data/26.3` skip with a message.
- [x] Non-editable install: data and plugins default to a per-user folder (`%APPDATA%/sft`,
  `~/.local/share/sft`), `SFT_HOME` overrides it, an editable checkout keeps the repo folders; docs
  ship inside the package.

Then: the GPU biome milestone below, as written.

## Biome checks on the GPU (milestone, done 2026-11-26)

The GPU only rejects what the 48-bit pre-check rejects; every survivor still needs biomes (and often
heights) on the CPU, so the GPU idles whenever the CPU check is the limit (most real queries). Porting
the climate path makes the GPU do the heavy lifting:
- float Perlin / NormalNoise / the climate density functions (point path first) in CUDA, literal
  ports with `--fmad=false`; per-seed noise setup (Xoroshiro, MD5-derived seeds) on the device;
- the climate parameter search (R-tree or a flattened list) for biome ids;
- lowering: structure start biome checks (`column_ok`, start biome) and `biome(...)` metrics become
  GPU atoms; the CPU still checks every survivor exactly.
- Definition of done: a GPU parity test against `Seeded::biome` on random seeds and positions (zero
  mismatches), superset parity for the lowered queries, and measured speedups.
Heights and terrain (final density, aquifer) stay on the CPU unless the biome port shows they are
worth it.

Done:
- [x] Device climate: the compiled climate samplers walked into a node array (`gpu::ClimateProgram`,
  any point-path sampler; seed-independent shape), per-seed Xoroshiro forks and Perlin layers, splines,
  cache nodes memoised by position (xz-only ones reused down a column), R-tree search. Parity: 200k
  random (seed, quart) lookups match `Seeded::biome` with zero mismatches.
- [x] Ties: where leaves of different biomes sit at the same distance the CPU's warm start decides,
  so the device returns every tied biome (about 1 lookup in 1,000) and atoms allow any of them.
- [x] Lowering (whole seeds only: range, random, shuffled; not structure-seed mode): `biome(...)` with
  lower and upper bounds on the fraction, and `column_ok` for structures checked that way before any
  terrain step (scattered features, igloos, shipwrecks, ocean ruins, monuments, mansions). Jigsaw
  structures keep their biome checks on the CPU.
- [x] Two stages: a thread per seed for placement, then a 128-thread block per survivor for biomes
  (its Perlin tables in shared memory, column quarts and grid points shared out). Batches start at
  256 units for biome programs (display watchdog).
- [x] Superset test: every CPU match survives, 11 queries x 2 modes; survivors are nearly the matches.
  Speed (RTX 5070 Ti vs 24 threads): 4.2x, 3.5x and 7.2x on structure-biome, village + biome and
  large biome-grid queries.

Possible next steps: biomes for structure-seed mode (a stage that checks each survivor's 65,536
sisters on the GPU), jigsaw start columns (needs the start-piece choice), `biome_area`/`island` grids.
