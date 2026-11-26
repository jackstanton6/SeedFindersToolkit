# Search modes and performance

| Mode | A unit is | Use it for |
|---|---|---|
| range (64-bit) | one world seed, `from .. to` | general searches, small queries, reproducible ranges |
| lower 48 bits (fixed upper 16) | one world seed whose upper 16 bits are `fixed` (0 – 65535) and whose lower 48 bits run `from .. to` | structure layouts within one "upper bits" family |
| upper 16 bits (sisters of a seed) | one sister of the seed in `fixed`: its lower 48 bits kept, upper 16 bits `from .. to` (0 – 65535) | biome or terrain variants of a structure layout you like (same structures, different biomes) |
| structure seeds (48-bit) + sisters | one 48-bit structure seed; on a pre-check pass, all 65,536 sister seeds are fully checked | rare structure layouts: quad huts, monument clusters, close villages |
| seed list file | one seed from a file (one integer per line) | re-checking seeds from crackers, other tools or earlier runs |
| plugin: … | one seed produced by a plugin seed source (`to - from + 1` of them) | e.g. text seeds (`example_python.text_seeds`) |

**range** fills `from`/`to` (both inclusive): *first 10M*, *full range* (all 64-bit seeds, all
48-bit values or 0 – 65535, depending on the mode), *32-bit (text seeds)* (every seed a text seed
can hash to), or *custom* once you edit them. They accept `10k`, `2.5M`, `1G`, `2^48`.

**shuffle** visits the units in a random order instead of counting up: every seed exactly once, so
Stop and Resume still work, and a stopped shuffled search has sampled the range evenly. *full range*
\+ shuffle replaces the old *random* mode in the window (the CLI keeps `--mode random`; `search
--shuffle KEY` shuffles any CLI search, the same KEY giving the same order). The GPU pre-filter
shuffles the same way. All sisters share their structure placement, so the GPU adds nothing in
*upper 16 bits* mode and it runs on the CPU.

## Stats explained

- **units**: done / total in the current mode's units. Progress, rate and ETA use these.
- **checks**: every seed run through the pre-check or the full check (structure-seed mode counts
  each sister seed). This is the "total calculations done".
- **prefilter pass**: share of checks whose 48-bit placement pre-check passed.
- **full evals**: seeds that got the full (biome/terrain/validity) evaluation.
- **matches**: engine matches. **py rejected**: matches later dropped by `py(...)` conditions.
- **est. total / est. rate**: from **Estimate**, which runs the search itself for 3 seconds (Stop
  ends it early) and extrapolates. In structure-seed mode the rate also accounts for the 65,536
  sister checks each passing structure seed costs, which a 3-second window rarely sees in full.
  **eta** is live, from the measured rate. Progress moves during sister expansion too.
- **match rate / 95% range / first match**: the rarity calculator: matches per 64-bit seed with a
  95% Wilson interval and the expected time to the next match. It updates live while the search
  runs (Start no longer waits for a sample), and Estimate fills it before you start. With no match
  yet, "first match" is a lower bound (`> 00:12:00`).
- **kept**: rows in the results table (with `maximize`/`minimize`, the current top N).

## Choosing a mode

If a query is driven by structure placement (min counts, `near`, small radii), the 48-bit
pre-check rejects almost every structure seed. Structure-seed mode then does about one placement
check per 65,536 world seeds instead of one per seed. Use range mode (shuffled for an even sample)
for biome or terrain-driven queries, where every seed needs the full check anyway.

## Stopping, resuming, max results

Stop at any time. Every unit before the reported resume point is complete; Resume (GUI) or
`--start-unit N` (CLI) continues from it. The window remembers the units other threads had already
finished past the resume point; a CLI run cannot, so `--start-unit` may print a few seeds again (at
most one block of up to 256 units per thread). Its progress counts the units before N as done. `max` stops after that many matches; in the window it
counts every match of the search, resumes included, so Resume after reaching it finds nothing more
(start a new search with a higher max). With `maximize` or
`minimize` in the query the search never stops early: it keeps the best `max` seeds (default 100).

