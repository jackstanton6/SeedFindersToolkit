"""Seed Finders Toolkit: shared entry points for the CLI, the GUI and plugins."""
import csv
import json
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

    def __init__(self, version: str, query_text, mode="range", start=0, count=1_000_000, seeds=None, max_results=0, loaded=None):
        tree = query.parse(query_text) if isinstance(query_text, str) else query_text
        engine_tree, self.py_terms = query.split_python(tree)
        self.world = world(version)
        plugin_list, self.api = loaded or plugins.load(version)
        for t in self.py_terms:
            if t["name"] not in self.api.conditions:
                raise query.QueryError(f"unknown python condition '{t['name']}' (is its plugin loaded?)")
        self.search = engine().Search(
            self.world, json.dumps(engine_tree or {"op": "and", "args": []}), mode, start, count,
            seeds, plugins.natives(plugin_list), max_results,
        )
        self.rows: list[tuple[int, list]] = []
        self.py_rejected = 0

    def estimate(self, sample=4096, threads=None) -> dict:
        return json.loads(self.search.estimate(sample, threads or os.cpu_count()))

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
        for seed, details in self.search.take_results():
            if all(self.api.conditions[t["name"]](self.world, seed, **t.get("params", {})) for t in self.py_terms):
                new.append((seed, json.loads(details)))
            else:
                self.py_rejected += 1
        self.rows += new
        return new

    def stats(self) -> dict:
        s = dict(self.search.stats())
        done, total, elapsed = s["units_done"], s["units_total"], s["elapsed"]
        s["rate"] = done / elapsed if elapsed > 0 else 0.0
        s["checks_rate"] = s["checks"] / elapsed if elapsed > 0 else 0.0
        s["progress"] = done / total if total else 1.0
        s["eta"] = (total - done) / s["rate"] if s["rate"] > 0 else None
        s["py_rejected"] = self.py_rejected
        return s


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
