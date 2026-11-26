"""Engine behaviour through the Python API (needs data/26.3 and a built engine): query mistakes are
refused with a hint, failed branches leave no details or score, max results and resume accounting.
Run: python tests/test_engine.py"""
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import sft  # noqa: E402
from sft import query  # noqa: E402

SEED = 12345
if not (sft.data.data_dir("26.3") / "version.txt").exists():
    print("skipped: needs data/26.3 (run `python -m sft data 26.3`)")
    sys.exit(0)
w = sft.world("26.3")


def check(text: str, seed: int = SEED):
    res = w.check(seed, json.dumps(query.parse(text)))
    return None if res is None else json.loads(res)


def refused(text: str) -> str:
    try:
        check(text)
    except ValueError as e:
        return str(e)
    raise AssertionError(f"accepted: {text}")


def test_mistakes_are_refused_with_a_hint():
    assert "Did you mean 'within'?" in refused("village(withn=1)")
    assert "Did you mean '#is_jungle'?" in refused("biome('#is_jungl', within=500)")
    assert "Did you mean 'village'?" in refused("vilage(within=500)")
    assert "Did you mean 'water'?" in refused("lake('watr', within=64)")
    assert "Did you mean 'ocean_floor'?" in refused("height(heightmap='ocean_flor') > 0")
    assert "two whole numbers" in refused("village(at=(5,))")  # used to panic
    assert "at least one name" in refused("chain([])")


def test_failed_branches_leave_nothing_behind():
    # the first branch fails on village(within=1): neither its score nor its villages may show up
    assert check("(maximize(height()) and village(within=1)) or height() > -100") == [{"type": "score", "value": 0.0}]
    assert check("(village(within=3000) and village(within=1)) or height() > -100") == []
    assert check("not village(within=1)") == []
    kept = check("maximize(height()) and village(within=3000)")
    assert kept[-1]["type"] == "score" and any(d["type"] == "structure" for d in kept)


def test_anywhere_moves_the_condition_from_at():
    vs = w.structures_near(SEED, 5000, 5000, 1500, ["village"])
    assert vs, "this seed has a village near (5000, 5000)"
    q = lambda inner: json.dumps(query.parse(inner))  # noqa: E731
    assert len(w.scan(SEED, q("village(within=100, at=(5000, 5000))"), 5000, 5000, 1500, 64, "village")) == len(vs)
    assert check("anywhere(village(within=100, at=(5000, 5000)), at=(5000, 5000), within=1500, anchor='village')")
    assert check("anywhere(village(within=100), within=1500, anchor='village')")


def test_max_results_is_a_total_and_resume_keeps_the_clock():
    job = sft.Job("26.3", "village(within=300)", "range", 0, 1_000_000, max_results=5)
    job.start(4)
    resume = job.join()
    first = [s for s, _ in job.poll()]
    assert len(first) == 5 and job.stats()["matched"] == 5
    job.start(4, resume)  # the places are taken: nothing more is found, nothing is counted
    job.join()
    assert job.poll() == [] and job.stats()["matched"] == 5

    # stop and resume: no seed is reported twice, none is skipped, the rate stays a rate
    full = sft.Job("26.3", "village(within=300)", "range", 0, 60_000)
    full.start(4)
    full.join()
    want = sorted(s for s, _ in full.poll())
    job = sft.Job("26.3", "village(within=300)", "range", 0, 60_000)
    job.start(4)
    time.sleep(0.15)
    job.stop()
    resume = job.join()
    before = job.stats()
    job.start(4, resume)
    time.sleep(0.05)
    during = job.stats()
    job.join()
    assert sorted(s for s, _ in job.poll()) == want
    assert during["elapsed"] >= before["elapsed"] and during["rate"] < 3 * max(before["rate"], 1)
    assert job.stats()["units_done"] == 60_000


