# GUI guide

Start with `python -m sft` (or `sft`). Every panel is a dock or splitter: resize, detach or close it.

## Toolbar

`version` (prepared data folders) · `mode` ([Search modes](Search-Modes.md)) · `start` · `count`
(`10M`, `2^48`, ...) · `threads` · `max` (stop after N matches, 0 = unlimited) ·
**Estimate** (time a sample, project total time and match rate) · **Start** (calibrates, then
runs) · **Stop** · **Resume** (continue from the exact stop point) · **Export** · **Tools**:
- *Scan map seed for the query*: every place in the map's seed where the query matches, as numbered
  cyan pins and a list in the log ([Search modes](Search-Modes.md#one-seed-scan-loot-placement));
- *Crack seed from structures*: structure coordinates in, matching seeds into the results table;
- *Placement statistics*: odds and nearest-distance spread of one structure set around the map centre;
- *Search sister seeds of map seed*: the query over the 65,536 seeds with the same structures.

## Query editor (left)

- Buttons add `and` / `or` / `not` groups. **+ condition** adds any condition with sensible
  defaults, grouped by structures, biomes, terrain, loot, scores and plugins. A new condition goes
  into the selected group, next to the selected condition, or wraps a lone root in an `and`.
  **delete** removes the selected node.
- **presets** replaces the query with a ready-made one: speedrun filters (village plus portal,
  near-complete portal frames, treasure diamonds, temple and close stronghold), witch-hut and
  monument clusters, slime farms, mushroom islands, all-biome spawns, structure bingo and more.
- Selecting a node shows its parameters. Edit values as Python literals (`300`, `'village'`,
  `(0, 0)`, `['plains', 'meadow']`).
- The text box below is the same query as text. Edits in either view update the other. Errors show
  in red under the editor.
- The reference list holds every structure (with its set; `~approx` where not exact) and every
  biome. Double-click to insert the name at the text cursor.

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

Engine-rendered biome map of one seed. Hover for coordinates, biome and nearby structures in the
status bar. Drag to pan, wheel to zoom (1 to 256 blocks per pixel). `y` picks the biome layer
(cave biomes need low y). **height shading** shades by exact surface height (slower).
**structures** shows markers for the sets ticked under **sets** (mineshafts and buried treasure are
off by default because they appear in nearly every chunk); `?` marks approximate positions.
Markers are hidden when zoomed out past 12k blocks. **attempts** overlays every placement attempt of
one set: green squares generate, red crosses fail; for strongholds the eight ring bands are drawn.

Letters: V village, P outpost, S stronghold, M monument, W mansion, A ancient city, T trial
chambers, R ruined portal, D desert pyramid, J jungle temple, H swamp hut, I igloo, s shipwreck,
o ocean ruin, $ buried treasure, t trail ruins, c abandoned camp, m mineshaft.

## Log / plugins (bottom)

The log records estimates, starts and stops, re-checks and exports. The plugins tab lists each
plugin, its native library, its Python module and any load error. Panels that plugins add appear as
extra tabs here.