## One seed: scan, loot, placement

- **Scan** (GUI Tools menu, CLI `scan`): runs the query over every point of a grid (or every start
  of an anchor structure) in one seed and lists every place it matches, nearest first, as map pins.
  The query is moved from the scan centre to each point: around the origin its coordinates are
  relative to the point; for a scan centred elsewhere give its conditions that centre as `at`.

  ```sh
  python -m sft scan 12345 "village(within=150) >= 2" --within 10000 --step 128          # double villages
  python -m sft scan 12345 "cluster('swamp_hut', count=2, dist=128, afk=True, within=192)" --within 30000 --step 256
  python -m sft scan 12345 "biome('cherry_grove', within=64, fraction=0.9)" --anchor village --within 5000
  ```
- **Loot** (CLI `loot`): every chest of a structure type near a point, with its contents.
  `python -m sft loot 12345 buried_treasure --radius 1500`
- **Attempts overlay** (map panel): every placement attempt of a structure set, green squares for
  starts that generate and red crosses for failed ones. For strongholds the ring bands are drawn too.
- **Placement statistics** (Tools menu): samples 2,000 random seeds for one set around the map centre:
  the chance of at least one, the mean count and the distribution of the nearest distance.

## Seed cracking

Give the coordinates of structures you saw in a world, as reported by `/locate` (or any block of
the start chunk). The cracker finds the 48-bit structure seeds that place them all, then checks
their 65,536 sisters with biomes and any extra conditions.

```sh
python -m sft crack swamp_hut:-1200,840 desert_pyramid:310,-560 pillager_outpost:1040,520 shipwreck:-2160,-1136     --query "biome('plains', at=(0, 0))"
```

How it works: a random-spread offset is `nextInt(limit)`. When the limit is even, its low bits
depend only on the low bits of the seed, so those bits are pinned first (huts, temples and outposts
pin 3 bits per coordinate, shipwrecks 2). The rest is brute force, about 0.5 s per 2^28 seeds
(roughly 7 observations make it a second). Strongholds cannot be used (ring placement needs
biomes), and observations of ancient cities, monuments and mansions help the final check but not
the lifting.

A crack is one job: the progress bar shows the brute force (seeds tried), then the sister check,
and Stop (Ctrl-C in the CLI) works in both. Resume after stopping the brute force starts it again;
after the sister check started, Resume continues it. `--max N` stops after N matching seeds. Weak
observations are refused before anything runs (more than 2^38 seeds to brute-force, 2^44 on the
GPU) or once more than 2^24 structure seeds fit them; above 1,024 structure seeds the log warns
with the expected sister-check time. Use `/locate` positions: the piece positions that `sft locate`
prints can lie in a neighbouring chunk.

**Bedrock picks the sister.** Structures fix the low 48 bits; the floor (bedrock at y -63 to -60)
depends on all 64, so a bedrock pattern chooses among the sisters:

```sh
python -m sft crack swamp_hut:-1200,840 desert_pyramid:310,-560 --bedrock 0,-63,0:1 1,-63,0:0 2,-63,0:1 ...
```

Read the layer from the side of a ravine or by digging (F3 for the coordinates): `:1` is bedrock,
`:0` is not. Nearby floor blocks of one seed are strongly correlated (the game's positional random
depends mostly on a few bits per seed), so it takes more blocks than you would expect: in tests,
64 blocks left 1 to 5 sisters and 128 always picked the seed. Bedrock alone, without structures,
cannot be cracked by brute force (2^64 seeds).

In the GUI: Tools, Crack seed from structures (one `name x z` per line, an optional
`query: ...` line, and `bedrock x y z yes/no` lines). With GPU ticked the brute force runs on the graphics card (268M seeds in about
0.7 s on an RTX 5070 Ti, start-up included).

