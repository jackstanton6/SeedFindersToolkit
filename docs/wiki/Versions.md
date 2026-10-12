# Minecraft versions

Version support has a data half and a code half.

**Data (automatic).** `python -m sft data <version>` downloads that version's official server jar,
extracts `worldgen/` (noise, density functions, noise settings, structures, structure sets, template
pools), the worldgen biome tags and the structure templates into `data/<version>/`, and runs the
vanilla data generator for the multi-noise biome table. Version changes that live in data (new
biomes, retuned noises, new or moved structures, new jigsaw pools) need no code change. Pick the
version in the GUI toolbar or pass `--version` on the CLI.

**Code (ported by hand, then verified).** RNG, noise, the density-function node types, the climate
R-tree, the aquifer, structure placement types and each structure type's generation point are Rust
ports of game classes. When a version changes those classes (as 26.x did with float noise and the
compiled density functions), the port must follow.

## Adding a version

1. `python -m sft data <version>`, then `python tests/run_oracle.py <version>` and
   `python tests/run_oracle.py <version> structures`.
2. `cargo test --release -- --ignored`. If it passes, the version is supported as-is.
3. If not, decompile the new jar (see AGENTS.md), diff the classes listed in the AGENTS.md layout
   against the previous version, and port the changes. Where versions diverge, branch on a version
   field in `World`. Keep the old behaviour for older data folders.
4. If the overworld router reaches a density function type the engine doesn't know, loading the
   version fails with that function's name and type. Structure types without a port show as
   not exact (`World.structures()` reports `exact = False`; the map marks them with `?`) and never generate.

Older versions (1.18 to 1.21) use double-precision noise and the pre-26 density functions. Supporting
them means bringing back those code paths behind a version switch, verified the same way.
