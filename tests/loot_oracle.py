"""Chest loot ground truth from the official dedicated server (run headless under cache/oracle-server;
the project owner accepted the Minecraft EULA for this).

  python tests/loot_oracle.py fill            chests placed with explicit LootTable/LootTableSeed,
                                              unpacked, contents read from the region files
  python tests/loot_oracle.py world SEED...   structures near spawn generated for real; every
                                              unopened chest's LootTable/LootTableSeed read back

Writes cache/oracle/loot-fill.json and cache/oracle/loot-world-<seed>.json for engine/tests/oracle.rs.
"""
import gzip
import io
import json
import queue
import re
import shutil
import struct
import subprocess
import sys
import threading
import time
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
SERVER = ROOT / "cache" / "oracle-server"
OUT = ROOT / "cache" / "oracle"
VERSION = "26.3"
FILL_TABLES = [
    "buried_treasure", "ruined_portal", "shipwreck_map", "shipwreck_supply", "shipwreck_treasure", "desert_pyramid",
    "jungle_temple", "igloo_chest", "underwater_ruin_big", "underwater_ruin_small", "simple_dungeon", "pillager_outpost",
    "stronghold_library", "stronghold_corridor", "woodland_mansion", "ancient_city", "abandoned_mineshaft", "village/village_weaponsmith",
]
WORLD_STRUCTURES = ["buried_treasure", "ruined_portal", "shipwreck", "igloo", "desert_pyramid", "ocean_ruin", "jungle_temple"]


# --- NBT and region files -------------------------------------------------------------------------

def read_nbt(b: bytes):
    s = io.BytesIO(b)

    def num(fmt, size):
        return struct.unpack(fmt, s.read(size))[0]

    def payload(t):
        if t == 1: return num(">b", 1)
        if t == 2: return num(">h", 2)
        if t == 3: return num(">i", 4)
        if t == 4: return num(">q", 8)
        if t == 5: return num(">f", 4)
        if t == 6: return num(">d", 8)
        if t == 7: return s.read(num(">i", 4))
        if t == 8: return s.read(num(">H", 2)).decode("utf-8", "replace")
        if t == 9:
            et, n = num(">b", 1), num(">i", 4)
            return [payload(et) for _ in range(n)]
        if t == 10:
            d = {}
            while (tt := s.read(1)[0]) != 0:
                key = s.read(num(">H", 2)).decode("utf-8", "replace")  # before the value: d[k] = v evaluates v first
                d[key] = payload(tt)
            return d
        if t in (11, 12):
            n = num(">i", 4)
            size = 4 if t == 11 else 8
            return list(struct.unpack(f">{n}{'i' if t == 11 else 'q'}", s.read(size * n)))
        raise ValueError(f"tag {t}")
    t = s.read(1)[0]
    s.read(num(">H", 2))
    return payload(t)


def chunks(world: Path):
    """Every chunk NBT in the overworld region files."""
    for f in sorted((world / "dimensions/minecraft/overworld/region").glob("r.*.mca")):  # 26.x layout
        data = f.read_bytes()
        for i in range(1024):
            off, sectors = int.from_bytes(data[4 * i:4 * i + 3], "big"), data[4 * i + 3]
            if off == 0 or sectors == 0:
                continue
            length, comp = struct.unpack(">iB", data[off * 4096:off * 4096 + 5])
            raw = data[off * 4096 + 5:off * 4096 + 4 + length]
            raw = zlib.decompress(raw) if comp == 2 else gzip.decompress(raw) if comp == 1 else raw
            yield read_nbt(raw)


