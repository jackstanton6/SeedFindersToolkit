"""Query DSL <-> condition tree (the JSON the Rust engine evaluates).

The text form is a Python expression, parsed with `ast` and never evaluated:

    village(within=300) and pillager_outpost(within=500)
    near("village", "pillager_outpost", dist=150, within=1000)
    biome(["#is_jungle", "bamboo_jungle"], within=400, fraction=0.2) and not monument(within=2000)
    structure("village_plains", min=2, within=600, at=(1000, -500))
    climate("continentalness", at=(0, 0), min=0.6) and height(at=(0, 0), min=120)
    native("example_rust", "spawn_island") and py("example_python.even_seed")
    nearest("village") < 400 and cluster("swamp_hut", count=3, dist=128, within=5000)
    village(within=500) + 2 * pillager_outpost(within=500) >= 3
    maximize(biome_area("mushroom_fields", window=1024)) and anywhere(slime(radius=24) >= 6, within=500)
    loot("buried_treasure", "diamond", within=400) >= 2 and portal_frame(within=200) >= 8

Every condition is also a number (count, distance, area, height, ...): comparisons and arithmetic
use the number, a bare condition passes on its default range (e.g. count >= 1).
Any call that is not a known condition is shorthand for structure(name, ...).
py(...) conditions run in Python on the engine's matches, so they may only appear as top-level
`and` terms (see split_python).
"""
import ast
import math

POSITIONAL = {
    "structure": ["name"], "nearest": ["name"], "near": ["a", "b"], "cluster": ["names"], "chain": ["names"],
    "overlap": ["a", "b"], "bingo": ["names"], "biome": ["name"], "biome_area": ["name"], "biome_count": [],
    "biome_border": ["a", "b"], "island": [], "climate": ["param"], "height": [], "peak": [], "trench": [],
    "cliff": [], "overhang": [], "void": [], "lake": ["fluid"], "slime": [], "loot": ["name", "item"], "portal_frame": [],
    "portal_obsidian": [], "spawn_distance": [], "bedrock": ["blocks"],
    "block": ["name"], "spawn_block": ["name"], "ore_vein": ["type"],
    "native": ["plugin", "filter"], "py": ["name"], "maximize": ["expr"], "minimize": ["expr"], "anywhere": ["cond"],
}
CALLS = set(POSITIONAL)
NODE_ARGS = {"maximize": "expr", "minimize": "expr", "anywhere": "cond"}  # arguments that are queries, not literals
ARITH = {ast.Add: "add", ast.Sub: "sub", ast.Mult: "mul", ast.Div: "div"}
SYMBOL = {"add": "+", "sub": "-", "mul": "*", "div": "/"}


class QueryError(ValueError):
    pass


def _literal(node: ast.AST):
    try:
        v = ast.literal_eval(node)
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError) as e:  # e.g. {[]: 1} is a TypeError
        raise QueryError(f"only literals are allowed as arguments: {ast.unparse(node)}") from e
    return list(v) if isinstance(v, tuple) else v


def _expr(n: ast.AST) -> dict:
    if isinstance(n, ast.Constant) and isinstance(n.value, (int, float)) and not isinstance(n.value, bool):
        return {"op": "num", "value": n.value}
    if isinstance(n, ast.UnaryOp) and isinstance(n.op, ast.USub):
        return {"op": "neg", "a": _expr(n.operand)}
    if isinstance(n, ast.BinOp) and type(n.op) in ARITH:
        return {"op": ARITH[type(n.op)], "a": _expr(n.left), "b": _expr(n.right)}
    return _node(n)


def _compare(n: ast.Compare) -> dict:
    """a <= x < b etc. A bare metric takes the bounds itself (keeps details and pre-checks)."""
    items, bounds = [n.left, *n.comparators], {}
    exprs = [i for i in items if not (isinstance(i, ast.Constant) or isinstance(i, ast.UnaryOp) and isinstance(i.operand, ast.Constant))]
    if len(exprs) != 1:
        raise QueryError(f"compare one expression with numbers: {ast.unparse(n)}")
    for left, op, right in zip(items, n.ops, items[1:]):
        flip = right is exprs[0]  # 5 < x  ==  x > 5
        num = _literal(left if flip else right)
        kind = {ast.Lt: "lt", ast.LtE: "le", ast.Gt: "gt", ast.GtE: "ge", ast.Eq: "eq"}.get(type(op))
        if kind is None or not isinstance(num, (int, float)):
            raise QueryError(f"unsupported comparison: {ast.unparse(n)}")
        if flip:
            kind = {"lt": "gt", "le": "ge", "gt": "lt", "ge": "le", "eq": "eq"}[kind]
        if kind in ("ge", "eq"):
            bounds["min"] = num
        if kind in ("le", "eq"):
            bounds["max"] = num
        if kind == "gt":
            bounds["min"] = math.nextafter(num, math.inf)
        if kind == "lt":
            bounds["max"] = math.nextafter(num, -math.inf)
    e = _expr(exprs[0])
    if e["op"] in CALLS and e["op"] not in NODE_ARGS and "min" not in e and "max" not in e:
        return {**e, **bounds}
    return {"op": "cmp", "expr": e, **bounds}


