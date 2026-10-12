"""Builds the per-version data folder the engine reads (data/<version>/).

Everything comes from the official server jar, so no Mojang data lives in the repo:
  1. download server.jar from Mojang's version manifest (sha1-checked)
  2. copy worldgen JSON, structure templates, loot tables, enchantments and their tags out of the
     inner jar
  3. run the vanilla data generator for the multi-noise biome parameter table and item components
     (stack sizes, enchantability -> items.json)
Needs Java on PATH for step 3.
"""
import hashlib
import io
import json
import shutil
import subprocess
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
KEEP = ("data/minecraft/worldgen/", "data/minecraft/tags/worldgen/", "data/minecraft/structure/", "data/minecraft/loot_table/",
        "data/minecraft/enchantment/", "data/minecraft/tags/item/", "data/minecraft/tags/enchantment/")


def data_dir(version: str) -> Path:
    return ROOT / "data" / version


def _fetch(url: str) -> bytes:
    with urllib.request.urlopen(url) as r:
        return r.read()


def download_server_jar(version: str) -> Path:
    jar = ROOT / "cache" / f"server-{version}.jar"
    if jar.exists():
        return jar
    manifest = json.loads(_fetch(MANIFEST))
    entry = next(v for v in manifest["versions"] if v["id"] == version)
    server = json.loads(_fetch(entry["url"]))["downloads"]["server"]
    blob = _fetch(server["url"])
    if hashlib.sha1(blob).hexdigest() != server["sha1"]:
        raise RuntimeError("server.jar sha1 mismatch")
    jar.parent.mkdir(parents=True, exist_ok=True)
    jar.write_bytes(blob)
    return jar


def extract(version: str, server_jar: Path) -> None:
    out = data_dir(version)
    with zipfile.ZipFile(server_jar) as bundle:
        inner = bundle.read(f"META-INF/versions/{version}/server-{version}.jar")
    with zipfile.ZipFile(io.BytesIO(inner)) as jar:
        for name in jar.namelist():
            if name.startswith(KEEP) and not name.endswith("/"):
                dest = out / name.removeprefix("data/")
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(jar.read(name))

    # The overworld biome table is built in code; the data generator dumps it as a report.
    work = ROOT / "cache" / f"datagen-{version}"
    work.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["java", "-DbundlerMainClass=net.minecraft.data.Main", "-jar", str(server_jar.resolve()),
         "--reports", "--output", "generated"],
        cwd=work, check=True, capture_output=True,
    )
    shutil.copytree(work / "generated/reports/biome_parameters", out / "reports/biome_parameters", dirs_exist_ok=True)
    write_items(work / "generated/reports/minecraft/components/item", out / "items.json")
    (out / "version.txt").write_text(version)


def write_items(report: Path, dest: Path) -> None:
    """item id -> [max stack size, enchantability (0 = not enchantable)], what loot generation needs."""
    items = {}
    for f in sorted(report.glob("*.json")):
        c = json.loads(f.read_text())["components"]
        items[f"minecraft:{f.stem}"] = [c.get("minecraft:max_stack_size", 64), c.get("minecraft:enchantable", {}).get("value", 0)]
    dest.write_text(json.dumps(items, separators=(",", ":")))


def setup(version: str, server_jar: str | None = None) -> Path:
    jar = Path(server_jar) if server_jar else download_server_jar(version)
    extract(version, jar)
    return data_dir(version)
