# CLAUDE.md

@AGENTS.md

## Notes for Claude Code

- `PLANS.md` is the agreed scenario roadmap (tiers, building blocks, definition of done, declined
  items). Pick the next work from it, follow its order, and update it when an item ships or the plan
  changes. Don't add declined scenarios as dedicated features without asking.
- On Windows, cargo lives in `~/.cargo/bin` and may not be on PATH in the Bash tool:
  `export PATH="$HOME/.cargo/bin:$PATH"`. The project venv is `.venv/` (Python 3.14).
- Rebuild the Python extension after engine changes with `.venv/Scripts/python -m pip install -e .`.
  `cargo build` alone does not update `sft/_engine`.
- Check GUI changes headless: `QT_QPA_PLATFORM=offscreen QT_QPA_FONTDIR=C:/Windows/Fonts`, build
  `sft.gui.app.Main`, drive it, then `grab().save(...)` a screenshot and look at it.
- When porting a game class, read the decompiled source in `decompiled/` first, port it literally,
  then extend `oracle/Oracle.java` with a dump mode for it and add a parity test. "Looks right" is
  not verification; the oracle is.
- Keep diffs small and in the style of the surrounding code (short comments, no extra abstraction).
- Before making changes, ask the user a series of clarifying questions to ensure you provide the best
  possible solution. Ask at least four.