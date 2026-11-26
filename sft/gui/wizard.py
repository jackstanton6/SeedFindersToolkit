"""The "I want..." wizard: pick a goal, answer a few plain questions, get blocks."""
from PySide6.QtWidgets import (
    QCheckBox, QComboBox, QDialog, QFormLayout, QGridLayout, QHBoxLayout, QLabel, QPushButton, QSpinBox, QStackedWidget,
    QVBoxLayout, QWidget,
)

from sft import query
from sft.gui.blocks import LOOT_STRUCTURES, PRESETS, describe

TERRAIN = {  # choice -> (op, extra params, bound key, meaning of the size field)
    "a high mountain peak": ("peak", {}, "min", "peak y level at least"),
    "a deep ocean trench": ("trench", {}, "max", "ocean floor y level at most"),
    "a tall cliff": ("cliff", {"step": 8}, "min", "cliff height (blocks) at least"),
    "an overhang": ("overhang", {"step": 16}, "min", "overhang height (blocks) at least"),
    "an underground lava lake": ("lake", {"fluid": "lava", "step": 16}, "min", "lake depth (blocks) at least"),
    "an underground water lake": ("lake", {"fluid": "water", "step": 16}, "min", "lake depth (blocks) at least"),
}
RECORDS = {
    "the closest stronghold to spawn": {"op": "minimize", "expr": {"op": "nearest", "name": "stronghold", "within": 3000}},
    "the biggest mushroom island": {"op": "maximize", "expr": {"op": "island", "biomes": ["mushroom_fields"], "within": 3000, "window": 1500}},
    "the most slime chunks near spawn": {"op": "maximize", "expr": {"op": "slime", "radius": 128, "within": 300}},
    "the most structure types near spawn": {"op": "maximize", "expr": {"op": "bingo", "within": 1500}},
    "the highest mountain near spawn": {"op": "maximize", "expr": {"op": "peak", "within": 1000}},
    "the most biomes near spawn": {"op": "maximize", "expr": {"op": "biome_count", "within": 2000, "step": 32}},
}


SPAWN_GOALS = {  # choice -> build(values); every check is measured from the seed's real spawn point
    "spawn in a biome": lambda v: {"op": "biome", "name": v["name"], "at": "spawn"},
    "spawn on high ground (y at least Number)": lambda v: {"op": "height", "at": "spawn", "min": v["size"]},
    "a structure within Number blocks of spawn": lambda v: {"op": "structure", "name": v["structure"], "within": v["size"], "at": "spawn"},
    "spawn at least Number blocks from (0, 0)": lambda v: {"op": "spawn_distance", "min": v["size"]},
}


def _opt(d: dict, **kw) -> dict:
    return {**d, **{k: v for k, v in kw.items() if v not in (None, 0, False)}}


def _island(v):
    if v["island"]:
        return {"op": "island", "biomes": [v["name"]], "within": v["within"], "window": 1024, "step": 32, "min": v["min"]}
    return {"op": "biome_area", "name": v["name"], "within": v["within"], "window": 1024, "step": 32, "min": v["min"]}


def _terrain(v):
    op, extra, bound, _ = TERRAIN[v["kind"]]
    return {"op": op, **extra, "within": v["within"], bound: v["size"]}


# (title, subtitle, [(question, key, kind, default)], build(values) -> node). kind: structure, biome, item,
# int, bool, or a list of choices.
GOALS = [
    ("A structure near spawn", "village, stronghold, temple, monument, ancient city...",
     [("Which structure?", "name", "structure", "village"), ("How close to spawn? (blocks)", "within", "int", 300), ("At least how many?", "min", "int", 1)],
     lambda v: _opt({"op": "structure", "name": v["name"], "within": v["within"]}, min=v["min"] if v["min"] > 1 else None)),
    ("Two structures next to each other", "a village with a pillager outpost, a stronghold under a village...",
     [("First structure", "a", "structure", "village"), ("Second structure", "b", "structure", "pillager_outpost"),
      ("At most this far apart (blocks)", "dist", "int", 150), ("Search this far from spawn (blocks)", "within", "int", 1000)],
     lambda v: {"op": "near", "a": v["a"], "b": v["b"], "dist": v["dist"], "within": v["within"]}),
    ("Several of one structure together", "quad witch huts, double ocean monuments (farms)",
     [("Which structure?", "names", "structure", "swamp_hut"), ("How many together?", "count", "int", 3),
      ("All inside a circle of radius (blocks)", "dist", "int", 128), ("Search this far from spawn (blocks)", "within", "int", 10000),
      ("Whole structures inside the circle (AFK spot)", "afk", "bool", True)],
     lambda v: _opt({"op": "cluster", "names": v["names"], "count": v["count"], "dist": v["dist"], "within": v["within"]}, afk=v["afk"])),
    ("A biome near spawn", "cherry grove, mushroom fields, jungle, badlands...",
     [("Which biome?", "name", "biome", "cherry_grove"), ("How close to spawn? (blocks)", "within", "int", 300),
      ("Covering at least this % of that area (0 = anywhere in it)", "fraction", "int", 0)],
     lambda v: _opt({"op": "biome", "name": v["name"], "within": v["within"]}, step=32 if v["within"] > 1000 else None, fraction=v["fraction"] / 100 or None)),
    ("Where you spawn", "spawn in a jungle, on a mountain, next to a village, far from (0, 0)...",
     [("What?", "kind", list(SPAWN_GOALS), next(iter(SPAWN_GOALS))), ("Biome (for 'in a biome')", "name", "biome", "mushroom_fields"),
      ("Structure (for 'next to a structure')", "structure", "structure", "village"), ("Number (height, blocks)", "size", "int", 150)],
     lambda v: SPAWN_GOALS[v["kind"]](v)),
    ("A big biome area or an island", "a huge mushroom island, a giant jungle...",
     [("Which biome?", "name", "biome", "mushroom_fields"), ("At least this big (blocks²)", "min", "int", 100000),
      ("It must be an island", "island", "bool", True), ("Within this distance of spawn (blocks)", "within", "int", 2000)],
     _island),
    ("Mountains, cliffs or caves", "tall peaks, cliffs, overhangs, lava lakes",
     [("What?", "kind", list(TERRAIN), "a high mountain peak"), ("Size", "size", "int", 250),
      ("Within this distance of spawn (blocks)", "within", "int", 500)],
     _terrain),
    ("Items in chests", "diamonds in buried treasure, golden apples in temples...",
     [("Which structure's chests?", "name", LOOT_STRUCTURES, "buried_treasure"), ("Which item?", "item", "item", "diamond"),
      ("At least how many?", "min", "int", 2), ("Within this distance of spawn (blocks)", "within", "int", 500)],
     lambda v: _opt({"op": "loot", "name": v["name"], "item": v["item"], "within": v["within"]}, min=v["min"] if v["min"] > 1 else None)),
    ("Slime chunks", "lots of slime chunks for a slime farm",
     [("At least this many slime chunks", "min", "int", 30), ("Within this radius of the farm (blocks)", "radius", "int", 128),
      ("Farm spot at most this far from spawn (blocks)", "within", "int", 300)],
     lambda v: _opt({"op": "slime", "radius": v["radius"], "min": v["min"]}, within=v["within"])),
    ("A speedrun seed", "ready-made overworld filters for speedruns",
     [("Which filter?", "preset", [k for k in PRESETS if k.startswith("speedrun")], next(k for k in PRESETS if k.startswith("speedrun")))],
     lambda v: query.parse(PRESETS[v["preset"]])),
    ("The best seed (a record)", "rank seeds instead of filtering them",
     [("Find", "what", list(RECORDS), next(iter(RECORDS)))],
     lambda v: RECORDS[v["what"]]),
]


