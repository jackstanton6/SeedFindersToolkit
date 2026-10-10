"""DSL parse <-> text round trip and py(...) splitting. Run: python tests/test_query.py (or pytest)."""
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
    ]:
        tree = parse(text)
        assert parse(to_text(tree)) == tree, text


def test_shorthand_and_errors():
    assert parse("village") == {"op": "structure", "name": "village"}
    assert parse("stronghold(within=1500)") == {"op": "structure", "name": "stronghold", "within": 1500}
    for bad in ["village(within=x)", "1 + 2", "near('a', 'b', 'c')", "village(within="]:
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


if __name__ == "__main__":
    test_round_trip()
    test_shorthand_and_errors()
    test_split_python()
    print("ok")
