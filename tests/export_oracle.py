"""Round trip of `python -m sft export --mca` through the official dedicated server: export an area,
open it in the server (the chunks are force-loaded, then saved), and check that every block the
server writes back is the block we wrote. Needs Java and data/26.3 (EULA as for loot_oracle.py).

  python tests/export_oracle.py [SEED]
"""
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import loot_oracle as lo  # noqa: E402


from sft.worldfile import default_props  # noqa: E402

DEFAULTS = default_props()


def palette_name(p) -> str:
    """A 26.x palette entry (an id, {"": id} or {"id", "properties"}) as the full state
    "name[k=v,...]" with every property, sorted."""
    name, props = (p, {}) if isinstance(p, str) else (p[""], {}) if "" in p else (p["id"], p.get("properties", {}))
    full = {**DEFAULTS.get(name, {}), **props}
    return name + ("[" + ",".join(f"{k}={v}" for k, v in sorted(full.items())) + "]" if full else "")


def all_blocks(chunk: dict) -> dict:
    out = {}
    for sec in chunk.get("sections", []):
        bs = sec.get("block_states", {})
        pal = [palette_name(p) for p in bs.get("palette", [])]
        if not pal:
            continue
        bits = max(4, (len(pal) - 1).bit_length())
        per, mask = 64 // bits, (1 << bits) - 1
        data = [d % 2**64 for d in bs.get("data", [])]
        for i in range(4096):
            out[(i & 15, sec["Y"] * 16 + (i >> 8), i >> 4 & 15)] = pal[(data[i // per] >> (i % per * bits)) & mask] if data else pal[0]
    return out


def main(seed: int):
    level = "export-roundtrip"
    world = lo.SERVER / level
    shutil.rmtree(world, ignore_errors=True)
    lo.SERVER.mkdir(parents=True, exist_ok=True)
    # the area around the spawn, so the server loads it anyway
    import sft
    sx, sz = sft.world("26.3").spawn(seed)
    x0, z0 = (sx >> 4) * 16 - 32, (sz >> 4) * 16 - 32
    subprocess.run([sys.executable, "-m", "sft", "export", str(seed), str(x0), str(z0), str(x0 + 79), str(z0 + 79), "--mca", str(world)], check=True)
    ours = {(c["xPos"], c["zPos"]): all_blocks(c) for c in lo.chunks(world)}
    srv = lo.Server(level, seed, keep=True)
    try:
        srv.run([f"forceload add {x0} {z0} {x0 + 79} {z0 + 79}"])
        srv.save()
    finally:
        srv.stop()
    theirs = {(c["xPos"], c["zPos"]): (c.get("Status"), all_blocks(c)) for c in lo.chunks(world)}
    # features of the chunks the server generates around the export may spill one chunk into it,
    # so only chunks with exported neighbours on every side are compared
    inner = {k: b for k, b in ours.items() if all((k[0] + dx, k[1] + dz) in ours for dx in (-1, 0, 1) for dz in (-1, 0, 1))}
    bad = 0
    for k, b in inner.items():
        status, got = theirs.get(k, (None, {}))
        diff = sum(1 for p, n in b.items() if got.get(p) != n)
        if diff:
            ex = next(p for p, n in b.items() if got.get(p) != n)
            print(f"chunk {k} ({status}): {diff} blocks differ, e.g. {ex}: wrote {b[ex]} read {got.get(ex)}")
            bad += 1
    print(f"{len(ours)} exported chunks, {len(inner)} inner ones compared ({len(inner) * 98304:,} blocks), {bad} changed by the server")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main(int(sys.argv[1]) if len(sys.argv) > 1 else 12345)