def _node(n: ast.AST) -> dict:
    if isinstance(n, ast.Compare):
        return _compare(n)
    if isinstance(n, ast.BoolOp):
        return {"op": "and" if isinstance(n.op, ast.And) else "or", "args": [_node(v) for v in n.values]}
    if isinstance(n, ast.UnaryOp) and isinstance(n.op, ast.Not):
        return {"op": "not", "arg": _node(n.operand)}
    if isinstance(n, ast.Name):  # bare name: structure with defaults
        return {"op": "structure", "name": n.id}
    if not isinstance(n, ast.Call) or not isinstance(n.func, ast.Name):
        raise QueryError(f"expected a condition, got: {ast.unparse(n)}")
    name = n.func.id
    if name in NODE_ARGS:
        if len(n.args) != 1:
            raise QueryError(f"{name}() takes one query argument")
        arg = _expr(n.args[0]) if NODE_ARGS[name] == "expr" else _node(n.args[0])
        return {"op": name, NODE_ARGS[name]: arg, **{k.arg: _literal(k.value) for k in n.keywords}}
    args, kw = [_literal(a) for a in n.args], {k.arg: _literal(k.value) for k in n.keywords}
    if name not in CALLS:  # shorthand: village(within=300)
        args, name = [name, *args], "structure"
    names = POSITIONAL[name]
    if len(args) > len(names):
        raise QueryError(f"{name}() takes {len(names)} positional argument(s)")
    node = {"op": name, **dict(zip(names, args))}
    if name in ("native", "py"):
        node["params"] = kw
    else:
        node.update(kw)
    return node


def parse(text: str) -> dict:
    """DSL text -> condition tree."""
    try:
        tree = ast.parse(text.strip(), mode="eval")
    except SyntaxError as e:
        raise QueryError(f"syntax error at column {e.offset}: {e.msg}") from e
    return _node(tree.body)


def _arg(v) -> str:
    return repr(tuple(v)) if isinstance(v, list) and len(v) == 2 and all(isinstance(x, int) for x in v) else repr(v)


def _bound(v, toward: float) -> tuple[str, str]:
    """(operator suffix, number) for a bound; nextafter-shifted bounds print as strict."""
    orig = math.nextafter(v, toward)
    if isinstance(v, float) and orig == round(orig) and v != orig:
        return "", repr(int(orig))
    return "=", repr(v)


def expr_text(node: dict, parent: int = 0) -> str:
    op = node["op"]
    if op == "num":
        return repr(node["value"])
    if op == "neg":
        return f"-{expr_text(node['a'], 3)}"
    if op in SYMBOL:
        prec = 1 if op in ("add", "sub") else 2
        s = f"{expr_text(node['a'], prec)} {SYMBOL[op]} {expr_text(node['b'], prec + 1)}"
        return f"({s})" if prec < parent else s
    return to_text(node, "expr")


def to_text(node: dict, parent: str = "") -> str:
    """Condition tree -> DSL text (inverse of parse)."""
    op = node["op"]
    bounded = op not in ("cmp", "and", "or", "not", "num", "neg", *SYMBOL, *NODE_ARGS)
    if op == "cmp" or bounded and any(isinstance(node.get(k), (int, float)) for k in ("min", "max")):
        num = {k: v for k, v in node.items() if k in ("min", "max") and isinstance(v, (int, float))}
        e = expr_text(node["expr"]) if op == "cmp" else to_text({k: v for k, v in node.items() if k not in num})
        lo, hi = num.get("min"), num.get("max")
        if lo is not None and lo == hi:
            s = f"{e} == {repr(lo)}"
        else:
            s = e
            if lo is not None and hi is None:
                eq, n = _bound(lo, -math.inf)
                s = f"{s} >{eq} {n}"
            elif lo is not None:
                eq, n = _bound(lo, -math.inf)
                s = f"{n} <{eq} {s}"
            if hi is not None:
                eq, n = _bound(hi, math.inf)
                s = f"{s} <{eq} {n}"
        return f"({s})" if parent == "expr" else s
    if op in NODE_ARGS:
        key = NODE_ARGS[op]
        inner = expr_text(node[key]) if key == "expr" else to_text(node[key])
        extra = [f"{k}={_arg(v)}" for k, v in node.items() if k not in ("op", key)]
        return f"{op}({', '.join([inner, *extra])})"
    if op in ("and", "or"):
        s = f" {op} ".join(to_text(a, op) for a in node["args"])
        return f"({s})" if parent and parent != op else s
    if op == "not":
        return f"not {to_text(node['arg'], 'not')}"
    pos = POSITIONAL.get(op, [])
    name = node.get("name")
    if op == "structure" and isinstance(name, str) and name.isidentifier() and name not in CALLS and name not in ("and", "or", "not"):
        op, pos = name, ["name"]  # shorthand: village(within=300); "name" is then not printed again
        args = []
    else:
        args = [repr(node[k]) for k in pos if k in node]
    extra = node.get("params", {}) if op in ("native", "py") else {k: v for k, v in node.items() if k not in ("op", *pos)}
    args += [f"{k}={_arg(v)}" for k, v in extra.items()]
    return f"{op}({', '.join(args)})"


def nodes(tree):
    """Every condition/expression node in a tree."""
    if isinstance(tree, dict):
        if "op" in tree:
            yield tree
        for v in tree.values():
            yield from nodes(v)
    elif isinstance(tree, list):
        for v in tree:
            yield from nodes(v)


def split_python(node: dict) -> tuple[dict | None, list[dict]]:
    """Separates py(...) terms (run in Python on engine matches) from the engine query."""
    terms = node["args"] if node["op"] == "and" else [node]
    py = [t for t in terms if t["op"] == "py"]
    rest = [t for t in terms if t["op"] != "py"]

    if any(n["op"] == "py" for t in rest for n in nodes(t)):
        raise QueryError("py(...) conditions must be top-level 'and' terms")
    engine = None if not rest else rest[0] if len(rest) == 1 else {"op": "and", "args": rest}
    return engine, py
