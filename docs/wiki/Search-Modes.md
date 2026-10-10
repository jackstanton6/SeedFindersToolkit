# Search modes and performance

| Mode | A unit is | Use it for |
|---|---|---|
| range (64-bit) | one world seed, `start .. start+count` | general searches, small queries, reproducible ranges |
| structure seeds (48-bit) + sisters | one 48-bit structure seed; on a pre-check pass, all 65,536 sister seeds are fully checked | rare structure layouts: quad huts, monument clusters, close villages |
| random | one pseudo-random 64-bit seed (generator seed = `start`) | sampling the whole seed space evenly |
| seed list file | one seed from a file (one integer per line) | re-checking seeds from crackers, other tools or earlier runs |
| plugin: … | one seed produced by a plugin seed source | e.g. text seeds (`example_python.text_seeds`) |

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

## Choosing a mode

If a query is driven by structure placement (min counts, `near`, small radii), the 48-bit
pre-check rejects almost every structure seed. Structure-seed mode then does about one placement
check per 65,536 world seeds instead of one per seed. Use range or random mode for biome or
terrain-driven queries, where every seed needs the full check anyway.

## Stopping, resuming, max results

Stop at any time. Every unit before the reported resume point is complete; Resume (GUI) or
`--start-unit N` (CLI) continues from it. `max` stops after that many matches.

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
