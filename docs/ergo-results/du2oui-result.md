# DU2OUI (M4, epic QECVJZ): entry plumbing sweep — `tele` beside `memo`, `GateDeps.telemetry`, substrate + all call sites

M4 of the walk-telemetry collector migration (`docs/architecture/walk-telemetry-collector-sizing.md`
§2.3 threading map, §5 M4 row). M1-M3 landed; this chunk is the mechanical plumbing sweep that
threads the engine-owned `Option<&WalkTelemetry>` collector from the gate through the solve entries
to the walk. Default build behaviour is unchanged (the collector is `None` everywhere here).

## What changed (16 files)

**degenbot-solvers (engine crate):**

- **`src/cl/entries.rs`** — both solve entries gain `tele: Option<&WalkTelemetry>` immediately after
  `memo`: `solve_cl_piecewise(prepared, memo, tele, cfg, env)` and
  `solve_mixed_piecewise(hops, memo, tele, cfg, env)`. The private body
  `solve_cl_piecewise_inner` takes the same `tele` and forwards it to
  `solve_active_set_path(&hops, cfg, env, tele)`; the mixed body forwards `tele` likewise.
  `derive_and_solve_cl_piecewise` keeps its signature and threads `None` internally
  (`solve_cl_piecewise(&prepared, None, None, cfg, None)`), so the ~35 offline test/example touch
  points that use it are untouched. New `use super::tally::WalkTelemetry;`.
- **`src/profit_envelope/gate.rs`** — `GateDeps` gains
  `pub(crate) telemetry: Option<&'a crate::cl::WalkTelemetry>` beside `walk_memo`, plus a
  `#[must_use] pub fn telemetry(&self)` accessor mirroring `walk_memo()`. `per_block`'s struct
  literal seeds `telemetry: None` (the bot-owned injection point lands in the M5 ownership step);
  `offline()`/`Default` pick it up automatically.
- **`src/mixed/solve.rs`** — the two dispatch sites pass `gate.telemetry()` beside
  `gate.walk_memo()` into the entries.
- **`src/cl/tally.rs`** — `WalkTelemetry` derives `Debug` (required because `GateDeps` derives
  `Debug` and now carries `Option<&WalkTelemetry>`).
- **Call-site sweep (mechanical, compiler-driven):** `src/cl/path_sim.rs` (test),
  `src/cl/tests/memo.rs` (14), `src/cl/tests/mod.rs` (11), `src/cl/tests/refinements.rs` (4),
  `tests/cl_cache_lab_goldens.rs`, `tests/mobius_discriminant_domain.rs`,
  `benches/walkmemo_contention.rs`, `examples/cl_cache_lab.rs`, `examples/mixed_solve_replay.rs`,
  `examples/walkmemo_readout.rs` — every direct entry call adds `None` in the new `tele` slot.

**degenbot-substrate (outside engine/, easy to miss):**

- **`src/resolve/mod.rs`** — the two resolve-time production entry consumers
  (`all_cl_solve`, `mixed_solve`, previously passing `None` memo) now pass `None` telemetry as well.

**degenbot-bot:**

- **`src/arb_engine/walk_telemetry.rs`** — the memo-tap unit test's `solve_cl_piecewise` call adds
  `None` (compiler-forced by the entry signature change; the bot's production wiring is M5).

All remaining entry call sites were found by the compiler's missing-argument errors and migrated in
one sweep. No `.pyi`/FFI surface references the changed entry functions, so `just gen-stubs` was not
needed.

## A/B evidence (§5 protocol)

Fixture: `crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst`.
Normalization: cargo banner lines dropped; wall-clock values (`us`/`ns`/`ms`, `in Xs`) replaced with
`T`. Baseline `/tmp/dmrsry-baseline/cl_solve_replay.txt` was captured feature-off.

**A — feature-OFF (the baseline's own configuration), the required indistinguishable run:**

```
cd rust && cargo run --release -p degenbot-solvers --example cl_solve_replay -- \
  crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst
```

- normalized whole-output diff vs baseline: **empty** (345 normalised lines each, `diff` exit 0)
- identical summary on both sides:
  `replayed 156 path(s) | golden 26/26 exact match | gate-skipped 130 | deterministic 156/156 | iters=9`

**A (extra confidence) — feature-ON build:**

```
cargo run --release --features telemetry -p degenbot-solvers --example cl_solve_replay -- <fixture>
```

With the feature on, the per-path `WalkStats` counters (`sims=`, `refine=`, `pieces=`, `wsteps=`,
`left=`, `right=`, `anchor=`, `evtslv=`) report real values instead of the feature-off zeros — that is
the intended feature-on behaviour (the tallies now record). Every non-counter line (profit vector,
`bound=`, `input_delta`, `golden=OK`, summary) is **byte-identical** to baseline (189/189 normalised
non-`median=` lines, `diff` exit 0). Profits and goldens are unaffected in both configurations.

**C — zero-ambient audit (monotonic):**

```
grep -rn "thread_local!|Cell<" rust/crates/engine/degenbot-solvers/src/cl/ | grep -v tests | wc -l
```

`3` at M3 → **`3`** at M4 (M4 touches no ambient writer; the remaining three lines are the
`EVENT_CENSUS` thread-local block and its two cells, which migrate in a later chunk). Non-increasing,
as required.

## Gates run (foreground, scoped)

- `cargo test -p degenbot-solvers` → ok, 0 failed (lib **177** passed / 3 ignored; all integration
  targets green incl. `cl_cache_lab_goldens`; 0 doc-tests).
- `cargo test -p degenbot-solvers --features telemetry` → ok, 0 failed (lib **178** passed / 3
  ignored; all integration targets green; 0 doc-tests).
- `cargo test -p degenbot-substrate` → ok, 0 failed (lib **355** passed; 0 doc-tests).
- `cargo test -p degenbot-bot` → ok, 0 failed (lib **715** passed / 3 ignored; all integration
  targets green).
- `cargo clippy -p degenbot-solvers -p degenbot-substrate -p degenbot-bot --all-targets` → clean
  (rustc/clippy 1.98.1, no warnings). Also `cargo clippy -p degenbot-solvers --all-targets
  --features telemetry` → clean (extra confidence, not a required gate).
- `cargo fmt -p degenbot-solvers -p degenbot-substrate -p degenbot-bot` → applied (rustfmt clean,
  no unrelated-file churn).

## PENDING

None. No command detached past the harness bound.

## Foreign edits / tree

Tree clean at start (`git status --porcelain` empty). No foreign edits observed. Working-tree
changes are exactly the sixteen files above (nothing committed — left dirty for the manager).
`.ergo/plans.jsonl` untouched.

## Scope note for the reviewer

`GateDeps::per_block` currently seeds `telemetry: None`; the accessor and field are in place, and
`mixed/solve.rs` forwards `gate.telemetry()`. Passing a live collector into `per_block` (the
`Arc<WalkTelemetry>` beside `WalkMemo` at the engine, the two bot `per_block` call sites) is the M5
"degenbot-bot ownership" step, per the M4 scope row.
