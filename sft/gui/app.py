"""Main window: block query editor (+ DSL text), search controls, live stats, results, map, plugins, help."""
import json
import os
import random
import sys
import threading
import time

from PySide6.QtCore import QSettings, Qt, QTimer, Signal
from PySide6.QtGui import QAction, QBrush, QColor, QFont, QGuiApplication, QKeySequence, QPalette
from PySide6.QtWidgets import (
    QApplication, QCheckBox, QComboBox, QDockWidget, QFileDialog, QGridLayout, QHBoxLayout, QHeaderView, QInputDialog, QLabel,
    QLineEdit, QMainWindow, QMenu, QMessageBox, QPlainTextEdit, QProgressBar, QPushButton, QSpinBox, QSplitter,
    QTableWidget, QTableWidgetItem, QTabWidget, QToolBar, QToolButton, QVBoxLayout, QWidget,
)

import sft
from sft import plugins, query
from sft.gui.blocks import PRESETS, BlockEditor  # noqa: F401  (PRESETS kept importable from here)
from sft.gui.help import HelpWindow, Tour
from sft.gui.mapview import DEFAULT_SETS, MapView
from sft.gui.seedbits import SeedBits, split
from sft.gui.wizard import Wizard

MONO = QFont("Consolas", 9)
MAX_TABLE_ROWS = 200_000


def parse_count(text: str) -> int:
    """'2.5M' -> 2500000 (k, M, G, T suffixes; '2^32' also accepted)."""
    t = text.strip().replace("_", "").replace(",", "")
    if "^" in t:
        b, e = t.split("^")
        return int(b) ** int(e)
    mult = {"k": 10**3, "m": 10**6, "g": 10**9, "t": 10**12}.get(t[-1:].lower(), 1)
    return int(float(t[:-1]) * mult) if mult > 1 else int(t)


# search mode -> its unit; FULL: valid from/to per unit (upper16: the upper bits, lower48: the lower bits)
MODES = {"range (64-bit)": "range", "lower 48 bits (fixed upper 16)": "lower48", "upper 16 bits (sisters of a seed)": "upper16",
         "structure seeds (48-bit) + sisters": "structure_seeds", "seed list file": "list"}
FULL = {"range": (-2**63, 2**63 - 1), "lower48": (0, 2**48 - 1), "upper16": (0, 2**16 - 1), "structure_seeds": (0, 2**48 - 1)}
RANGES = ["first 10M", "full range", "32-bit (text seeds)", "custom"]


def signed64(v: int) -> int:
    return v - 2**64 if v >= 2**63 else v


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