def test_a_new_job_resumed_from_a_unit_counts_the_units_before_it():
    # the CLI resumes with a new Job (--start-unit): progress must reach 100%, not stop at the resumed part
    job = sft.Job("26.3", "village(within=300)", "range", 0, 20_000)
    job.start(4, 15_000)
    job.join()
    s = job.stats()
    assert s["units_done"] == 20_000 and s["progress"] == 1.0 and s["eta"] == 0, s
    assert s["rarity"]["p"] == s["matched"] / 5_000  # rarity only from the seeds this run checked


def test_crack_runs_as_a_job():
    # a seed is recovered from its own structures: brute force, then its sisters in the engine
    seed = -4172144997902289642
    # /locate positions (what players report), not piece positions: those can lie in a neighbouring chunk
    obs = [(n, x, z) for m in ("swamp_hut", "desert_pyramid", "pillager_outpost", "igloo", "jungle_pyramid")
           for x, z, n in w.attempts(seed, m, 0, 0, 3000) if n]
    job = sft.CrackJob("26.3", obs, max_results=1)
    t = time.time()
    job.start(8)
    while job.running():
        time.sleep(0.05)
        job.stats()  # live in both phases
    job.join()
    assert not job.stats()["error"] and job.stats()["matched"] == 1, job.stats()
    # five structures leave sisters that fit too: the first match need not be `seed`, but it must match
    assert (seed & (2**48 - 1)) in job.found, len(job.found)
    q = json.dumps(query.parse(job.query))
    (found, _), = job.poll()
    assert w.check(found, q) is not None and w.check(seed, q) is not None
    notes = job.take_notes()
    assert any(n.startswith("crack: ") for n in notes) and any("sister seeds" in n for n in notes), notes
    print(f"  crack of {len(obs)} observations: {time.time() - t:.1f}s")

    # many structure seeds: a warning with the expected sister-check time, and the search still runs
    warn, sft.CRACK_WARN = sft.CRACK_WARN, 100
    try:
        job = sft.CrackJob("26.3", obs, max_results=1)
        job.start(8)
        job.join()
        notes = job.take_notes()
        assert any(n.startswith("warning: 765 structure seeds") and " h)" in n for n in notes) and job.stats()["matched"] == 1, notes
    finally:
        sft.CRACK_WARN = warn

    # a weak crack (one pyramid: ~2^42 seeds) stops at once and starts over on resume
    job = sft.CrackJob("26.3", obs[:1], max_work=2**44)
    job.start(4)
    time.sleep(0.3)
    job.stop()
    t = time.time()
    assert job.join() == 0 and time.time() - t < 2 and not job.running()
    s = job.stats()
    assert 0 < s["units_done"] < s["units_total"] and s["progress"] < 1 and job.found is None, s


def test_loot_without_tables_is_refused():
    import shutil
    import tempfile
    d = Path(tempfile.mkdtemp()) / "26.3"
    shutil.copytree(sft.data.data_dir("26.3"), d, ignore=lambda src, names: ["loot_table"] if "loot_table" in names else [])
    try:
        bare = sft.engine().World(str(d))
        q = json.dumps(query.parse('loot("buried_treasure", "diamond", within=400)'))
        try:
            bare.check(SEED, q)
        except ValueError as e:
            assert "python -m sft data 26.3" in str(e), e
        else:
            raise AssertionError("a loot query ran without loot tables")
        assert bare.check(SEED, json.dumps(query.parse("village(within=3000)"))) is not None  # the rest still works
    finally:
        shutil.rmtree(d.parent)


