Implemented by delegated worker + supervisor adversarial review + fixes.

Landed: d8db52c05 feat(config): XDG Base Directory defaults for run artifacts, state, and DB.

What shipped:
- ~/.config/degenbot is configuration only: config.toml loader honors $XDG_CONFIG_HOME (absolute-only, non-empty) after DEGENBOT_CONFIG.
- runs_dir / state_dir / DB defaults moved to the XDG state home ($XDG_STATE_HOME when absolute, else $HOME/.local/state). Precedence untouched: DEGENBOT_DB_PATH / DEGENBOT_RUNS_DIR / DEGENBOT_STATE_DIR + TOML keys win, explicit values expand as written.
- Component-aware prefix matching in expand_state_path_with (~/.local/stateful is not the state home).
- Python parity (_xdg_config_home/_xdg_state_home, DB_PATH under state home, DatabaseSettings default late-bound via Field(default_factory)).
- Test isolation: autouse fixture pins DB_PATH away from the real state home; late-binding fixed an actual leak that created a real ~/.local/state/degenbot/db/degenbot.db during test runs.

Adversarial review found (both fixed, red-green):
- HIGH: resolve_configured_path rebased EXPLICITLY-configured runs/state paths when XDG_STATE_HOME was set - only the built-in default may rebase (Source-aware rule, same as the DB seam). Reproduced red, fixed green (tests/xdg_explicit_path.rs).
- HIGH: raw string strip_prefix corrupted sibling paths (~/.local/stateful). Fixed with component-aware matching + unit pins.
- MEDIUM: Python tests wrote to the real state home (fixed; leaked dir removed).
- LOW (accepted divergence): HOME-unset behavior differs slightly (Rust returns the literal un-expanded path, Python falls to passwd home) - documented.

Live cutover verified: release binary rebuilt, old journal seeded into the new state home by copy (7 parked + 5 tentative reloaded, 0 skipped), sidecar live in session 20260918T185837Z-2653038 under ~/.local/state/degenbot/logs.

Follow-up (non-goals recorded):
- operator.sock could move to $XDG_RUNTIME_DIR/degenbot with a state-home fallback, but XDG_RUNTIME_DIR is wiped at logout - decoupled to a fleet-daemon-specific decision.
- Historical docs (ADR-051, autonomous-user-journey) still mention ~/.config/degenbot/degenbot.db as past fact - left as historical record.