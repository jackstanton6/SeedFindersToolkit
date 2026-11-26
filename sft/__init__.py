"""Seed Finders Toolkit: shared entry points for the CLI, the GUI and plugins."""
import csv
import json
import math
import os
import threading
import time
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


def prepare(version: str, query_text, loaded=None) -> tuple[str, list, list[dict], "plugins.Api", list]:
    """A query split for the engine: (engine query JSON, natives it uses [(plugin, library)],
    py(...) terms, plugin api, plugin list). Unknown py conditions are refused here."""
    tree = query.parse(query_text) if isinstance(query_text, str) else query_text
    engine_tree, py_terms = query.split_python(tree)
    plugin_list, api = loaded or plugins.load(version)
    for t in py_terms:
        if t["name"] not in api.conditions:
            raise query.QueryError(f"unknown python condition '{t['name']}' (is its plugin loaded?)")
    used = {n.get("plugin") for n in query.nodes(engine_tree) if n["op"] == "native"}
    natives = [n for n in plugins.natives(plugin_list) if n[0] in used]
    return json.dumps(engine_tree or {"op": "and", "args": []}), natives, py_terms, api, plugin_list


def _py_ok(api, w, seed: int, py_terms) -> bool:
    return all(api.conditions[t["name"]](w, seed, **t.get("params", {})) for t in py_terms)


def check(version: str, seed: int, query_text, loaded=None) -> list | None:
    """Evaluates a query on one seed, plugins included: the match details, or None."""
    q, natives, py_terms, api, _ = prepare(version, query_text, loaded)
    w = world(version)
    res = w.check(seed, q, natives)
    return json.loads(res) if res is not None and _py_ok(api, w, seed, py_terms) else None


def scan(version: str, seed: int, query_text, x: int, z: int, within: int, step: int = 64, anchor=None, loaded=None) -> list:
    """Every anchor where the query, moved from (x, z) there, matches: [(x, z, details)], nearest
    first. py(...) conditions only see the seed, so they hold at every anchor or at none."""
    q, natives, py_terms, api, _ = prepare(version, query_text, loaded)
    w = world(version)
    if not _py_ok(api, w, seed, py_terms):
        return []
    return [(ax, az, json.loads(d)) for ax, az, d in w.scan(seed, q, x, z, within, step, anchor, natives)]


class Job:
    """One search: the engine part runs in Rust threads; py(...) terms filter its matches here."""

    def __init__(self, version: str, query_text, mode="range", start=0, count=1_000_000, seeds=None, max_results=0, loaded=None, gpu=False,
                 shuffle=None):
        self.mode = mode
        q, natives, self.py_terms, self.api, _ = prepare(version, query_text, loaded)
        self.world = world(version)
        self.search = engine().Search(self.world, q, mode, start, count, seeds, natives, max_results, gpu, shuffle)
        self.gpu_note = self.search.gpu_note
        self.rows: list[tuple[int, list]] = []
        self.py_rejected = 0
        # maximize()/minimize(): rows are the best `keep` seeds by score, best first
        self.ranked, self.keep, self.max_results = self.search.ranked, max_results or 100, max_results
        self.seeds_per_unit = 65536 if mode in ("structure_seeds", "structure_list") else 1
        self.skipped = 0  # units before the first start's from_unit (a CLI resume): done by an earlier run
        self.started = False

    def estimate(self, seconds=3.0, threads=None) -> dict:
        """Runs the search for `seconds` (stop() ends it early) and extrapolates; see rarity()."""
        return json.loads(self.search.estimate(seconds, threads or os.cpu_count()))

    def start(self, threads=None, from_unit=0):
        if not self.started:  # later starts resume this job, whose counters already include what it did
            self.skipped, self.started = from_unit, True
        self.search.start(threads or os.cpu_count(), from_unit)

    def stop(self):
        self.search.stop()

    def take_notes(self) -> list[str]:
        """Messages for the user since the last call (a CrackJob reports its phases here)."""
        return []

    def running(self) -> bool:
        return self.search.running()

    def join(self) -> int:
        """Blocks until done; returns the unit to resume from."""
        return self.search.join()

    def poll(self) -> list[tuple[int, list]]:
        """New matches since the last poll, after py(...) filters."""
        new = []
        for seed, details, sc in self.search.take_results():
            if _py_ok(self.api, self.world, seed, self.py_terms):
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
        s["units_done"] = done + self.skipped
        s["progress"] = s["units_done"] / total if total else 1.0
        s["eta"] = (total - s["units_done"]) / s["rate"] if s["rate"] > 0 else None
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


def bedrock_query(blocks) -> str:
    """DSL for observed floor blocks [(x, y, z, is_bedrock)] (y -63..-60 decide; the bottom is all bedrock)."""
    return f"bedrock(blocks={[(int(x), int(y), int(z), bool(b)) for x, y, z, b in blocks]})" if blocks else ""


def observation_query(observations) -> str:
    """DSL requiring each observed start [(structure, x, z)] within a chunk of where it was seen."""
    return " and ".join(f"structure({n!r}, at=({x}, {z}), within=24)" for n, x, z in observations)


