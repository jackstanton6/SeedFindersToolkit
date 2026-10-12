# Seed Finders Toolkit

A seed finder for advanced users, built around a bit-exact port of Minecraft Java 26.3 world
generation. Write a query (structures, clusters and chains of structures, biomes, islands, terrain,
slime chunks, chest loot, plugin filters, or a score to maximize), and a Rust engine scans billions
of seeds in parallel while the PySide6 GUI shows progress, rate, estimated total time, ETA and the
odds of a match. It also scans one seed for every place a pattern occurs and cracks seeds from
structures seen in a world.

## Status

| Layer | State | Verified against the real game |
|---|---|---|
| RNG, 26.x float noise, density functions | done | bit-exact climate values on 12k points |
| Biomes, point and chunk (volume) paths | done | 12k points + 80 chunk columns, 4 seeds |
| Terrain heights incl. aquifers (`WORLD_SURFACE_WG`, `OCEAN_FLOOR_WG`) | done | 3,200 columns |
| Structure placement + start validity (all overworld types) | done | ~1,100 starts, 4 seeds, incl. all 128 stronghold rings |
| Piece details (igloo basements, shipwreck/portal templates, ocean ruin clusters) | done | 327 pieces |
| Slime chunks | done | 80k chunks |
| Chest loot: tables, and chest seeds for buried treasure, shipwrecks, ruined portals, igloos, desert pyramids | done | 432 fills + 138 chests in real worlds (dedicated server) |
| Ruined portal obsidian and frame completeness | done | 40 portals in real worlds |
| Mineshaft depth | approximate (placement exact) | M3 |
| Spawn point, surface blocks, features (trees, cane, cacti, ores), carvers, full blocks | planned | M2 to M4 |
| CUDA brute-force stage | planned | n/a |

Each "verified" row means an oracle test that runs the actual 26.3 server classes passes with zero
mismatches. See [docs/wiki/Accuracy.md](docs/wiki/Accuracy.md).

## Install (from source: Windows, Linux, macOS)

Requires Python 3.10+, a Rust toolchain (`rustup`), and Java 21+ (only for the one-time data setup).

```sh
python -m venv .venv
.venv/Scripts/activate            # Windows; on Linux/macOS: source .venv/bin/activate
pip install -e .                  # builds the Rust engine (maturin) and installs PySide6
python -m sft data 26.3           # downloads the official server jar, extracts worldgen data
python -m sft                     # GUI
```

No Mojang code or data is in this repository. `python -m sft data <version>` downloads the official
server jar from Mojang (sha1-checked), extracts worldgen JSON, tags and structure templates into
`data/<version>/`, and runs the vanilla data generator for the biome parameter table.

## Quick taste (CLI)

```sh
python -m sft search "village(within=200) and near('village', 'pillager_outpost', dist=120, within=600)" --count 2000000
python -m sft search "biome('mushroom_fields', within=300) and monument(within=800)" --mode structure_seeds --count 100000
python -m sft search "maximize(island(biomes=['mushroom_fields'], within=3000))" --count 100000 --max 10
python -m sft search "portal_frame(within=150) + loot('ruined_portal', 'obsidian', within=150) >= 10 and village(within=300)"
python -m sft check 12345 "stronghold(within=1500)"
python -m sft locate 12345 village --radius 2000
python -m sft loot 12345 buried_treasure --radius 1500
python -m sft scan 12345 "village(within=150) >= 2" --within 10000 --step 128
python -m sft crack swamp_hut:-1200,840 desert_pyramid:310,-560 pillager_outpost:1040,520 shipwreck:-2160,-1136
```

## Documentation

- [Wiki home](docs/wiki/Home.md): concepts and workflow
- [Query language](docs/wiki/Query-Language.md): every condition and parameter
- [Search modes and performance](docs/wiki/Search-Modes.md)
- [GUI guide](docs/wiki/GUI.md)
- [Writing plugins (Rust + Python)](docs/wiki/Plugins.md)
- [Accuracy and verification](docs/wiki/Accuracy.md)
- [Adding a Minecraft version](docs/wiki/Versions.md)

## Credits and prior art

- World generation is ported from the 26.3 game code (decompiled with
  [Vineflower](https://github.com/Vineflower/vineflower)) and checked against the game itself.
- [cubiomes](https://github.com/Cubitect/cubiomes) (MIT, Cubitect) is the reference seed-finding
  library this project learned from: structure placement, the R-tree biome search and stronghold
  rings follow the same game algorithms it documents. No cubiomes code is included.
- The broader seedfinding community (Minecraft@Home, SeedfindingMC) for the 48-bit structure-seed
  and sister-seed techniques used by the search modes, and the seed-cracking community
  ([SeedCrackerX](https://github.com/19MisterX98/SeedcrackerX) and the libraries before it) for the
  idea of recovering structure seeds from observed structures. The low-bit lifting here was written
  from the game code; no third-party code is included.
- The Wilson score interval (E. B. Wilson, 1927) behind the rarity estimate.

Minecraft is a trademark of Mojang Studios. This project is not affiliated with Mojang or Microsoft.

## License

MIT
