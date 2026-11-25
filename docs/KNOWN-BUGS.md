# Known bugs and open follow-ups

Found in the 2026-11-12 code review and **not fixed yet**. The bugs were found by reading the code
and have not been reproduced; check each one before fixing it. Remove an entry when it is fixed.

## Bugs

5. **Loot queries fail silently.** When the loot tables did not load (`World.loot` is `Err`), the
   `loot` metric returns 0 (`Metric::Loot` in `engine/src/query.rs`) instead of refusing the query,
   and `Loot::fill` errors are swallowed there and in `PyWorld.chests`.
6. **`check` and the GUI's re-check ignore plugins.** `PyWorld.parse` passes no native libraries, so
   a query with `native(...)` fails with "native plugin not loaded"; `Main._recheck` and
   `cmd_scan` also drop `py(...)` terms without saying so.
7. **CLI edge cases.** An empty `--seeds-file` silently
   falls back to range mode (`"list" if seeds`); `scan` with only `py(...)` terms sends a null query.
8. **Two `island()` conditions overwrite each other's details.** `Eval::island` removes every
   earlier `island` detail before adding its own.
9. **Small robustness gaps.** `query._literal` only catches `ValueError`, so input such as
   `village(within={[]: 1})` escapes as a `TypeError` instead of a `QueryError`; the example
   plugin's markdown exporter raises `KeyError` on details without a `name` (anchors, islands,
   chests).

## Performance (measured or estimated, not done)

- **Per-seed setup** is still about 40-50 µs (`Seeded::new`), most of it rebuilding the sampler tree
  for every seed. The name hashes and spline tables are shared now; the remaining step is a
  seed-independent compiled tree with a per-seed noise table (a larger change to `density.rs`).
- **Allocation churn in terrain sampling.** `vol_of` allocates a buffer per node per call and every
  column allocates a context, aquifer vectors and a map (`iterate_column`); a scratch pool in `Ctx`
  would remove most of it. A height at a fresh seed costs about 0.3 ms.
- **Piece details are computed for every valid start**, used or not (`piece_details`: chest seeds,
  jungle temple floats). Computing them on demand needs `Hit` to carry the piece random.
- `Eval::hit` clones the cached `Hit` (tags, chests) on every lookup; loot tables are interpreted
  from JSON on every fill; `slime()` inside `anywhere()` has no pre-check; `anywhere()` rebuilds and
  sorts its anchor list per seed.

## Quality

- **Start and Estimate block the GUI thread** while `Search::new` measures the order of `and`
  children (0.3 s plus up to 0.2 s per child, longer when one evaluation is slow).
- **Ranked searches rebuild the whole results table** on every tick with a new result; with the
  GUI's default max of 10,000 that is 10,000 rows, re-sorted in `Job.poll` as well.
- `height`, `climate`, `biome_at` and `loot` hold the GIL (`check` releases it now).
- `near`, `chain` and `overlap` use fixed slack (128 and 384 blocks) where `cluster` uses the
  per-structure `Structures::reach`.
- `data/`, `plugins/` and the docs are found relative to the package parent, so only an editable
  install (`pip install -e .`) works.
- The block editor finds valid structure names by calling `World.attempts` and catching errors,
  which computes stronghold rings at startup; a `resolve` binding would do.
- No CI. `tests/test_engine.py` covers the search runner now, but needs `data/26.3`.
