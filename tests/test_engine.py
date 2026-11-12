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


if __name__ == "__main__":
    test_mistakes_are_refused_with_a_hint()
    test_failed_branches_leave_nothing_behind()
    test_anywhere_moves_the_condition_from_at()
    test_max_results_is_a_total_and_resume_keeps_the_clock()
    print("ok")
