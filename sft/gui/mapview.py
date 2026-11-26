"""Biome/height/structure map of one seed, rendered by the engine. Drag to pan, wheel to zoom."""
import threading
import zlib

from PySide6.QtCore import QPoint, QRect, Qt, QTimer, Signal
from PySide6.QtGui import QColor, QFont, QImage, QPainter, QPen
from PySide6.QtWidgets import QWidget

# Familiar seed-map colours; unknown biomes get a stable hash colour.
BIOME_COLORS = {
    "ocean": 0x000070, "deep_ocean": 0x000030, "warm_ocean": 0x0000AC, "lukewarm_ocean": 0x000090, "deep_lukewarm_ocean": 0x000040,
    "cold_ocean": 0x202070, "deep_cold_ocean": 0x202038, "frozen_ocean": 0x7070D6, "deep_frozen_ocean": 0x404090,
    "river": 0x0000FF, "frozen_river": 0xA0A0FF, "beach": 0xFADE55, "snowy_beach": 0xFAF0C0, "stony_shore": 0xA2A284,
    "plains": 0x8DB360, "sunflower_plains": 0xB5DB88, "snowy_plains": 0xFFFFFF, "ice_spikes": 0xB4DCDC,
    "desert": 0xFA9418, "savanna": 0xBDB25F, "savanna_plateau": 0xA79D64, "windswept_savanna": 0xE5DA87,
    "badlands": 0xD94515, "eroded_badlands": 0xFF6D3D, "wooded_badlands": 0xB09765,
    "forest": 0x056621, "flower_forest": 0x2D8E49, "birch_forest": 0x307444, "old_growth_birch_forest": 0x589C6C,
    "dark_forest": 0x40511A, "pale_garden": 0x9AA39A, "taiga": 0x0B6A5F, "snowy_taiga": 0x31554A,
    "old_growth_pine_taiga": 0x596651, "old_growth_spruce_taiga": 0x818E79, "jungle": 0x537B09, "sparse_jungle": 0x628B17,
    "bamboo_jungle": 0x768E14, "swamp": 0x07F9B2, "mangrove_swamp": 0x2CCC8E, "mushroom_fields": 0xFF00FF,
    "meadow": 0x60A445, "cherry_grove": 0xFFB7D5, "grove": 0x47726C, "snowy_slopes": 0xC4C4C4, "frozen_peaks": 0xA0A0A0,
    "jagged_peaks": 0xDCDCC8, "stony_peaks": 0x7B8F74, "windswept_hills": 0x606060, "windswept_gravelly_hills": 0x789878,
    "windswept_forest": 0x507050, "dripstone_caves": 0x4E3012, "lush_caves": 0x283C00, "deep_dark": 0x031F29,
    "sulfur_caves": 0xBFB64A, "dappled_forest": 0x2F7A3A,
}
MARKERS = [  # (substring, colour, letter)
    ("village", 0xFFD040, "V"), ("pillager", 0xC04040, "P"), ("stronghold", 0xFFFFFF, "S"), ("monument", 0x40E0FF, "M"),
    ("mansion", 0x8B5A2B, "W"), ("ancient_city", 0x30FFC0, "A"), ("trial", 0xFF8C00, "T"), ("ruined_portal", 0xA040FF, "R"),
    ("desert_pyramid", 0xFFE0A0, "D"), ("jungle", 0x80FF80, "J"), ("swamp_hut", 0x40A040, "H"), ("igloo", 0xE0F0FF, "I"),
    ("shipwreck", 0x9A7B4F, "s"), ("ocean_ruin", 0x6080A0, "o"), ("buried", 0xFFD700, "$"), ("trail", 0xC08060, "t"),
    ("abandoned_camp", 0xD2B48C, "c"), ("mineshaft", 0x808080, "m"),
]
# Every-chunk sets (mineshafts, buried treasure) are opt-in: scanning them over a map is slow.
DEFAULT_SETS = [
    "villages", "pillager_outposts", "strongholds", "ocean_monuments", "woodland_mansions", "ancient_cities", "trial_chambers",
    "desert_pyramids", "jungle_temples", "swamp_huts", "igloos", "ruined_portals", "shipwrecks", "ocean_ruins", "trail_ruins",
    "abandoned_camp",
]
MAX_MARKER_RADIUS = 12_000