def test_check_and_scan_run_plugins_like_a_search():
    # native(...) and py(...) terms: check and scan must agree with a search over the same seeds
    q = 'village(within=2000) and native("example_rust", "tall_spawn", min=70) and py("example_python.seed_contains", text="7")'
    job = sft.Job("26.3", q, "range", 0, 3000)
    job.start(4)
    job.join()
    found = {s for s, _ in job.poll()}
    assert found and job.py_rejected, (len(found), job.py_rejected)
    assert {s for s in range(3000) if sft.check("26.3", s, q) is not None} == found
    seed = min(found)
    assert sft.scan("26.3", seed, q, 0, 0, 200, 100)  # the query holds at (0, 0) of a matching seed
    no7 = next(s for s in range(3000) if "7" not in str(s))
    assert sft.scan("26.3", no7, q, 0, 0, 200, 100) == []  # py terms are per seed: no anchor matches
    assert sft.scan("26.3", seed, 'py("example_python.seed_contains", text="7")', 0, 0, 100, 100)  # py terms alone


def test_two_islands_keep_their_own_details():
    islands = [d for d in check("island(within=3000) and island(within=3000, at=(2000, 0), min_circularity=0.1)", 0) if d["type"] == "island"]
    assert len(islands) == 2, islands  # the second used to delete the first one's detail
    assert {json.dumps(d["condition"], sort_keys=True) for d in islands} == {
        json.dumps({"op": "island", "within": 3000}), json.dumps({"min_circularity": 0.1, "op": "island", "within": 3000})}, islands


def test_gpu_biome_checks_find_the_same_seeds():
    if not sft.cuda_status()[0]:
        print("  (no CUDA device: GPU biome test skipped)")
        return
    q = "desert_pyramid(within=300) and biome('desert', within=200) and not biome('#is_ocean', within=500)"
    found = {}
    for gpu in (False, True):
        job = sft.Job("26.3", q, "range", 1 << 33, 50_000, gpu=gpu)
        job.start(8)
        job.join()
        found[gpu] = sorted(s for s, _ in job.poll())
        if gpu:
            assert "with biomes" in job.gpu_note, job.gpu_note
    assert found[True] == found[False] and found[True], (len(found[True]), len(found[False]))
    note = sft.Job("26.3", q, "structure_seeds", 0, 10, gpu=True).gpu_note
    assert "with biomes" not in note, note  # structure seeds are not whole seeds: biomes stay on the CPU


def test_point_biome_checks_are_block_level():
    # find a block whose F3 biome differs from its 4x4x4 cell biome (near a biome edge)
    names = w.biomes()
    cells = w.biome_map(SEED, 0, 0, 256, 1, 1, 64)
    x = next(x for x in range(256) if w.block_biome(SEED, x, 64, 0) != cells[x])
    block, cell = names[w.block_biome(SEED, x, 64, 0)], names[cells[x]]
    assert check(f"biome('{block}', at=({x}, 0))") is not None
    assert check(f"biome('{cell}', at=({x}, 0))") is None
    assert check(f"biome('{cell}', at=({x}, 0), exact=False)") is not None


def test_spawn_anchors_conditions():
    x, z = w.spawn(SEED)
    d = (x * x + z * z) ** 0.5
    res = check(f"spawn_distance() >= {d - 1} and spawn_distance() <= {d + 1}")
    assert res is not None and {"type": "spawn", "x": x, "z": z} in res, res
    assert check(f"spawn_distance() > {d + 1}") is None
    # at='spawn' is the same as at=(x, z) with the spawn's coordinates
    names = w.biomes()
    here = names[w.block_biome(SEED, x, 64, z)]
    assert check(f"biome('{here}', at='spawn')") is not None
    assert check(f"village(within=500, at='spawn')") == check(f"village(within=500, at=({x}, {z}))")
    assert "spawn" in refused("biome('plains', at='spwn')")


def test_block_conditions():
    # y = -64 is bedrock everywhere: one per column in the disc
    columns = sum(1 for x in range(-8, 9) for z in range(-8, 9) if x * x + z * z <= 64)
    res = check("block('bedrock', within=8, y=(-64, -64))")
    assert res is not None and {"type": "blocks", "x": 0, "z": 0, "count": columns, "exact": False} in res, res
    assert check(f"block('bedrock', within=8, y=(-64, -64)) > {columns}") is None
    assert check("block('#minecraft:base_stone_overworld', y=(-30, -20)) >= 1") is not None
    assert check("ore_vein('copper', count='all', within=32) >= 0") is not None
    assert check("spawn_block(['grass_block', 'sand', 'snow_block', 'stone', 'dirt', 'gravel', 'podzol', 'red_sand', 'mud', 'mycelium']) >= 0") is not None
    assert "unknown block 'stonee'" in refused("block('stonee')")
    assert "depth must be" in refused("block('stone', depth='deep')")


