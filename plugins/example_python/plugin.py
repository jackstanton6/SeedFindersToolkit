"""Example Python plugin: one of each extension point.

    py("example_python.seed_contains", text="777")       condition (runs on engine matches)
    py("example_python.spawn_biome", names=["cherry_grove", "meadow"])
    mode "plugin: example_python.text_seeds"             seed source: text seeds -> String.hashCode
    export as "example_python.markdown"                  exporter
    "seed info" tab                                      GUI panel
"""
from pathlib import Path

WORDS = ["minecraft", "seed", "dragon", "village", "diamond", "creeper", "notch", "herobrine", "spawn", "island"]


def java_hash(s: str) -> int:
    """java.lang.String.hashCode over UTF-16 code units: how the game turns a text seed into a number."""
    data, h = s.encode("utf-16-be"), 0
    for i in range(0, len(data), 2):
        h = (31 * h + (data[i] << 8 | data[i + 1])) & 0xFFFFFFFF
    return h - (1 << 32) if h >= 1 << 31 else h


def text_seeds():
    """Words (words.txt beside this file if present) plus numeric suffixes, as text seeds."""
    path = Path(__file__).with_name("words.txt")
    words = path.read_text().split() if path.exists() else WORDS
    for n in range(10**7):
        for w in words:
            yield java_hash(w if n == 0 else f"{w}{n}")


def export_markdown(rows, path):
    lines = ["| seed | matches |", "|---|---|"]
    lines += [f"| {seed} | " + ", ".join(f"{d.get('name', d.get('type', '?')).split(':')[-1]} ({d['x']}, {d['z']})" for d in details if "x" in d) + " |"
              for seed, details in rows]
    Path(path).write_text("\n".join(lines) + "\n")


def seed_info_panel(main):
    from PySide6.QtWidgets import QHBoxLayout, QLineEdit, QPlainTextEdit, QPushButton, QVBoxLayout, QWidget

    box = QWidget()
    seed = QLineEdit("0")
    out = QPlainTextEdit(readOnly=True)

    def show():
        w, s = main.world, int(seed.text())
        lines = [f"biome at spawn: {w.biome_at(s, 0, 64, 0)}", f"surface height at 0,0: {w.height(s, 0, 0, False)}", ""]
        lines += [f"{n.split(':')[-1]:28} {x:7} {y:4} {z:7}" for n, x, y, z, _ in
                  sorted(w.structures_near(s, 0, 0, 1000, None), key=lambda t: t[1] ** 2 + t[3] ** 2)]
        out.setPlainText("\n".join(lines))

    row = QHBoxLayout()
    row.addWidget(seed)
    row.addWidget(QPushButton("inspect", clicked=show))
    lay = QVBoxLayout(box)
    lay.addLayout(row)
    lay.addWidget(out)
    return box


def register(api):
    api.condition("seed_contains", lambda world, seed, text="777": text in str(seed))
    api.condition("spawn_biome", lambda world, seed, names=(): world.biome_at(seed, 0, 64, 0).split(":")[-1] in names)
    api.seed_source("text_seeds", text_seeds)
    api.exporter("markdown", export_markdown, "md")
    api.panel("seed info", seed_info_panel)
