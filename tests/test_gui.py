"""GUI checks, offscreen (needs data/26.3): blocks <-> text keep every query, every palette block and
wizard goal builds a valid query, drag and drop moves blocks. Run: python tests/test_gui.py"""
import json
import os
import sys
import tempfile
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from PySide6.QtCore import QByteArray, QMimeData, QPointF, QSettings, Qt  # noqa: E402
from PySide6.QtGui import QDropEvent  # noqa: E402
from PySide6.QtWidgets import QApplication  # noqa: E402

QSettings.setDefaultFormat(QSettings.Format.IniFormat)
QSettings.setPath(QSettings.Format.IniFormat, QSettings.Scope.UserScope, tempfile.mkdtemp())  # keep the real settings untouched
app = QApplication(sys.argv)
from sft import query  # noqa: E402
from sft.gui import blocks, wizard  # noqa: E402
from sft.gui.app import Main  # noqa: E402


def drop(body, node, dragging=None, y=10_000):
    m = QMimeData()
    m.setData(blocks.MIME, QByteArray(json.dumps(node).encode()))
    body.owner.editor.dragging = dragging
    body.dropEvent(QDropEvent(QPointF(5, y), Qt.DropAction.MoveAction | Qt.DropAction.CopyAction, m, Qt.MouseButton.LeftButton, Qt.KeyboardModifier.NoModifier))
    body.owner.editor.dragging = None


def main():
    w = Main("26.3")
    ed = w.editor
    for name, text in blocks.PRESETS.items():
        ed.set_text(text)
        ed.changed()  # text regenerated from the blocks
        assert query.parse(ed.text.toPlainText()) == query.parse(text), (name, ed.text.toPlainText())
    ed.set_text("")
    for ops in blocks.TEMPLATES.values():
        for op, d in ops.items():
            ed.add({"op": op, **json.loads(json.dumps(d))})
    query.parse(ed.text.toPlainText())
    for _, _, fields, build in wizard.GOALS:
        node = build({k: d for _, k, _, d in fields})
        assert query.parse(query.to_text(node)) == node
    ed.set_text("village(within=300) and (monument(within=800) or desert_pyramid(within=200))")
    group = next(k for k in ed.root.body.kids if isinstance(k, blocks.GroupBlock))
    village = next(k for k in ed.root.body.kids if isinstance(k, blocks.ConditionBlock))
    drop(group.body, {"op": "structure", "name": "igloo"})
    assert ed.text.toPlainText() == "village(within=300) and (monument(within=800) or desert_pyramid(within=200) or igloo())"
    drop(group.body, village.node(), dragging=village, y=0)
    assert ed.text.toPlainText() == "village(within=300) or monument(within=800) or desert_pyramid(within=200) or igloo()"
    drop(group.body, group.node(), dragging=group)  # into itself: refused
    assert ed.text.toPlainText().count(" or ") == 3
    group.body.kids[0].wrap("none")
    assert ed.text.toPlainText().startswith("not village(within=300) or ")
    print("ok")


if __name__ == "__main__":
    main()
