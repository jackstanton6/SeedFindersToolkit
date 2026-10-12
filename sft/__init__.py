"""Seed Finders Toolkit: shared entry points for the CLI, the GUI and plugins."""
import csv
import json
import math
import os
from pathlib import Path

from . import data, plugins, query

try:
    from . import _engine
    _engine_error = None
except ImportError as e:  # engine not built yet
    _engine, _engine_error = None, e

__version__ = "0.1.0"


def engine():
    if _engine is None:
        raise RuntimeError("Rust engine not built: run `pip install -e .` (needs a Rust toolchain)") from _engine_error
    return _engine


def versions() -> list[str]:
    """Versions with a prepared data folder."""
    root = data.ROOT / "data"
    return sorted(p.name for p in root.iterdir() if (p / "version.txt").exists()) if root.exists() else []


_worlds = {}


def world(version: str = "26.3"):
    if version not in _worlds:
        d = data.data_dir(version)
        if not (d / "version.txt").exists():
            raise FileNotFoundError(f"no data for {version}: run `python -m sft data {version}`")
        _worlds[version] = engine().World(str(d))
    return _worlds[version]


class Job:
    """One search: the engine part runs in Rust threads; py(...) terms filter its matches here."""

    def __init__(self, version: str, query_text, mode="range", start=0, count=1_000_000, seeds=None, max_results=0, loaded=None, gpu=False):
        tree = query.parse(query_text) if isinstance(query_text, str) else query_text
        engine_tree, self.py_terms = query.split_python(tree)
        self.world = world(version)
        plugin_list, self.api = loaded or plugins.load(version)
        for t in self.py_terms:
            if t["name"] not in self.api.conditions:
                raise query.QueryError(f"unknown python condition '{t['name']}' (is its plugin loaded?)")
        used = {n.get("plugin") for n in query.nodes(engine_tree) if n["op"] == "native"}
        self.search = engine().Search(
            self.world, json.dumps(engine_tree or {"op": "and", "args": []}), mode, start, count,
            seeds, [n for n in plugins.natives(plugin_list) if n[0] in used], max_results, gpu,
        )
        self.gpu_note = self.search.gpu_note
        self.rows: list[tuple[int, list]] = []
        self.py_rejected = 0
        # maximize()/minimize(): rows are the best `keep` seeds by score, best first
        self.ranked, self.keep = self.search.ranked, max_results or 100
        self.seeds_per_unit = 65536 if mode == "structure_seeds" else 1

    def estimate(self, seconds=3.0, threads=None) -> dict:
        """Runs the search for `seconds` (stop() ends it early) and extrapolates; see rarity()."""
        return json.loads(self.search.estimate(seconds, threads or os.cpu_count()))

    def start(self, threads=None, from_unit=0):
        self.search.start(threads or os.cpu_count(), from_unit)

    def stop(self):
        self.search.stop()

    def running(self) -> bool:
        return self.search.running()

    def join(self) -> int:
        """Blocks until done; returns the unit to resume from."""
        return self.search.join()

    def poll(self) -> list[tuple[int, list]]:
        """New matches since the last poll, after py(...) filters."""
        new = []
        for seed, details, sc in self.search.take_results():
            if all(self.api.conditions[t["name"]](self.world, seed, **t.get("params", {})) for t in self.py_terms):
                d = json.loads(details)
                new.append((seed, d + [{"type": "score", "value": sc}] if self.ranked else d))
            else:
                self.py_rejected += 1
        self.rows += new
        if self.ranked:
            self.rows = sorted(self.rows, key=lambda r: -score(r[1]))[: self.keep]
        return new

    def stats(self) -> dict:
        s = dict(self.search.stats())
        done, total, elapsed = s["units_done"], s["units_total"], s["elapsed"]
        s["rate"] = done / elapsed if elapsed > 0 else 0.0
        s["checks_rate"] = s["checks"] / elapsed if elapsed > 0 else 0.0
        s["progress"] = done / total if total else 1.0
        s["eta"] = (total - done) / s["rate"] if s["rate"] > 0 else None
        s["py_rejected"] = self.py_rejected
        s["error"] = self.search.error
        # live rarity: the running search is its own calibration sample
        seeds = done * self.seeds_per_unit
        s["rarity"] = rarity({"sample": seeds, "matches": s["matched"], "seeds_per_sec": seeds / elapsed if elapsed > 0 else 0})
        return s


def sisters(structure_seeds) -> list[int]:
    """All 64-bit seeds sharing these low 48 bits (same structure placement, different biomes)."""
    out = []
    for s in structure_seeds:
        low = s & (2**48 - 1)
        out += [((hi << 48) | low) - (2**64 if hi >= 2**15 else 0) for hi in range(2**16)]
    return out


def observation_query(observations) -> str:
    """DSL requiring each observed start [(structure, x, z)] within a chunk of where it was seen."""
    return " and ".join(f"structure({n!r}, at=({x}, {z}), within=24)" for n, x, z in observations)


def cuda_status() -> tuple[bool, str]:
    """(usable, device name or why not) for the CUDA pre-filter."""
    return engine().cuda_status() if _engine is not None else (False, "engine not built")


def crack(version: str, observations, extra_query: str = "", max_work=None, gpu=False) -> tuple[list[int], "Job | None"]:
    """Observed structure starts [(structure, x, z)] -> 48-bit structure seeds, plus a list Job over
    their sisters checking every observation with biomes (and `extra_query`, e.g. biome or height
    observations). Run the Job for the full 64-bit seeds. `gpu` brute-forces on the CUDA device."""
    found = world(version).crack([(n, x, z) for n, x, z in observations], max_work, gpu)
    q = " and ".join(t for t in (observation_query(observations), extra_query) if t)
    return found, (Job(version, q, "list", seeds=sisters(found)) if found else None)


def score(details: list) -> float:
    """The objective score stored in a match's details (0 if none)."""
    return next((d["value"] for d in details if d.get("type") == "score"), 0.0)


def rarity(est: dict, z: float = 1.96) -> dict:
    """Match probability per 64-bit seed with a 95% Wilson interval, and the expected time to the
    first match, from an estimate() sample (or live stats)."""
    n, k = est.get("sample", 0), est.get("matches", 0)
    if not n:
        return {}
    p = k / n
    centre, half = (p + z * z / (2 * n)) / (1 + z * z / n), z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / (1 + z * z / n)
    lo, hi = max(0.0, centre - half), min(1.0, centre + half)
    rate = est.get("seeds_per_sec", 0)
    first = (lambda q: 1 / (q * rate) if q > 0 and rate > 0 else math.inf)
    return {"p": p, "low": lo, "high": hi, "seconds_to_first": first(p), "seconds_to_first_range": (first(hi), first(lo))}


def export(rows, path: str | Path):
    """Writes (seed, details) rows as .csv, .json or .txt (seeds only) by file extension."""
    path = Path(path)
    if path.suffix == ".json":
        path.write_text(json.dumps([{"seed": s, "details": d} for s, d in rows], indent=1))
    elif path.suffix == ".csv":
        with path.open("w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["seed", "details"])
            w.writerows((s, json.dumps(d)) for s, d in rows)
    else:
        path.write_text("".join(f"{s}\n" for s, _ in rows))
