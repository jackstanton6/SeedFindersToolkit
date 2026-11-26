"""Writing generated areas to files Minecraft tools read: Anvil region files (.mca) with a minimal
level.dat, and Sponge schematics (.schem, version 3). Standard library only."""
import gzip
import io
import math
import struct
import time
import zlib
from pathlib import Path

# --- NBT -------------------------------------------------------------------------------------------
# Values are written by Python type; wrap a value to pick another tag.


class Byte(int): pass
class Short(int): pass
class Int(int): pass
class LongArray(list): pass
class IntArray(list): pass
class ByteArray(bytes): pass


def _tag(v) -> int:
    if isinstance(v, Byte): return 1
    if isinstance(v, Short): return 2
    if isinstance(v, Int): return 3
    if isinstance(v, bool): return 1
    if isinstance(v, int): return 4
    if isinstance(v, float): return 6
    if isinstance(v, ByteArray): return 7
    if isinstance(v, str): return 8
    if isinstance(v, LongArray): return 12
    if isinstance(v, IntArray): return 11
    if isinstance(v, list): return 9
    if isinstance(v, dict): return 10
    raise TypeError(f"no NBT tag for {type(v)}")


def _payload(out: io.BytesIO, v) -> None:
    t = _tag(v)
    if t == 1: out.write(struct.pack(">b", int(v)))
    elif t == 2: out.write(struct.pack(">h", v))
    elif t == 3: out.write(struct.pack(">i", v))
    elif t == 4: out.write(struct.pack(">q", v))
    elif t == 6: out.write(struct.pack(">d", v))
    elif t == 7:
        out.write(struct.pack(">i", len(v)))
        out.write(v)
    elif t == 8:
        b = v.encode("utf-8")
        out.write(struct.pack(">H", len(b)))
        out.write(b)
    elif t == 9:
        et = _tag(v[0]) if v else 0
        out.write(struct.pack(">bi", et, len(v)))
        for x in v:
            _payload(out, x)
    elif t == 10:
        for k, x in v.items():
            _named(out, k, x)
        out.write(b"\0")
    elif t == 11:
        out.write(struct.pack(f">i{len(v)}i", len(v), *v))
    elif t == 12:
        out.write(struct.pack(f">i{len(v)}q", len(v), *v))


def _named(out: io.BytesIO, name: str, v) -> None:
    out.write(struct.pack(">b", _tag(v)))
    b = name.encode("utf-8")
    out.write(struct.pack(">H", len(b)))
    out.write(b)
    _payload(out, v)


def nbt(root: dict, name: str = "") -> bytes:
    out = io.BytesIO()
    _named(out, name, root)
    return out.getvalue()


# --- block states ----------------------------------------------------------------------------------

def default_props(version: str = "26.3") -> dict[str, dict[str, str]]:
    """Each block's default state properties, from the data generator's block report."""
    import json
    from sft import data
    report = json.loads((data.data_dir(version) / "reports" / "blocks.json").read_text())
    return {name: next((s.get("properties", {}) for s in b["states"] if s.get("default")), {}) for name, b in report.items()}


def parse_state(s: str) -> tuple[str, dict[str, str]]:
    name, _, props = s.partition("[")
    return name, dict(p.split("=", 1) for p in props.rstrip("]").split(",")) if props else {}


def palette_nbt(states: list[str], defaults: dict) -> list:
    """26.x block palettes: a default state is its id, any other {"id", "properties": the non-default
    ones}; a list mixing both wraps the ids as {"": id} (NBT lists hold one type)."""
    out = []
    for s in states:
        name, props = parse_state(s)
        changed = {k: v for k, v in props.items() if defaults.get(name, {}).get(k) != v}
        out.append({"id": name, "properties": changed} if changed else name)
    if any(isinstance(p, dict) for p in out):
        out = [p if isinstance(p, dict) else {"": p} for p in out]
    return out


def _pack(indices: list[int], bits: int) -> LongArray:
    """PalettedContainer storage (SimpleBitStorage): entries never span longs."""
    per = 64 // bits
    out = []
    for i in range(0, len(indices), per):
        v = 0
        for j, x in enumerate(indices[i:i + per]):
            v |= x << (j * bits)
        out.append(v - (1 << 64) if v >= 1 << 63 else v)
    return LongArray(out)


# --- Anvil -----------------------------------------------------------------------------------------