def cuda_status() -> tuple[bool, str]:
    """(usable, device name or why not) for the CUDA pre-filter."""
    return engine().cuda_status() if _engine is not None else (False, "engine not built")


CRACK_WARN = 1024  # structure seeds: more than this and the sister check gets a time warning


class CrackJob:
    """Observed structure starts [(structure, x, z)] -> seeds: the brute force for the 48-bit
    structure seeds, then a search of their sisters checking every observation with biomes (and
    `extra_query`, e.g. biome or height observations). The interface of Job: progress is the brute
    force first (seeds tried), then the sister search. `gpu` brute-forces on the CUDA device."""

    mode = "crack"
    seeds_per_unit = 65536

    def __init__(self, version: str, observations, extra_query: str = "", max_work=None, gpu=False, max_results=0, loaded=None):
        self.version, self.max_results, self.loaded = version, max_results, loaded or plugins.load(version)
        self.query = " and ".join(t for t in (observation_query(observations), extra_query) if t)
        # a bad extra query is refused now, not after the brute force
        self.ranked = Job(version, self.query, "structure_list", seeds=[], loaded=self.loaded).ranked
        self.crack = engine().Crack(world(version), [(n, x, z) for n, x, z in observations], max_work, gpu)
        self.gpu_note = "crack brute force on the GPU" if gpu else ""
        self.found: list[int] | None = None  # structure seeds, once the brute force has finished
        self.job: Job | None = None  # the sister search
        self.error = None
        self.notes: list[str] = []
        self.py_rejected = 0
        self._thread = None
        self._stopped = False
        self._lock = threading.Lock()
        self._clock = (0.0, None)  # brute force: seconds of finished runs, start of the current one

    @property
    def rows(self):
        return self.job.rows if self.job else []

    def estimate(self, seconds=3.0, threads=None) -> dict:
        return {"note": "no estimate for a crack: progress shows the brute force, then the sister check"}

    def start(self, threads=None, from_unit=0):
        """Starts the brute force (again from the beginning if it was stopped), or resumes the sister search."""
        if self.job:
            self.job.start(threads, from_unit)
            return
        self._stopped, self.error = False, None
        self._clock = (self._clock[0], time.monotonic())
        self._thread = threading.Thread(target=self._run, args=(threads,), daemon=True)
        self._thread.start()

    def _run(self, threads):
        try:
            found = self.crack.run()
        except Exception as e:
            self.error = f"crack failed: {e}"
            return
        finally:
            self._clock = (self._clock[0] + time.monotonic() - self._clock[1], None)
        if self._stopped:
            return  # incomplete: Resume runs the brute force again
        self.found = found
        self.notes.append(f"crack: {len(found)} structure seed(s)" + (f": {' '.join(map(str, found[:50]))}" if found else
                                                                       "; check the coordinates and structure names"))
        if not found:
            return
        try:
            job = Job(self.version, self.query, "structure_list", seeds=found, max_results=self.max_results, loaded=self.loaded)
            if len(found) > CRACK_WARN:
                t = job.estimate(1.0, threads).get("seconds_total")
                self.notes.append(f"warning: {len(found)} structure seeds, so {len(found) * 65536:,} sister seeds to check"
                                  + (f" (about {t / 3600:.1f} h)" if t else "")
                                  + "; more observations (huts, temples, outposts, shipwrecks) narrow it down")
            else:
                self.notes.append(f"checking {len(found) * 65536:,} sister seeds")
        except Exception as e:
            self.error = f"sister search failed: {e}"
            return
        with self._lock:
            self.job = job
            if not self._stopped:
                job.start(threads)

    def stop(self):
        with self._lock:
            self._stopped = True
            self.crack.stop()
            if self.job:
                self.job.stop()

    def running(self) -> bool:
        return bool(self._thread and self._thread.is_alive()) or bool(self.job and self.job.running())

    def join(self) -> int:
        """Blocks until done; returns the unit to resume from (the brute force restarts at 0)."""
        if self._thread:
            self._thread.join()
        if self.job:
            return self.job.join()
        return self.crack.work if self.found is not None else 0

    def poll(self):
        return self.job.poll() if self.job else []

    def take_notes(self) -> list[str]:
        notes, self.notes = self.notes, []
        return notes

    def stats(self) -> dict:
        if self.job:
            s = self.job.stats()
            s["error"] = s["error"] or self.error
            return s
        done, total = self.crack.done, self.crack.work
        elapsed = self._clock[0] + (time.monotonic() - self._clock[1] if self._clock[1] is not None else 0.0)
        rate = done / elapsed if elapsed > 0 else 0.0
        return {"units_done": done, "units_total": total, "checks": done, "prefilter_passed": 0, "full_evals": 0, "matched": 0,
                "elapsed": elapsed, "rate": rate, "checks_rate": rate, "progress": done / total if total else 1.0,
                "eta": (total - done) / rate if rate > 0 else None, "py_rejected": 0, "error": self.error, "rarity": {}}


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
