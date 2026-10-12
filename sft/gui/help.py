"""In-app help (the wiki pages, rendered) and the guided tour (a spotlight over each part of the window)."""
from pathlib import Path

from PySide6.QtCore import QEvent, QObject, QPoint, QRect, QRectF, Qt, QUrl
from PySide6.QtGui import QColor, QDesktopServices, QPainter, QPainterPath, QTextCursor
from PySide6.QtWidgets import QFrame, QHBoxLayout, QLabel, QListWidget, QPushButton, QSplitter, QTextBrowser, QVBoxLayout, QWidget

DOCS = Path(__file__).resolve().parents[2] / "docs" / "wiki"
PAGES = [("Getting started", "Getting-Started.md"), ("Using the window", "GUI.md"), ("Query language", "Query-Language.md"),
         ("Search modes", "Search-Modes.md"), ("Accuracy", "Accuracy.md"), ("Plugins", "Plugins.md"), ("Versions", "Versions.md"),
         ("Overview", "Home.md")]


class HelpWindow(QWidget):
    def __init__(self):
        super().__init__(None, Qt.WindowType.Window)
        self.setWindowTitle("Seed Finders Toolkit help")
        self.resize(1000, 760)
        self.pages = QListWidget()
        self.pages.addItems([t for t, _ in PAGES])
        self.pages.setMaximumWidth(200)
        self.pages.currentRowChanged.connect(lambda r: r >= 0 and self.open(PAGES[r][1]))
        self.view = QTextBrowser(openLinks=False)
        self.view.anchorClicked.connect(self.follow)
        self.view.setStyleSheet("QTextBrowser { padding: 10px; font-size: 10.5pt }")
        self.view.document().setDefaultStyleSheet("a { color: #7ab0ff; } code { color: #e0c080; }")
        split = QSplitter()
        split.addWidget(self.pages)
        split.addWidget(self.view)
        split.setSizes([180, 820])
        QVBoxLayout(self).addWidget(split)
        self.current = None

    def open(self, page: str, find: str | None = None):
        """Shows a wiki page; `find` scrolls to the first occurrence of that text (a heading)."""
        if page != self.current:
            p = DOCS / page
            self.view.setMarkdown(p.read_text(encoding="utf-8") if p.exists() else f"*{page} is not installed with this copy.*")
            self.current = page
            self.pages.blockSignals(True)
            self.pages.setCurrentRow(next((i for i, (_, f) in enumerate(PAGES) if f == page), -1))
            self.pages.blockSignals(False)
        self.view.moveCursor(QTextCursor.MoveOperation.Start)
        if find:
            self.view.find(find)
        self.show()
        self.raise_()
        self.activateWindow()

    def follow(self, url: QUrl):
        if url.scheme() in ("http", "https"):
            QDesktopServices.openUrl(url)
            return
        page = url.path().split("/")[-1] or self.current
        self.open(page if page.endswith(".md") else f"{page}.md", url.fragment().replace("-", " ") or None)


class Tour(QWidget):
    """Dims the window except one widget and explains it. steps: [(widget or None, title, text)]; a step
    without a widget shows in the middle. on_close(result): "done", "skip", or "start" (welcome card)."""

    def __init__(self, main: QWidget, steps, on_close, welcome=False):
        super().__init__(main)
        self.main, self.steps, self.i, self.on_close, self.welcome = main, steps, 0, on_close, welcome
        self.bubble = QFrame(self)
        self.bubble.setStyleSheet("QFrame { background: #2b3440; border: 1px solid #6a8bb0; border-radius: 8px } QLabel { border: none }")
        bl = QVBoxLayout(self.bubble)
        self.title = QLabel()
        self.title.setStyleSheet("font-weight: bold; font-size: 11pt")
        self.text = QLabel(wordWrap=True)
        self.text.setMinimumWidth(340)
        self.text.setMaximumWidth(400)
        self.count = QLabel()
        self.count.setStyleSheet("color: #9aa8b8")
        row = QHBoxLayout()
        self.skip = QPushButton("Not now" if welcome else "Skip tour", clicked=lambda: self._finish("skip"))
        self.back = QPushButton("Back", clicked=lambda: self.go(-1))
        self.next = QPushButton("Start the tour" if welcome else "Next", clicked=self._next)
        self.next.setDefault(True)
        row.addWidget(self.count)
        row.addStretch()
        for b in (self.skip, self.back, self.next):
            row.addWidget(b)
        bl.addWidget(self.title)
        bl.addWidget(self.text)
        bl.addLayout(row)
        main.installEventFilter(self)
        self.setGeometry(main.rect())
        self.show()
        self.raise_()
        self.go(0)

    def eventFilter(self, obj: QObject, e: QEvent):
        if obj is self.main and e.type() == QEvent.Type.Resize:
            self.setGeometry(self.main.rect())
            self.go(0)
        return False

    def target(self) -> QRect | None:
        w = self.steps[self.i][0]
        if w is None or not w.isVisible():
            return None
        return QRect(w.mapTo(self.main, QPoint(0, 0)), w.size()).adjusted(-4, -4, 4, 4)

    def go(self, d: int):
        self.i = max(0, min(len(self.steps) - 1, self.i + d))
        _, title, text = self.steps[self.i]
        self.title.setText(title)
        self.text.setText(text)
        last = self.i == len(self.steps) - 1
        self.count.setText("" if self.welcome else f"{self.i + 1} / {len(self.steps)}")
        self.back.setVisible(self.i > 0)
        if not self.welcome:
            self.next.setText("Done" if last else "Next")
        self.bubble.adjustSize()
        r, b, W, H = self.target(), self.bubble.size(), self.width(), self.height()
        if r is None:
            pos = QPoint((W - b.width()) // 2, (H - b.height()) // 2)
        elif r.bottom() + b.height() + 12 < H:
            pos = QPoint(r.left(), r.bottom() + 10)
        elif r.top() - b.height() - 12 > 0:
            pos = QPoint(r.left(), r.top() - b.height() - 10)
        else:  # tall target: beside it
            pos = QPoint(r.right() + 10 if r.right() + b.width() + 12 < W else r.left() - b.width() - 10, r.top() + 20)
        self.bubble.move(max(8, min(pos.x(), W - b.width() - 8)), max(8, min(pos.y(), H - b.height() - 8)))
        self.update()

    def _next(self):
        if self.welcome:
            self._finish("start")
        elif self.i == len(self.steps) - 1:
            self._finish("done")
        else:
            self.go(1)

    def paintEvent(self, _):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        path = QPainterPath()
        path.addRect(QRectF(self.rect()))
        r = self.target()
        if r is not None:
            hole = QPainterPath()
            hole.addRoundedRect(QRectF(r), 6, 6)
            path = path.subtracted(hole)
            p.setPen(QColor(120, 170, 230))
            p.drawRoundedRect(QRectF(r), 6, 6)
        p.fillPath(path, QColor(0, 0, 0, 150))

    def mousePressEvent(self, e):
        e.accept()  # the window underneath is paused while the tour runs

    def _finish(self, result: str):
        self.main.removeEventFilter(self)
        self.hide()
        self.deleteLater()
        self.on_close(result)
