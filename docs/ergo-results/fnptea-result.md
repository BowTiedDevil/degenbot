# FNPTEA (C2.3): the degenerate-path capture moves behind an engine-owned sink

The degenerate-path disk capture left the solver crate. `capture_degenerate_path`
(and its `GateCaptureCfg { out_path, max_paths }` config, static `Mutex`/`AtomicU64`
counter, and `OpenOptions` append) is gone from `degenbot-solvers`. The solver now
builds a typed capture EVENT and hands it to a caller-supplied sink handle; the engine
implements the file-writing adapter.

## What changed

**Solver (`degenbot-solvers`)**
- New `src/profit_envelope/capture.rs` (public API via `profit_envelope`):
  - `DegeneratePathEvent<'a>` — the typed event (`reject_hop`, `reject_reason`,
    owned `Vec<CapturedHop<'a>>` borrowing each CL hop's `IntV3TickRangeSequence`),
    with `from_hops(..)` and `to_jsonl_row()`.
  - `DegeneratePathSink: Debug + Send + Sync` — `try_reserve()` (the cap check the
    retired process-global counter owned) + `capture(&event)`.
  - `CapturedHop<'a>`; the CL range serializer `cl_seq_to_json` moved here.
- `profit_envelope/gate.rs`: `GateDeps::capture` is now
  `Option<&'a dyn DegeneratePathSink>` (the `WalkMemo` owner-handle precedent);
  `per_block` takes the sink. `GateCaptureCfg` deleted. The `DegenerateHop` exit
  calls `sink.try_reserve()` then `sink.capture(&DegeneratePathEvent::from_hops(..))`.
- `profit_envelope/derive.rs`: `capture_degenerate_path`, `cl_seq_to_json`, and all
  `std::io`/`OpenOptions` use deleted.
- Row serialization is unchanged: `to_jsonl_row()` builds the same
  `serde_json::json!` document the old code wrote, so rows stay byte-identical.

**Engine (`degenbot-bot`)**
- `arb_engine/solver_capture.rs`: `GateCaptureCfg` construction replaced by
  `DegeneratePathFileSink` + `gate_capture_from_cfg` (still the owner of the
  `DEGENBOT_GATE_CAPTURE*`-loaded typed config). The sink implements
  `DegeneratePathSink`: atomic cap, `Mutex<()>` write lock, `OpenOptions::append`,
  serialize-before-lock, poisoned-guard recovery — the exact adapter moved from the
  solver. Added a `degenerate_path_file_sink_tests` unit test (rows + cap).
- `arb_engine/solve_cycle.rs`: `SolveCycleShared::gate_capture: Option<DegeneratePathFileSink>`.
- `arb_engine/lane_walk.rs`: passes `ctx.gate_capture.as_ref().map(.. as &dyn DegeneratePathSink)`.

**Tests**
- `tests/profit_envelope_tests.rs`: the m6776w harness now substitutes an in-memory
  `InMemoryCaptureSink` (no temp file) and pins the row **byte-for-byte**
  (`assert_eq!(row, &expected.to_string(), "capture row format drifted")`).

## Acceptance

- `rg "std::fs|File::|OpenOptions" rust/crates/engine/degenbot-solvers/src` → the only
  hits are in `src/capture_fixture.rs` (the committed-fixture loader: lines 37/104 in
  the public `read_fixture`/`decode_zst_file`; lines 158/167 inside its `#[cfg(test)]`
  unit tests). **Verified** its only consumers are tests/examples (`tests/*.rs`,
  `examples/*.rs`, `cfg(test)` code in `profit_envelope/kernel.rs`, and the
  `executor_ab_probe` harness) — i.e. the test-only fixture loader the task names as
  exempt. It is **not** itself `#[cfg(test)]`-gated (the module is `pub` so
  integration tests / examples can import it); left untouched, nothing added or
  removed. The chunk's target — the degenerate-path disk capture — leaves zero hits.
- m6776w JSONL capture test passes against the new in-memory sink with the row pinned
  byte-for-byte against the historical schema.
- Clippy / rustfmt clean (below).

## Gates run (foreground)

- `cargo test -p degenbot-solvers` → **ok**: 177 + 7 + 1 + 3 + 3 + 1 + 20 + 4 + 1 + 7 + 1 + 2 + 0 doc-tests, 0 failed (3 ignored).
- `cargo test -p degenbot-bot` → **ok**: 715 + 9 + 1 + 2 + 1, 0 failed (3 ignored).
- `cargo clippy -p degenbot-solvers -p degenbot-bot --all-targets` → clean (1.98.1).
- `cargo fmt -p degenbot-solvers -p degenbot-bot -- --check` → clean.
- `cargo test -p degenbot-solvers --test profit_envelope_tests m6776w` → 4 passed.

Nothing marked PENDING; no command detached.

## Foreign tree activity

`.devcontainer/Dockerfile` carries an unrelated pre-existing modification (git status
shows it dirty at run start). Left completely alone.
