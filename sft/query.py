"""Query DSL <-> condition tree (the JSON the Rust engine evaluates).

The text form is a Python expression, parsed with `ast` and never evaluated:

    village(within=300) and pillager_outpost(within=500)
    near("village", "pillager_outpost", dist=150, within=1000)
    biome(["#is_jungle", "bamboo_jungle"], within=400, fraction=0.2) and not monument(within=2000)
    structure("village_plains", min=2, within=600, at=(1000, -500))
    climate("continentalness", at=(0, 0), min=0.6) and height(at=(0, 0), min=120)
    native("example_rust", "spawn_island") and py("example_python.even_seed")

Any call that is not a known condition is shorthand for structure(name, ...).
py(...) conditions run in Python on the engine's matches, so they may only appear as top-level
`and` terms (see split_python).
"""
import ast

CALLS = {"structure", "near", "biome", "climate", "height", "native", "py"}
POSITIONAL = {"structure": ["name"], "near": ["a", "b"], "biome": ["name"], "climate": ["param"], "native": ["plugin", "filter"], "py": ["name"]}


class QueryError(ValueError):
    pass


def _literal(node: ast.AST):
    try:
        v = ast.literal_eval(node)
    except ValueError as e:
        raise QueryError(f"only literals are allowed as arguments: {ast.unparse(node)}") from e
    return list(v) if isinstance(v, tuple) else v


def _node(n: ast.AST) -> dict:
    if isinstance(n, ast.BoolOp):
        return {"op": "and" if isinstance(n.op, ast.And) else "or", "args": [_node(v) for v in n.values]}
    if isinstance(n, ast.UnaryOp) and isinstance(n.op, ast.Not):
        return {"op": "not", "arg": _node(n.operand)}
    if isinstance(n, ast.Name):  # bare name: structure with defaults
        return {"op": "structure", "name": n.id}
    if not isinstance(n, ast.Call) or not isinstance(n.func, ast.Name):
        raise QueryError(f"expected a condition, got: {ast.unparse(n)}")
    name, args = n.func.id, [_literal(a) for a in n.args]
    kw = {k.arg: _literal(k.value) for k in n.keywords}
    if name not in CALLS:  # shorthand: village(within=300)
        args, name = [name, *args], "structure"
    names = POSITIONAL.get(name, [])
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


def to_text(node: dict, parent: str = "") -> str:
    """Condition tree -> DSL text (inverse of parse)."""
    op = node["op"]
    if op in ("and", "or"):
        s = f" {op} ".join(to_text(a, op) for a in node["args"])
        return f"({s})" if parent and parent != op else s
    if op == "not":
        return f"not {to_text(node['arg'], 'not')}"
    pos = POSITIONAL.get(op, [])
    args = [repr(node[k]) for k in pos if k in node]
    extra = node.get("params", {}) if op in ("native", "py") else {k: v for k, v in node.items() if k not in ("op", *pos)}
    args += [f"{k}={_arg(v)}" for k, v in extra.items()]
    return f"{op}({', '.join(args)})"


def split_python(node: dict) -> tuple[dict | None, list[dict]]:
    """Separates py(...) terms (run in Python on engine matches) from the engine query."""
    terms = node["args"] if node["op"] == "and" else [node]
    py = [t for t in terms if t["op"] == "py"]
    rest = [t for t in terms if t["op"] != "py"]

    def has_py(n):
        return n["op"] == "py" or any(has_py(c) for c in n.get("args", [])) or ("arg" in n and has_py(n["arg"]))

    if any(has_py(t) for t in rest):
        raise QueryError("py(...) conditions must be top-level 'and' terms")
    engine = None if not rest else rest[0] if len(rest) == 1 else {"op": "and", "args": rest}
    return engine, py
