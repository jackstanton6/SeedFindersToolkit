"""Seed bits panel: a seed split into its upper 16 and lower 48 bits (the structure seed). Sister seeds share
the lower 48 bits, so they share structure placement; editing either half rebuilds the 64-bit seed."""
from PySide6.QtCore import Qt
from PySide6.QtGui import QFont, QGuiApplication
from PySide6.QtWidgets import QComboBox, QGridLayout, QHBoxLayout, QLabel, QLineEdit, QPushButton, QWidget

MONO = QFont("Consolas", 9)
LOW = 2**48 - 1
BASES = {"dec": 10, "hex": 16, "bin": 2}
UPPER_COLOR, LOWER_COLOR = "#e0a040", "#60a8e0"


def split(seed: int) -> tuple[int, int]:
    """signed 64-bit seed -> (upper 16, lower 48)"""
    u = seed & (2**64 - 1)
    return u >> 48, u & LOW


def join(upper: int, lower: int) -> int:
    v = upper << 48 | lower
    return v - 2**64 if v >= 2**63 else v


def fmt(v: int, bits: int, base: str) -> str:
    """v (an unsigned bits-wide part) as text in base 'dec', 'hex' or 'bin'."""
    if base == "dec":
        return str(v)  # ungrouped: pasted into the game as is
    digits = bits if base == "bin" else bits // 4
    # zero-padded to the full width, groups of 4 digits so sisters line up column by column
    return format(v, f"0{digits + digits // 4 - 1}_{base[0] if base == 'bin' else 'x'}").replace("_", " ")


def parse(text: str, base: str) -> int:
    """inverse of fmt: spaces and underscores are ignored, 0x / 0b prefixes accepted"""
    return int(text.replace(" ", "").replace("_", ""), BASES[base])


class SeedBits(QWidget):
    def __init__(self, search_sisters, search_upper, on_edit=lambda seed: None):
        """search_sisters(lower48) / search_upper(upper16): fill the search fields for that kind of search;
        on_edit(seed): the user typed a new seed or half here."""
        super().__init__()
        self.on_edit = on_edit
        self.seed_edit, self.upper_edit, self.lower_edit = QLineEdit("0"), QLineEdit(), QLineEdit()
        self.base = QComboBox()
        self.base.addItems(list(BASES))
        self.bits = QLabel(textFormat=Qt.TextFormat.RichText, textInteractionFlags=Qt.TextInteractionFlag.TextSelectableByMouse)
        for w in (self.seed_edit, self.upper_edit, self.lower_edit, self.bits):
            w.setFont(MONO)
        self.upper_edit.setStyleSheet(f"color: {UPPER_COLOR}")
        self.lower_edit.setStyleSheet(f"color: {LOWER_COLOR}")
        self.seed_edit.setToolTip("the full 64-bit seed (decimal, as in game)")
        self.upper_edit.setToolTip("upper 16 bits: what differs between sister seeds (biomes, terrain)")
        self.lower_edit.setToolTip("lower 48 bits = the structure seed: shared by all 65536 sisters (structure placement, slime chunks)")
        self.seed_edit.editingFinished.connect(self._from_seed)
        self.upper_edit.editingFinished.connect(self._from_halves)
        self.lower_edit.editingFinished.connect(self._from_halves)
        self.base.currentTextChanged.connect(lambda _: self.set_seed(self.seed))
        clip = QGuiApplication.clipboard()
        buttons = QHBoxLayout()
        for text, tip, fn in (
                ("copy seed", "", lambda: clip.setText(str(self.seed))),
                ("copy upper 16", "in the shown format", lambda: clip.setText(self.upper_edit.text())),
                ("copy lower 48", "in the shown format", lambda: clip.setText(self.lower_edit.text())),
                ("search sisters", "mode 'upper 16 bits': keep these lower 48 bits, try all 65536 upper halves",
                 lambda: search_sisters(split(self.seed)[1])),
                ("search this upper 16", "mode 'lower 48 bits': keep these upper 16 bits, vary the structure seed",
                 lambda: search_upper(split(self.seed)[0]))):
            buttons.addWidget(QPushButton(text, clicked=fn, toolTip=tip))
        buttons.addStretch()
        g = QGridLayout(self)
        g.setContentsMargins(4, 2, 4, 2)
        for r, (label, w) in enumerate((("seed", self.seed_edit), ("upper 16", self.upper_edit), ("lower 48", self.lower_edit))):
            g.addWidget(QLabel(label), r, 0)
            g.addWidget(w, r, 1)
        g.addWidget(QLabel("format"), 0, 2)
        g.addWidget(self.base, 0, 3)
        g.addWidget(self.bits, 3, 0, 1, 4)
        g.addLayout(buttons, 4, 0, 1, 4)
        g.setRowStretch(5, 1)
        self.seed = 0
        self.set_seed(0)

    def set_seed(self, seed: int):
        self.seed = seed
        upper, lower = split(seed)
        b = self.base.currentText()
        self.seed_edit.setText(str(seed))
        self.upper_edit.setText(fmt(upper, 16, b))
        self.lower_edit.setText(fmt(lower, 48, b))
        self.bits.setText(f"<span style='color:{UPPER_COLOR}'>{fmt(upper, 16, b)}</span> | "
                          f"<span style='color:{LOWER_COLOR}'>{fmt(lower, 48, b)}</span>")

    def _from_seed(self):
        try:
            seed = int(self.seed_edit.text().strip())
        except ValueError:
            seed = self.seed
        self._edited(seed if -2**63 <= seed < 2**63 else self.seed)

    def _from_halves(self):
        b = self.base.currentText()
        try:
            upper, lower = parse(self.upper_edit.text(), b), parse(self.lower_edit.text(), b)
        except ValueError:
            upper, lower = split(self.seed)
        if not (0 <= upper < 2**16 and 0 <= lower <= LOW):
            upper, lower = split(self.seed)  # out of range: back to the last valid seed
        self._edited(join(upper, lower))

    def _edited(self, seed: int):
        changed = seed != self.seed
        self.set_seed(seed)
        if changed:
            self.on_edit(seed)
