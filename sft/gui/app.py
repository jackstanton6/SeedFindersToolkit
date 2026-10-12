"""Main window: query editor (tree + DSL), search controls, live stats, results, map, plugins."""
import ast
import json
import os
import sys
import threading
import time

from PySide6.QtCore import Qt, QTimer, Signal
from PySide6.QtGui import QAction, QColor, QFont, QGuiApplication, QPalette
from PySide6.QtWidgets import (
    QApplication, QCheckBox, QComboBox, QDockWidget, QFileDialog, QGridLayout, QHBoxLayout, QHeaderView, QInputDialog, QLabel,
    QLineEdit, QListWidget, QMainWindow, QMenu, QMessageBox, QPlainTextEdit, QProgressBar, QPushButton, QSpinBox,
    QSplitter, QTableWidget, QTableWidgetItem, QTabWidget, QToolBar, QToolButton, QTreeWidget, QTreeWidgetItem,
    QVBoxLayout, QWidget,
)

import sft
from sft import plugins, query
from sft.gui.mapview import DEFAULT_SETS, MapView

MONO = QFont("Consolas", 9)
_COUNT = {"op": "structure", "name": "village", "within": 1000}
TEMPLATES = {  # category -> condition -> default node (without "op")
    "structures": {
        "structure": {"name": "village", "within": 500},
        "nearest": {"name": "stronghold", "within": 3000, "max": 1500},
        "near": {"a": "village", "b": "pillager_outpost", "dist": 150, "within": 1000},
        "cluster": {"names": "swamp_hut", "count": 3, "dist": 128, "within": 5000, "afk": True},
        "chain": {"names": ["village", "pillager_outpost", "village"], "dist": 400, "within": 2000},
        "overlap": {"a": "village", "b": "pillager_outpost", "within": 3000, "margin": 0},
        "bingo": {"within": 1500, "min": 12},
    },
    "biomes": {
        "biome": {"name": "mushroom_fields", "within": 500, "y": 64, "step": 16},
        "biome_area": {"name": "mushroom_fields", "within": 2000, "window": 1024, "step": 32, "min": 100000},
        "biome_count": {"within": 1000, "step": 32, "min": 20},
        "biome_border": {"a": "desert", "b": "snowy_plains", "within": 2000, "step": 32},
        "island": {"within": 3000, "window": 1024, "step": 32, "min_circularity": 0.5, "min": 50000},
    },
    "terrain": {
        "climate": {"param": "continentalness", "at": [0, 0], "y": 64, "min": 0.5},
        "height": {"at": [0, 0], "min": 120},
        "peak": {"within": 500, "step": 32, "min": 250},
        "trench": {"within": 1000, "step": 32, "max": -20},
        "cliff": {"within": 300, "step": 8, "min": 40},
        "overhang": {"within": 200, "step": 16, "min": 20},
        "void": {"within": 64, "step": 16, "min": 30},
        "lake": {"fluid": "lava", "within": 200, "step": 16, "min": 10},
        "slime": {"radius": 128, "within": 300, "min": 14},
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
DEFAULTS = {op: node for group in TEMPLATES.values() for op, node in group.items()}
PRESETS = {  # ready-made queries (overworld speedrun parts and common hunts)
    "speedrun: village + ruined portal at spawn": "village(within=160) and ruined_portal(within=160, tags=['placement:on_land_surface'])",
    "speedrun: shipwreck + buried treasure at spawn": "shipwreck(within=200, tags=['chest:']) and buried_treasure(within=200)",
    "speedrun: desert temple + village": "desert_pyramid(within=200) and village(within=300)",
    "speedrun: close stronghold": "nearest('stronghold', within=2000) < 1450 and village(within=300)",
    "quad witch huts (AFK spot)": "cluster('swamp_hut', count=4, dist=128, within=20000, afk=True)",
    "triple witch huts": "cluster('swamp_hut', count=3, dist=128, within=10000, afk=True)",
    "double ocean monument": "cluster('monument', count=2, dist=128, within=10000, afk=True)",
    "slime farm": "slime(radius=128, within=500) >= 16",
    "mushroom island near spawn": "island(biomes=['mushroom_fields'], within=1500, window=1024)",
    "biggest mushroom island (record)": "maximize(island(biomes=['mushroom_fields'], within=3000, window=1500))",
    "all biomes near spawn": "biome_count(within=3000, step=32) >= 45",
    "structure bingo": "bingo(within=1500) >= 13",
    "village overlapping an outpost": "overlap('village', 'pillager_outpost', within=3000)",
    "stronghold under a village": "near('stronghold', 'village', dist=96, within=3000)",
    "three strongholds close together": "cluster('stronghold', count=3, dist=1200, within=3000)",
    "basement igloo near spawn": "igloo(within=600, tags=['basement'])",
    "ocean ruin city": "cluster('ocean_ruin', count=5, dist=96, within=3000)",
}
GROUPS = ("and", "or", "not")
MAX_TABLE_ROWS = 200_000


def parse_count(text: str) -> int:
    """'2.5M' -> 2500000 (k, M, G, T suffixes; '2^32' also accepted)."""
    t = text.strip().replace("_", "").replace(",", "")
    if "^" in t:
        b, e = t.split("^")
        return int(b) ** int(e)
    mult = {"k": 10**3, "m": 10**6, "g": 10**9, "t": 10**12}.get(t[-1:].lower(), 1)
    return int(float(t[:-1] if mult > 1 else t) * mult)


def fmt_secs(s) -> str:
    if s is None:
        return "-"
    s = int(s)
    d, rem = divmod(s, 86400)
    return (f"{d}d " if d else "") + f"{rem // 3600:02}:{rem % 3600 // 60:02}:{rem % 60:02}"


def fmt_num(v) -> str:
    v = float(v)
    for unit, div in (("T", 1e12), ("G", 1e9), ("M", 1e6), ("k", 1e3)):
        if abs(v) >= div:
            return f"{v / div:.2f}{unit}"
    return f"{v:.0f}"


def summary(details: list) -> str:
    def one(d):
        if d.get("type") == "island":
            return f"island {d['area']:.0f} blocks@{d['x']},{d['z']}"
        return f"{d.get('name', d.get('type', '?')).split(':')[-1]}@{d['x']},{d['z']}"
    return ", ".join(one(d) for d in details if "x" in d)[:300]


class QueryEditor(QWidget):
    """Condition tree and DSL text kept in sync; leaf parameters are edited as Python literals.
    The text is the source of truth for searches."""

    def __init__(self):
        super().__init__()
        self._syncing = False
        self.tree = QTreeWidget()
        self.tree.setHeaderLabels(["condition", "as text"])
        self.tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.ResizeToContents)
        self.tree.currentItemChanged.connect(self._show_params)
        self.params = QTableWidget(0, 2)
        self.params.setHorizontalHeaderLabels(["parameter", "value (python literal)"])
        self.params.horizontalHeader().setStretchLastSection(True)
        self.params.itemChanged.connect(self._param_edited)
        self.text = QPlainTextEdit()
        self.text.setFont(MONO)
        self.text.setPlaceholderText("village(within=300) and biome('#is_jungle', within=400)")
        self._text_timer = QTimer(self, singleShot=True, interval=350, timeout=self._text_to_tree)
        self.text.textChanged.connect(lambda: self._syncing or self._text_timer.start())
        self.error = QLabel()
        self.error.setStyleSheet("color: #ff8080")
        self.error.setWordWrap(True)
        self.reference = QListWidget()
        self.reference.setFont(MONO)
        self.reference.setToolTip("double-click to insert at the cursor")
        self.reference.itemDoubleClicked.connect(lambda it: self.text.insertPlainText(repr(it.data(Qt.ItemDataRole.UserRole))))

        buttons = QHBoxLayout()
        for op in GROUPS:
            buttons.addWidget(QToolButton(text=op, clicked=lambda _=False, o=op: self.add(o)))
        for title, items in (("+ condition", None), ("presets", PRESETS)):
            b = QToolButton(text=title, popupMode=QToolButton.ToolButtonPopupMode.InstantPopup)
            m = QMenu(b)
            if items is None:
                for cat, ops in TEMPLATES.items():
                    sub = m.addMenu(cat)
                    for op in ops:
                        sub.addAction(op, lambda o=op: self.add(o))
            else:
                for name, text in items.items():
                    m.addAction(name, lambda t=text: self.set_text(t))
            b.setMenu(m)
            buttons.addWidget(b)
        buttons.addWidget(QToolButton(text="delete", clicked=self.delete))
        buttons.addStretch()

        lay = QVBoxLayout(self)
        lay.setContentsMargins(2, 2, 2, 2)
        lay.addLayout(buttons)
        split = QSplitter(Qt.Orientation.Vertical)
        for w in (self.tree, self.params, self.text, self.reference):
            split.addWidget(w)
        split.setSizes([260, 140, 90, 160])
        lay.addWidget(split)
        lay.addWidget(self.error)

    def fill_reference(self, world):
        self.reference.clear()
        rows = [(f"structure  {n.split(':')[-1]:30} set {s.split(':')[-1]}{'' if exact else '  ~approx'}", n.split(":")[-1])
                for n, s, exact, possible in sorted(world.structures()) if possible]
        rows += [(f"biome      {b.split(':')[-1]}", b.split(":")[-1]) for b in world.biomes()]
        for label, value in rows:
            self.reference.addItem(label)
            self.reference.item(self.reference.count() - 1).setData(Qt.ItemDataRole.UserRole, value)

    # tree <-> dict
    def _item(self, node: dict) -> QTreeWidgetItem:
        it = QTreeWidgetItem([node["op"], "" if node["op"] in GROUPS else query.to_text(node)])
        it.setData(0, Qt.ItemDataRole.UserRole, {k: v for k, v in node.items() if k not in ("args", "arg")})
        for c in node.get("args", []) + ([node["arg"]] if "arg" in node else []):
            it.addChild(self._item(c))
        it.setExpanded(True)
        return it

    def _node(self, it: QTreeWidgetItem) -> dict:
        node = dict(it.data(0, Qt.ItemDataRole.UserRole))
        kids = [self._node(it.child(i)) for i in range(it.childCount())]
        if node["op"] in ("and", "or"):
            node["args"] = kids
        elif node["op"] == "not":
            if len(kids) != 1:
                raise query.QueryError("'not' needs exactly one child")
            node["arg"] = kids[0]
        return node

    def query(self) -> dict:
        return query.parse(self.text.toPlainText())

    def set_text(self, text: str):
        self.text.setPlainText(text)
        self._text_to_tree()

    def _text_to_tree(self):
        try:
            node = query.parse(self.text.toPlainText()) if self.text.toPlainText().strip() else None
            self.error.setText("")
        except query.QueryError as e:
            self.error.setText(str(e))
            return
        self.tree.clear()
        if node:
            self.tree.addTopLevelItem(self._item(node))

    def _tree_to_text(self):
        top = self.tree.topLevelItem(0)
        try:
            node = self._node(top) if top else None
            self.error.setText("")
        except query.QueryError as e:
            self.error.setText(str(e))
            return
        self._syncing = True
        self.text.setPlainText(query.to_text(node) if node else "")
        self._syncing = False

    def _wrap_root(self, extra: QTreeWidgetItem):
        top = self.tree.takeTopLevelItem(0)
        root = self._item({"op": "and", "args": []})
        root.addChild(top)
        root.addChild(extra)
        self.tree.addTopLevelItem(root)

    def add(self, op: str):
        it = self._item({"op": op, **json.loads(json.dumps(DEFAULTS.get(op, {})))})
        cur = self.tree.currentItem()
        if self.tree.topLevelItem(0) is None:
            self.tree.addTopLevelItem(it)
        elif cur is None or (cur.parent() is None and cur.data(0, Qt.ItemDataRole.UserRole)["op"] not in GROUPS):
            self._wrap_root(it)  # adding next to a lone root condition makes an AND
        elif cur.data(0, Qt.ItemDataRole.UserRole)["op"] in GROUPS:
            cur.addChild(it)
        else:
            cur.parent().addChild(it)
        self.tree.setCurrentItem(it)
        self._tree_to_text()

    def delete(self):
        cur = self.tree.currentItem()
        if cur is not None:
            (cur.parent() or self.tree.invisibleRootItem()).removeChild(cur)
            self._tree_to_text()

    def _show_params(self, cur, _prev=None):
        self._syncing = True
        self.params.setRowCount(0)
        for k, v in (cur.data(0, Qt.ItemDataRole.UserRole).items() if cur else []):
            if k == "op":
                continue
            r = self.params.rowCount()
            self.params.insertRow(r)
            key = QTableWidgetItem(k)
            key.setFlags(key.flags() & ~Qt.ItemFlag.ItemIsEditable)
            self.params.setItem(r, 0, key)
            self.params.setItem(r, 1, QTableWidgetItem(repr(tuple(v) if isinstance(v, list) and len(v) == 2 else v)))
        self._syncing = False

    def _param_edited(self, item):
        if self._syncing or item.column() != 1:
            return
        cur = self.tree.currentItem()
        node = dict(cur.data(0, Qt.ItemDataRole.UserRole))
        key = self.params.item(item.row(), 0).text()
        try:
            v = ast.literal_eval(item.text())
        except (ValueError, SyntaxError):
            self.error.setText(f"{key}: not a python literal")
            return
        node[key] = list(v) if isinstance(v, tuple) else v
        cur.setData(0, Qt.ItemDataRole.UserRole, node)
        cur.setText(1, "" if node["op"] in GROUPS else query.to_text(node))
        self._tree_to_text()


class Main(QMainWindow):
    estimated = Signal(object)
    finished = Signal(object, object)  # (callback, result) from a background thread

    def __init__(self, version: str):
        super().__init__()
        self.setWindowTitle("Seed Finders Toolkit")
        self.world = None
        self.job: sft.Job | None = None
        self.resume_unit = 0
        self.start_after_estimate = False
        self.panels_added = False
        self.estimated.connect(self._show_estimate)
        self.finished.connect(lambda done, result: done(result))

        # controls
        bar = QToolBar("search")
        bar.setMovable(False)
        self.addToolBar(bar)
        self.version = QComboBox()
        self.version.addItems(sft.versions() or [version])
        self.version.setCurrentText(version)
        self.mode = QComboBox()
        self.start = QLineEdit("0")
        self.count = QLineEdit("10M")
        self.count.setToolTip("units to search; accepts 10k, 2.5M, 1G, 2^48")
        self.threads = QSpinBox(minimum=1, maximum=os.cpu_count() or 1, value=os.cpu_count() or 1)
        self.max_results = QSpinBox(minimum=0, maximum=10**9, value=0, specialValueText="unlimited")
        for w in (self.start, self.count):
            w.setFont(MONO)
            w.setMaximumWidth(170)
        for label, w in (("version", self.version), ("mode", self.mode), ("start", self.start), ("count", self.count),
                         ("threads", self.threads), ("max", self.max_results)):
            bar.addWidget(QLabel(f" {label} "))
            bar.addWidget(w)
        bar.addSeparator()
        for name, fn in (("Estimate", self.estimate), ("Start", self.start_search), ("Stop", self.stop_search),
                         ("Resume", self.resume_search), ("Export", self.export)):
            bar.addAction(QAction(name, self, triggered=fn))
        tools = QToolButton(text="Tools", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup)
        tm = QMenu(tools)
        tm.addAction("Scan map seed for the query...", self.scan)
        tm.addAction("Crack seed from structures...", self.crack)
        tm.addAction("Placement statistics...", self.placement_stats)
        tm.addAction("Search sister seeds of map seed", self.sisters)
        tools.setMenu(tm)
        bar.addWidget(tools)

        # query, stats, results
        self.editor = QueryEditor()
        self.results = QTableWidget(0, 4)
        self.results.setHorizontalHeaderLabels(["seed", "score", "matches", "details"])
        self.results.horizontalHeader().setSectionResizeMode(2, QHeaderView.ResizeMode.Stretch)
        self.results.setFont(MONO)
        self.results.setSortingEnabled(True)
        self.results.setSelectionBehavior(QTableWidget.SelectionBehavior.SelectRows)
        self.results.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.results.customContextMenuRequested.connect(self._result_menu)
        self.results.itemSelectionChanged.connect(self._result_selected)

        self.progress = QProgressBar(maximum=1000)
        self.stat = {}
        grid = QGridLayout()
        keys = ["units", "rate", "checks", "checks/s", "prefilter pass", "full evals", "matches", "py rejected",
                "elapsed", "eta", "est. total", "est. rate", "match rate", "95% range", "first match", "kept"]
        for i, k in enumerate(keys):
            lab = QLabel("-")
            lab.setFont(MONO)
            lab.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
            grid.addWidget(QLabel(k), i // 4, (i % 4) * 2)
            grid.addWidget(lab, i // 4, (i % 4) * 2 + 1)
            self.stat[k] = lab
        stats_box = QWidget()
        sl = QVBoxLayout(stats_box)
        sl.setContentsMargins(4, 2, 4, 2)
        sl.addWidget(self.progress)
        sl.addLayout(grid)

        middle = QSplitter(Qt.Orientation.Vertical)
        middle.addWidget(stats_box)
        middle.addWidget(self.results)
        middle.setSizes([110, 600])
        split = QSplitter()
        split.addWidget(self.editor)
        split.addWidget(middle)
        split.setSizes([480, 820])
        self.setCentralWidget(split)

        # map dock
        self.map = MapView()
        self.map.hovered.connect(lambda s: self.statusBar().showMessage(s))
        self.map_seed = QLineEdit("0")
        self.map_seed.setFont(MONO)
        self.map_x = QSpinBox(minimum=-30_000_000, maximum=30_000_000)
        self.map_z = QSpinBox(minimum=-30_000_000, maximum=30_000_000)
        self.map_y = QSpinBox(minimum=-64, maximum=319, value=64)
        self.map_scale = QComboBox()
        self.map_scale.addItems([str(s) for s in (1, 2, 4, 8, 16, 32, 64, 128, 256)])
        self.map_scale.setCurrentText("8")
        self.map_height = QCheckBox("height shading")
        self.map_structs = QCheckBox("structures", checked=True)
        self.map_sets = QToolButton(text="sets", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup)
        self.map_sets.setMenu(QMenu(self.map_sets))
        self.map_attempts = QComboBox(toolTip="overlay every placement attempt of a set (x = failed start)")
        self.map_attempts.currentTextChanged.connect(self._map_from_controls)
        row1, row2 = QHBoxLayout(), QHBoxLayout()
        for w in (QLabel("seed"), self.map_seed, QLabel("x"), self.map_x, QLabel("z"), self.map_z):
            row1.addWidget(w)
        for w in (QLabel("y"), self.map_y, QLabel("blocks/px"), self.map_scale, self.map_height, self.map_structs,
                  self.map_sets, QLabel("attempts"), self.map_attempts, QPushButton("render", clicked=self._map_from_controls)):
            row2.addWidget(w)
        map_box = QWidget()
        ml = QVBoxLayout(map_box)
        ml.setContentsMargins(2, 2, 2, 2)
        ml.addLayout(row1)
        ml.addLayout(row2)
        ml.addWidget(self.map, 1)
        dock = QDockWidget("map", self)
        dock.setWidget(map_box)
        self.addDockWidget(Qt.DockWidgetArea.RightDockWidgetArea, dock)

        # log / plugins dock
        self.tabs = QTabWidget()
        self.log = QPlainTextEdit(readOnly=True)
        self.log.setFont(MONO)
        self.plugin_table = QTableWidget(0, 5)
        self.plugin_table.setHorizontalHeaderLabels(["plugin", "version", "native", "python", "status"])
        self.plugin_table.horizontalHeader().setSectionResizeMode(4, QHeaderView.ResizeMode.Stretch)
        self.tabs.addTab(self.log, "log")
        self.tabs.addTab(self.plugin_table, "plugins")
        bottom = QDockWidget("log / plugins", self)
        bottom.setWidget(self.tabs)
        self.addDockWidget(Qt.DockWidgetArea.BottomDockWidgetArea, bottom)

        self.timer = QTimer(self, interval=250, timeout=self._tick)
        self.version.currentTextChanged.connect(self.load_version)
        self.load_version(self.version.currentText())
        self.editor.set_text("village(within=300) and pillager_outpost(within=600)")

    def say(self, msg: str):
        self.log.appendPlainText(f"[{time.strftime('%H:%M:%S')}] {msg}")

    def load_version(self, version: str):
        try:
            self.world = sft.world(version)
        except Exception as e:
            self.world = None
            self.say(f"cannot load {version}: {e}")
            QMessageBox.warning(self, "data missing", f"{e}\n\nRun: python -m sft data {version}")
            return
        self.loaded = plugins.load(version)
        found, api = self.loaded
        self.mode.clear()
        self.mode.addItems(["range (64-bit)", "structure seeds (48-bit) + sisters", "random", "seed list file"]
                           + [f"plugin: {n}" for n in api.seed_sources])
        self.plugin_table.setRowCount(0)
        for p in found:
            r = self.plugin_table.rowCount()
            self.plugin_table.insertRow(r)
            vals = [p.name, p.meta.get("version", "?"), p.native or "-", p.meta.get("python", "-"), "; ".join(e.strip() for e in p.errors) or "ok"]
            for c, v in enumerate(vals):
                self.plugin_table.setItem(r, c, QTableWidgetItem(str(v)))
        if not self.panels_added:
            self.panels_added = True
            for title, factory in api.panels:
                try:
                    self.tabs.addTab(factory(self), title)
                except Exception as e:
                    self.say(f"plugin panel '{title}' failed: {e}")
        menu = self.map_sets.menu()
        menu.clear()
        set_names = sorted({s for _, s, _, possible in self.world.structures() if possible})
        self.map_attempts.blockSignals(True)
        self.map_attempts.clear()
        self.map_attempts.addItems([""] + [n.split(":")[-1] for n in set_names])
        self.map_attempts.blockSignals(False)
        for set_name in set_names:
            short = set_name.split(":")[-1]
            a = menu.addAction(short)
            a.setCheckable(True)
            a.setChecked(short in DEFAULT_SETS)
            a.toggled.connect(self._map_sets_changed)
        self.editor.fill_reference(self.world)
        self.map.set_world(self.world)
        self.say(f"loaded {version}: {len(self.world.biomes())} biomes, {len(self.world.structures())} structures, {len(found)} plugin(s)")

    # search
    def _make_job(self) -> sft.Job | None:
        if self.world is None:
            return None
        try:
            mode_text, seeds, mode = self.mode.currentText(), None, "range"
            if mode_text.startswith("structure"):
                mode = "structure_seeds"
            elif mode_text.startswith("random"):
                mode = "random"
            elif mode_text.startswith("seed list"):
                path, _ = QFileDialog.getOpenFileName(self, "seed list (one per line)")
                if not path:
                    return None
                seeds, mode = [int(x) for x in open(path).read().split()], "list"
            elif mode_text.startswith("plugin: "):
                src = self.loaded[1].seed_sources[mode_text.removeprefix("plugin: ")]
                seeds, mode = [s for _, s in zip(range(parse_count(self.count.text())), src())], "list"
            return sft.Job(self.version.currentText(), self.editor.query(), mode, int(self.start.text()),
                           parse_count(self.count.text()), seeds, self.max_results.value(), self.loaded)
        except Exception as e:
            QMessageBox.warning(self, "cannot start", str(e))
            return None

    def _calibrate(self, job):
        self.say("calibrating (timing a sample)...")
        threads = self.threads.value()

        def work():
            try:
                self.estimated.emit(job.estimate(2048, threads))
            except Exception as e:
                self.estimated.emit({"note": f"estimate failed: {e}"})
        threading.Thread(target=work, daemon=True).start()

    def estimate(self):
        if self.job and self.job.running():
            return
        job = self._make_job()
        if job is not None:
            self._calibrate(job)

    def _show_estimate(self, est: dict):
        if "seconds_total" in est:
            self.stat["est. total"].setText(fmt_secs(est["seconds_total"]))
            self.stat["est. rate"].setText(f"{fmt_num(est['units_per_sec'])}/s")
            r = sft.rarity(est)
            self.stat["match rate"].setText(f"{r['p']:.3g}")
            self.stat["95% range"].setText(f"{r['low']:.2g}..{r['high']:.2g}")
            first = fmt_secs(r["seconds_to_first"]) if r["p"] else "> " + fmt_secs(r["seconds_to_first_range"][0])
            self.stat["first match"].setText(first)
            self.say(f"estimate: {fmt_num(est['units_per_sec'])} units/s, prefilter pass {est['prefilter_rate']:.3%}, "
                     f"match rate {r['p']:.3g} (95% {r['low']:.2g}..{r['high']:.2g}), first match ~{first}, "
                     f"total {fmt_secs(est['seconds_total'])}")
        else:
            self.say(est.get("note", "no estimate"))
        if self.start_after_estimate:
            self.start_after_estimate = False
            self.job.start(self.threads.value())
            self.timer.start()
            self.say(f"search started: {query.to_text(self.editor.query())}")

    def start_search(self):
        if self.job and self.job.running():
            return
        job = self._make_job()
        if job is None:
            return
        self.job, self.resume_unit = job, 0
        self.results.setRowCount(0)
        self.start_after_estimate = True
        self._calibrate(job)

    def stop_search(self):
        if self.job:
            self.job.stop()

    def resume_search(self):
        if self.job and not self.job.running() and self.resume_unit < self.job.stats()["units_total"]:
            self.job.start(self.threads.value(), self.resume_unit)
            self.timer.start()
            self.say(f"resumed at unit {self.resume_unit}")

    def _add_row(self, seed: int, details: list):
        if self.results.rowCount() >= MAX_TABLE_ROWS:
            return
        self.results.setSortingEnabled(False)
        r = self.results.rowCount()
        self.results.insertRow(r)
        seed_item = QTableWidgetItem()
        seed_item.setData(Qt.ItemDataRole.DisplayRole, seed)
        seed_item.setData(Qt.ItemDataRole.UserRole, details)
        self.results.setItem(r, 0, seed_item)
        score_item = QTableWidgetItem()
        if self.job is not None and self.job.ranked:
            score_item.setData(Qt.ItemDataRole.DisplayRole, round(sft.score(details), 4))
        self.results.setItem(r, 1, score_item)
        self.results.setItem(r, 2, QTableWidgetItem(summary(details)))
        self.results.setItem(r, 3, QTableWidgetItem(json.dumps(details)))
        self.results.setSortingEnabled(True)

    def _show_rows(self, new):
        if self.job.ranked:  # top-N: the kept rows change as better seeds arrive
            if new:
                self.results.setRowCount(0)
                for seed, details in self.job.rows:
                    self._add_row(seed, details)
                self.results.sortItems(1, Qt.SortOrder.DescendingOrder)
        else:
            for seed, details in new:
                self._add_row(seed, details)

    def _tick(self):
        if self.job is None:
            return
        self._show_rows(self.job.poll())
        s = self.job.stats()
        self.stat["kept"].setText(fmt_num(len(self.job.rows)))
        self.progress.setValue(int(s["progress"] * 1000))
        self.progress.setFormat(f"{s['progress']:.3%}")
        self.stat["units"].setText(f"{fmt_num(s['units_done'])} / {fmt_num(s['units_total'])}")
        self.stat["rate"].setText(f"{fmt_num(s['rate'])}/s")
        self.stat["checks"].setText(fmt_num(s["checks"]))
        self.stat["checks/s"].setText(f"{fmt_num(s['checks_rate'])}/s")
        self.stat["prefilter pass"].setText(f"{s['prefilter_passed'] / max(s['checks'], 1):.3%}")
        self.stat["full evals"].setText(fmt_num(s["full_evals"]))
        self.stat["matches"].setText(fmt_num(s["matched"]))
        self.stat["py rejected"].setText(fmt_num(s["py_rejected"]))
        self.stat["elapsed"].setText(fmt_secs(s["elapsed"]))
        self.stat["eta"].setText(fmt_secs(s["eta"]))
        if not self.job.running():
            self.timer.stop()
            self.resume_unit = self.job.join()
            self._show_rows(self.job.poll())  # stragglers after the last tick
            done = self.resume_unit >= s["units_total"]
            self.say(f"search {'finished' if done else f'stopped, resume at unit {self.resume_unit}'}: "
                     f"{fmt_num(s['matched'])} matches, {fmt_num(s['checks'])} checks in {fmt_secs(s['elapsed'])}")

    # results
    def _selected(self) -> list[tuple[int, list]]:
        rows = sorted({i.row() for i in self.results.selectedItems()})
        items = [self.results.item(r, 0) for r in rows]
        return [(int(it.data(Qt.ItemDataRole.DisplayRole)), it.data(Qt.ItemDataRole.UserRole) or []) for it in items if it]

    def _result_selected(self):
        sel = self._selected()
        if sel:
            seed, details = sel[0]
            self.map_seed.setText(str(seed))
            first = next((d for d in details if "x" in d), None)
            if first:
                self.map_x.setValue(first["x"])
                self.map_z.setValue(first["z"])
            self._map_from_controls()

    def _result_menu(self, pos):
        sel = self._selected()
        if not sel:
            return
        m = QMenu(self)
        clip = QGuiApplication.clipboard()
        m.addAction("copy seed(s)", lambda: clip.setText("\n".join(str(s) for s, _ in sel)))
        first = next((d for d in sel[0][1] if "x" in d), None)
        if first:
            m.addAction("copy /tp to first match", lambda: clip.setText(f"/tp @s {first['x']} {first.get('y', 100)} {first['z']}"))
        m.addAction("re-check (verify)", lambda: self._recheck(sel[0][0]))
        m.exec(self.results.viewport().mapToGlobal(pos))

    def _recheck(self, seed: int):
        engine_tree, _ = query.split_python(self.editor.query())
        res = self.world.check(seed, json.dumps(engine_tree)) if engine_tree else "[]"
        self.say(f"re-check {seed}: {'match ' + res if res is not None else 'NO MATCH'}")

    def export(self):
        if not self.job or not self.job.rows:
            QMessageBox.information(self, "export", "no results yet")
            return
        exporters = self.loaded[1].exporters
        filters = ";;".join(["CSV (*.csv)", "JSON (*.json)", "Seed list (*.txt)"] + [f"{n} (*.{ext})" for n, (_, ext) in exporters.items()])
        path, chosen = QFileDialog.getSaveFileName(self, "export results", "results.csv", filters)
        if not path:
            return
        name = chosen.split(" (")[0]
        (exporters[name][0] if name in exporters else sft.export)(self.job.rows, path)
        self.say(f"exported {len(self.job.rows)} rows to {path}")

    # tools
    def _background(self, fn, done):
        threading.Thread(target=lambda: self.finished.emit(done, _try(fn)), daemon=True).start()

    def _map_seed(self) -> int | None:
        try:
            return int(self.map_seed.text())
        except ValueError:
            QMessageBox.warning(self, "seed", "set a seed in the map panel first")
            return None

    def scan(self):
        seed = self._map_seed()
        if seed is None:
            return
        within, ok = QInputDialog.getInt(self, "scan", "search radius around the map centre (blocks)", 5000, 100, 100_000)
        if not ok:
            return
        step, ok = QInputDialog.getInt(self, "scan", "anchor grid step (blocks); coordinates in the query are relative", 64, 4, 4096)
        if not ok:
            return
        tree, _ = query.split_python(self.editor.query())
        x, z = self.map_x.value(), self.map_z.value()
        self.say(f"scanning seed {seed} within {within} of ({x}, {z})...")

        def done(hits):
            if isinstance(hits, Exception):
                self.say(f"scan failed: {hits}")
                return
            self.map.pins = [(hx, hz, str(i + 1)) for i, (hx, hz, _) in enumerate(hits[:200])]
            self.map.update()
            self.say(f"scan: {len(hits)} place(s) match" + "".join(f"\n  {hx} {hz}  {summary(json.loads(d))}" for hx, hz, d in hits[:50]))
        self._background(lambda: self.world.scan(seed, json.dumps(tree), x, z, within, step), done)

    def crack(self):
        text, ok = QInputDialog.getMultiLineText(
            self, "crack", "One structure per line as 'name x z' (block coordinates from /locate or F3).\n"
            "Huts, temples, outposts, igloos and shipwrecks narrow the search the most.\n"
            "Optional last line 'query: ...' adds conditions (biomes, heights) for the sister check.", "")
        if not ok or not text.strip():
            return
        obs, extra = [], ""
        try:
            for line in text.strip().splitlines():
                if line.startswith("query:"):
                    extra = line.removeprefix("query:").strip()
                elif line.strip():
                    n, x, z = line.replace(",", " ").split()
                    obs.append((n, int(x), int(z)))
        except ValueError:
            QMessageBox.warning(self, "crack", f"cannot read line: {line}")
            return
        self.say(f"cracking from {len(obs)} structure(s)...")

        def done(res):
            if isinstance(res, Exception):
                self.say(f"crack failed: {res}")
                return
            found, job = res
            self.say(f"crack: {len(found)} structure seed(s): {' '.join(map(str, found))}")
            if job:
                self.job, self.resume_unit = job, 0
                self.results.setRowCount(0)
                self.job.start(self.threads.value())
                self.timer.start()
                self.say(f"checking {len(found) * 65536:,} sister seeds")
        self._background(lambda: sft.crack(self.version.currentText(), obs, extra), done)

    def placement_stats(self):
        names = [self.map_attempts.itemText(i) for i in range(1, self.map_attempts.count())]
        name, ok = QInputDialog.getItem(self, "placement statistics", "structure set", names, 0, False)
        if not ok:
            return
        radius, ok = QInputDialog.getInt(self, "placement statistics", "radius around the map centre", 1000, 16, 50_000)
        if not ok:
            return
        n = 2000
        x, z = self.map_x.value(), self.map_z.value()
        seeds = [int.from_bytes(os.urandom(8), "little", signed=True) for _ in range(n)]

        def done(res):
            if isinstance(res, Exception):
                self.say(f"placement stats failed: {res}")
                return
            counts = [len(v) for v in res]
            near = sorted(min(((px - x) ** 2 + (pz - z) ** 2) ** 0.5 for px, pz in v) for v in res if v)
            q = (lambda f: f"{near[int(f * (len(near) - 1))]:.0f}") if near else (lambda f: "-")
            self.say(f"{name} within {radius} of ({x}, {z}) over {n} random seeds: at least one {sum(c > 0 for c in counts) / n:.1%}, "
                     f"mean count {sum(counts) / n:.2f}, max {max(counts)}; nearest distance median {q(0.5)}, 10% {q(0.1)}, 90% {q(0.9)}")
        self._background(lambda: self.world.placement_stats(name, x, z, radius, seeds), done)

    def sisters(self):
        seed = self._map_seed()
        if seed is None or (self.job and self.job.running()):
            return
        try:
            self.job = sft.Job(self.version.currentText(), self.editor.query(), "list", seeds=sft.sisters([seed]),
                               max_results=self.max_results.value(), loaded=self.loaded)
        except Exception as e:
            QMessageBox.warning(self, "cannot start", str(e))
            return
        self.resume_unit = 0
        self.results.setRowCount(0)
        self.job.start(self.threads.value())
        self.timer.start()
        self.say(f"searching the 65536 sister seeds of {seed}")

    # map
    def _map_sets_changed(self):
        self.map.structure_filter = [a.text() for a in self.map_sets.menu().actions() if a.isChecked()]
        self.map.request()

    def _map_from_controls(self):
        try:
            seed = int(self.map_seed.text())
        except ValueError:
            return
        self.map.y = self.map_y.value()
        self.map.scale = int(self.map_scale.currentText())
        self.map.show_height = self.map_height.isChecked()
        self.map.show_structures = self.map_structs.isChecked()
        self.map.attempts_set = self.map_attempts.currentText()
        if seed != self.map.seed:
            self.map.pins = []
        self.map.show_seed(seed, self.map_x.value(), self.map_z.value())


def _try(fn):
    try:
        return fn()
    except Exception as e:  # shown in the log by the caller
        return e


def dark(app: QApplication):
    app.setStyle("Fusion")
    p = QPalette()
    base, alt, text = QColor(30, 31, 34), QColor(40, 42, 46), QColor(220, 220, 220)
    for role, c in ((QPalette.ColorRole.Window, alt), (QPalette.ColorRole.Base, base), (QPalette.ColorRole.AlternateBase, alt),
                    (QPalette.ColorRole.Text, text), (QPalette.ColorRole.WindowText, text), (QPalette.ColorRole.Button, alt),
                    (QPalette.ColorRole.ButtonText, text), (QPalette.ColorRole.Highlight, QColor(60, 110, 180)),
                    (QPalette.ColorRole.HighlightedText, QColor(255, 255, 255)), (QPalette.ColorRole.ToolTipBase, base),
                    (QPalette.ColorRole.ToolTipText, text)):
        p.setColor(role, c)
    app.setPalette(p)


def run(version: str = "26.3"):
    app = QApplication.instance() or QApplication(sys.argv)
    dark(app)
    w = Main(version)
    w.resize(1700, 980)
    w.show()
    app.exec()