class Main(QMainWindow):
    estimated = Signal(object)
    finished = Signal(object, object)  # (callback, result) from a background thread
    cuda = Signal(bool, str)

    def __init__(self, version: str):
        super().__init__()
        self.setWindowTitle("Seed Finders Toolkit")
        self.world = None
        self.job: sft.Job | None = None
        self.resume_unit = 0
        self.estimating = self.hinted = False
        self.panels_added = False
        self.estimated.connect(self._show_estimate)
        self.finished.connect(lambda done, result: done(result))
        QApplication.setOrganizationName("SeedFindersToolkit")
        QApplication.setApplicationName("sft")
        self.settings = QSettings()  # default format: the registry on Windows, tests redirect it
        self.help_window = None

        # controls
        bar = QToolBar("search")
        bar.setMovable(False)
        self.addToolBar(bar)
        self.version = QComboBox()
        self.version.addItems(sft.versions() or [version])
        self.version.setCurrentText(version)
        self.mode = QComboBox()
        self.range = QComboBox()
        self.range.addItems(RANGES)
        self.lo, self.hi, self.fixed = QLineEdit("0"), QLineEdit("9999999"), QLineEdit("0", enabled=False)
        self.shuffle = QCheckBox("shuffle", toolTip="visit the range in a random order (every seed once, no repeats)")
        self.threads = QSpinBox(minimum=1, maximum=os.cpu_count() or 1, value=os.cpu_count() or 1)
        self.max_results = QSpinBox(minimum=0, maximum=10**9, value=10_000, specialValueText="unlimited",
                                    toolTip="stop after this many matches (with maximize/minimize: keep the best this many)")
        for w in (self.lo, self.hi, self.fixed):
            w.setFont(MONO)
            w.setMaximumWidth(170)
        for w in (self.lo, self.hi):
            w.textEdited.connect(lambda _: self.range.setCurrentText("custom"))
        self.mode.currentTextChanged.connect(self._mode_changed)
        self.range.currentTextChanged.connect(self._apply_range)
        tips = {"version": "Minecraft version (prepared data folders)", "mode": "which seeds to search (Help > Search modes)",
                "range": "fills from/to; the full range depends on the mode", "from": "first seed (inclusive); accepts 10k, 2.5M, 2^48",
                "to": "last seed (inclusive)", "fixed": "the part of the seed the mode keeps fixed",
                "threads": "CPU threads", "max": "stop after this many matches (best N when ranking)"}
        for label, w in (("version", self.version), ("mode", self.mode), ("range", self.range), ("from", self.lo), ("to", self.hi),
                         ("fixed", self.fixed), ("threads", self.threads), ("max", self.max_results)):
            lab = QLabel(f" {label} ")
            lab.setToolTip(tips[label])
            w.setToolTip(w.toolTip() or tips[label])
            bar.addWidget(lab)
            bar.addWidget(w)
        bar.addWidget(self.shuffle)
        self.use_gpu = QCheckBox("GPU", enabled=False, toolTip="run the seed-only part of the query (structure placement, slime chunks) on the GPU")
        self.gpu_status = QLabel(" checking CUDA… ")
        self.gpu_status.setStyleSheet("color: #9aa8b8")
        bar.addSeparator()
        bar.addWidget(self.use_gpu)
        bar.addWidget(self.gpu_status)
        self.cuda.connect(self._cuda_ready)
        threading.Thread(target=lambda: self.cuda.emit(*sft.cuda_status()), daemon=True).start()
        bar.addSeparator()
        self.actions_bar = {}
        tips = {"Estimate": "run the search for 3 seconds and project the total time and how rare matches are",
                "Start": "start searching", "Stop": "stop (Resume continues from here)", "Resume": "continue a stopped search",
                "Export": "save the results as CSV, JSON or a seed list"}
        for name, fn in (("Estimate", self.estimate), ("Start", self.start_search), ("Stop", self.stop_search),
                         ("Resume", self.resume_search), ("Export", self.export)):
            a = QAction(name, self, triggered=fn, toolTip=tips[name])
            bar.addAction(a)
            self.actions_bar[name] = bar.widgetForAction(a)
        tools = QToolButton(text="Tools", popupMode=QToolButton.ToolButtonPopupMode.InstantPopup)
        tm = QMenu(tools)
        tm.addAction("Scan map seed for the query...", self.scan)
        tm.addAction("Crack seed from structures...", self.crack)
        tm.addAction("Placement statistics...", self.placement_stats)
        tm.addAction("Search sister seeds of map seed", self.sisters)
        tm.addAction("Seed bits (upper 16 / lower 48)...", lambda: (self.bits_dock.show(), self.bits_dock.raise_()))
        tools.setMenu(tm)
        bar.addWidget(tools)
        self.toolbar = bar

        hm = self.menuBar().addMenu("&Help")
        hm.addAction("Getting started", QKeySequence("F1"), lambda: self.show_help("Getting-Started.md"))
        hm.addAction("Building queries", lambda: self.show_help("GUI.md", "Query editor"))
        hm.addAction("Every condition (query language)", lambda: self.show_help("Query-Language.md"))
        hm.addAction("Search modes", lambda: self.show_help("Search-Modes.md"))
        hm.addSeparator()
        hm.addAction("Take the tour", self.tour)
        hm.addAction("About", lambda: QMessageBox.about(self, "Seed Finders Toolkit", f"Seed Finders Toolkit {sft.__version__}\n"
                     "Bit-exact Minecraft Java seed finding. Help > Overview lists the projects it builds on."))

        # query, stats, results
        self.editor = BlockEditor(open_wizard=self.open_wizard, show_help=self.help_on)
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
        self.stats_box = stats_box
        split = QSplitter()
        split.addWidget(self.editor)
        split.addWidget(middle)
        split.setSizes([860, 520])
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
        self.map_box = map_box

        # seed bits dock, under the map; follows the map seed
        self.seed_bits = SeedBits(lambda low: self._fill_search("upper16", low, "full range"),
                                  lambda up: self._fill_search("lower48", up, "first 10M"),
                                  lambda seed: (self.map_seed.setText(str(seed)), self._map_from_controls()))
        self.bits_dock = QDockWidget("seed bits", self)
        self.bits_dock.setWidget(self.seed_bits)
        self.addDockWidget(Qt.DockWidgetArea.RightDockWidgetArea, self.bits_dock)
        self.splitDockWidget(dock, self.bits_dock, Qt.Orientation.Vertical)

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
        self.resizeDocks([bottom], [150], Qt.Orientation.Vertical)  # the query and results get the height

        self.timer = QTimer(self, interval=250, timeout=self._tick)
        self.version.currentTextChanged.connect(self.load_version)
        self.load_version(self.version.currentText())
        self.editor.set_text("village(within=300) and pillager_outpost(within=600)")
        if not self.settings.value("tour_offered", False, type=bool):
            QTimer.singleShot(700, self.welcome)

    # help and tour
    def show_help(self, page: str, find: str | None = None):
        if self.help_window is None:
            self.help_window = HelpWindow()
        self.help_window.open(page, find)

    def help_on(self, op: str):
        self.show_help("Query-Language.md", f"{op}(")

    def tour_steps(self):
        e = self.editor
        return [
            (e.blocks_palette, "1. Building blocks", "Every kind of condition, by category. Drag one onto the canvas, or double-click it."),
            (e.canvas, "2. Your query", "Blocks read like sentences: change the dropdowns and numbers in place. “ALL of these” means every "
             "block must hold; switch it to ANY or NONE, or nest groups. Drag the ⠿ grip to move a block; ⋮ has more."),
            (e.wizard_btn, "3. Not sure where to start?", "“I want…” asks a few plain questions (a structure near spawn, a slime farm, "
             "a speedrun seed…) and builds the blocks for you. Presets has ready-made queries."),
            (e.summary, "4. In plain words", "What the query finds, in words. The text box above it is the same query as text, for "
             "copying or sharing; edits there update the blocks."),
            (self.toolbar, "5. What to search", "mode picks the seeds: a range, structure seeds (much faster for structure queries), "
             "the sisters of one seed or a list. range and from/to set which seeds (10M, 2^48…); shuffle visits them in a random "
             "order. Tick GPU when it says “CUDA compatible”."),
            (self.actions_bar["Estimate"], "6. Estimate, then Start", "Estimate runs the search for 3 seconds and tells you how long the "
             "whole range takes and how rare matches are. Start runs it; Stop and Resume pick up exactly where you left off."),
            (self.stats_box, "7. Live progress", "Progress, speed, time left, matches found and the expected time to the next match."),
            (self.results, "8. Results", "Matching seeds appear here as they are found. Click one to see it on the map; right-click to copy "
             "the seed or a /tp command."),
            (self.map_box, "9. Map", "The selected seed's biomes and structures. Drag to pan, scroll to zoom. Tools > Scan shows every "
             "place in this seed where the query matches."),
            (None, "That's it", "Help (F1) has the full guide and every condition explained. You can take this tour again from the Help menu."),
        ]

    def welcome(self):
        self.settings.setValue("tour_offered", True)
        Tour(self, [(None, "Welcome to Seed Finders Toolkit", "Find Minecraft seeds by what is in them: structures, biomes, terrain, "
                     "chest loot. Take a one-minute tour of the window?")], lambda r: r == "start" and self.tour(), welcome=True)

    def tour(self):
        Tour(self, self.tour_steps(), lambda r: None)

    def open_wizard(self):
        def add(node, replace):
            if replace or not self.editor.root.body.kids:
                self.editor.set_text(query.to_text(node))
            else:
                self.editor.add(node)
        Wizard(self, self.editor.names, add).exec()

    def _cuda_ready(self, ok: bool, msg: str):
        self.use_gpu.setEnabled(ok)
        self.use_gpu.setChecked(ok and self.settings.value("use_gpu", True, type=bool))
        self.use_gpu.toggled.connect(lambda v: self.settings.setValue("use_gpu", v))
        self.gpu_status.setText(" CUDA compatible " if ok else " no CUDA ")
        self.gpu_status.setStyleSheet(f"color: {'#6fcf7f' if ok else '#9aa8b8'}")
        self.gpu_status.setToolTip(msg if ok else f"GPU not available: {msg}")
        self.use_gpu.setToolTip((f"{msg}: " if ok else "") + self.use_gpu.toolTip())

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
        self.mode.addItems(list(MODES) + [f"plugin: {n}" for n in api.seed_sources])
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
    def _mode_changed(self, text: str):
        kind = MODES.get(text)
        self.fixed.setEnabled(kind in ("lower48", "upper16"))
        self.fixed.setToolTip({"lower48": "upper 16 bits kept fixed (0 – 65535)",
                               "upper16": "seed or structure seed whose lower 48 bits are kept"}.get(kind, "not used in this mode"))
        self._apply_range(self.range.currentText())

    def _fill_search(self, kind: str, fixed: int, range_name: str):
        """seed bits buttons: set mode, fixed part and range; Start runs it"""
        self.mode.setCurrentText(next(t for t, k in MODES.items() if k == kind))
        self.fixed.setText(str(fixed))
        self.range.setCurrentText(range_name)
        self._apply_range(range_name)  # same name again emits nothing
        self.say(f"search fields set: {self.mode.currentText()}, fixed {fixed}, {self.lo.text()} … {self.hi.text()}; press Start")

    def _apply_range(self, name: str):
        if name == "custom":
            return
        lo, hi = FULL.get(MODES.get(self.mode.currentText()), FULL["range"])
        lo, hi = {"first 10M": (0, min(hi, 10**7 - 1)), "full range": (lo, hi), "32-bit (text seeds)": (-2**31, 2**31 - 1)}[name]
        self.lo.setText(str(lo))
        self.hi.setText(str(hi))

    def _make_job(self) -> sft.Job | None:
        if self.world is None:
            return None
        try:
            mode_text = self.mode.currentText()
            kind = MODES.get(mode_text, "plugin")
            lo, hi = parse_count(self.lo.text()), parse_count(self.hi.text())
            first, last = FULL.get(kind, FULL["range"])
            if not first <= lo <= hi <= last:
                raise ValueError(f"from/to must be within {first} … {last} in this mode, with from ≤ to")
            # ponytail: all 2^64 seeds don't fit a u64 count, so "full range" skips the very last seed
            start, count, seeds, mode = lo, min(hi - lo + 1, 2**64 - 1), None, "range"
            if kind == "lower48":
                upper = parse_count(self.fixed.text())
                if not 0 <= upper < 2**16:
                    raise ValueError("fixed upper 16 bits must be 0 … 65535")
                start = signed64(upper << 48 | lo)
            elif kind == "upper16":
                low = parse_count(self.fixed.text()) & (2**48 - 1)
                seeds, mode = [signed64(u << 48 | low) for u in range(lo, hi + 1)], "list"
            elif kind == "structure_seeds":
                mode = kind
            elif kind == "list":
                path, _ = QFileDialog.getOpenFileName(self, "seed list (one per line)")
                if not path:
                    return None
                seeds, mode = [int(x) for x in open(path).read().split()], "list"
                if not seeds:
                    raise ValueError(f"no seeds in {path}")
            elif kind == "plugin":
                src = self.loaded[1].seed_sources[mode_text.removeprefix("plugin: ")]
                seeds, mode = [s for _, s in zip(range(count), src())], "list"
            job = sft.Job(self.version.currentText(), self.editor.query(), mode, start, count, seeds, self.max_results.value(),
                          self.loaded, gpu=self.use_gpu.isChecked(), shuffle=random.getrandbits(64) if self.shuffle.isChecked() else None)
            if job.gpu_note:
                self.say(job.gpu_note)
            return job
        except Exception as e:
            QMessageBox.warning(self, "cannot start", str(e))
            return None

    def estimate(self):
        """Runs the search for a few seconds (Stop ends it) and extrapolates totals and rarity."""
        if self.busy():
            return
        job = self._make_job()
        if job is None:
            return
        # a new job: the old search's resume point and rows don't belong to it
        self.job, self.estimating, self.resume_unit = job, True, 0
        self.results.setRowCount(0)
        self.say("estimating (running the search for 3 s)...")
        threads = self.threads.value()

        def work():
            try:
                self.estimated.emit(job.estimate(3.0, threads))
            except Exception as e:
                self.estimated.emit({"note": f"estimate failed: {e}"})
        threading.Thread(target=work, daemon=True).start()

    def busy(self) -> bool:
        return self.estimating or bool(self.job and self.job.running())

    def _show_estimate(self, est: dict):
        self.estimating = False
        if "seconds_total" in est:
            self.stat["est. total"].setText(fmt_secs(est["seconds_total"]))
            self.stat["est. rate"].setText(f"{fmt_num(est['units_per_sec'])}/s")
            r = self._show_rarity(sft.rarity(est))
            self.say(f"estimate: {fmt_num(est['units_per_sec'])} units/s, prefilter pass {est['prefilter_rate']:.3%}, "
                     f"match rate {r}, total {fmt_secs(est['seconds_total'])}")
        else:
            self.say(est.get("note", "no estimate"))

    def _show_rarity(self, r: dict) -> str:
        if not r:
            return "-"
        self.stat["match rate"].setText(f"{r['p']:.3g}")
        self.stat["95% range"].setText(f"{r['low']:.2g}..{r['high']:.2g}")
        first = fmt_secs(r["seconds_to_first"]) if r["p"] else "> " + fmt_secs(r["seconds_to_first_range"][0])
        self.stat["first match"].setText(first)
        return f"{r['p']:.3g} per seed (95% {r['low']:.2g}..{r['high']:.2g}), first match ~{first}"

    def start_search(self):
        if self.busy():
            return
        job = self._make_job()
        if job is None:
            return
        self.job, self.resume_unit, self.hinted = job, 0, False
        self.results.setRowCount(0)
        self.job.start(self.threads.value())
        self.timer.start()
        self.say(f"search started: {query.to_text(self.editor.query())}")

    def stop_search(self):
        if self.job:
            self.job.stop()

    def resume_search(self):
        if self.job and not self.busy() and self.resume_unit < self.job.stats()["units_total"]:
            if not self.job.ranked and 0 < self.job.max_results <= self.job.stats()["matched"]:
                self.say("max results reached (max counts every match of a search, resumes included): start a new search with a higher max")
                return
            self.job.start(self.threads.value(), self.resume_unit)
            self.timer.start()
            self.say(f"resumed at unit {self.resume_unit}")

    def _add_row(self, seed: int, details: list):
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

    def _show_rows(self, new):
        if not new:
            return
        self.results.setSortingEnabled(False)  # once per batch: re-sorting per row freezes big batches
        if self.job.ranked:  # top-N: the kept rows change as better seeds arrive
            self.results.setRowCount(0)
            for seed, details in self.job.rows:
                self._add_row(seed, details)
        else:
            for seed, details in new[: max(0, MAX_TABLE_ROWS - self.results.rowCount())]:
                self._add_row(seed, details)
        self.results.setSortingEnabled(True)
        if self.job.ranked:
            self.results.sortItems(1, Qt.SortOrder.DescendingOrder)

    def _tick(self):
        if self.job is None:
            return
        self._show_rows(self.job.poll())
        for note in self.job.take_notes():
            self.say(note)
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
        if s["units_done"] > 0:
            self._show_rarity(s["rarity"])
        if self.job.mode == "structure_seeds" and s["checks"] > 1000 and s["prefilter_passed"] / s["checks"] > 0.5 and not self.hinted:
            self.hinted = True
            self.say("hint: most structure seeds pass this query's pre-check (no rare structure condition), so "
                     "structure-seed mode checks every sister seed and is no faster than range mode")
        if not self.job.running():
            self.timer.stop()
            self.resume_unit = self.job.join()
            self._show_rows(self.job.poll())  # stragglers after the last tick
            for note in self.job.take_notes():
                self.say(note)
            done = self.resume_unit >= s["units_total"]
            if s["error"]:
                self.say(s["error"])
                QMessageBox.warning(self, "search stopped", s["error"])
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
            self._tint_sisters(split(seed)[1])
            self.map_seed.setText(str(seed))
            first = next((d for d in details if "x" in d), None)
            if first:
                self.map_x.setValue(first["x"])
                self.map_z.setValue(first["z"])
            self._map_from_controls()

    def _tint_sisters(self, lower: int):
        """tint every result row whose seed shares these lower 48 bits (a sister of the selected seed)"""
        # ponytail: walks every row per click (up to MAX_TABLE_ROWS); index rows by lower 48 if it ever lags
        tint, plain = QBrush(QColor(70, 58, 25)), QBrush()
        self.results.setSortingEnabled(False)
        for r in range(self.results.rowCount()):
            it = self.results.item(r, 0)
            brush = tint if it and split(int(it.text()))[1] == lower else plain
            for c in range(self.results.columnCount()):
                if (cell := self.results.item(r, c)) is not None:
                    cell.setBackground(brush)
        self.results.setSortingEnabled(True)

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
        try:
            res = sft.check(self.version.currentText(), seed, self.editor.query(), self.loaded)
        except Exception as e:
            self.say(f"re-check {seed} failed: {e}")
            return
        self.say(f"re-check {seed}: {'match ' + json.dumps(res) if res is not None else 'NO MATCH'}")

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
        step, ok = QInputDialog.getInt(self, "scan", "anchor grid step (blocks); the query is moved from the map centre to each anchor", 64, 4, 4096)
        if not ok:
            return
        tree, version = self.editor.query(), self.version.currentText()
        x, z = self.map_x.value(), self.map_z.value()
        self.say(f"scanning seed {seed} within {within} of ({x}, {z})...")

        def done(hits):
            if isinstance(hits, Exception):
                self.say(f"scan failed: {hits}")
                return
            self.map.pins = [(hx, hz, str(i + 1)) for i, (hx, hz, _) in enumerate(hits[:200])]
            self.map.update()
            self.say(f"scan: {len(hits)} place(s) match" + "".join(f"\n  {hx} {hz}  {summary(d)}" for hx, hz, d in hits[:50]))
        self._background(lambda: sft.scan(version, seed, tree, x, z, within, step, loaded=self.loaded), done)

    def crack(self):
        text, ok = QInputDialog.getMultiLineText(
            self, "crack", "One structure per line as 'name x z' (block coordinates from /locate or F3).\n"
            "Huts, temples, outposts, igloos and shipwrecks narrow the search the most.\n"
            "Optional last line 'query: ...' adds conditions (biomes, heights) for the sister check.
"
            "Lines 'bedrock x y z yes' / 'bedrock x y z no' add the floor pattern (y -63 to -60): it picks the seed among the sisters.", "")
        if not ok or not text.strip():
            return
        obs, extra, floor = [], "", []
        try:
            for line in text.strip().splitlines():
                if line.startswith("query:"):
                    extra = line.removeprefix("query:").strip()
                elif line.startswith("bedrock"):
                    _, x, y, z, flag = line.replace(",", " ").split()
                    floor.append((int(x), int(y), int(z), flag.lower() in ("yes", "1", "true", "y")))
                elif line.strip():
                    n, x, z = line.replace(",", " ").split()
                    obs.append((n, int(x), int(z)))
        except ValueError:
            QMessageBox.warning(self, "crack", f"cannot read line: {line}")
            return
        extra = " and ".join(q for q in (extra, sft.bedrock_query(floor)) if q)
        if self.busy():
            QMessageBox.information(self, "crack", "a search is running: stop it first")
            return
        try:
            job = sft.CrackJob(self.version.currentText(), obs, extra, gpu=self.use_gpu.isChecked(),
                               max_results=self.max_results.value(), loaded=self.loaded)
        except Exception as e:
            QMessageBox.warning(self, "crack", str(e))
            return
        self.job, self.resume_unit, self.hinted = job, 0, False
        self.results.setRowCount(0)
        self.job.start(self.threads.value())
        self.timer.start()
        self.say(f"cracking from {len(obs)} structure(s): {job.crack.work:,} seeds to brute-force, then the sisters of each structure seed found")

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
        if seed is None or self.busy():
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
        self.seed_bits.set_seed(seed)
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
