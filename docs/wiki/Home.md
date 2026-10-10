# Seed Finders Toolkit wiki

Pages: [Query language](Query-Language.md) · [Search modes](Search-Modes.md) · [GUI](GUI.md) ·
[Plugins](Plugins.md) · [Accuracy](Accuracy.md) · [Versions](Versions.md)

## Concepts you need

- **World seed vs structure seed.** Structure *placement* (which chunk a village is attempted in)
  depends only on the low 48 bits of the seed. Biomes, terrain and therefore structure *validity*
  depend on all 64 bits. Each 48-bit structure seed has 65,536 "sister" world seeds that share
  every placement.
- **Two-stage evaluation.** Every query has a cheap 48-bit pre-check (placement only) and a full
  check (biomes, heights, structure start validity). The full check only runs when the pre-check
  passes. The stats panel shows how much the pre-check filters out ("prefilter pass").
- **Exact vs approximate.** Everything marked verified in [Accuracy](Accuracy.md) matches the real
  game bit-for-bit. Where something is approximate (currently mineshaft depth), the GUI and CLI mark
  it with `~approx` / `?`.
- **Structure positions.** Distances are measured from the structure's *locate position*: its start
  chunk corner plus the set's `locate_offset`, as `/locate` reports it. Match details report the
  actual start position (x, y, z) the game uses for its biome check.

## Workflow

1. `python -m sft data 26.3` once (downloads the official server jar, extracts worldgen data).
2. Write a query: GUI tree/text editor, or the CLI. Start cheap: put placement-heavy conditions
   (structures, `near`) first and expensive ones (large-radius `biome`, `height`) after them.
3. **Estimate.** Times a sample and projects the total run time and match rate. If the projected
   match rate is tiny, switch to structure-seed mode (see [Search modes](Search-Modes.md)).
4. **Start.** Watch rate, checks, prefilter pass, matches, elapsed and ETA. Stop at any time; Resume
   continues from the exact unit where it stopped.
5. Inspect matches on the map (click a row), right-click to copy a seed or a `/tp`, re-check
   (verify) a seed, export results as CSV/JSON/TXT or a plugin format.

## Examples

```python
# a village and an outpost both close to spawn, and close to each other
village(within=250) and near("village", "pillager_outpost", dist=150, within=400)

# mushroom island near spawn with a monument next to it
biome("mushroom_fields", within=400) and monument(within=600)

# three swamp huts within 400 blocks of 0,0 (quad-hut style hunting starts here)
swamp_hut(within=400, min=3)

# first-ring stronghold near spawn, but no ocean at spawn
stronghold(within=1200) and not biome("#is_ocean", within=0)

# tall mountains at spawn
height(at=(0, 0), min=180) and biome(["jagged_peaks", "frozen_peaks"], within=64)
```
