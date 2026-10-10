"""Command line: `python -m sft [gui|data|search|check|locate|plugins]` (no command = GUI)."""
import argparse
import json
import os
import sys
import time

import sft
from sft import plugins, query


def fmt_secs(s) -> str:
    if s is None:
        return "?"
    s = int(s)
    return f"{s // 86400}d {s % 86400 // 3600:02}:{s % 3600 // 60:02}:{s % 60:02}" if s >= 86400 else f"{s // 3600:02}:{s % 3600 // 60:02}:{s % 60:02}"


def cmd_search(a):
    seeds = [int(x) for x in open(a.seeds_file).read().split()] if a.seeds_file else None
    job = sft.Job(a.version, a.query, "list" if seeds else a.mode, a.start, a.count, seeds, a.max)
    est = job.estimate(a.sample, a.threads)
    if "seconds_total" in est:
        print(f"estimate: {est['units_per_sec']:,.0f} units/s, prefilter pass {est['prefilter_rate']:.2%}, "
              f"total {fmt_secs(est['seconds_total'])}", file=sys.stderr)
    job.start(a.threads, a.start_unit)
    try:
        while job.running():
            time.sleep(0.5)
            for seed, details in job.poll():
                print(seed, json.dumps(details) if a.details else "", flush=True)
            s = job.stats()
            print(f"\r{s['progress']:7.2%}  {s['units_done']:,.0f}/{s['units_total']:,.0f}  {s['rate']:,.0f}/s  checks {s['checks']:,.0f}  "
                  f"matches {s['matched']:,.0f}  elapsed {fmt_secs(s['elapsed'])}  eta {fmt_secs(s['eta'])}   ", end="", file=sys.stderr, flush=True)
    except KeyboardInterrupt:
        job.stop()
    resume = job.join()
    for seed, details in job.poll():
        print(seed, json.dumps(details) if a.details else "")
    print(f"\nstopped; resume with --start-unit {resume}" if resume < job.stats()["units_total"] else "\ndone", file=sys.stderr)
    if a.out:
        sft.export(job.rows, a.out)


def cmd_check(a):
    w = sft.world(a.version)
    engine_tree, py_terms = query.split_python(query.parse(a.query))
    res = w.check(a.seed, json.dumps(engine_tree)) if engine_tree else "[]"
    if res is not None and py_terms:
        _, api = plugins.load(a.version)
        res = res if all(api.conditions[t["name"]](w, a.seed, **t.get("params", {})) for t in py_terms) else None
    print("match" if res is not None else "no match", res or "")


def cmd_locate(a):
    for name, x, y, z, exact in sft.world(a.version).structures_near(a.seed, a.x, a.z, a.radius, [a.structure]):
        print(f"{name:40} {x:8} {y:5} {z:8}{'' if exact else '  (approximate y/validity)'}")


def cmd_plugins(a):
    found, api = plugins.load(a.version)
    for p in found:
        print(f"{p.name:20} {p.meta.get('version', '?'):8} native={'yes' if p.native else 'no':3}  {p.meta.get('description', '')}")
        for e in p.errors:
            print("    error:", e.strip())
    print("python conditions:", ", ".join(api.conditions) or "-")
    print("seed sources:", ", ".join(api.seed_sources) or "-")
    print("exporters:", ", ".join(api.exporters) or "-")


def main():
    p = argparse.ArgumentParser(prog="sft", description="Seed Finders Toolkit")
    p.add_argument("--version", default="26.3", help="Minecraft version (data/<version>)")
    sub = p.add_subparsers(dest="cmd")
    sub.add_parser("gui", help="start the GUI (default)")
    d = sub.add_parser("data", help="download + extract worldgen data for a version")
    d.add_argument("mc_version")
    d.add_argument("--jar", help="use this server.jar instead of downloading")
    s = sub.add_parser("search", help="run a search headless")
    s.add_argument("query")
    s.add_argument("--mode", default="range", choices=["range", "structure_seeds", "random"])
    s.add_argument("--start", type=int, default=0, help="first seed / structure seed / random generator seed")
    s.add_argument("--start-unit", type=int, default=0, help="resume point printed by a stopped run")
    s.add_argument("--count", type=int, default=1_000_000)
    s.add_argument("--seeds-file", help="search these seeds (one per line) instead")
    s.add_argument("--threads", type=int, default=os.cpu_count())
    s.add_argument("--max", type=int, default=0, help="stop after this many matches")
    s.add_argument("--sample", type=int, default=2048, help="units timed for the estimate")
    s.add_argument("--details", action="store_true", help="print match details")
    s.add_argument("--out", help="write results (.csv/.json/.txt)")
    c = sub.add_parser("check", help="evaluate a query on one seed")
    c.add_argument("seed", type=int)
    c.add_argument("query")
    lo = sub.add_parser("locate", help="list valid structure starts near a point")
    lo.add_argument("seed", type=int)
    lo.add_argument("structure")
    lo.add_argument("--x", type=int, default=0)
    lo.add_argument("--z", type=int, default=0)
    lo.add_argument("--radius", type=int, default=2000)
    sub.add_parser("plugins", help="list plugins and what they register")
    a = p.parse_args()

    if a.cmd in (None, "gui"):
        from sft.gui.app import run
        run(a.version)
    elif a.cmd == "data":
        print("data ready in", sft.data.setup(a.mc_version, a.jar))
    else:
        {"search": cmd_search, "check": cmd_check, "locate": cmd_locate, "plugins": cmd_plugins}[a.cmd](a)


if __name__ == "__main__":
    main()