def chunk_nbt(data_version: int, cx: int, cz: int, palette: list[str], blocks: list[int], biomes: list[str], defaults: dict, min_y: int = -64) -> dict:
    sections = []
    height = len(blocks) // 256
    for sy in range(height // 16):
        part = blocks[sy * 4096:(sy + 1) * 4096]  # y, z, x order = section index order
        used = sorted(set(part))
        local = {g: i for i, g in enumerate(used)}
        bs = {"palette": palette_nbt([palette[g] for g in used], defaults)}
        if len(used) > 1:
            bs["data"] = _pack([local[g] for g in part], max(4, math.ceil(math.log2(len(used)))))
        # biome cells of this section: engine order is y fastest, then x, then z (4 x 96 x 4)
        cells = [biomes[(z * 4 + x) * (height // 4) + sy * 4 + y] for y in range(4) for z in range(4) for x in range(4)]
        bused = sorted(set(cells))
        bl = {b: i for i, b in enumerate(bused)}
        bio = {"palette": bused}
        if len(bused) > 1:
            bio["data"] = _pack([bl[b] for b in cells], max(1, math.ceil(math.log2(len(bused)))))
        sections.append({"Y": Byte(min_y // 16 + sy), "block_states": bs, "biomes": bio})
    return {
        "DataVersion": Int(data_version), "xPos": Int(cx), "zPos": Int(cz), "yPos": Int(min_y // 16),
        # full: tools show it as is; the game keeps it and recomputes light and heightmaps
        "Status": "minecraft:full", "LastUpdate": 0, "InhabitedTime": 0, "isLightOn": Byte(0),
        "sections": sections, "block_entities": [], "structures": {"starts": {}, "References": {}},
    }


def write_regions(world_dir: Path, data_version: int, chunks) -> list[Path]:
    """chunks: (cx, cz, palette, blocks, heights, biomes) from World.chunks. Writes the overworld's
    region files under world_dir (26.x layout, dimensions/minecraft/overworld/region)."""
    regions: dict[tuple[int, int], dict[int, bytes]] = {}
    defaults = default_props()
    for cx, cz, palette, blocks, _heights, biomes in chunks:
        raw = zlib.compress(nbt(chunk_nbt(data_version, cx, cz, palette, blocks, biomes, defaults)))
        regions.setdefault((cx >> 5, cz >> 5), {})[(cx & 31) + (cz & 31) * 32] = raw
    folder = world_dir / "dimensions" / "minecraft" / "overworld" / "region"
    folder.mkdir(parents=True, exist_ok=True)
    out = []
    now = int(time.time())
    for (rx, rz), entries in regions.items():
        header, stamps, body = bytearray(4096), bytearray(4096), bytearray()
        sector = 2
        for i, raw in sorted(entries.items()):
            payload = struct.pack(">ib", len(raw) + 1, 2) + raw
            payload += b"\0" * (-len(payload) % 4096)
            n = len(payload) // 4096
            header[4 * i:4 * i + 4] = struct.pack(">I", sector << 8 | n)
            stamps[4 * i:4 * i + 4] = struct.pack(">I", now)
            body += payload
            sector += n
        path = folder / f"r.{rx}.{rz}.mca"
        path.write_bytes(bytes(header) + bytes(stamps) + bytes(body))
        out.append(path)
    return out


def write_level_dat(world_dir: Path, data_version: int, seed: int, name: str, spawn: tuple[int, int, int]) -> Path:
    """A minimal 26.x world: level.dat (name, version, spawn) and data/minecraft/world_gen_settings.dat
    (the seed and the vanilla dimensions), as the server writes them."""
    data = {
        "DataVersion": Int(data_version), "version": Int(19133), "version_history": IntArray([data_version]), "LevelName": name,
        "GameType": Int(1), "LastPlayed": int(time.time() * 1000), "allowCommands": Byte(1), "initialized": Byte(1),
        "spawn": {"pos": IntArray(list(spawn)), "dimension": "minecraft:overworld", "yaw": 0.0, "pitch": 0.0},
        "DataPacks": {"Enabled": ["vanilla"], "Disabled": []},
    }
    path = world_dir / "level.dat"
    world_dir.mkdir(parents=True, exist_ok=True)
    path.write_bytes(gzip.compress(nbt({"Data": data})))

    def dim(kind: str, source: dict) -> dict:
        return {"type": f"minecraft:{kind}", "generator": {"type": "minecraft:noise", "settings": f"minecraft:{kind.replace('the_', '')}", "biome_source": source}}
    gen = {
        "seed": seed, "generate_structures": Byte(1), "bonus_chest": Byte(0),
        "dimensions": {
            "minecraft:overworld": dim("overworld", {"type": "minecraft:multi_noise", "preset": "minecraft:overworld"}),
            "minecraft:the_nether": dim("the_nether", {"type": "minecraft:multi_noise", "preset": "minecraft:nether"}),
            "minecraft:the_end": dim("the_end", {"type": "minecraft:the_end"}),
        },
    }
    settings = world_dir / "data" / "minecraft" / "world_gen_settings.dat"
    settings.parent.mkdir(parents=True, exist_ok=True)
    settings.write_bytes(gzip.compress(nbt({"data": gen, "DataVersion": Int(data_version)})))
    return path


# --- Sponge schematic v3 ---------------------------------------------------------------------------

def _varints(values: list[int]) -> bytes:
    out = bytearray()
    for v in values:
        while True:
            b = v & 0x7F
            v >>= 7
            out.append(b | (0x80 if v else 0))
            if not v:
                break
    return bytes(out)


def write_schem(path: Path, data_version: int, box: tuple[int, int, int, int, int, int], block_at) -> Path:
    """box = (x0, y0, z0, x1, y1, z1) inclusive; block_at(x, y, z) -> block state string."""
    x0, y0, z0, x1, y1, z1 = box
    w, h, l = x1 - x0 + 1, y1 - y0 + 1, z1 - z0 + 1
    palette: dict[str, int] = {}
    idx = []
    for y in range(y0, y1 + 1):  # index = x + z * width + y * width * length
        for z in range(z0, z1 + 1):
            for x in range(x0, x1 + 1):
                idx.append(palette.setdefault(block_at(x, y, z), len(palette)))
    schem = {
        "Version": Int(3), "DataVersion": Int(data_version), "Width": Short(w), "Height": Short(h), "Length": Short(l),
        "Offset": IntArray([x0, y0, z0]),
        "Blocks": {"Palette": {k: Int(v) for k, v in palette.items()}, "Data": ByteArray(_varints(idx)), "BlockEntities": []},
    }
    path.write_bytes(gzip.compress(nbt({"Schematic": schem})))
    return path
