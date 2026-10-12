"""Block query builder: the query as nested blocks that read like sentences, a palette to drag
conditions from, and the DSL text box kept in sync (blocks <-> text). The text is still the source
of truth for searches; blocks are another view of the same tree (sft.query)."""
import ast
import json
import math
import re

from PySide6.QtCore import QByteArray, QMimeData, QPoint, QRect, QSize, Qt, QTimer
from PySide6.QtGui import QDrag, QFont
from PySide6.QtWidgets import (
    QAbstractItemView, QCheckBox, QComboBox, QFrame, QHBoxLayout, QLabel, QLayout, QLineEdit, QMenu, QPlainTextEdit,
    QPushButton, QScrollArea, QSpinBox, QSplitter, QToolButton, QTreeWidget, QTreeWidgetItem, QVBoxLayout, QWidget,
)

from sft import query

MONO = QFont("Consolas", 9)
MIME = "application/x-sft-block"
SPAWN = [0, 0]

# op: (category, palette label, sentence, extra params under "more", engine defaults, help)
SPECS = {
    "structure": ("structures", "structure near a point", "at least {min} {name} within {within} blocks of {at}", ["biome", "tags", "max"],
                  {"within": 1000, "at": SPAWN, "min": 1}, "Structures whose start is within the radius and that really generate (biome and terrain checks included)."),
    "nearest": ("structures", "nearest structure", "the nearest {name} is at most {max} blocks from {at}", ["within", "min", "biome", "tags"],
                {"within": 3000, "at": SPAWN}, "Distance to the closest generated start, searched up to 'within' blocks away."),
    "near": ("structures", "two structures close together", "at least {min} {a} with a {b} within {dist} blocks of it, searching {within} blocks around {at}",
             ["biome", "tags", "max"], {"dist": 128, "within": 1000, "at": SPAWN, "min": 1}, "A structure with another one close by, measured between start positions."),
    "cluster": ("structures", "group of structures (farms)", "{count} or more {names} inside one circle of radius {dist}, searching {within} blocks around {at}; whole structures inside (AFK) {afk}",
                ["min", "max"], {"count": 2, "dist": 128, "within": 1000, "at": SPAWN, "afk": False}, "Quad witch huts, double monuments: the most structures one circle can cover."),
    "chain": ("structures", "chain of structures", "the chain {names}, each within {dist} blocks of the previous, searching {within} blocks around {at}", ["min", "max"],
              {"dist": 256, "within": 1000, "at": SPAWN}, "Structures in a row, each close to the one before."),
    "overlap": ("structures", "overlapping structures", "a {a} overlapping a {b}, searching {within} blocks around {at}", ["margin", "min", "max"],
                {"within": 1000, "at": SPAWN, "margin": 0, "min": 1}, "Structures whose boxes intersect, like a temple inside a village."),
    "bingo": ("structures", "many structure types", "at least {min} different structure types within {within} blocks of {at}", ["names", "max"],
              {"within": 2000, "at": SPAWN}, "How many kinds of structure have a start nearby."),
    "biome": ("biomes", "biome near a point", "biome {name} within {within} blocks of {at}", ["fraction", "y", "step"],
              {"within": 0, "at": SPAWN, "y": 64, "step": 16, "fraction": 0}, "A biome somewhere in the circle; 'fraction' asks for that share of it."),
    "biome_area": ("biomes", "large biome area", "a {name} area of at least {min} blocks² touching the {within}-block circle around {at}", ["window", "y", "step", "max"],
                   {"within": 0, "window": 512, "at": SPAWN, "y": 64, "step": 16, "min": 1}, "The largest connected patch of a biome."),
    "biome_count": ("biomes", "many biomes", "at least {min} different biomes within {within} blocks of {at}", ["among", "y", "step", "max"],
                    {"within": 0, "at": SPAWN, "y": 64, "step": 16, "min": 1}, "How many biomes appear in the circle."),
    "biome_border": ("biomes", "two biomes touching", "{a} touching {b} along at least {min} blocks within {within} blocks of {at}", ["y", "step", "max"],
                     {"within": 0, "at": SPAWN, "y": 64, "step": 16, "min": 1}, "Length of the border between two biomes."),
    "island": ("biomes", "island", "an island of at least {min} blocks² within {within} blocks of {at}", ["biomes", "window", "min_circularity", "max_elongation", "lagoon", "y", "step", "max"],
               {"within": 0, "window": 1024, "at": SPAWN, "y": 63, "step": 16, "min_circularity": 0, "lagoon": False, "min": 1}, "Land surrounded by ocean, optionally of given biomes and shape."),
    "climate": ("terrain", "climate value", "{param} at {at} between {min} and {max}", ["y"], {"at": SPAWN, "y": 64},
                "A raw climate parameter: continentalness > 0.8 is far inland, erosion > 0.6 is flat."),
    "height": ("terrain", "terrain height", "terrain height at {at} between {min} and {max}", ["heightmap"], {"at": SPAWN, "heightmap": "surface"},
               "Exact surface height at one spot."),
    "peak": ("terrain", "mountain peak", "the highest point within {within} blocks of {at} is at least {min}", ["step", "max"], {"within": 0, "at": SPAWN, "step": 32},
             "Highest surface in the circle (y level)."),
    "trench": ("terrain", "ocean trench", "the lowest ocean floor within {within} blocks of {at} is at most {max}", ["step", "min"], {"within": 0, "at": SPAWN, "step": 32},
               "Lowest ocean floor in the circle (y level)."),
    "cliff": ("terrain", "cliff", "a cliff at least {min} blocks tall within {within} blocks of {at}", ["step", "max"], {"within": 0, "at": SPAWN, "step": 32, "min": 1},
              "Biggest height step between neighbouring samples."),
    "overhang": ("terrain", "overhang", "an overhang at least {min} blocks high within {within} blocks of {at}", ["step", "max"], {"within": 0, "at": SPAWN, "step": 32, "min": 1},
                 "Longest air gap under solid ground above sea level."),
    "void": ("terrain", "underground air gap", "an air gap at least {min} blocks tall below sea level within {within} blocks of {at}", ["step", "max"],
             {"within": 0, "at": SPAWN, "step": 32, "min": 1}, "Longest air run below sea level (caves)."),
    "lake": ("terrain", "underground lake", "a {fluid} lake at least {min} blocks deep within {within} blocks of {at}", ["step", "max"],
             {"fluid": "lava", "within": 0, "at": SPAWN, "step": 32, "min": 1}, "Aquifer lava or water under the surface."),
    "slime": ("terrain", "slime chunks", "at least {min} slime chunks within {radius} blocks of a spot within {within} blocks of {at}", ["max"],
              {"radius": 128, "within": 0, "at": SPAWN, "min": 1}, "Slime chunks around the best spot (seed-only, very fast; GPU capable)."),
    "loot": ("loot", "items in chests", "at least {min} {item} in {name} chests within {within} blocks of {at}", ["enchant", "total", "max"],
             {"within": 1000, "at": SPAWN, "min": 1, "total": False}, "Exact chest contents of buried treasure, shipwrecks, ruined portals, igloos, temples, ocean ruins."),
    "portal_frame": ("loot", "ruined portal frame", "a ruined portal frame scoring at least {min} of 10 within {within} blocks of {at}", ["max"],
                     {"within": 1000, "at": SPAWN, "min": 10}, "10 means the frame is complete and can be lit; each obsidian still missing costs 1."),
    "portal_obsidian": ("loot", "ruined portal obsidian", "a ruined portal with at least {min} obsidian within {within} blocks of {at}", ["max"],
                        {"within": 1000, "at": SPAWN, "min": 1}, "All plain obsidian of the portal."),
    "native": ("plugins", "native plugin filter", "plugin {plugin} filter {filter}", ["params", "min", "max"], {}, "A filter from a Rust/C plugin."),
    "py": ("plugins", "python plugin condition", "python condition {name}", ["params"], {}, "A Python plugin condition (top-level 'all of' only)."),
}
# blocks holding another block
CONTAINERS = {
    "anywhere": ("scores", "somewhere in an area", "somewhere within {within} blocks of {at}, checked every {step} blocks:", ["anchor"],
                 {"within": 1000, "at": SPAWN, "step": 64}, "The inner condition, moved to any spot in the area (coordinates inside are relative)."),
    "maximize": ("scores", "rank: highest first", "rank seeds, highest first, by:", [], {}, "Keeps the best seeds by this number instead of filtering."),
    "minimize": ("scores", "rank: lowest first", "rank seeds, lowest first, by:", [], {}, "Keeps the best seeds by this number, lowest first."),
}
GROUPS = {"and": "ALL of these", "or": "ANY of these", "none": "NONE of these"}
COLORS = {"structures": "#4a7fc0", "biomes": "#4f9a5d", "terrain": "#9a7a45", "loot": "#c09a3a", "scores": "#8a5fb5", "plugins": "#707070",
          "and": "#5b6b7a", "or": "#7a6aa0", "none": "#a05050", "expr": "#8a5fb5"}