def test_chunks_blocks_and_schematic():
    import gzip
    import tempfile
    from sft import worldfile
    sys.path.insert(0, str(Path(__file__).parent))
    from loot_oracle import read_nbt
    ((cx, cz, palette, blocks, heights, biomes),) = w.chunks(SEED, 3, -2, 3, -2)
    assert len(blocks) == 16 * 16 * 384 and len(heights) == 512 and len(biomes) == 4 * 96 * 4
    x, z = 3 * 16 + 5, -2 * 16 + 9
    top = heights[9 * 16 + 5]
    assert palette[blocks[((top + 64) * 16 + 9) * 16 + 5]] == w.block(SEED, x, top, z) != "minecraft:air"
    assert w.block(SEED, x, top + 1, z) == "minecraft:air" and w.block(SEED, x, -64, z) == "minecraft:bedrock"
    # schematic: Sponge v3, varint indices in x, z, y order
    box = (x - 2, top - 3, z - 2, x + 2, top + 1, z + 2)
    path = Path(tempfile.mkdtemp()) / "t.schem"
    worldfile.write_schem(path, 5023, box, lambda bx, by, bz: w.block(SEED, bx, by, bz))
    s = read_nbt(gzip.decompress(path.read_bytes()))["Schematic"]
    names = {v: k for k, v in s["Blocks"]["Palette"].items()}
    data, idx, i = s["Blocks"]["Data"], [], 0
    while i < len(data):
        v = shift = 0
        while True:
            b = data[i]
            i += 1
            v |= (b & 0x7F) << shift
            shift += 7
            if b < 0x80:
                break
        idx.append(v)
    W, H, L = s["Width"], s["Height"], s["Length"]
    assert (W, H, L) == (5, 5, 5) and len(idx) == 125
    for k, v in enumerate(idx):
        bx, bz, by = box[0] + k % W, box[2] + k // W % L, box[1] + k // (W * L)
        assert names[v] == w.block(SEED, bx, by, bz)


def test_cli_edge_cases():
    import subprocess
    import tempfile
    cli = lambda *args: subprocess.run([sys.executable, "-m", "sft", *args], capture_output=True, text=True)  # noqa: E731
    empty = Path(tempfile.mkdtemp()) / "seeds.txt"
    empty.write_text("\n")
    r = cli("search", "village(within=300)", "--seeds-file", str(empty), "--sample", "0")
    assert r.returncode != 0 and "no seeds in" in r.stderr, r.stderr  # used to search seeds 0.. instead
    r = cli("search", "village(within=300)", "--count", "2000", "--max", "3", "--sample", "0")
    assert r.returncode == 0 and len(r.stdout.split()) == 3, r


if __name__ == "__main__":
    test_mistakes_are_refused_with_a_hint()
    test_failed_branches_leave_nothing_behind()
    test_anywhere_moves_the_condition_from_at()
    test_max_results_is_a_total_and_resume_keeps_the_clock()
    test_a_new_job_resumed_from_a_unit_counts_the_units_before_it()
    test_crack_runs_as_a_job()
    test_loot_without_tables_is_refused()
    test_check_and_scan_run_plugins_like_a_search()
    test_two_islands_keep_their_own_details()
    test_gpu_biome_checks_find_the_same_seeds()
    test_point_biome_checks_are_block_level()
    test_spawn_anchors_conditions()
    test_block_conditions()
    test_chunks_blocks_and_schematic()
    test_cli_edge_cases()
    print("ok")
