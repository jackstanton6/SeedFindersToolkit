"""Plugins: one folder per plugin under plugins/, described by plugin.toml.

    name = "example"                      # id used in queries: native("example", ...) / py("example.fn")
    version = "0.1.0"
    description = "What it adds"
    minecraft = ["26.3"]                  # supported versions, or ["*"]
    python = "plugin.py"                  # optional: module with register(api)
    native = "target/release/example"     # optional: Rust/C cdylib path, extension omitted

A Python module's register(api) may add query conditions, seed sources, exporters and GUI panels.
A native library exports `sft_abi_version` and `sft_filter` (see sft/docs/Plugins.md).
"""
import importlib.util
import sys
import tomllib
import traceback
from dataclasses import dataclass, field
from pathlib import Path

from . import data

PLUGIN_DIR = data.ROOT / "plugins"


@dataclass
class Plugin:
    name: str
    path: Path
    meta: dict
    native: str | None = None
    errors: list[str] = field(default_factory=list)


@dataclass
class Api:
    """What register(api) can extend. Keys are "<plugin>.<name>"."""
    plugin: str
    conditions: dict = field(default_factory=dict)    # fn(world, seed, **params) -> bool
    seed_sources: dict = field(default_factory=dict)  # fn(**params) -> iterable[int]
    exporters: dict = field(default_factory=dict)     # (fn(rows, path), file extension)
    panels: list = field(default_factory=list)        # (title, factory(main_window) -> QWidget)

    def condition(self, name, fn):
        self.conditions[f"{self.plugin}.{name}"] = fn

    def seed_source(self, name, fn):
        self.seed_sources[f"{self.plugin}.{name}"] = fn

    def exporter(self, name, fn, extension="txt"):
        self.exporters[f"{self.plugin}.{name}"] = (fn, extension)

    def panel(self, title, factory):
        self.panels.append((title, factory))


def _native_path(folder: Path, stem: str) -> str | None:
    """Cargo names cdylibs name.dll / libname.so / libname.dylib."""
    p = folder / stem
    for cand in (p.with_suffix(".dll"), p.parent / f"lib{p.name}.so", p.parent / f"lib{p.name}.dylib", p.with_suffix(".so"), p.with_suffix(".dylib")):
        if cand.exists():
            return str(cand)
    return None


def load(version: str, root: Path = PLUGIN_DIR) -> tuple[list[Plugin], Api]:
    """Loads every plugin folder; failures are recorded on the plugin, never raised."""
    plugins, merged = [], Api("")
    for toml in sorted(root.glob("*/plugin.toml")):
        folder = toml.parent
        try:
            meta = tomllib.loads(toml.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as e:
            plugins.append(Plugin(folder.name, folder, {}, errors=[f"plugin.toml: {e}"]))
            continue
        p = Plugin(meta.get("name", folder.name), folder, meta)
        plugins.append(p)
        supported = meta.get("minecraft", ["*"])
        if "*" not in supported and version not in supported:
            p.errors.append(f"supports {supported}, not {version}")
            continue
        if meta.get("native"):
            p.native = _native_path(folder, meta["native"])
            if p.native is None:
                p.errors.append(f"native library '{meta['native']}' not built (run cargo build --release in {folder})")
        if meta.get("python"):
            api = Api(p.name)
            try:
                spec = importlib.util.spec_from_file_location(f"sft_plugin_{p.name}", folder / meta["python"])
                module = importlib.util.module_from_spec(spec)
                sys.modules[spec.name] = module
                spec.loader.exec_module(module)
                module.register(api)
            except Exception:  # a broken plugin must not take the app down
                p.errors.append(traceback.format_exc(limit=3))
                continue
            merged.conditions.update(api.conditions)
            merged.seed_sources.update(api.seed_sources)
            merged.exporters.update(api.exporters)
            merged.panels.extend(api.panels)
    return plugins, merged


def natives(plugins: list[Plugin]) -> list[tuple[str, str]]:
    """(name, library path) pairs for the engine; only plugins whose library loaded cleanly."""
    return [(p.name, p.native) for p in plugins if p.native]