class Wizard(QDialog):
    """add(node, replace) receives the finished condition."""

    def __init__(self, parent, names: dict, add):
        super().__init__(parent)
        self.setWindowTitle("I want…")
        self.resize(660, 470)
        self.names, self.add = names, add
        self.pages = QStackedWidget()
        QVBoxLayout(self).addWidget(self.pages)
        first = QWidget()
        fl = QVBoxLayout(first)
        fl.addWidget(QLabel("<h3>What are you looking for?</h3>Pick one. You can add more conditions afterwards, "
                            "and change everything in the blocks."))
        grid = QGridLayout()
        for i, (title, sub, _, _) in enumerate(GOALS):
            b = QPushButton(f"{title}\n{sub}")
            b.setStyleSheet("text-align: left; padding: 8px")
            b.setMinimumHeight(54)
            b.clicked.connect(lambda _=False, i=i: self.open_goal(i))
            grid.addWidget(b, i // 2, i % 2)
        fl.addLayout(grid)
        fl.addStretch()
        self.pages.addWidget(first)
        self.form_page = None

    def open_goal(self, i):
        title, sub, fields, build = GOALS[i]
        page = QWidget()
        pl = QVBoxLayout(page)
        pl.addWidget(QLabel(f"<h3>{title}</h3>{sub}"))
        form = QFormLayout()
        getters = {}
        preview = QLabel(wordWrap=True)
        preview.setStyleSheet("color: #b8c4d0; padding: 6px; border: 1px solid #3a3d42; border-radius: 4px")
        size_hint = QLabel()

        def values():
            return {k: g() for k, g in getters.items()}

        def update(*_):
            try:
                preview.setText("This finds seeds with " + describe(build(values())) + ".")
            except Exception as e:  # unfinished input
                preview.setText(str(e))
            if "kind" in getters:
                size_hint.setText("size = " + TERRAIN[getters["kind"]()][3])
        for label, key, kind, default in fields:
            if kind == "int":
                w = QSpinBox(minimum=-64 if key == "size" else 0, maximum=30_000_000, value=default, singleStep=16 if default >= 100 else 1)
                w.valueChanged.connect(update)
                getters[key] = w.value
            elif kind == "bool":
                w = QCheckBox(checked=default)
                w.toggled.connect(update)
                getters[key] = w.isChecked
            else:
                pool = kind if isinstance(kind, list) else self.names.get({"structure": "structures", "biome": "biomes", "item": "items"}[kind], [])
                w = QComboBox(editable=not isinstance(kind, list))
                w.addItems(pool)
                w.setCurrentText(default)
                w.currentTextChanged.connect(update)
                getters[key] = w.currentText
            form.addRow(label, w)
        if "kind" in getters:
            form.addRow("", size_hint)
        pl.addLayout(form)
        pl.addWidget(preview)
        pl.addStretch()

        def done(replace):
            try:
                node = build(values())
            except Exception as e:
                preview.setText(f"cannot build this: {e}")
                return
            self.add(node, replace)
            self.accept()
        buttons = QHBoxLayout()
        buttons.addWidget(QPushButton("← back", clicked=lambda: self.pages.setCurrentIndex(0)))
        buttons.addStretch()
        buttons.addWidget(QPushButton("Replace my query", clicked=lambda: done(True), toolTip="start over with just this"))
        add = QPushButton("Add to my query", clicked=lambda: done(False), toolTip="seeds must also match what is already there")
        add.setDefault(True)
        buttons.addWidget(add)
        pl.addLayout(buttons)
        if self.form_page is not None:
            self.pages.removeWidget(self.form_page)
            self.form_page.deleteLater()
        self.form_page = page
        self.pages.addWidget(page)
        self.pages.setCurrentWidget(page)
        update()
