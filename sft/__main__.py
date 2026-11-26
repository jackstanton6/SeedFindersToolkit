"""Command line: `python -m sft [gui|data|search|check|scan|locate|loot|crack|spawn|chunk|export|plugins]` (no command = GUI)."""
import argparse
import json
import os
import sys
import time

import sft
from sft import plugins


def fmt_secs(s) -> str:
    if s is None:
        return "?"
    s = int(s)
    return f"{s // 86400}d {s % 86400 // 3600:02}:{s % 3600 // 60:02}:{s % 60:02}" if s >= 86400 else f"{s // 3600:02}:{s % 3600 // 60:02}:{s % 60:02}"


def cmd_search(a):
    seeds = [int(x) for x in open(a.seeds_file).read().split()] if a.seeds_file else None
    if seeds == []:
        raise ValueError(f"no seeds in {a.seeds_file}")
    if a.sisters is not None:
        seeds = sft.sisters([a.sisters])
    job = sft.Job(a.version, a.query, "list" if seeds else a.mode, a.start, a.count, seeds, a.max, gpu=a.gpu, shuffle=a.shuffle)
    run_job(job, a)


def run_job(job, a):
    if job.gpu_note:
        print(job.gpu_note, file=sys.stderr)
    est = job.estimate(a.sample, a.threads) if a.sample > 0 and job.mode != "crack" else {}
    if "note" in est:
        print("estimate:", est["note"], file=sys.stderr)
    if "seconds_total" in est:
        r = sft.rarity(est)
        first = fmt_secs(r["seconds_to_first"]) if r["p"] else "> " + fmt_secs(r["seconds_to_first_range"][0])
        print(f"estimate: {est['units_per_sec']:,.0f} units/s, prefilter pass {est['prefilter_rate']:.2%}, "
              f"total {fmt_secs(est['seconds_total'])}, match rate {r['p']:.3g} (95% {r['low']:.3g}..{r['high']:.3g}), "
              f"first match in ~{first}", file=sys.stderr)
    job.start(a.threads, a.start_unit)
    try:
        while job.running():
            time.sleep(0.5)
            for note in job.take_notes():
                print("\n" + note, file=sys.stderr)
            for seed, details in job.poll():
                print(seed, json.dumps(details) if a.details else "", flush=True)
            s = job.stats()
            print(f"\r{s['progress']:7.2%}  {s['units_done']:,.0f}/{s['units_total']:,.0f}  {s['rate']:,.0f}/s  checks {s['checks']:,.0f}  "
                  f"matches {s['matched']:,.0f}  elapsed {fmt_secs(s['elapsed'])}  eta {fmt_secs(s['eta'])}   ", end="", file=sys.stderr, flush=True)
    except KeyboardInterrupt:
        job.stop()
    resume = job.join()
    for note in job.take_notes():
        print("\n" + note, file=sys.stderr)
    if job.stats()["error"]:
        print("\n" + job.stats()["error"], file=sys.stderr)
    for seed, details in job.poll():
        print(seed, json.dumps(details) if a.details else "")
    print(f"\nstopped; resume with --start-unit {resume}" if resume < job.stats()["units_total"] else "\ndone", file=sys.stderr)
    if job.ranked:
        print("best:", *(f"{seed} ({sft.score(d):g})" for seed, d in job.rows), file=sys.stderr)
    if a.out:
        sft.export(job.rows, a.out)


def cmd_check(a):
    res = sft.check(a.version, a.seed, a.query)
    print("match" if res is not None else "no match", json.dumps(res) if res is not None else "")


def cmd_scan(a):
    for x, z, details in sft.scan(a.version, a.seed, a.query, a.x, a.z, a.within, a.step, a.anchor):
        print(f"{x:8} {z:8}  {json.dumps(details) if a.details else ''}")


def cmd_crack(a):
    obs = []
    for o in a.observations:  # village:120,-340
        name, _, xz = o.partition(":")
        x, z = (int(v) for v in xz.split(","))
        obs.append((name, x, z))
    blocks = []
    for b in a.bedrock or []:  # x,y,z:1
        xyz, _, flag = b.partition(":")
        x, y, z = (int(v) for v in xyz.split(","))
        blocks.append((x, y, z, flag.strip() not in ("0", "no", "false")))
    query = " and ".join(q for q in (a.query, sft.bedrock_query(blocks)) if q)
    run_job(sft.CrackJob(a.version, obs, query, gpu=a.gpu, max_results=a.max), a)