## Cost model (measured, 24-thread desktop, 26.3)

| Operation | Cost |
|---|---|
| 48-bit placement pre-check | well under 1 µs per structure set and grid cell |
| per-seed sampler setup (first biome query of a seed) | ~0.19 ms |
| biome lookup | ~3.2 µs per thread (~7.5M/s on 24 threads) |
| terrain height (`height`, surface-anchored structure checks) | ~0.32 ms per column per thread |
| stronghold ring position | ~3,000 biome lookups each |

Example: `village(within=200) and pillager_outpost(within=400)` runs at about 190k seeds/s in
range mode, with every match fully validated.

Threads default to all cores; lower it to keep the machine responsive.

## GPU (CUDA)

With an NVIDIA GPU (compute capability 7.5 or newer, any recent driver) the toolbar shows **CUDA
compatible**; tick **GPU** (CLI: `--gpu`). The GPU then runs the part of the query that depends on
the seed alone, in batches of up to 16M seeds:

- structure placement counts (`structure`, `loot`, `portal_*`, and `near` as a necessary
  condition), including every frequency-reduction method of the vanilla sets;
- `cluster` of one structure: some start with `count - 1` others close enough (for quad witch huts
  this keeps about 1 structure seed in 900);
- slime chunks;
- biomes, in range, random and shuffled searches (whole seeds; not in structure-seed mode, where
  biomes depend on each sister): `biome(...)` near a point, and for desert pyramids, jungle temples,
  swamp huts, igloos, shipwrecks, ocean ruins, monuments and mansions whether each start's column
  has a biome the structure needs. The GPU computes climate and biomes exactly as the CPU does;
  where two biomes tie (a point on the border of two parameter boxes, about 1 lookup in 1,000) it
  allows both. Structures placed by jigsaw (villages, outposts, ancient cities, trial chambers) get
  their biomes checked on the CPU.

Every seed the GPU passes still gets the exact CPU pre-check (exclusion zones such as outposts near
villages, the exact cluster geometry) and the full check, so results are identical with and without
it (`cargo test --release --test gpu -- --ignored` checks that the kernel keeps every seed the CPU
keeps). The GPU filters the next batch while the CPU checks the current one, and in structure-seed
mode the sisters of each surviving structure seed are shared among all threads. Seed cracking brute-forces on the GPU too (`crack --gpu`, Tools menu with GPU ticked),
with a work limit of 2^44 seeds instead of 2^38.

What to expect (RTX 5070 Ti vs 24 CPU threads): the seed-only check runs at about 3.4 billion seeds/s
against 190 million/s on the CPU. The whole search speeds up by the same factor only when nearly all
seeds fail that check: `slime(radius=64) >= 12` goes from 59M to 496M seeds/s. When many seeds
survive to biome and terrain checks, those dominate and the GPU changes little, and in structure-seed
mode the 65,536 sister checks per surviving structure seed stay on the CPU. Quad witch huts
(structure seeds): 760 structure seeds/s on the CPU, 890/s with the GPU. Low GPU utilisation in
such searches is expected: the GPU finishes its batch quickly and waits for the CPU.

With biomes on the GPU (range mode, 64k seeds, against the CPU's pre-check and full check):
`desert_pyramid(within=300)` 477k seeds/s vs 115k (4.2x); `village(within=400) and
biome('cherry_grove', within=600)` 10.4k vs 3.0k (3.5x); `biome('mushroom_fields', within=1500)`
1.46k vs 202 (7.2x). The GPU's survivors are then nearly all real matches, so the CPU has little
left to do. The log says `(with biomes)` when they are on.

Building with GPU support needs the CUDA toolkit's `nvcc` when the engine is compiled (`pip install
-e .` picks it up from `CUDA_PATH` or `PATH`); running needs only the NVIDIA driver. Without either,
the checkbox stays off and says why.
