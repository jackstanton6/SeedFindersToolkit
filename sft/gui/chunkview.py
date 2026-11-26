"""Chunk inspector: one generated chunk as a top-down layer (pick y) and a vertical slice (pick z)."""
from PySide6.QtCore import QPoint, Qt
from PySide6.QtGui import QColor, QImage, QPainter
from PySide6.QtWidgets import QDialog, QHBoxLayout, QLabel, QSlider, QVBoxLayout, QWidget

from sft.gui.mapview import block_color

MIN_Y = -64


class _Pixels(QWidget):
    """A block grid drawn as an image; hovering shows the block."""

    def __init__(self, view, w: int, h: int, cell_w: int, cell_h: int):
        super().__init__()
        self.view, self.w, self.h, self.cell_w, self.cell_h = view, w, h, cell_w, cell_h
        self.setFixedSize(w * cell_w, h * cell_h)
        self.setMouseTracking(True)
        self.image: QImage | None = None
        self.at = lambda i, j: ""  # (column, row) -> description

    def set_rgb(self, rgb: bytes):
        self._rgb = rgb  # QImage does not copy
        self.image = QImage(rgb, self.w, self.h, 3 * self.w, QImage.Format.Format_RGB888)
        self.update()

    def paintEvent(self, _):
        if self.image is not None:
            QPainter(self).drawImage(self.rect(), self.image)

    def mouseMoveEvent(self, e):
        p: QPoint = e.position().toPoint()
        self.view.info.setText(self.at(p.x() // self.cell_w, p.y() // self.cell_h))


class ChunkView(QDialog):
    def __init__(self, parent, world, seed: int, cx: int, cz: int, y: int):
        super().__init__(parent)
        self.setWindowTitle(f"chunk {cx} {cz} (blocks {cx * 16}..{cx * 16 + 15}, {cz * 16}..{cz * 16 + 15}) - seed {seed}")
        ((_, _, self.palette, self.blocks, self.heights, _),) = world.chunks(seed, cx, cz, cx, cz)
        self.cx, self.cz = cx, cz
        self.colours = [block_color(n, False).to_bytes(3, "big") for n in self.palette]
        self.top = _Pixels(self, 16, 16, 16, 16)
        self.side = _Pixels(self, 16, 384, 14, 2)
        self.y = QSlider(Qt.Orientation.Horizontal, minimum=MIN_Y, maximum=319, value=y, toolTip="y of the top-down layer")
        self.z = QSlider(Qt.Orientation.Horizontal, minimum=0, maximum=15, value=8, toolTip="z of the vertical slice (relative to the chunk)")
        self.y_label, self.z_label = QLabel(), QLabel()
        self.info = QLabel("hover a block")
        self.info.setMinimumWidth(300)
        self.y.valueChanged.connect(self.draw)
        self.z.valueChanged.connect(self.draw)
        left = QVBoxLayout()
        left.addWidget(QLabel("from above, at y:"))
        row = QHBoxLayout()
        row.addWidget(self.y)
        row.addWidget(self.y_label)
        left.addLayout(row)
        left.addWidget(self.top)
        left.addWidget(QLabel("from the side (x left to right, y up), at z:"))
        row = QHBoxLayout()
        row.addWidget(self.z)
        row.addWidget(self.z_label)
        left.addLayout(row)
        left.addWidget(self.info)
        left.addWidget(QLabel("Terrain, surface blocks, bedrock, ore veins and caves; no trees or structures yet."))
        left.addStretch(1)
        lay = QHBoxLayout(self)
        lay.addLayout(left)
        lay.addWidget(self.side)
        self.draw()

    def block(self, x: int, y: int, z: int) -> str:
        return self.palette[self.blocks[((y - MIN_Y) * 16 + z) * 16 + x]]

    def draw(self):
        y, z = self.y.value(), self.z.value()
        self.y_label.setText(str(y))
        self.z_label.setText(str(self.cz * 16 + z))
        self.top.set_rgb(b"".join(self.colours[self.blocks[((y - MIN_Y) * 16 + zz) * 16 + x]] for zz in range(16) for x in range(16)))
        self.side.set_rgb(b"".join(self.colours[self.blocks[((yy - MIN_Y) * 16 + z) * 16 + x]] for yy in range(319, MIN_Y - 1, -1) for x in range(16)))
        name = lambda s: s.removeprefix("minecraft:")  # noqa: E731
        self.top.at = lambda i, j: f"{self.cx * 16 + i} {y} {self.cz * 16 + j}: {name(self.block(i, y, j))}" if 0 <= i < 16 and 0 <= j < 16 else ""
        self.side.at = lambda i, j: (f"{self.cx * 16 + i} {319 - j} {self.cz * 16 + z}: {name(self.block(i, 319 - j, z))}"
                                     if 0 <= i < 16 and 0 <= j < 384 else "")
