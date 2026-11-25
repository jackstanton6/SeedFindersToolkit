"""DSL parse <-> text round trip and py(...) splitting. Run: python tests/test_query.py (or pytest)."""
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from sft.query import QueryError, parse, split_python, to_text  # noqa: E402


def test_round_trip():
    for text in [
        "village(within=300) and pillager_outpost(within=600)",
        "near('village', 'pillager_outpost', dist=150, within=1000)",
        "(biome(['#is_jungle', 'bamboo_jungle'], within=400, fraction=0.2) or monument(within=500)) and not mansion(within=3000)",
        "climate('continentalness', at=(0, 0), min=0.6) and height(at=(10, -20), min=120)",
        "native('example_rust', 'spawn_island', radius=96)",
        "nearest('village') < 400 and cluster('swamp_hut', count=3, dist=128, within=5000)",
        "village(within=500) + 2 * pillager_outpost(within=500) >= 3",
        "maximize(biome_area('mushroom_fields', window=1024) - nearest('monument') / 10)",
        "anywhere(slime(radius=24) >= 6, within=500, step=32)",
        "5 < -height(at=(0, 0)) <= 10.5 or island(lagoon=True) == 4096",
    ]:
        tree = parse(text)
        assert parse(to_text(tree)) == tree, text


def test_shorthand_and_errors():
    assert parse("village") == {"op": "structure", "name": "village"}
    assert parse("stronghold(within=1500)") == {"op": "structure", "name": "stronghold", "within": 1500}
    for bad in ["village(within=x)", "1 + 2", "near('a', 'b', 'c')", "village(within=", "village(within={[]: 1})", "village(within={1, ()} - {[]})"]:
        try:
            parse(bad)
        except QueryError:
            continue
        raise AssertionError(f"accepted: {bad}")


def test_example_markdown_exporter_takes_any_detail():
    import importlib.util
    import tempfile
    spec = importlib.util.spec_from_file_location("example_python", Path(__file__).resolve().parent.parent / "plugins/example_python/plugin.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    out = Path(tempfile.mkdtemp()) / "r.md"
    mod.export_markdown([(1, [{"type": "island", "x": 5, "z": 6, "area": 9.0}, {"type": "anchor", "x": 1, "z": 2}, {"name": "minecraft:village", "x": 3, "z": 4}])], out)
    assert "island (5, 6)" in out.read_text() and "village (3, 4)" in out.read_text(), out.read_text()


def test_comparisons():
    assert parse("nearest('village') < 400") == {"op": "nearest", "name": "village", "max": math.nextafter(400, -math.inf)}
    assert parse("300 <= village(within=500)") == {"op": "structure", "name": "village", "within": 500, "min": 300}
    assert to_text(parse("nearest('village') < 400")) == "nearest('village') < 400"
    assert to_text(parse("slime(radius=64) >= 12")) == "slime(radius=64) >= 12"
    assert to_text(parse("12 <= slime() <= 20")) == "12 <= slime() <= 20"
    assert parse("village + 1 > 2")["op"] == "cmp"
    for bad in ["village < monument", "1 < 2", "village + 1"]:
        try:
            parse(bad)
        except QueryError:
            continue
        raise AssertionError(f"accepted: {bad}")


def test_split_python():
    engine, py = split_python(parse("village(within=300) and py('p.f', x=1)"))
    assert engine == {"op": "structure", "name": "village", "within": 300}
    assert py == [{"op": "py", "name": "p.f", "params": {"x": 1}}]
    try:
        split_python(parse("village or py('p.f')"))
    except QueryError:
        return
    raise AssertionError("py under 'or' must be rejected")


def test_home_folder():
    import os
    import tempfile
    from sft import data
    old = os.environ.pop("SFT_HOME", None)
    try:
        repo = Path(__file__).resolve().parent.parent
        assert data.home(repo) == repo  # a source checkout keeps its own data/, cache/ and plugins/
        installed = Path(tempfile.mkdtemp()) / "site-packages"  # an installed package: no pyproject.toml beside it
        assert data.home(installed).name == "sft" and installed not in data.home(installed).parents
        os.environ["SFT_HOME"] = str(installed / "elsewhere")
        assert data.home(repo) == installed / "elsewhere"
    finally:
        os.environ.pop("SFT_HOME", None)
        if old is not None:
            os.environ["SFT_HOME"] = old


if __name__ == "__main__":
    test_round_trip()
    test_shorthand_and_errors()
    test_comparisons()
    test_split_python()
    test_example_markdown_exporter_takes_any_detail()
    test_home_folder()
    print("ok")
