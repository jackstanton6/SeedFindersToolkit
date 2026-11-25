# Getting started

Seed Finders Toolkit finds Minecraft Java seeds by what is in them: structures, biomes, terrain and
chest loot. Everything it reports is computed the way the game computes it, so a seed it finds is a
seed you can type into Minecraft.

## Your first search in four steps

1. **Say what you want.** Press **I want…** above the query and pick a goal, for example *A
   structure near spawn*. Answer the questions and press **Add to my query**. Or drag a block from
   the list on the left onto the canvas, or pick a ready-made query from **presets**.
2. **Adjust the blocks.** Each block reads like a sentence. Change the dropdowns and numbers in
   place: *at least 1 village within 300 blocks of x 0 z 0*. The line under the canvas says what
   the whole query finds, in plain words.
3. **Choose the seeds.** In the toolbar, **mode** *range* searches seeds one after another from
   **start**; **count** is how many (`10M`, `1G`, `2^48`). For structure-heavy queries, *structure
   seeds* is usually much faster (see [Search modes](Search-Modes.md)).
4. **Estimate, then Start.** **Estimate** runs the search for 3 seconds and tells you how long the
   whole range takes and how rare matches are. **Start** runs it. Matches appear in the results
   table; click one to see it on the map. **Stop** and **Resume** pick up exactly where you left off.

## Combining conditions

The top block, **ALL of these**, means a seed must match every block inside it. Switch it to
**ANY of these** (at least one) or **NONE of these** (none of them), or add a group from the
*logic* section of the list to nest them: *ALL of (a village, ANY of (a monument, a mushroom
island))*.

- Drag a block by its **⠿** grip to move it, also into or out of groups.
- **⋮** on a block deletes, duplicates or wraps it in a group.
- **more ▸** shows the rarer options of a block (a biome filter for structures, tags such as an
  igloo basement, the sample spacing of biome checks).
- **undo** reverts the last change.

## Rankings instead of filters

The *scores* section has **rank: highest first** and **rank: lowest first**. Put a block inside,
for example *nearest structure* set to stronghold, and the search keeps the best seeds instead of
every match (the 100 best by default; set **max** to change it). The score column shows each seed's
number.

## The text box

Under the blocks is the same query as text, for example
`village(within=300) and pillager_outpost(within=600)`. Copy it to share a query, or type there:
the blocks update as you type. Every condition is listed in [Query language](Query-Language.md).

## GPU

If the toolbar shows **CUDA compatible**, tick **GPU**. The graphics card then checks the parts of
the query that depend on the seed alone (where structures can be placed, slime chunks) and the
processor checks biomes and terrain for what is left. Results are identical either way. It helps
most when those seed-only conditions are rare, and in seed cracking (*Tools*).

## When something takes long

- The progress bar, speed and *eta* update while the search runs, also for slow queries.
- *first match* is the expected time until the next match. If it says days, make the query looser
  (bigger distances, fewer conditions) or use *structure seeds* mode.
- Large-area biome, island and terrain conditions are the slowest. Keep their radius small;
  structure conditions are checked first anyway.

## Next

- [Using the window](GUI.md): every panel and tool.
- [Query language](Query-Language.md): every condition and its options.
- [Search modes](Search-Modes.md): ranges, structure seeds, scanning one seed, cracking.
