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
| first-piece boxes and piece details (igloo basements, shipwreck and portal templates, ocean ruin size and clusters) | 327 rows | 0 mismatches |
| slime chunks | 80,000 chunks | 0 mismatches |
| chest contents (`LootTable.fill`): items, counts, enchantments, damage, potions, stews, horns | 456 containers (incl. 9-slot dispensers), 19 tables | 0 mismatches |
| structure chest loot seeds (buried treasure, shipwrecks, ruined portals, igloo basements, desert pyramids, ocean ruins, jungle temple chests and trap dispensers) | 250 containers, 4 real worlds | 0 mismatches |
| ruined portal obsidian after aging (`portal_obsidian`, `portal_frame`) | 40 portals, 4 real worlds | 0 mismatches |
| GPU pre-filter vs the CPU pre-check (`engine/tests/gpu.rs`) | 2^20 seeds × 11 queries × 3 modes | identical seed sets (outposts: superset, exclusion zones stay on the CPU) |

Loot is checked against the official dedicated server, run headless under `cache/oracle-server`
(you must accept the Minecraft EULA to run it). `tests/loot_oracle.py fill` places chests with
explicit loot seeds and reads their unpacked contents from the region files; `world SEED...`
generates the area around the structures the engine predicts and reads every unopened chest's
`LootTableSeed` and every obsidian block.

Run it yourself (needs Java and `python -m sft data 26.3`):

```sh
python tests/run_oracle.py 26.3                 # climate, chunk biomes, heights
python tests/run_oracle.py 26.3 structures slime
python tests/loot_oracle.py fill                # dedicated server, EULA
python tests/loot_oracle.py world 0 12345 35467 59104
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
- **Beached shipwreck chests.** A beached wreck that fits a worldgen region rolls its height in
  whichever chunk the game happens to generate first, which shifts the loot seeds of chests in that
  chunk. These chests are flagged `exact: false` (in the oracle worlds 6 of 7 still matched).
- **Chest y.** Loot seeds and x/z are exact; the y of a chest is estimated (terrain fitting of
  pieces is not simulated), except for ruined portals.
- **Chest loot not covered yet:** jungle temples, villages, outposts, mansions,
  strongholds, mineshafts, ancient cities, trial chambers, dungeons. Their tables can be rolled
  (`World.loot(table, seed)`), but the per-structure loot seeds need their piece layouts (M3).
- **location_check** (abandoned camp and mineshaft tables) is evaluated on noise-biome resolution.
- **Portal frames** are the 4×5 frame that holds the most template obsidian; for broken templates
  with several equally good frames the best one for the seed is taken.
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
block-level output, Nether and End. See the milestones in AGENTS.md and PLANS.md.
