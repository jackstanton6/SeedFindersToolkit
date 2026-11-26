# GUI guide

Start with `python -m sft` (or `sft`). Every panel is a dock or splitter: resize, detach or close it.
New here? Read [Getting started](Getting-Started.md) or take the tour (Help menu); it is offered
the first time the window opens.

## Toolbar

`version` (prepared data folders) · `mode` ([Search modes](Search-Modes.md)) · `range` (preset for
from/to) · `from` · `to` (inclusive; `10M`, `2^48`, ...) · `fixed` (the kept part of the seed in the
lower-48 / upper-16 modes) · **shuffle** (random order, no repeats) · `threads` · `max` (stop after N matches, default 10,000, 0 = unlimited) ·
**GPU** with its status: *CUDA compatible* (tick it to pre-filter on the graphics card) or *no
CUDA* (hover for the reason; see [Search modes](Search-Modes.md#gpu-cuda)) ·
**Estimate** (runs the search for 3 seconds and projects the total time and how rare matches are;
Stop ends it early) · **Start** (starts at once) · **Stop** · **Resume** (continue from the exact
stop point) · **Export** · **Tools**:
- *Scan map seed for the query*: every place in the map's seed where the query matches, as numbered
  cyan pins and a list in the log ([Search modes](Search-Modes.md#one-seed-scan-loot-placement));
- *Crack seed from structures*: structure coordinates in, matching seeds into the results table;
- *Placement statistics*: odds and nearest-distance spread of one structure set around the map centre;
- *Search sister seeds of map seed*: the query over the 65,536 seeds with the same structures.
- *Seed bits*: opens the **seed bits** panel (under the map). It splits the map seed into its upper 16
  bits (orange; what differs between sisters: biomes, terrain) and lower 48 bits (blue; the structure
  seed shared by all sisters) in decimal, hex or binary. Edit either half to jump to that seed; *search
  sisters* / *search this upper 16* fill the mode, fixed and range fields for you. Selecting a result
  tints every result that is a sister of it.

## Query editor (left)

The query is a stack of blocks that read like sentences.

- **Palette** (far left): every kind of block by category (logic, structures, biomes, terrain, loot,
  scores, plugins). Drag one onto the canvas, into any group, or double-click it to add it at the
  end. Hover for what it does.
- **Canvas**: the top block, **ALL of these**, holds the query. Its dropdown switches between
  **ALL** (every block inside must hold), **ANY** (at least one) and **NONE** (none of them). Groups
  nest. Each block's sentence has its options in place: structure, biome and item dropdowns (type
  to search), numbers, coordinates (`x 0 z 0` is spawn) and checkboxes. **more ▸** shows rarer
  options (a structure's start biome and piece tags, sample spacing and height of biome checks, an
  upper bound). A number left at its default is not written into the query.
- Each block: **⠿** drags it (into another group, out of one, or to reorder), **⋮** deletes,
  duplicates, wraps it in an ALL/ANY/NONE group, ungroups a group, or opens its help.
  **+ add a condition** under a group adds a block from a menu.
- **Rankings** (*scores*): *rank: highest first* and *rank: lowest first* hold one block whose
  number ranks the seeds (`maximize`/`minimize`). *somewhere in an area* (`anywhere`) moves the
  block inside to every spot of an area. *number comparison* takes an expression such as
  `village(within=500) - pillager_outpost(within=500)`.
- **✨ I want…** opens a wizard: pick a goal (a structure near spawn, two structures together,
  farms, a biome, a big island, terrain, chest items, slime chunks, a speedrun filter, a record),
  answer a few questions, and it adds the blocks (or replaces the query). **presets** loads
  ready-made queries; **undo** reverts the last change; **clear** starts over.
- Under the canvas, the query **as text** (the [query language](Query-Language.md)) and a summary
  in plain words. Editing the text updates the blocks; errors show in red.

## Stats (top centre)

Progress bar plus: units, rate, checks (total calculations), checks/s, prefilter pass, full evals,
matches, py rejected, elapsed, eta, est. total, est. rate, match rate, its 95% range, expected time
to the first match, and kept rows. Definitions are in [Search modes](Search-Modes.md#stats-explained).

## Results (centre)

Sortable table: seed, score (queries with `maximize`/`minimize`; the table then holds the live top
N, best first), match summary (`structure@x,z`, islands, anchors), full details JSON (including chest
contents for `loot` conditions). Up to 200k rows are shown; every match is kept for export. Click a row to show that seed on the map, centred on its
first match. Right-click: copy seeds, copy `/tp` to the first match, re-check (re-evaluates the seed
from scratch; the result goes to the log).

## Map (right)

Engine-rendered biome map of one seed. Hover for coordinates, biome (the block-level biome F3
shows) and nearby structures in the status bar. The gold diamond is the world spawn.

The layer box switches to **surface blocks** (the top block of the generated chunks: grass, sand,
snow, terracotta, water...) or **blocks at y** (the blocks at the `y` level; ore veins, their raw
blocks and filler stand out, everything else is dimmed). Block layers generate every chunk on
screen, so they need 4 blocks per pixel or less. Double-click the map to open the **chunk
inspector** for that chunk: the chunk from above at a chosen y and from the side at a chosen z,
hover for each block. Drag to pan, wheel to zoom (1 to 256 blocks per pixel). `y` picks the biome layer
(cave biomes need low y). **height shading** shades by exact surface height (slower).
**structures** shows markers for the sets ticked under **sets** (mineshafts and buried treasure are
off by default because they appear in nearly every chunk); `?` marks approximate positions.
Markers are hidden when zoomed out past 12k blocks. **attempts** overlays every placement attempt of
one set: green squares generate, red crosses fail; for strongholds the eight ring bands are drawn.

Letters: V village, P outpost, S stronghold, M monument, W mansion, A ancient city, T trial
chambers, R ruined portal, D desert pyramid, J jungle temple, H swamp hut, I igloo, s shipwreck,
o ocean ruin, $ buried treasure, t trail ruins, c abandoned camp, m mineshaft.

## Help

**Help > Getting started** (F1) opens this wiki inside the app. The block menu's *help* entry jumps
to that condition in the [query language](Query-Language.md). **Help > Take the tour** walks
through the window again.

## Log / plugins (bottom)

The log records estimates, starts and stops (and whether the GPU pre-filter is on), engine errors,
re-checks and exports. The plugins tab lists each
plugin, its native library, its Python module and any load error. Panels that plugins add appear as
extra tabs here.