def blocks(chunk: dict, wanted: set[str]):
    """(x, y, z, name) of blocks in `wanted` from a chunk's packed section palettes."""
    for sec in chunk.get("sections", []):
        bs = sec.get("block_states", {})
        # 26.x: plain ids, or {"": id} when the list mixes plain ids and states with properties
        pal = [p if isinstance(p, str) else p.get("", p.get("Name", p.get("id"))) for p in bs.get("palette", [])]
        if not wanted.intersection(pal):
            continue
        bits = max(4, (len(pal) - 1).bit_length())
        per, mask = 64 // bits, (1 << bits) - 1
        data = [d % 2**64 for d in bs.get("data", [])]
        for i in range(4096):
            name = pal[(data[i // per] >> (i % per * bits)) & mask] if data else pal[0]
            if name in wanted:
                yield chunk["xPos"] * 16 + (i & 15), sec["Y"] * 16 + (i >> 8), chunk["zPos"] * 16 + (i >> 4 & 15), name


def canonical(item: dict) -> dict:
    """Region item NBT -> the engine's Stack JSON (item, count, enchantments, extra)."""
    c = item.get("components", {})
    out = {"item": item["id"], "count": item.get("count", 1)}
    ench = {**c.get("minecraft:enchantments", {}), **c.get("minecraft:stored_enchantments", {})}
    ench = ench.get("levels", ench)  # older layout wrapped the map in "levels"
    if ench:
        out["enchantments"] = sorted([k, v] for k, v in ench.items())
    extra = []
    if "minecraft:damage" in c:
        extra.append(f"damage:{c['minecraft:damage']}")
    if "minecraft:potion_contents" in c:
        p = c["minecraft:potion_contents"]
        extra.append(f"potion:{p['potion'] if isinstance(p, dict) else p}")
    for e in c.get("minecraft:suspicious_stew_effects", []):
        extra.append(f"stew:{e['id']}:{e.get('duration', 160)}")
    if "minecraft:instrument" in c:
        extra.append(f"instrument:{c['minecraft:instrument']}")
    if "minecraft:ominous_bottle_amplifier" in c:
        extra.append(f"amplifier:{c['minecraft:ominous_bottle_amplifier']}")
    if extra:
        out["extra"] = sorted(extra)
    return out


# --- server -------------------------------------------------------------------------------------

class Server:
    def __init__(self, level: str, seed: int):
        from sft import data
        jar = data.download_server_jar(VERSION)
        SERVER.mkdir(parents=True, exist_ok=True)
        (SERVER / "eula.txt").write_text("eula=true\n")
        props = {
            "level-name": level, "level-seed": str(seed), "online-mode": "false", "spawn-protection": "0",
            "max-tick-time": "-1", "view-distance": "3", "simulation-distance": "3", "enable-rcon": "false",
            "server-port": "25599", "sync-chunk-writes": "true", "region-file-compression": "deflate",
        }
        (SERVER / "server.properties").write_text("".join(f"{k}={v}\n" for k, v in props.items()))
        shutil.rmtree(SERVER / level, ignore_errors=True)
        self.level = SERVER / level
        self.p = subprocess.Popen(["java", "-Xmx6G", "-jar", str(jar), "nogui"], cwd=SERVER, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        self.lines: queue.Queue[str] = queue.Queue()
        threading.Thread(target=lambda: [self.lines.put(line) for line in self.p.stdout], daemon=True).start()
        self.n = 0
        self.wait(r"Done \(", 600)

    def wait(self, pattern: str, timeout=600) -> str:
        end = time.time() + timeout
        while time.time() < end:
            try:
                line = self.lines.get(timeout=1)
            except queue.Empty:
                if self.p.poll() is not None:
                    raise RuntimeError("server exited")
                continue
            if re.search(pattern, line):
                return line
            if "ERROR" in line or "Exception" in line:
                print("  server:", line.rstrip())
        raise TimeoutError(pattern)

    def run(self, commands: list[str]):
        """Sends commands and waits until the server has executed all of them."""
        for c in commands:
            self.p.stdin.write(c + "\n")
        self.n += 1
        self.p.stdin.write(f"say sync-{self.n}\n")
        self.p.stdin.flush()
        self.wait(rf"sync-{self.n}\b")

    def save(self):
        self.p.stdin.write("save-all flush\n")
        self.p.stdin.flush()
        self.wait(r"Saved the game", 300)

    def stop(self):
        self.p.stdin.write("stop\n")
        self.p.stdin.flush()
        self.p.wait(300)


# --- modes --------------------------------------------------------------------------------------

def fill(per_table=24):
    srv = Server("loot-fill", 0)
    try:
        srv.run(["forceload add 0 0 63 63"])
        time.sleep(10)  # let the forced chunks generate
        cases, cmds = [], []
        for i in range(len(FILL_TABLES) * per_table):
            t, k = FILL_TABLES[i // per_table], i % per_table
            seed = (k * 0x9E3779B97F4A7C15 + zlib.crc32(t.encode())) % 2**64
            seed = seed - 2**64 if seed >= 2**63 else seed  # signed, both halves
            x, z, y = 2 * (i % 32), 2 * (i // 32 % 32), 100 + 2 * (i // 1024)
            cases.append({"table": f"minecraft:chests/{t}", "seed": seed, "pos": [x, y, z]})
            cmds.append(f'setblock {x} {y} {z} minecraft:chest{{LootTable:"minecraft:chests/{t}",LootTableSeed:{seed}L}} replace')
            cmds.append(f"item replace block {x} {y} {z} container.0 from block {x} {y} {z} container.0")
        for j in range(0, len(cmds), 200):
            srv.run(cmds[j:j + 200])
        srv.save()
        by_pos = {}
        for ch in chunks(srv.level):
            for be in ch.get("block_entities", []):
                by_pos[(be["x"], be["y"], be["z"])] = be
        for c in cases:
            be = by_pos.get(tuple(c["pos"]))
            if be is None or "LootTable" in be:
                raise RuntimeError(f"chest not unpacked: {c} -> {be}")
            c["slots"] = [None] * 27
            for it in be.get("Items", []):
                c["slots"][it["Slot"]] = canonical(it)
    finally:
        srv.stop()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "loot-fill.json").write_text(json.dumps(cases))
    print(f"wrote {len(cases)} fills")


def world(seed: int, radius=1200):
    import sft
    w = sft.world(VERSION)
    found = w.structures_near(seed, 0, 0, radius, WORLD_STRUCTURES)
    chunk_set = set()
    for name, x, y, z, _ in found:
        for dx in range(-3, 4):  # pieces spill over a few chunks (ruin clusters, pyramids, wrecks)
            for dz in range(-3, 4):
                chunk_set.add(((x >> 4) + dx, (z >> 4) + dz))
    srv = Server(f"loot-world-{seed}", seed)
    try:
        cmds = [f"forceload add {cx * 16} {cz * 16}" for cx, cz in sorted(chunk_set)]
        for j in range(0, len(cmds), 100):
            srv.run(cmds[j:j + 100])
        for _ in range(120):  # wait until every forced chunk is fully generated
            time.sleep(10)
            srv.save()
            full = {(ch["xPos"], ch["zPos"]) for ch in chunks(srv.level) if ch.get("Status") == "minecraft:full"}
            missing = chunk_set - full
            print(f"  {len(chunk_set) - len(missing)}/{len(chunk_set)} chunks generated")
            if not missing:
                break
        chests, obsidian = [], []
        for ch in chunks(srv.level):
            for be in ch.get("block_entities", []):
                if "LootTable" in be:
                    chests.append([be["x"], be["y"], be["z"], be["id"], be["LootTable"], be.get("LootTableSeed", 0)])
            obsidian += [[x, y, z, n == "minecraft:crying_obsidian"] for x, y, z, n in blocks(ch, {"minecraft:obsidian", "minecraft:crying_obsidian"})]
    finally:
        srv.stop()
    starts = [[n, x, y, z] for n, x, y, z, _ in found]
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / f"loot-world-{seed}.json").write_text(json.dumps({"seed": seed, "radius": radius, "starts": starts, "chests": sorted(chests), "obsidian": sorted(obsidian)}))
    print(f"seed {seed}: {len(found)} structures, {len(chests)} loot containers")


if __name__ == "__main__":
    if sys.argv[1:2] == ["fill"]:
        fill()
    elif sys.argv[1:2] == ["world"]:
        for s in sys.argv[2:] or ["0", "12345"]:
            world(int(s))
    else:
        print(__doc__)
