# HKSKOF (M5, epic QECVJZ): degenbot-bot owns the collector — `BlockScope` wiring

M5 of the walk-telemetry collector migration (`docs/architecture/walk-telemetry-collector-sizing.md`
§5 M5 row). M1-M4 landed (the collector exists in `cl/tally.rs`, the entries take
`tele: Option<&WalkTelemetry>`, `GateDeps` carries the field, `per_block` seeds `None`). This chunk
is the bot-side ownership step: the engine constructs the `Arc<WalkTelemetry>` next to the
`Arc<WalkMemo>`, the `BlockScope` owns it, and both bot `GateDeps::per_block` sites pass it into the
walk instead of `None`. Default-build behaviour is unchanged — the collector is observational and
nothing drains it yet.

## What changed (4 files, +56/-21)

- **`rust/crates/engine/degenbot-bot/src/arb_engine/solve_cycle.rs`**
  - `BlockScope` gains `walk_telemetry: Arc<::degenbot_solvers::cl::WalkTelemetry>` beside
    `walk_memo` and `prefix_cache`, so the boundary's per-block resources live in one place; its
    doc comment is updated to say so.
  - `BlockScope::new(walk_memo, walk_telemetry)` takes both handles; new
    `#[must_use] pub(crate) fn walk_telemetry(&self) -> &Arc<WalkTelemetry>` accessor mirrors
    `walk_memo()`.
  - `SolveCycleShared::for_test` (the one test-scaffold constructor the three fixtures share) passes
    `Arc::new(WalkTelemetry::new())` — so the scaffold sites get the scope default automatically and
    need no edit of their own.
  - **Second `per_block` site:** `SolveCycle::solve_all` (the cold-start / non-streaming production
    arm, `:2162`) builds its deps with `.with_telemetry(Some(&walk_telemetry))`; the collector `Arc`
    is cloned into the `'static` bin jobs exactly like `memo` and `prefix_store`. This site is not
    named in the §4.3 item-17 list, but it is a live production `GateDeps::per_block` construction
    and leaving it `None` would have kept cold-start paths out of the census.
- **`rust/crates/engine/degenbot-bot/src/arb_engine/mod.rs`** — engine construction: the
  `Arc<WalkTelemetry>` is built adjacent to the `Arc<WalkMemo>` and handed to `BlockScope::new`.
- **`rust/crates/engine/degenbot-bot/src/arb_engine/lane_walk.rs`** — `solve_one_path`'s
  `GateDeps::per_block(...)` gains `.with_telemetry(Some(&**ctx.scope.walk_telemetry()))` (the
  `None` seed is replaced). The rest of the file — the `outcome.stats` reads, the OTel attributes,
  the `WALK_DENSE_ALERTED` latch — is untouched.
- **`rust/crates/engine/degenbot-solvers/src/profit_envelope/gate.rs`** — one additive
  `impl<'a> GateDeps<'a>` block with `#[must_use] pub fn with_telemetry(mut self, telemetry:
  Option<&'a WalkTelemetry>) -> Self`, plus a comment update on `per_block`'s `None` seed.

### API note for the reviewer (deliberate deviation from a literal reading of §4.3 item 17)

The M4 result doc anticipated "passing a live collector into `per_block`" (a sixth parameter).
Adding a parameter would force an edit to the 9 solvers-side `per_block` call sites
(`profit_envelope_tests.rs` x8 + `gate.rs`'s own test) — 2 extra files and churn inside a crate M5
does not otherwise touch, directly against this step's "keep the diff ≤4 files" risk advisory
(`lane_walk.rs` layout-churn). The builder keeps the ownership contract identical (the deps for a
block carry every handle the walk needs) while keeping `per_block`'s arity — and therefore every
existing caller — unchanged. The `per_block` comment now points at `with_telemetry` as the bot's
attachment point.

## A/B

- **walkmemo_stats.jsonl byte-equivalence per epoch:** unaffected by construction — this chunk
  touches no `WalkMemo` code and no `emit_epoch_boundary`/`final_drain` path (the collector observes;
  the memo caches — §2.5). The in-crate boundary-tap unit tests, which byte-compare emitted lines,
  pass in `cargo test -p degenbot-bot` (see gates). No runtime JSONL capture was run: the two
  required gates are the bot test + clippy.
- **C (zero-ambient audit)** — `rg -n "thread_local!|Cell<" rust/crates/engine/degenbot-solvers/src/cl/ | grep -v tests`:
  **3** before → **3** after. The remaining three lines are the `EVENT_CENSUS` thread-local block and
  its two cells (`cl/telemetry.rs:71,72,76`), which are M6's demolition target. Non-increasing, as
  required.

## Gates run (foreground, scoped)

- `cargo test -p degenbot-bot` → **ok, 0 failed** (lib **715 passed** / 3 ignored; all integration
  targets green; 0 doc-tests).
- `cargo clippy -p degenbot-bot --all-targets` → **clean, no warnings** (rustc/clippy **1.98.1**,
  `rust-toolchain.toml` pinned).
- Extra (scoped to a crate I touched): `cargo test -p degenbot-solvers --lib` → **ok, 0 failed**
  (177 passed / 3 ignored) — covers the additive `GateDeps::with_telemetry` change.
- `cargo fmt -p degenbot-bot -p degenbot-solvers` → applied before finishing (no unrelated-file
  churn; `git status` shows exactly the four files below).

## FFI

No FFI/`.pyi` surface is involved → `just gen-stubs` not run.

## PENDING

None. No command detached past the harness bound.

## Foreign edits / tree

Tree clean at start (`git status --porcelain` empty). No foreign edits observed. Working-tree
changes are exactly the four files above (nothing committed — left dirty for the manager).
`.ergo/plans.jsonl` untouched.