MAX_BLOCK_SCALE = 4  # block layers generate every chunk on screen
# Map item-like colours for common generated blocks; ore veins stand out on the "blocks at y" layer.
BLOCK_COLORS = {
    "air": 0x101014, "cave_air": 0x101014, "water": 0x3F76E4, "lava": 0xE0621A, "stone": 0x7D7D7D, "deepslate": 0x4D4D50,
    "bedrock": 0x2A2A2A, "grass_block": 0x5E9D34, "dirt": 0x86603D, "coarse_dirt": 0x77553B, "podzol": 0x6A4A2A,
    "mycelium": 0x6F6265, "mud": 0x3C3837, "sand": 0xDBD3A0, "sandstone": 0xD8CB9B, "red_sand": 0xBE6621,
    "red_sandstone": 0xB5621F, "gravel": 0x847F7F, "clay": 0xA0A6B3, "snow_block": 0xF8FDFD, "powder_snow": 0xF0F6F6,
    "ice": 0x91B7FD, "packed_ice": 0x8DB4FA, "blue_ice": 0x74A8FD, "calcite": 0xDFE0DC, "tuff": 0x6C6D66,
    "granite": 0x95675A, "diorite": 0xBCBCBC, "andesite": 0x888889, "terracotta": 0x985E43, "dripstone_block": 0x866B5C,
    "moss_block": 0x596E2D, "cinnabar": 0x8C2A24, "sulfur": 0xC9B83D,
    "raw_iron_block": 0xF0B898, "deepslate_iron_ore": 0xD08A6A, "iron_ore": 0xD08A6A,
    "raw_copper_block": 0xE07F4F, "copper_ore": 0xC87452,
}
VEIN_BLOCKS = {"raw_iron_block", "deepslate_iron_ore", "iron_ore", "raw_copper_block", "copper_ore", "tuff", "granite"}


