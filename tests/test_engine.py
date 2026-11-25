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


if __name__ == "__main__":
    test_mistakes_are_refused_with_a_hint()
    test_failed_branches_leave_nothing_behind()
    test_anywhere_moves_the_condition_from_at()
    test_max_results_is_a_total_and_resume_keeps_the_clock()
    test_a_new_job_resumed_from_a_unit_counts_the_units_before_it()
    test_crack_runs_as_a_job()
    print("ok")
