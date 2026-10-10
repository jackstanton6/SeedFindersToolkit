# Accuracy and verification

The engine is a literal port of the 26.3 world-generation code. Every layer marked "verified" is
checked by an oracle: `oracle/Oracle.java` runs the actual 26.3 server classes (registries loaded
the way the server loads them), dumps ground truth for random positions and seeds, and
`engine/tests/oracle.rs` compares the Rust engine against it. Seeds used: 0, 12345, 8675309,
-4172144997902289642.

| Check | Size | Result |
|---|---|---|
| six climate parameters, bit-for-bit (float bits) | 12,000 points | 0 mismatches |
| biome via point resolver (structure checks) | 12,000 points | 0 mismatches |
| chunk biomes via volume resolver (what chunks store) | 80 chunk columns × 1,536 quarts | 0 mismatches |
| `WORLD_SURFACE_WG` and `OCEAN_FLOOR_WG` heights (incl. aquifers) | 1,600 columns × 2 | 0 mismatches |
| structure starts in a 97×97-chunk window: every set, placement chunk, chosen variant, start position | ~1,100 rows | 0 mismatches (mineshafts excluded, see below) |
| stronghold ring positions | 128 per seed | 0 mismatches |

Run it yourself (needs Java and `python -m sft data 26.3`):

```sh
python tests/run_oracle.py 26.3                 # climate, chunk biomes, heights
python tests/run_oracle.py 26.3 structures
cd engine && cargo test --release -- --ignored
```

## Known approximations

- **Mineshaft depth.** Placement chunks are exact, but the start's y comes from the full mineshaft
  piece layout, which isn't ported yet; y=50 is used. Validity can differ when the real start
  depth lands in a biome the mineshaft can't use (deep dark, or badlands for the normal variant).
  Results show these as `~approx` / `?`. Ported in M3.
- **R-tree tie-breaking.** The game warm-starts each biome search from the previous result of the
  same thread, which only matters when two biome entries are at exactly the same distance. The
  engine keeps a warm start per evaluation context, so exact ties could in theory resolve
  differently. None appeared in the samples above.
- **Stronghold angles** use the platform `cos`/`sin`. Java may differ in the last bit, which can only
  matter for a value within ~1e-13 of a rounding boundary.

## Why both evaluation paths exist

26.x evaluates density functions either point-by-point (`sampleValue`) or over volumes
(`sampleVolume`), and the results are not always bit-identical: interpolation steps differ, and
noise with non-power-of-two scales associates multiplications differently. The game uses the point
path for structure biome checks and the volume path for chunk biomes, column checks and heights.
The engine does the same, including the sampler cache contexts that can hand a volume value to a
later point query.

## Not modelled yet

Spawn point, surface blocks, features (trees, sugar cane, cacti, ores, geodes), carvers, full
block-level output, Nether and End. See the milestones in AGENTS.md.