def block_color(state: str, veins: bool) -> int:
    name = state.split("[")[0].removeprefix("minecraft:")
    if name.endswith("_terracotta"):
        colour = {"white": 0xD1B2A1, "orange": 0xA05325, "yellow": 0xBA8523, "red": 0x8E3C2E, "brown": 0x4D3323,
                  "light_gray": 0x876B62}.get(name.removesuffix("_terracotta"), 0x985E43)
    else:
        colour = BLOCK_COLORS.get(name, zlib.crc32(name.encode()) & 0xFFFFFF)
    if veins and name not in VEIN_BLOCKS and name not in ("air", "cave_air", "water", "lava"):
        r, g, b = colour >> 16, colour >> 8 & 255, colour & 255  # everything else dimmed
        colour = (r // 3) << 16 | (g // 3) << 8 | b // 3
    return colour


def biome_color(name: str) -> int:
    short = name.split(":")[-1]
    return BIOME_COLORS.get(short, zlib.crc32(short.encode()) & 0xFFFFFF)


class MapView(QWidget):
    """Owns view state (seed, centre, scale, y) and re-renders through the engine on change."""

    hovered = Signal(str)
    chunk_opened = Signal(int, int)  # double click: (chunk x, chunk z)
    _rendered = Signal(int, object)  # (generation, result) from the render thread

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setMouseTracking(True)
        self.setMinimumSize(320, 320)
        self.world = None
        self.seed, self.cx, self.cz, self.scale, self.y = 0, 0, 0, 8, 64
        self.show_height, self.show_structures = False, True
        self.layer = "biomes"  # or "surface blocks", "blocks at y"
        self.block_names: list[str] = []
        self.spawn: tuple[int, int] | None = None
        self.note = ""
        self.structure_filter: list[str] = list(DEFAULT_SETS)
        self.image: QImage | None = None
        self.ids: list[int] = []
        self.grid = (0, 0, 1, 1)  # x0, z0, w, h of the rendered image (cells of `scale` blocks)
        self.markers = []
        self.attempts_set = ""  # overlay every placement attempt of this set (failed ones as x)
        self.attempts = []
        self.pins: list[tuple[int, int, str]] = []  # scan hits etc.
        self._drag: QPoint | None = None
        self._timer = QTimer(self, singleShot=True, interval=150, timeout=self.render)
        self._gen = 0  # latest render request; older results are dropped
        self._rendered.connect(self._apply)
        self.names: list[str] = []

    def set_world(self, world):
        self.world = world
        self.names = world.biomes()
        self.request()

    def show_seed(self, seed: int, x: int | None = None, z: int | None = None):
        self.seed = seed
        if x is not None:
            self.cx, self.cz = x, z
        self.request()

    def request(self):
        self._timer.start()

    def render(self):
        """Renders in a background thread (the engine releases the GIL) so the window stays responsive."""
        if self.world is None:
            return
        self._gen += 1
        gen, world, seed, scale, y = self._gen, self.world, self.seed, self.scale, self.y
        w, h = max(self.width() // 2, 16), max(self.height() // 2, 16)  # 2x2 screen pixels per cell
        x0, z0 = self.cx - w * scale // 2, self.cz - h * scale // 2
        cx, cz, heights, structs, sets, att = self.cx, self.cz, self.show_height, self.show_structures, list(self.structure_filter), self.attempts_set
        layer = self.layer

        def work():
            try:
                note, block_names = "", []
                palette = [biome_color(n).to_bytes(3, "big") for n in self.names]
                ids = world.biome_map(seed, x0, z0, w, h, scale, y)
                rgb = bytearray(b"".join(palette[i] for i in ids))
                if layer != "biomes":
                    if scale > MAX_BLOCK_SCALE:
                        note = f"zoom in to {MAX_BLOCK_SCALE} blocks/px or less for block layers (every chunk is generated)"
                    else:
                        block_names, bids = world.block_map(seed, x0, z0, w, h, scale, y if layer == "blocks at y" else None)
                        colours = [block_color(n, layer == "blocks at y").to_bytes(3, "big") for n in block_names]
                        rgb = bytearray(b"".join(colours[i] for i in bids))
                        ids = bids
                if heights:
                    for k, hv in enumerate(world.height_map(seed, x0, z0, w, h, scale)):
                        f = max(0.35, min(1.4, 0.55 + (hv - 40) / 200))  # brightness by surface height
                        for c in range(3):
                            rgb[3 * k + c] = min(255, int(rgb[3 * k + c] * f))
                radius = int(max(w, h) * scale * 0.75)
                markers = world.structures_near(seed, cx, cz, radius, sets) if structs and sets and radius <= MAX_MARKER_RADIUS else []
                attempts = world.attempts(seed, att, cx, cz, radius) if att and radius <= MAX_MARKER_RADIUS else []
                spawn = world.spawn(seed)
                self._rendered.emit(gen, (bytes(rgb), ids, (x0, z0, w, h), markers, attempts, spawn, block_names, note))
            except Exception as e:  # e.g. an unknown set name; keep the old image
                self._rendered.emit(gen, e)
        threading.Thread(target=work, daemon=True).start()

    def _apply(self, gen: int, res):
        if gen != self._gen or isinstance(res, Exception):
            return
        self._rgb, self.ids, self.grid, self.markers, self.attempts, self.spawn, self.block_names, self.note = res  # QImage does not copy; keep the buffer alive
        _, _, w, h = self.grid
        self.image = QImage(self._rgb, w, h, 3 * w, QImage.Format.Format_RGB888)
        self.update()

    def to_screen(self, x, z) -> QPoint:
        x0, z0, w, h = self.grid
        return QPoint(int((x - x0) / self.scale * self.width() / w), int((z - z0) / self.scale * self.height() / h))

    def to_world(self, p: QPoint) -> tuple[int, int]:
        x0, z0, w, h = self.grid
        return int(x0 + p.x() * w / max(self.width(), 1) * self.scale), int(z0 + p.y() * h / max(self.height(), 1) * self.scale)

    def paintEvent(self, _):
        p = QPainter(self)
        p.fillRect(self.rect(), QColor(20, 20, 24))
        if self.image is None:
            p.setPen(QColor(160, 160, 160))
            p.drawText(self.rect(), Qt.AlignmentFlag.AlignCenter, "select a seed")
            return
        p.drawImage(self.rect(), self.image)
        p.setFont(QFont("Consolas", 8, QFont.Weight.Bold))
        for name, x, _, z, exact in self.markers:
            colour, letter = next(((c, l) for s, c, l in MARKERS if s in name), (0xFFFFFF, "?"))
            q = self.to_screen(x, z)
            p.setPen(QPen(QColor(0, 0, 0), 1))
            p.setBrush(QColor(colour))
            p.drawEllipse(q, 6, 6)
            p.drawText(QRect(q.x() - 6, q.y() - 7, 12, 14), Qt.AlignmentFlag.AlignCenter, letter if exact else letter + "?")
        for x, z, name in self.attempts:
            q = self.to_screen(x, z)
            p.setPen(QPen(QColor(80, 255, 80) if name else QColor(255, 70, 70), 2))
            p.setBrush(Qt.BrushStyle.NoBrush)
            if name:
                p.drawRect(q.x() - 4, q.y() - 4, 8, 8)
            else:
                p.drawLine(q.x() - 4, q.y() - 4, q.x() + 4, q.y() + 4)
                p.drawLine(q.x() - 4, q.y() + 4, q.x() + 4, q.y() - 4)
        if "stronghold" in self.attempts_set:  # ring bands: (128 + 192c) chunks +- 40
            o = self.to_screen(0, 0)
            p.setPen(QPen(QColor(255, 255, 255, 70), 1))
            for c in range(8):
                for r in ((88 + 192 * c) * 16, (168 + 192 * c) * 16):
                    d = int(r / self.scale * self.width() / max(self.grid[2], 1))
                    p.drawEllipse(o, d, d)
        p.setPen(QPen(QColor(0, 255, 255), 2))
        p.setBrush(Qt.BrushStyle.NoBrush)
        for x, z, label in self.pins:
            q = self.to_screen(x, z)
            p.drawEllipse(q, 9, 9)
            if label:
                p.drawText(q.x() + 11, q.y() + 4, label)
        if self.spawn:  # world spawn: a gold diamond
            q = self.to_screen(*self.spawn)
            p.setPen(QPen(QColor(0, 0, 0), 1))
            p.setBrush(QColor(255, 200, 40))
            p.drawPolygon([QPoint(q.x(), q.y() - 8), QPoint(q.x() + 8, q.y()), QPoint(q.x(), q.y() + 8), QPoint(q.x() - 8, q.y())])
        if self.note:
            p.setPen(QColor(255, 255, 255))
            p.drawText(self.rect().adjusted(8, 8, -8, -8), Qt.AlignmentFlag.AlignTop | Qt.AlignmentFlag.AlignLeft, self.note)
        q = self.to_screen(0, 0)  # world origin crosshair
        p.setPen(QPen(QColor(255, 255, 255, 160), 1))
        p.drawLine(q.x() - 8, q.y(), q.x() + 8, q.y())
        p.drawLine(q.x(), q.y() - 8, q.x(), q.y() + 8)

    def mouseDoubleClickEvent(self, e):
        if self.image is not None:
            x, z = self.to_world(e.position().toPoint())
            self.chunk_opened.emit(x >> 4, z >> 4)

    def mousePressEvent(self, e):
        self._drag = e.position().toPoint()

    def mouseReleaseEvent(self, _):
        self._drag = None

    def mouseMoveEvent(self, e):
        pos = e.position().toPoint()
        if self._drag is not None:
            d = pos - self._drag
            _, _, w, h = self.grid
            self.cx -= int(d.x() * w * self.scale / max(self.width(), 1))
            self.cz -= int(d.y() * h * self.scale / max(self.height(), 1))
            self._drag = pos
            self.request()
        elif self.image is not None and self.ids:
            x, z = self.to_world(pos)
            _, _, w, h = self.grid
            i, j = pos.x() * w // max(self.width(), 1), pos.y() * h // max(self.height(), 1)
            # the block-level (F3) biome at the hovered block, not the map cell's
            biome = self.names[self.world.block_biome(self.seed, x, self.y, z)].split(":")[-1]
            if self.block_names and 0 <= i < w and 0 <= j < h:
                biome += "  " + self.block_names[self.ids[j * w + i]].removeprefix("minecraft:")
            if self.spawn and (self.to_screen(*self.spawn) - pos).manhattanLength() < 12:
                biome += f"  |  world spawn ({self.spawn[0]}, {self.spawn[1]})"
            near = [m for m in self.markers if (self.to_screen(m[1], m[3]) - pos).manhattanLength() < 12]
            extra = "  |  " + ", ".join(f"{m[0].split(':')[-1]} ({m[1]}, {m[2]}, {m[3]})" for m in near) if near else ""
            self.hovered.emit(f"x {x}  z {z}  y {self.y}  {biome}{extra}")

    def wheelEvent(self, e):
        steps = [1, 2, 4, 8, 16, 32, 64, 128, 256]
        i = steps.index(self.scale) if self.scale in steps else 3
        i = max(0, i - 1) if e.angleDelta().y() > 0 else min(len(steps) - 1, i + 1)
        self.scale = steps[i]
        self.request()

    def resizeEvent(self, _):
        self.request()
