"""Ground truth for structures and decoration from the official dedicated server (EULA as for
loot_oracle.py). A world is generated with a datapack that switches parts off, an area is force-
loaded to FULL chunks, and the chunks with generated neighbours on every side are written as
fixtures (palette + run-length blocks in y, z, x order, like the Oracle.java chunk modes).

  python tests/decoration_oracle.py MODE SEED X Z [RADIUS_CHUNKS]

Pick areas away from the spawn chunks: those tick while the server starts.

MODE: terrain   no features, no structures (must equal the TERRAIN chunks)
      structures  no features (structure pieces and the beardifier)
      features    no structures (the decoration features)
      full        everything
Writes cache/oracle/deco-MODE-SEED-X-Z.json.
"""
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import loot_oracle as lo  # noqa: E402
from export_oracle import all_blocks  # noqa: E402
from sft import data  # noqa: E402

OUT = Path(__file__).resolve().parent.parent / "cache" / "oracle"


def datapack(world: Path, features: bool, structures: bool):
    """world/datapacks/sft: biomes without features and/or structures that never fit a biome."""
    src = data.data_dir("26.3") / "minecraft" / "worldgen"
    pack = world / "datapacks" / "sft"
    (pack / "data" / "minecraft" / "worldgen").mkdir(parents=True, exist_ok=True)
    (pack / "pack.mcmeta").write_text(json.dumps({"pack": {"description": "sft oracle", "min_format": [121, 0], "max_format": [121, 0]}}))
    if not features:
        d = pack / "data" / "minecraft" / "worldgen" / "biome"
        d.mkdir(parents=True, exist_ok=True)
        for f in (src / "biome").glob("*.json"):
            b = json.loads(f.read_text())
            b["features"] = [[] for _ in b.get("features", [])]
            (d / f.name).write_text(json.dumps(b))
    if not structures:
        d = pack / "data" / "minecraft" / "worldgen" / "structure"
        for f in (src / "structure").rglob("*.json"):
            s = json.loads(f.read_text())
            s["biomes"] = []
            dest = d / f.relative_to(src / "structure")
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(json.dumps(s))


def main(mode: str, seed: int, x: int, z: int, radius: int = 4):
    level = f"deco-{mode}"
    world = lo.SERVER / level
    shutil.rmtree(world, ignore_errors=True)
    datapack(world, features=mode in ("features", "full"), structures=mode in ("structures", "full"))
    srv = lo.Server(level, seed, keep=True)
    cx, cz = x >> 4, z >> 4
    try:
        # no ticks: no random ticks (grass spread) and no scheduled fluid ticks, so the chunks are
        # exactly what generation (incl. postProcessGeneration's one fluid tick) left
        srv.run(["tick freeze"])
        # one extra ring so every compared chunk has FULL neighbours
        srv.run([f"forceload add {(cx - radius - 1) * 16} {(cz - radius - 1) * 16} {(cx + radius + 1) * 16 + 15} {(cz + radius + 1) * 16 + 15}"])
        srv.run(["say generating"])
        import time
        time.sleep(15)
        srv.save()
    finally:
        srv.stop()
    out = []
    for c in lo.chunks(world):
        k = (c["xPos"], c["zPos"])
        if c.get("Status") != "minecraft:full" or abs(k[0] - cx) > radius or abs(k[1] - cz) > radius:
            continue
        b = all_blocks(c)
        palette, runs, last, run = {}, [], None, 0
        for y in range(-64, 320):
            for lz in range(16):
                for lx in range(16):
                    i = palette.setdefault(b.get((lx, y, lz), "minecraft:air"), len(palette))
                    if i == last:
                        run += 1
                        continue
                    if run:
                        runs += [last, run]
                    last, run = i, 1
        runs += [last, run]
        out.append({"x": k[0], "z": k[1], "palette": list(palette), "runs": runs})
    path = OUT / f"deco-{mode}-{seed}-{x}-{z}.json"
    path.write_text(json.dumps({"seed": seed, "mode": mode, "chunks": out}))
    print(f"wrote {len(out)} chunks to {path}")


if __name__ == "__main__":
    a = sys.argv[1:]
    main(a[0], int(a[1]), int(a[2]), int(a[3]), int(a[4]) if len(a) > 4 else 4)