CATEGORIES = ["structures", "biomes", "terrain", "loot", "scores", "plugins"]

_COUNT = {"op": "structure", "name": "village", "within": 1000}
TEMPLATES = {  # category -> condition -> default node (without "op")
    "structures": {
        "structure": {"name": "village", "within": 500},
        "nearest": {"name": "stronghold", "within": 3000, "max": 1500},
        "near": {"a": "village", "b": "pillager_outpost", "dist": 150, "within": 1000},
        "cluster": {"names": "swamp_hut", "count": 3, "dist": 128, "within": 5000, "afk": True},
        "chain": {"names": ["village", "pillager_outpost", "village"], "dist": 400, "within": 2000},
        "overlap": {"a": "desert_pyramid", "b": "village", "within": 3000},
        "bingo": {"within": 1500, "min": 12},
    },
    "biomes": {
        "biome": {"name": "mushroom_fields", "within": 500},
        "biome_area": {"name": "mushroom_fields", "within": 2000, "window": 1024, "step": 32, "min": 100000},
        "biome_count": {"within": 1000, "step": 32, "min": 20},
        "biome_border": {"a": "desert", "b": "snowy_plains", "within": 2000, "step": 32},
        "island": {"within": 3000, "window": 1024, "step": 32, "min_circularity": 0.5, "min": 50000},
    },
    "terrain": {
        "climate": {"param": "continentalness", "min": 0.5},
        "height": {"min": 120},
        "peak": {"within": 500, "min": 250},
        "trench": {"within": 1000, "max": -20},
        "cliff": {"within": 300, "step": 8, "min": 40},
        "overhang": {"within": 200, "step": 16, "min": 20},
        "void": {"within": 64, "step": 16, "min": 30},
        "lake": {"fluid": "lava", "within": 200, "step": 16, "min": 10},
        "slime": {"radius": 128, "within": 300, "min": 14},
    },
    "loot": {
        "loot": {"name": "buried_treasure", "item": "diamond", "within": 500, "min": 2},
        "portal_frame": {"within": 300, "min": 8},
        "portal_obsidian": {"within": 300, "min": 10},
    },
    "scores": {
        "maximize": {"expr": _COUNT},
        "minimize": {"expr": {"op": "nearest", "name": "stronghold", "within": 3000}},
        "cmp": {"expr": {"op": "sub", "a": _COUNT, "b": {**_COUNT, "name": "pillager_outpost"}}, "min": 2},
        "anywhere": {"cond": {"op": "slime", "radius": 24, "min": 5}, "within": 1000, "step": 32},
    },
    "plugins": {
        "native": {"plugin": "example_rust", "filter": "spawn_island", "params": {}},
        "py": {"name": "example_python.seed_contains", "params": {"text": "777"}},
    },
}
PRESETS = {  # ready-made queries (overworld speedrun parts and common hunts)
    "speedrun: village + ruined portal at spawn": "village(within=160) and ruined_portal(within=160, tags=['placement:on_land_surface'])",
    "speedrun: near-complete portal frame": "portal_frame(within=200) >= 8 and village(within=300)",
    "speedrun: portal + chest obsidian completes the frame": "portal_frame(within=150) + loot('ruined_portal', 'obsidian', within=150) >= 10",
    "speedrun: portal chest with flint and steel": "loot('ruined_portal', 'flint_and_steel', within=200) and village(within=300)",
    "speedrun: buried treasure with diamonds": "loot('buried_treasure', 'diamond', within=300) >= 2",
    "speedrun: shipwreck iron": "loot('shipwreck', 'iron_ingot', within=200, total=True) >= 8",
    "enchanted golden apple in a desert temple": "loot('desert_pyramid', 'enchanted_golden_apple', within=2000)",
    "speedrun: shipwreck + buried treasure at spawn": "shipwreck(within=200, tags=['chest:']) and buried_treasure(within=200)",
    "speedrun: desert temple + village": "desert_pyramid(within=200) and village(within=300)",
    "speedrun: close stronghold": "nearest('stronghold', within=2000) < 1450 and village(within=300)",
    "quad witch huts (AFK spot)": "cluster('swamp_hut', count=4, dist=128, within=20000, afk=True)",
    "triple witch huts": "cluster('swamp_hut', count=3, dist=128, within=10000, afk=True)",
    "double ocean monument": "cluster('monument', count=2, dist=128, within=10000, afk=True)",
    "slime farm (top ~1%)": "slime(radius=128, within=300) >= 37",
    "mushroom island near spawn": "island(biomes=['mushroom_fields'], within=1500, window=1024)",
    "biggest mushroom island (record)": "maximize(island(biomes=['mushroom_fields'], within=3000, window=1500))",
    "almost every biome near spawn": "biome_count(within=3000, step=32) >= 54",
    "structure bingo": "bingo(within=1500) >= 14",
    "desert temple inside a village": "overlap('desert_pyramid', 'village', within=3000)",
    "stronghold under a village": "near('stronghold', 'village', dist=96, within=3000)",
    "all three ring-1 strongholds close to spawn": "stronghold(within=1800) >= 3",
    "basement igloo near spawn": "igloo(within=600, tags=['basement'])",
    "ocean ruin cluster near spawn": "ocean_ruin(within=500, tags=['cluster'])",
}
STRUCT_KEYS = {"structure": ["name"], "nearest": ["name"], "loot": ["name"], "near": ["a", "b"], "overlap": ["a", "b"], "anywhere": ["anchor"]}
STRUCT_LISTS = {"cluster": "names", "chain": "names", "bingo": "names"}
BIOME_KEYS = {"biome": ["name"], "biome_area": ["name"], "biome_border": ["a", "b"]}
BIOME_LISTS = {"island": "biomes", "biome_count": "among"}
INT_KEYS = {"within", "dist", "radius", "step", "window", "count", "y", "margin"}
CHOICES = {"param": ["temperature", "humidity", "continentalness", "erosion", "depth", "weirdness"], "fluid": ["lava", "water"],
           "heightmap": ["surface", "ocean_floor"]}