def cmd_loot(a):
    for c in json.loads(sft.world(a.version).chests(a.seed, a.structure, a.x, a.z, a.radius)):
        items = ", ".join(f"{s['count']} {s['item'].split(':')[-1]}" + (f" {s['enchantments']}" if "enchantments" in s else "") for s in c["items"])
        print(f"{c['structure'].split(':')[-1]:20} {c['x']:7} {c['y']:4} {c['z']:7}  {c['table'].split('/')[-1]:20} {items}"
              + ("" if c["exact"] else "  (seed approximate)"))


def cmd_locate(a):
    for name, x, y, z, exact in sft.world(a.version).structures_near(a.seed, a.x, a.z, a.radius, [a.structure]):
        print(f"{name:40} {x:8} {y:5} {z:8}{'' if exact else '  (approximate y/validity)'}")


def cmd_spawn(a):
    w = sft.world(a.version)
    x, z = w.spawn(a.seed)
    print(f"spawn {x} {z} (chunk {x >> 4} {z >> 4}; the centre of the spawn chunk), biome {w.biomes()[w.block_biome(a.seed, x, 64, z)].split(':')[-1]}")


def cmd_chunk(a):
    w = sft.world(a.version)
    ((cx, cz, palette, blocks, heights, biomes),) = w.chunks(a.seed, a.cx, a.cz, a.cx, a.cz)
    if a.out:
        with open(a.out, "w") as f:
            json.dump({"x": cx, "z": cz, "min_y": -64, "palette": palette, "blocks": blocks, "heights": heights, "biomes": biomes}, f)
        print("wrote", a.out)
        return
    counts = {}
    for b in blocks:
        counts[palette[b]] = counts.get(palette[b], 0) + 1
    for name, n in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"{n:7} {name}")
    print(f"surface height {min(heights[:256])}..{max(heights[:256])}, ocean floor {min(heights[256:])}..{max(heights[256:])}")


