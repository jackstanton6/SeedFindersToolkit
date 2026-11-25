# Writing plugins

A plugin is a folder in `plugins/` (in the checkout, or in the per-user folder of a normal install,
or under `SFT_HOME`) with a `plugin.toml`. It can contain Python, a native (Rust/C)
library, or both. Drop the folder in and restart; the **plugins** tab (or `python -m sft plugins`)
shows what loaded and any errors. A broken plugin never takes the app down.

```toml
# plugins/my_plugin/plugin.toml
name = "my_plugin"                    # id used in queries
version = "0.1.0"
description = "What it adds"
minecraft = ["26.3"]                  # supported versions, or ["*"]
python = "plugin.py"                  # optional
native = "target/release/my_plugin"   # optional: cdylib path without extension
```

The two examples, `plugins/example_rust` and `plugins/example_python`, are complete templates.

## Python

`plugin.py` defines `register(api)`:

| Call | Signature of what you pass | Appears as |
|---|---|---|
| `api.condition(name, fn)` | `fn(world, seed, **params) -> bool` | `py("my_plugin.name", key=value)` in queries |
| `api.seed_source(name, fn)` | `fn() -> iterable of int` | mode `plugin: my_plugin.name` (first `to - from + 1` seeds) |
| `api.exporter(name, fn, ext)` | `fn(rows, path)`; rows are `(seed, details)` | an extra format in Export |
| `api.panel(title, factory)` | `factory(main_window) -> QWidget` | a tab in the bottom dock |

`world` is the engine `World` (also available as `sft.world(version)`):

```python
world.biomes()                                   # biome ids; index = id in biome maps
world.structures()                               # (structure, set, exact, overworld-possible)
world.biome_at(seed, x, y, z) -> "minecraft:plains"
world.height(seed, x, z, ocean_floor=False) -> int
world.climate(seed, x, y, z) -> [temperature, humidity, continentalness, erosion, depth, weirdness]
world.structures_near(seed, x, z, radius, names=None) -> [(structure, x, y, z, exact)]
world.biome_map(seed, x0, z0, w, h, scale, y) -> [biome id, ...]    # row-major, z rows
world.height_map(seed, x0, z0, w, h, scale) -> [height, ...]
world.check(seed, query_json, natives=[]) -> details JSON or None   # engine part only
sft.check(version, seed, query) -> details or None                   # with native(...) and py(...) terms
sft.scan(version, seed, query, x, z, within, step=64, anchor=None) -> [(x, z, details)]
```

Python conditions run on the engine's matches (after the Rust stage), so they can be slow-ish but
should still be selective. In queries they must be top-level `and` terms. `check`, `scan` and the
window's re-check run them too; they only see the seed, so in a scan they hold at every place or at none.

## Native (Rust or anything with a C ABI)

A `cdylib` exporting two functions. It runs inside the engine's worker threads, so it is as fast as
built-in conditions.

```rust
#[no_mangle] pub extern "C" fn sft_abi_version() -> u32 { 1 }

/// 1 = match, 0 = no match, negative = error (counts as no match)
#[no_mangle] pub unsafe extern "C" fn sft_filter(host: *const SftHost, name: *const c_char, params_json: *const c_char) -> i32
```

`name` is the filter name from the query (`native("my_plugin", "<name>", ...)`), and `params_json`
holds the keyword arguments as JSON. Host API (ABI 1; copy the struct from
`plugins/example_rust/src/lib.rs`):

| Field | Meaning |
|---|---|
| `abi: u32` | host ABI version |
| `ctx: *mut c_void` | opaque per-seed context: pass it back to every callback |
| `seed: i64` | the world seed being evaluated |
| `biome_at(ctx, x, y, z) -> i32` | biome id at a block (point path, as structure checks use); ~3 µs |
| `biome_id(ctx, name) -> i32` | id for `"plains"` / `"minecraft:plains"`, -1 if unknown |
| `surface_height(ctx, x, z) -> i32` | exact `WORLD_SURFACE_WG` height; ~0.3 ms, not cached |
| `count_structures(ctx, name, x, z, radius) -> i32` | generated starts within radius (-1 if unknown name); cached per seed |

Rules:
- `sft_filter` is called concurrently from many threads. Keep it free of shared mutable state.
- Don't keep `host` or `ctx` pointers after returning.
- Do cheap checks first and return early; call `surface_height` sparingly.
- Build with `cargo build --release` inside the plugin folder. Cargo names the library
  `my_plugin.dll` (Windows), `libmy_plugin.so` (Linux) or `libmy_plugin.dylib` (macOS); the loader
  finds whichever exists.

The ABI only grows by appending fields, and `ABI_VERSION` is bumped when it changes. The loader
refuses a plugin whose `sft_abi_version()` doesn't match.

## Both in one plugin

Put `python` and `native` in the same `plugin.toml`. A common split is: native for the hot filter,
Python for a seed source that feeds it, an exporter for the results, and a panel that visualises
them.