BIOME_TAGS = ["#is_ocean", "#is_deep_ocean", "#is_beach", "#is_river", "#is_mountain", "#is_hill", "#is_forest", "#is_taiga",
              "#is_jungle", "#is_savanna", "#is_badlands"]
LOOT_STRUCTURES = ["buried_treasure", "shipwreck", "ruined_portal", "igloo", "desert_pyramid", "jungle_temple", "ocean_ruin"]


def spec(op):
    return SPECS.get(op) or CONTAINERS.get(op)


# ------------------------------------------------------------------------------------------------
# Plain-English description (also used by the wizard preview)
# ------------------------------------------------------------------------------------------------

def _say(v) -> str:
    if isinstance(v, list) and len(v) == 2 and all(isinstance(x, int) for x in v):
        return "spawn" if v == [0, 0] else f"({v[0]}, {v[1]})"
    if isinstance(v, list):
        return ", ".join(_say(x) for x in v)
    if isinstance(v, bool):
        return "yes" if v else "no"
    if isinstance(v, float):
        return "any" if math.isinf(v) else f"{v:.10g}"
    return str(v).replace("_", " ").removeprefix("#is ").removeprefix("minecraft:")


def describe(node: dict | None) -> str:
    """A query in words: 'at least 1 village within 300 blocks of spawn and ...'."""
    if not node:
        return "every seed"
    op = node["op"]
    if op in ("and", "or"):
        parts = [describe(a) for a in node["args"]]
        return (" and " if op == "and" else " or ").join(f"({p})" if " and " in p or " or " in p else p for p in parts) or "every seed"
    if op == "not":
        return f"not {describe(node['arg'])}"
    if op == "cmp":
        lo, hi = node.get("min"), node.get("max")
        rng = f"at least {_say(lo)}" if hi is None else f"at most {_say(hi)}" if lo is None else f"between {_say(lo)} and {_say(hi)}"
        return f"{query.expr_text(node['expr'])} is {rng}"
    sp = spec(op)
    if sp is None:
        return query.to_text(node)
    values = {**sp[4], **node}

    def fill(m):
        k = m.group(1)
        v = values.get(k)
        if v is None:
            return {"min": "any", "max": "any"}.get(k, "?")
        return _say(v)
    s = re.sub(r"\{(\w+)\}", fill, sp[2]).rstrip(":")
    if op == "anywhere":
        s += f" {describe(node['cond'])}"
    elif op in ("maximize", "minimize"):
        s += " " + (describe(node["expr"]) if node["expr"]["op"] in SPECS else query.expr_text(node["expr"]))
    return s.replace("at least any ", "").replace(" between any and any", "").replace("at most any blocks", "any distance")