def cmd_export(a):
    from pathlib import Path
    from sft import worldfile
    w = sft.world(a.version)
    dv = int((sft.data.data_dir(a.version) / "data_version.txt").read_text())
    x0, z0, x1, z1 = min(a.x0, a.x1), min(a.z0, a.z1), max(a.x0, a.x1), max(a.z0, a.z1)
    chunks = w.chunks(a.seed, x0 >> 4, z0 >> 4, x1 >> 4, z1 >> 4)
    if a.schem:
        by = {(c[0], c[1]): c for c in chunks}
        y0, y1 = a.y

        def block_at(x, y, z):
            cx, cz, palette, blocks, _, _ = by[(x >> 4, z >> 4)]
            return palette[blocks[((y + 64) * 16 + (z & 15)) * 16 + (x & 15)]]
        print("wrote", worldfile.write_schem(Path(a.schem), dv, (x0, y0, z0, x1, y1, z1), block_at))
    else:
        out = Path(a.mca)
        sx, sz = w.spawn(a.seed)
        files = worldfile.write_regions(out, dv, chunks)
        worldfile.write_level_dat(out, dv, a.seed, f"sft {a.seed}", (sx, 80, sz))
        print(f"wrote {len(chunks)} chunks in {len(files)} region file(s) and level.dat to {out}")
    print("note: terrain, surface blocks and caves only (structures and decoration are not placed yet)")


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
    s.add_argument("--count", type=int, default=1_000_000)
    s.add_argument("--seeds-file", help="search these seeds (one per line) instead")
    s.add_argument("--sisters", type=int, help="search the 65536 sister seeds of this seed instead")
    s.add_argument("--shuffle", type=int, metavar="KEY", help="visit the seeds in a shuffled order (no repeats; same KEY, same order)")

    def run_args(s):
        s.add_argument("--threads", type=int, default=os.cpu_count())
        s.add_argument("--max", type=int, default=0, help="stop after this many matches (best N with maximize)")
        s.add_argument("--sample", type=float, default=3.0, help="seconds spent estimating first (0 = skip)")
        s.add_argument("--start-unit", type=int, default=0, help="resume point printed by a stopped run")
        s.add_argument("--details", action="store_true", help="print match details")
        s.add_argument("--out", help="write results (.csv/.json/.txt)")
        s.add_argument("--gpu", action="store_true", help="pre-filter (and crack) on the CUDA GPU")
    run_args(s)
    c = sub.add_parser("check", help="evaluate a query on one seed")
    c.add_argument("seed", type=int)
    c.add_argument("query")
    sc = sub.add_parser("scan", help="find every place in one seed where a query matches")
    sc.add_argument("seed", type=int)
    sc.add_argument("query", help="written as it should hold at --x/--z; it is moved from there to each anchor")
    sc.add_argument("--x", type=int, default=0)
    sc.add_argument("--z", type=int, default=0)
    sc.add_argument("--within", type=int, default=5000)
    sc.add_argument("--step", type=int, default=64, help="anchor grid spacing in blocks")
    sc.add_argument("--anchor", help="anchor at the starts of this structure instead of a grid")
    sc.add_argument("--details", action="store_true")
    cr = sub.add_parser("crack", help="find seeds from structures seen in a world")
    cr.add_argument("observations", nargs="+", help="structure:x,z (block coordinates), e.g. swamp_hut:-120,840")
    cr.add_argument("--query", default="", help="extra conditions the seed must meet (biomes, heights...)")
    cr.add_argument("--bedrock", nargs="+", help="observed floor blocks x,y,z:1 (bedrock) or x,y,z:0 (not), y -63..-60; "
                                                 "they pick the right one of each structure seed's 65,536 sisters")
    run_args(cr)
    lo = sub.add_parser("locate", help="list valid structure starts near a point")
    lo.add_argument("seed", type=int)
    lo.add_argument("structure")
    lo.add_argument("--x", type=int, default=0)
    lo.add_argument("--z", type=int, default=0)
    lo.add_argument("--radius", type=int, default=2000)
    lt = sub.add_parser("loot", help="list chests and their contents near a point")
    lt.add_argument("seed", type=int)
    lt.add_argument("structure", help="buried_treasure, shipwreck, ruined_portal, igloo, desert_pyramid, ocean_ruin, jungle_temple")
    lt.add_argument("--x", type=int, default=0)
    lt.add_argument("--z", type=int, default=0)
    lt.add_argument("--radius", type=int, default=1000)
    sp = sub.add_parser("spawn", help="the world spawn of a seed")
    sp.add_argument("seed", type=int)
    ch = sub.add_parser("chunk", help="a generated chunk: block counts and heights (or --out JSON)")
    ch.add_argument("seed", type=int)
    ch.add_argument("cx", type=int, help="chunk x (block x >> 4)")
    ch.add_argument("cz", type=int)
    ch.add_argument("--out", help="write palette, blocks (y, z, x order from y -64), heightmaps and biomes as JSON")
    ex = sub.add_parser("export", help="write a generated area as Anvil region files or a Sponge schematic")
    ex.add_argument("seed", type=int)
    for k in ("x0", "z0", "x1", "z1"):
        ex.add_argument(k, type=int, help="block coordinates of opposite corners" if k == "x0" else None)
    dest = ex.add_mutually_exclusive_group(required=True)
    dest.add_argument("--mca", help="world folder to write (region files + a minimal level.dat)")
    dest.add_argument("--schem", help=".schem file to write (Sponge v3)")
    ex.add_argument("--y", type=int, nargs=2, default=[-64, 319], metavar=("LO", "HI"), help="y range for --schem")
    sub.add_parser("plugins", help="list plugins and what they register")
    a = p.parse_args()

    if a.cmd in (None, "gui"):
        from sft.gui.app import run
        run(a.version)
    elif a.cmd == "data":
        print("data ready in", sft.data.setup(a.mc_version, a.jar))
    else:
        try:
            {"search": cmd_search, "check": cmd_check, "scan": cmd_scan, "locate": cmd_locate, "loot": cmd_loot, "crack": cmd_crack, "plugins": cmd_plugins,
             "spawn": cmd_spawn, "chunk": cmd_chunk, "export": cmd_export}[a.cmd](a)
        except (ValueError, FileNotFoundError) as e:  # bad query, unknown name, missing data: no traceback
            sys.exit(f"error: {e}")


if __name__ == "__main__":
    main()
