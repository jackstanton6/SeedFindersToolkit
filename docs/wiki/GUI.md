# GUI guide

Start with `python -m sft` (or `sft`). Every panel is a dock or splitter: resize, detach or close it.

## Toolbar

`version` (prepared data folders) · `mode` ([Search modes](Search-Modes.md)) · `start` · `count`
(`10M`, `2^48`, ...) · `threads` · `max` (stop after N matches, 0 = unlimited) ·
**Estimate** (time a sample, project total time and match rate) · **Start** (calibrates, then
runs) · **Stop** · **Resume** (continue from the exact stop point) · **Export**.

## Query editor (left)

- Buttons add `and` / `or` / `not` groups and each condition type with sensible defaults. A new
  condition goes into the selected group, next to the selected condition, or wraps a lone root in an
  `and`. **delete** removes the selected node.
- Selecting a node shows its parameters. Edit values as Python literals (`300`, `'village'`,
  `(0, 0)`, `['plains', 'meadow']`).
- The text box below is the same query as text. Edits in either view update the other. Errors show
  in red under the editor.
- The reference list holds every structure (with its set; `~approx` where not exact) and every
  biome. Double-click to insert the name at the text cursor.

## Stats (top centre)

Progress bar plus: units, rate, checks (total calculations), checks/s, prefilter pass, full evals,
matches, py rejected, elapsed, eta, est. total, est. rate. Definitions are in
[Search modes](Search-Modes.md#stats-explained).

## Results (centre)

Sortable table: seed, match summary (`structure@x,z`), full details JSON. Up to 200k rows are
shown; every match is kept for export. Click a row to show that seed on the map, centred on its
first match. Right-click: copy seeds, copy `/tp` to the first match, re-check (re-evaluates the seed
from scratch; the result goes to the log).

## Map (right)

Engine-rendered biome map of one seed. Hover for coordinates, biome and nearby structures in the
status bar. Drag to pan, wheel to zoom (1 to 256 blocks per pixel). `y` picks the biome layer
(cave biomes need low y). **height shading** shades by exact surface height (slower).
**structures** shows markers for the sets ticked under **sets** (mineshafts and buried treasure are
off by default because they appear in nearly every chunk); `?` marks approximate positions.
Markers are hidden when zoomed out past 12k blocks.

Letters: V village, P outpost, S stronghold, M monument, W mansion, A ancient city, T trial
chambers, R ruined portal, D desert pyramid, J jungle temple, H swamp hut, I igloo, s shipwreck,
o ocean ruin, $ buried treasure, t trail ruins, c abandoned camp, m mineshaft.

## Log / plugins (bottom)

The log records estimates, starts and stops, re-checks and exports. The plugins tab lists each
plugin, its native library, its Python module and any load error. Panels that plugins add appear as
extra tabs here.