# ------------------------------------------------------------------------------------------------
# Layout helper
# ------------------------------------------------------------------------------------------------

class FlowLayout(QLayout):
    """Lays widgets out like words: left to right, wrapping (Qt's flow layout example)."""

    def __init__(self, parent=None, spacing=4):
        super().__init__(parent)
        self.items = []
        self.setSpacing(spacing)
        self.setContentsMargins(0, 0, 0, 0)

    def addItem(self, item):
        self.items.append(item)

    def count(self):
        return len(self.items)

    def itemAt(self, i):
        return self.items[i] if 0 <= i < len(self.items) else None

    def takeAt(self, i):
        return self.items.pop(i) if 0 <= i < len(self.items) else None

    def expandingDirections(self):
        return Qt.Orientation(0)

    def hasHeightForWidth(self):
        return True

    def heightForWidth(self, w):
        return self._place(QRect(0, 0, w, 0), True)

    def setGeometry(self, r):
        super().setGeometry(r)
        self._place(r, False)

    def sizeHint(self):
        return self.minimumSize()

    def minimumSize(self):
        s = QSize()
        for it in self.items:
            s = s.expandedTo(it.minimumSize())
        return s

    def _place(self, rect, test):
        x, y, line = rect.x(), rect.y(), 0
        for it in self.items:
            hint = it.sizeHint()
            if x + hint.width() > rect.right() + 1 and x > rect.x():
                x, y, line = rect.x(), y + line + self.spacing(), 0
            if not test:
                it.setGeometry(QRect(QPoint(x, y + max(0, 24 - hint.height()) // 2), hint))  # centre words on fields
            x, line = x + hint.width() + self.spacing(), max(line, hint.height(), 24)
        return y + line - rect.y()


# ------------------------------------------------------------------------------------------------
# Fields: one widget per parameter; get() is None when the parameter should be left out
# ------------------------------------------------------------------------------------------------

def _fmt(v) -> str:
    return "" if v is None or isinstance(v, float) and math.isinf(v) else f"{v:.10g}" if isinstance(v, float) else str(v)


class Field:
    def __init__(self, widget, get, signal):
        self.widget, self.get = widget, get
        self.changed = lambda: None
        signal.connect(lambda *_: self.changed())


def bind(f: Field, data: dict, key: str, default, editor):
    """Writes the field into `data` on every change (left out when empty or the default)."""
    def changed():
        v = f.get()
        if v is None or v == default:
            data.pop(key, None)
        else:
            data[key] = v
        editor.changed()
    f.changed = changed
    return f


def _row(*widgets) -> QWidget:
    box = QWidget()
    lay = QHBoxLayout(box)
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(2)
    for w in widgets:
        lay.addWidget(w)
    return box


def make_field(editor, op: str, key: str, value, default) -> Field:
    """A widget for one parameter, and how to read it back."""
    names = editor.names
    if key in STRUCT_KEYS.get(op, []) or key in BIOME_KEYS.get(op, []) or key in ("biome", "item") or key in CHOICES:
        w = QComboBox(editable=key not in CHOICES)
        w.setMinimumContentsLength(13)
        w.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        pool = (CHOICES.get(key) or (names["loot_structures"] if op == "loot" and key == "name" else names["structures"] if key in STRUCT_KEYS.get(op, [])
                else names["items"] if key == "item" else names["biomes"]))
        w.addItems(([""] if key in ("biome", "anchor") else []) + pool)
        w.setCurrentText(str(value if value is not None else default if default is not None else ""))
        return Field(w, lambda: w.currentText().strip() or None, w.currentTextChanged)
    if key in (STRUCT_LISTS.get(op), BIOME_LISTS.get(op), "tags", "names"):
        w = QLineEdit(", ".join(value) if isinstance(value, list) else value or "", placeholderText="comma separated", toolTip="comma-separated names")
        w.setMinimumWidth(140)
        box = w
        if key != "tags":
            b = QToolButton(text="▾", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup, toolTip="add a name")
            m = QMenu(b)
            for n in (names["biomes"] if key == BIOME_LISTS.get(op) else names["structures"])[:80]:
                m.addAction(n, lambda n=n: w.setText(", ".join(x for x in [w.text().strip(" ,"), n] if x)))
            b.setMenu(m)
            box = _row(w, b)

        def get_list():
            v = [x.strip() for x in w.text().split(",") if x.strip()]
            return None if not v else v[0] if len(v) == 1 and key != "tags" else v
        return Field(box, get_list, w.textChanged)
    if key == "at":
        xs, zs = (QSpinBox(minimum=-30_000_000, maximum=30_000_000, singleStep=16, fixedWidth=74) for _ in range(2))
        v = value or default or SPAWN
        xs.setValue(v[0])
        zs.setValue(v[1])
        box = _row(QLabel("x"), xs, QLabel("z"), zs)
        box.setToolTip("block coordinates (x 0, z 0 = spawn)")
        f = Field(box, lambda: [xs.value(), zs.value()], xs.valueChanged)
        zs.valueChanged.connect(lambda *_: f.changed())
        return f
    if isinstance(default, bool) or isinstance(value, bool) or key in ("afk", "total", "lagoon"):
        w = QCheckBox(checked=bool(value if value is not None else default))
        return Field(w, w.isChecked, w.toggled)
    if key in INT_KEYS and (value is None or isinstance(value, int)):
        w = QSpinBox(minimum=-30_000_000 if key == "y" else 0, maximum=30_000_000, singleStep=16 if key in ("within", "dist", "radius", "window") else 1,
                     fixedWidth=78)
        w.setValue(int(value if value is not None else default if default is not None else 0))
        return Field(w, w.value, w.valueChanged)
    if key == "params":
        w = QLineEdit(repr(value or {}), toolTip="a Python dict, e.g. {'text': '777'}")
        w.setFont(MONO)

        def get_params():
            try:
                v = ast.literal_eval(w.text() or "{}")
                return v if isinstance(v, dict) else {}
            except (ValueError, SyntaxError):
                return value or {}
        return Field(w, get_params, w.textChanged)
    if key in ("min", "max", "fraction", "min_circularity", "max_elongation") or isinstance(value, (int, float)):
        w = QLineEdit(_fmt(value if value is not None else default), placeholderText="any" if key in ("min", "max") else "")
        w.setFixedWidth(64)

        def get_num():
            t = w.text().strip()
            if not t:
                return None
            if value is not None and _fmt(value) == t:
                return value  # keep a strict bound from the text (x < 400 is stored just under 400)
            try:
                f = float(t)
            except ValueError:
                return value
            return int(f) if f.is_integer() and "." not in t and "e" not in t.lower() else f
        return Field(w, get_num, w.textChanged)
    w = QLineEdit("" if value is None else str(value))
    w.setMinimumWidth(100)
    return Field(w, lambda: w.text().strip() or None, w.textChanged)


# ------------------------------------------------------------------------------------------------
# Blocks
# ------------------------------------------------------------------------------------------------

class Handle(QLabel):
    """Drag grip of a block."""

    def __init__(self, block):
        super().__init__("⠿")
        self.block = block
        self.setCursor(Qt.CursorShape.OpenHandCursor)
        self.setToolTip("drag to move")
        self._press = None

    def mousePressEvent(self, e):
        self._press = e.position().toPoint()

    def mouseMoveEvent(self, e):
        if self._press is None or (e.position().toPoint() - self._press).manhattanLength() < 6:
            return
        self._press = None
        ed = self.block.editor
        mime = QMimeData()
        mime.setData(MIME, QByteArray(json.dumps(self.block.node()).encode()))
        drag = QDrag(self)
        drag.setMimeData(mime)
        drag.setPixmap(self.block.grab().scaledToWidth(min(max(self.block.width(), 1), 360)))
        ed.dragging = self.block
        drag.exec(Qt.DropAction.MoveAction)
        ed.dragging = None


class Block(QFrame):
    def __init__(self, editor, kind: str):
        super().__init__()
        self.editor, self.kind, self.holder = editor, kind, None
        self.setObjectName("block")
        self.restyle()
        self.outer = QVBoxLayout(self)
        self.outer.setContentsMargins(6, 4, 4, 4)
        self.outer.setSpacing(3)
        self.head = QHBoxLayout()
        self.head.setSpacing(4)
        self.outer.addLayout(self.head)
        self.handle = Handle(self)
        self.head.addWidget(self.handle, 0, Qt.AlignmentFlag.AlignTop)

    def restyle(self):
        self.setStyleSheet(f"QFrame#block {{ background: rgba(255,255,255,0.045); border: 1px solid #3a3d42; "
                           f"border-left: 4px solid {COLORS.get(self.kind, '#666')}; border-radius: 5px; }}")

    def add_menu(self) -> QMenu:
        b = QToolButton(text="⋮", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup, autoRaise=True, toolTip="block options")
        m = QMenu(b)
        m.addAction("delete", self.remove)
        m.addAction("duplicate", lambda: self.holder.insert(self.holder.index(self) + 1, make_block(self.editor, self.node())))
        for k, title in GROUPS.items():
            m.addAction(f"put inside '{title}'", lambda k=k: self.wrap(k))
        b.setMenu(m)
        self.head.addWidget(b, 0, Qt.AlignmentFlag.AlignTop)
        return m

    def remove(self):
        if self.holder:
            self.holder.take(self)
            self.deleteLater()
            self.editor.changed()

    def wrap(self, kind):
        h, i = self.holder, self.holder.index(self)
        h.take(self)
        g = GroupBlock(self.editor, kind, [])
        g.body.insert(0, self, notify=False)
        h.insert(i, g)

    def contains(self, w) -> bool:
        while w is not None:
            if w is self:
                return True
            w = w.parentWidget()
        return False


class ConditionBlock(Block):
    def __init__(self, editor, node: dict, bounds=True):
        op = node["op"]
        sp = SPECS[op]
        super().__init__(editor, sp[0])
        self.data = dict(node)
        self.setToolTip(sp[5])
        defaults = sp[4]
        sentence = QWidget()
        flow = FlowLayout(sentence)
        shown = set(re.findall(r"\{(\w+)\}", sp[2]))
        for tok in re.split(r"(\{\w+\})", sp[2]):
            if tok.startswith("{"):
                key = tok[1:-1]
                if key not in ("min", "max") or bounds:
                    flow.addWidget(self._field(op, key, defaults.get(key)).widget)
            else:
                for word in tok.split():
                    flow.addWidget(QLabel(word))
        self.head.addWidget(sentence, 1)
        hidden = () if bounds else ("min", "max")
        extra = [k for k in sp[3] if k not in shown and k not in hidden]
        extra += [k for k in node if k not in shown and k not in extra and k != "op" and k not in hidden]
        menu = self.add_menu()
        if extra:
            more = QWidget()
            grid = FlowLayout(more, 10)
            for k in extra:
                grid.addWidget(_row(QLabel(k.replace("_", " ")), self._field(op, k, defaults.get(k)).widget))
            more.setVisible(any(k in node for k in extra))
            toggle = QToolButton(text="more ▾" if more.isVisible() else "more ▸", autoRaise=True, toolTip="more options")
            toggle.clicked.connect(lambda: (more.setVisible(not more.isVisible()), toggle.setText("more ▾" if more.isVisible() else "more ▸")))
            self.outer.addWidget(more)
            self.head.insertWidget(self.head.count() - 1, toggle, 0, Qt.AlignmentFlag.AlignTop)
        menu.addAction(f"help on '{sp[1]}'", lambda: editor.show_help(op))

    def _field(self, op, key, default):
        return bind(make_field(self.editor, op, key, self.data.get(key), default), self.data, key, default, self.editor)

    def node(self) -> dict:
        return dict(self.data)


class TextBlock(Block):
    """A number expression (cmp, or arithmetic inside a ranking) or anything without a sentence, as DSL text."""

    def __init__(self, editor, node: dict, expr=False):
        super().__init__(editor, "expr")
        self.expr, self.data = expr, dict(node)
        cmp = node["op"] == "cmp"
        row = QWidget()
        flow = FlowLayout(row)
        self.edit = QLineEdit(query.expr_text(node["expr"]) if cmp else query.expr_text(node) if expr else query.to_text(node))
        self.edit.setFont(MONO)
        self.edit.setMinimumWidth(220)
        self.edit.textChanged.connect(self._edited)
        if cmp:
            flow.addWidget(QLabel("the number"))
        flow.addWidget(self.edit)
        self.bounds = {}
        if cmp:
            for word, k in (("is between", "min"), ("and", "max")):
                flow.addWidget(QLabel(word))
                w = QLineEdit(_fmt(node.get(k)), placeholderText="any")
                w.setFixedWidth(70)
                w.textChanged.connect(self._edited)
                self.bounds[k] = w
                flow.addWidget(w)
        self.head.addWidget(row, 1)
        self.add_menu()
        self.setToolTip("numbers combine with + - * /, e.g. village(within=500) - pillager_outpost(within=500)")

    def _edited(self):
        t = self.edit.text().strip()
        try:
            n = query._expr(ast.parse(t, mode="eval").body) if self.expr or self.data["op"] == "cmp" else query.parse(t)
        except (query.QueryError, SyntaxError, ValueError) as e:
            self.edit.setStyleSheet("border: 1px solid #c05050")
            self.edit.setToolTip(str(e))
            return
        self.edit.setStyleSheet("")
        if self.data["op"] == "cmp":
            self.data["expr"] = n
            for k, w in self.bounds.items():
                old = self.data.get(k)
                try:
                    v = float(w.text())
                    self.data[k] = old if old is not None and _fmt(old) == w.text().strip() else int(v) if v.is_integer() else v
                except ValueError:
                    self.data.pop(k, None)
        else:
            self.data = n
        self.editor.changed()

    def node(self) -> dict:
        return dict(self.data)


class Body(QWidget):
    """Where blocks sit: a group's list, or a container's single slot. Accepts drops."""

    def __init__(self, owner, max_kids=None):
        super().__init__()
        self.owner, self.kids, self.max_kids = owner, [], max_kids
        self.lay = QVBoxLayout(self)
        self.lay.setContentsMargins(14, 0, 0, 0)
        self.lay.setSpacing(4)
        self.setAcceptDrops(True)
        self.setMinimumHeight(8)

    def index(self, b):
        return self.kids.index(b)

    def take(self, b):
        self.kids.remove(b)
        self.lay.removeWidget(b)
        b.setParent(None)
        b.holder = None

    def insert(self, i, b, notify=True):
        if self.max_kids == 1:
            for old in list(self.kids):
                self.take(old)
                old.deleteLater()
            i = 0
        self.kids.insert(i, b)
        self.lay.insertWidget(i, b)
        b.holder = self
        if notify:
            b.editor.changed()

    def _ok(self, e) -> bool:
        ed = self.owner.editor
        ok = e.mimeData().hasFormat(MIME) and not (ed.dragging is not None and ed.dragging.contains(self))
        if ok:
            e.setDropAction(Qt.DropAction.MoveAction if ed.dragging is not None else Qt.DropAction.CopyAction)
            e.accept()
        else:
            e.ignore()
        return ok

    def dragEnterEvent(self, e):
        self._ok(e)

    def dragMoveEvent(self, e):
        self._ok(e)

    def dropEvent(self, e):
        if not self._ok(e):
            return
        ed = self.owner.editor
        y = e.position().toPoint().y()
        i = next((j for j, k in enumerate(self.kids) if y < k.geometry().center().y()), len(self.kids))
        src = ed.dragging
        if src is not None and src.holder is not None:
            if src.holder is self and self.index(src) < i:
                i -= 1
            src.holder.take(src)
            b = src
        else:
            b = make_block(ed, json.loads(bytes(e.mimeData().data(MIME)).decode()))
        self.insert(min(i, len(self.kids)), b)


def kinds_menu(parent, add) -> QMenu:
    """Menu of every block kind by category; `add(node)` receives a copy of the default node."""
    m = QMenu(parent)
    g = m.addMenu("logic")
    for k, title in GROUPS.items():
        g.addAction(title, lambda k=k: add({"op": "not", "arg": {"op": "or", "args": []}} if k == "none" else {"op": k, "args": []}))
    for cat in CATEGORIES:
        sub = m.addMenu(cat)
        for op, d in TEMPLATES[cat].items():
            sp = spec(op)
            a = sub.addAction(sp[1] if sp else "number comparison", lambda op=op, d=d: add({"op": op, **json.loads(json.dumps(d))}))
            a.setToolTip(sp[5] if sp else "compare a number expression")
    m.setToolTipsVisible(True)
    return m


class GroupBlock(Block):
    def __init__(self, editor, kind: str, kids: list, root=False):
        super().__init__(editor, kind)
        self.root = root
        self.title = QComboBox()
        self.title.addItems(list(GROUPS.values()))
        self.title.setCurrentText(GROUPS[kind])
        self.title.setToolTip("ALL: every condition must hold · ANY: at least one · NONE: none of them")
        self.title.currentIndexChanged.connect(self._kind_changed)
        self.head.addWidget(self.title)
        self.head.addStretch()
        if root:
            self.handle.hide()  # the top group stays put
        else:
            self.add_menu().addAction("ungroup (keep the blocks)", self.ungroup)
        self.body = Body(self)
        self.outer.addWidget(self.body)
        self.plus = QToolButton(text="+ add a condition", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup, autoRaise=True)
        self.plus.setMenu(kinds_menu(self.plus, lambda n: self.body.insert(len(self.body.kids), make_block(editor, n))))
        self.outer.addWidget(self.plus, 0, Qt.AlignmentFlag.AlignLeft)
        for n in kids:
            self.body.insert(len(self.body.kids), make_block(editor, n), notify=False)

    def _kind_changed(self, i):
        self.kind = list(GROUPS)[i]
        self.restyle()
        self.editor.changed()

    def ungroup(self):
        h, i = self.holder, self.holder.index(self)
        for k in list(self.body.kids):
            self.body.take(k)
            h.insert(i, k, notify=False)
            i += 1
        h.take(self)
        self.deleteLater()
        self.editor.changed()

    def node(self) -> dict:
        args = [k.node() for k in self.body.kids]
        if self.kind == "none":
            return {"op": "not", "arg": args[0] if len(args) == 1 else {"op": "or", "args": args}}
        return {"op": self.kind, "args": args}


class ContainerBlock(Block):
    """anywhere / maximize / minimize: a sentence with one block inside."""

    def __init__(self, editor, node: dict):
        op = node["op"]
        sp = CONTAINERS[op]
        super().__init__(editor, sp[0])
        self.data = {k: v for k, v in node.items() if k not in ("cond", "expr")}
        self.setToolTip(sp[5])
        sentence = QWidget()
        flow = FlowLayout(sentence)
        for tok in re.split(r"(\{\w+\})", sp[2]):
            if tok.startswith("{"):
                key = tok[1:-1]
                flow.addWidget(bind(make_field(editor, op, key, node.get(key), sp[4].get(key)), self.data, key, sp[4].get(key), editor).widget)
            else:
                for word in tok.split():
                    flow.addWidget(QLabel(word))
        self.head.addWidget(sentence, 1)
        self.add_menu()
        if op == "anywhere":
            f = bind(make_field(editor, op, "anchor", node.get("anchor"), None), self.data, "anchor", None, editor)
            row = QHBoxLayout()
            row.addWidget(QLabel("or only at the starts of"))
            row.addWidget(f.widget)
            row.addStretch()
            self.outer.addLayout(row)
        self.body = Body(self, max_kids=1)
        self.body.setToolTip("drop one block here")
        self.outer.addWidget(self.body)
        inner = node.get("cond") or node.get("expr")
        if inner:
            self.body.insert(0, make_block(editor, inner, expr=op != "anywhere"), notify=False)

    def node(self) -> dict:
        op = self.data["op"]
        inner = self.body.kids[0].node() if self.body.kids else {"op": "and", "args": []}
        if op != "anywhere":
            inner = {k: v for k, v in inner.items() if k not in ("min", "max")}
        return {**self.data, ("cond" if op == "anywhere" else "expr"): inner}


def make_block(editor, node: dict, expr=False) -> Block:
    op = node["op"]
    if op in ("and", "or"):
        return GroupBlock(editor, op, node["args"])
    if op == "not":
        a = node["arg"]
        return GroupBlock(editor, "none", a["args"] if a["op"] == "or" else [a])
    if op in CONTAINERS:
        return ContainerBlock(editor, node)
    if op in SPECS:
        return ConditionBlock(editor, node, bounds=not expr)
    return TextBlock(editor, node, expr=expr)


# ------------------------------------------------------------------------------------------------
# Palette and editor
# ------------------------------------------------------------------------------------------------

class Palette(QTreeWidget):
    """Every block kind by category; drag one onto the canvas or double-click to add it."""

    def __init__(self, editor):
        super().__init__()
        self.setHeaderHidden(True)
        self.setDragEnabled(True)
        self.setDragDropMode(QAbstractItemView.DragDropMode.DragOnly)
        self.setDefaultDropAction(Qt.DropAction.CopyAction)
        self.setIndentation(10)
        logic = QTreeWidgetItem(["logic"])
        self.addTopLevelItem(logic)
        for k, title in GROUPS.items():
            it = QTreeWidgetItem([title])
            it.setData(0, Qt.ItemDataRole.UserRole, {"op": "not", "arg": {"op": "or", "args": []}} if k == "none" else {"op": k, "args": []})
            it.setToolTip(0, {"and": "every condition inside must hold", "or": "at least one must hold", "none": "none may hold"}[k])
            logic.addChild(it)
        for cat in CATEGORIES:
            top = QTreeWidgetItem([cat])
            self.addTopLevelItem(top)
            for op, d in TEMPLATES[cat].items():
                sp = spec(op)
                it = QTreeWidgetItem([sp[1] if sp else "number comparison"])
                it.setData(0, Qt.ItemDataRole.UserRole, {"op": op, **d})
                it.setToolTip(0, (sp[5] if sp else "compare a number expression") + "\n(drag onto the canvas, or double-click)")
                top.addChild(it)
        self.expandAll()
        self.itemDoubleClicked.connect(lambda it: it.data(0, Qt.ItemDataRole.UserRole) and editor.add(json.loads(json.dumps(it.data(0, Qt.ItemDataRole.UserRole)))))

    def mimeTypes(self):
        return [MIME]

    def mimeData(self, items):
        node = items[0].data(0, Qt.ItemDataRole.UserRole) if items else None
        if not node:
            return None
        m = QMimeData()
        m.setData(MIME, QByteArray(json.dumps(node).encode()))
        return m

    def supportedDropActions(self):
        return Qt.DropAction.CopyAction


class BlockEditor(QWidget):
    """Palette | blocks canvas, then a plain-English summary and the DSL text. query() reads the text."""

    def __init__(self, open_wizard=None, show_help=None):
        super().__init__()
        self.dragging = None
        self._syncing = False
        self.history: list[str] = []
        self.names = {"structures": ["village"], "biomes": ["plains"], "items": ["diamond"], "loot_structures": LOOT_STRUCTURES}
        self.show_help = show_help or (lambda op: None)

        self.blocks_palette = Palette(self)
        self.blocks_palette.setMinimumWidth(200)
        self.blocks_palette.setMaximumWidth(260)
        self.root = GroupBlock(self, "and", [], root=True)
        self.canvas = QScrollArea(widgetResizable=True)
        holder = QWidget()
        hl = QVBoxLayout(holder)
        hl.setContentsMargins(4, 4, 4, 4)
        hl.addWidget(self.root)
        hl.addStretch()
        self.canvas.setWidget(holder)
        self.summary = QLabel(wordWrap=True)
        self.summary.setStyleSheet("color: #b8c4d0; padding: 2px")
        self.summary.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        self.text = QPlainTextEdit()
        self.text.setFont(MONO)
        self.text.setPlaceholderText("village(within=300) and biome('#is_jungle', within=400)")
        self.text.setToolTip("the same query as text; edits here update the blocks")
        self._text_timer = QTimer(self, singleShot=True, interval=400, timeout=self._text_to_blocks)
        self.text.textChanged.connect(lambda: self._syncing or self._text_timer.start())
        self.error = QLabel(wordWrap=True)
        self.error.setStyleSheet("color: #ff8080")

        bar = QHBoxLayout()
        self.wizard_btn = QPushButton("✨ I want…", toolTip="answer a few questions and get the blocks")
        self.wizard_btn.setStyleSheet("font-weight: bold; padding: 3px 10px")
        if open_wizard:
            self.wizard_btn.clicked.connect(open_wizard)
        bar.addWidget(self.wizard_btn)
        self.presets = QToolButton(text="presets", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup, toolTip="ready-made queries")
        pm = QMenu(self.presets)
        for name, text in PRESETS.items():
            pm.addAction(name, lambda t=text: self.set_text(t))
        self.presets.setMenu(pm)
        bar.addWidget(self.presets)
        bar.addWidget(QToolButton(text="undo", toolTip="undo the last change", clicked=self.undo))
        bar.addWidget(QToolButton(text="clear", toolTip="remove every block", clicked=lambda: self.set_text("")))
        bar.addStretch()

        top = QSplitter()
        top.addWidget(self.blocks_palette)
        top.addWidget(self.canvas)
        top.setSizes([210, 600])
        bottom = QWidget()
        bl = QVBoxLayout(bottom)
        bl.setContentsMargins(0, 0, 0, 0)
        bl.addWidget(QLabel("as text (advanced; edits here update the blocks):"))
        bl.addWidget(self.text)
        split = QSplitter(Qt.Orientation.Vertical)
        split.addWidget(top)
        split.addWidget(bottom)
        split.setSizes([560, 110])
        lay = QVBoxLayout(self)
        lay.setContentsMargins(2, 2, 2, 2)
        lay.addLayout(bar)
        lay.addWidget(split, 1)
        lay.addWidget(self.summary)
        lay.addWidget(self.error)

    def fill_reference(self, world):
        """Names for the dropdowns: structures (sets first: 'village' means any village), biomes, items."""
        structs = sorted({n.split(":")[-1] for n, _, _, possible in world.structures() if possible})
        generic = []
        for s in sorted({s.split(":")[-1] for _, s, _, possible in world.structures() if possible}):
            for cand in (s.removesuffix("s"), s):
                try:
                    world.attempts(0, cand, 0, 0, 1)
                    generic.append(cand)
                    break
                except ValueError:
                    pass
        self.names["structures"] = list(dict.fromkeys(generic + structs))
        self.names["biomes"] = sorted(b.split(":")[-1] for b in world.biomes()) + BIOME_TAGS
        try:
            from sft import data
            self.names["items"] = sorted(k.split(":")[-1] for k in json.loads((data.data_dir(world.version) / "items.json").read_text()))
        except (OSError, ValueError):
            pass
        self._text_to_blocks()

    # blocks <-> text
    def node(self) -> dict | None:
        kids = self.root.body.kids
        if self.root.kind == "and" and len(kids) == 1:
            return kids[0].node()
        return self.root.node() if kids else None

    def changed(self):
        """A block changed: regenerate the text."""
        node = self.node()
        self._syncing = True
        self.text.setPlainText(query.to_text(node) if node else "")
        self._syncing = False
        self.error.setText("")
        self._after(node)

    def _after(self, node):
        self.summary.setText("Finds seeds with " + describe(node) + "." if node else "No conditions yet: drag blocks in from the left, or press “I want…”.")
        t = self.text.toPlainText()
        if not self.history or self.history[-1] != t:
            self.history.append(t)
            del self.history[:-100]

    def _text_to_blocks(self):
        t = self.text.toPlainText()
        try:
            node = query.parse(t) if t.strip() else None
        except query.QueryError as e:
            self.error.setText(str(e))
            return
        self.error.setText("")
        for k in list(self.root.body.kids):
            self.root.body.take(k)
            k.deleteLater()
        kind, kids = ("and", []) if node is None else (node["op"], node["args"]) if node["op"] in ("and", "or") else ("and", [node])
        self.root.title.blockSignals(True)
        self.root.title.setCurrentText(GROUPS[kind])
        self.root.title.blockSignals(False)
        self.root.kind = kind
        self.root.restyle()
        for n in kids:
            self.root.body.insert(len(self.root.body.kids), make_block(self, n), notify=False)
        self._after(node)

    def set_text(self, text: str):
        self._syncing = True
        self.text.setPlainText(text)
        self._syncing = False
        self._text_to_blocks()

    def query(self) -> dict:
        return query.parse(self.text.toPlainText())

    def add(self, node: dict):
        """Adds a block at the end of the top group."""
        self.root.body.insert(len(self.root.body.kids), make_block(self, node))

    def undo(self):
        if len(self.history) > 1:
            self.history.pop()
            self.set_text(self.history[-1])
