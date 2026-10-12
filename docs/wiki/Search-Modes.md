# Search modes and performance

| Mode | A unit is | Use it for |
|---|---|---|
| range (64-bit) | one world seed, `start .. start+count` | general searches, small queries, reproducible ranges |
| structure seeds (48-bit) + sisters | one 48-bit structure seed; on a pre-check pass, all 65,536 sister seeds are fully checked | rare structure layouts: quad huts, monument clusters, close villages |
| random | one pseudo-random 64-bit seed (generator seed = `start`) | sampling the whole seed space evenly |
| seed list file | one seed from a file (one integer per line) | re-checking seeds from crackers, other tools or earlier runs |
| plugin: … | one seed produced by a plugin seed source | e.g. text seeds (`example_python.text_seeds`) |
| sisters | the 65,536 seeds sharing one seed's low 48 bits (same structures, different biomes) | biome or terrain variants of a structure layout you like |

`count` accepts `10k`, `2.5M`, `1G`, `2^48`.

## Stats explained

- **units**: done / total in the current mode's units. Progress, rate and ETA use these.
- **checks**: every seed run through the pre-check or the full check (structure-seed mode counts
  each sister seed). This is the "total calculations done".
- **prefilter pass**: share of checks whose 48-bit placement pre-check passed.
- **full evals**: seeds that got the full (biome/terrain/validity) evaluation.
- **matches**: engine matches. **py rejected**: matches later dropped by `py(...)` conditions.
- **est. total / est. rate**: from the calibration sample taken before each start (or Estimate).
  **eta** is live, from the measured rate.
- **match rate / 95% range / first match**: the rarity calculator. The calibration sample's
  matches give the probability per unit with a 95% Wilson interval and the expected time to the
  first match. With no match in the sample, "first match" is a lower bound (`> 00:12:00`).
- **kept**: rows in the results table (with `maximize`/`minimize`, the current top N).

## Choosing a mode

If a query is driven by structure placement (min counts, `near`, small radii), the 48-bit
pre-check rejects almost every structure seed. Structure-seed mode then does about one placement
check per 65,536 world seeds instead of one per seed. Use range or random mode for biome or
terrain-driven queries, where every seed needs the full check anyway.

## Stopping, resuming, max results

Stop at any time. Every unit before the reported resume point is complete; Resume (GUI) or
`--start-unit N` (CLI) continues from it. `max` stops after that many matches. With `maximize` or
`minimize` in the query the search never stops early: it keeps the best `max` seeds (default 100).

## One seed: scan, loot, placement

- **Scan** (GUI Tools menu, CLI `scan`): runs the query over every point of a grid (or every start
  of an anchor structure) in one seed, with the query's coordinates relative to each point, and
  lists every place it matches, nearest first, as map pins.

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

In the GUI: Tools, Crack seed from structures (one `name x z` per line, an optional
`query: ...` line).

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

Threads default to all cores; lower it to keep the machine responsive. A CUDA brute-force stage for
48-bit placement scans is planned (M4).
