# GQ2CAD (PC9, epic GZPPS3): the SolveCycle transient stash goes behind a ticket (half-step)

STATUS: **IMPLEMENTED (green scoped gates + both red paths recorded).** The draw verdict
moved off the cycle (`admission_draw_zero` is DELETED) onto a linear `CycleTicket` that the
Resolved row carries and the epoch consumes; the cycle-transient stash is module-private; the
block cursor's four read accessors collapsed into ONE value accessor (`BlockCoordinates`).
BASELINE CITED: `docs/ergo-results/hqylek-result.md` run 2 (green); landed
`937153a29, 4a03f5e04, 231334a8f, ad035c48a, 345a8c118, 5de576784, d18c49b8d, c2f35dafd,
e33b2c6c8, 0ec15ef0d`. Re-verified against the CURRENT tree (the P4 / PreparedClHop landing
touched `solve_cycle.rs` after the review snapshot: the draw/epoch seam and the
`cursor.advance_processed` call sites were re-shaped around, not around the snapshot).

## Shapes (read from the diff)

- **`arb_engine/solve_cycle.rs` — the ticket.** `draw(&EpochDelta, head) -> CycleTicket` (was
  `-> Vec<AffectedKey>` and a `bool` field). `CycleTicket { affected: Vec<AffectedKey>,
  draw_zero: bool }` — both fields module-private, no `Clone`, no constructor outside `draw`
  (a `#[cfg(test)] for_test` mints one behind the sanctioned harness door), no accessor for the
  verdict at all. `run_epoch(ticket: CycleTicket, ...)` takes the ticket BY VALUE and
  destructures it, so the drawn keys ARE the epoch's affected set and the verdict is read only
  there. The retired `admission_draw_zero` field is gone from the struct and from the engine's
  construction literal.
- **`arb_engine/solve_cycle.rs` — the stash.** `CycleTransients { last_arm, pending_new_paths,
  paths_same_state_this_cycle }` is one module-private value with a cold `Default`; `SolveCycle`
  holds it as `pub(crate) transients` (the field is reachable so the engine's literal can stand
  the cold stash up; its CONTENTS are unreadable outside the module), and the module's own
  `#[cfg(test)]` doors (`pending_new_paths`, `pending_new_paths_mut`, `same_state_this_cycle`)
  are the only white-box reads, surfaced through `test_harness` free functions.
- **`bot_core/stage_handlers.rs` — the carrier.** `AffectedPaths(Vec<AffectedKey>)` became
  `AffectedPaths(Cell<Option<CycleTicket>>)` with `drawn(ticket)` / `take_ticket(&self)` (a
  take-once slot) and the test-only `with_ticket(&self, read)` peek. The `Cell` is the deliberate
  consequence of the stage trait: `StageHandlers::on_solve(&self, work: &Solve)` reaches the row
  BY BORROW, so the ticket cannot be moved out of `&Solve`; the slot makes the epoch's single
  take a runtime-checked invariant instead of a silent second solve. The row's `Clone`/`PartialEq`
  derives are gone (a take-once token is neither copyable nor comparable) and `Default` is the
  empty row a test double returns.
- **`arb_engine/engine_stages.rs` — the wiring.** `on_resolve` wraps the draw's ticket
  (`AffectedPaths::drawn`), `on_solve` takes it exactly once (`work.paths.take_ticket()`) and
  hands it to `run_solve_cycle(ticket, ...)` -> `run_engine_cycle(..., ticket, ...)` ->
  `run_epoch(ticket, ...)`. The empty-affected fast path reads `ticket.is_empty()`.
- **`arb_engine/block_cursor.rs` — one value accessor.** `results_block()`,
  `last_processed_block()`, `last_solved_block()`, `has_logs_this_block()` and the anchored
  derivation collapse into `BlockCursor::coordinates() -> BlockCoordinates` (four `pub(crate)`
  coordinate fields + `BlockCoordinates::is_anchored()`); every read site now takes one coherent
  reading of the cursor.
- **`arb_engine/test_harness.rs` — the sanctioned door.** `cycle_ticket(&[AffectedKey])`,
  `pending_new_paths`, `pending_new_paths_mut`, `same_state_this_cycle` — the white-box drives
  reach the privatized stash and the ticket through these free functions and never re-index a
  private field.

## Ticket semantics + both probe records

The staleness class: the draw ran in the Resolved hook and the epoch read its verdict in a LATER
call. Now (a) the cycle holds no verdict field, (b) the ticket cannot be minted outside the draw,
and (c) the epoch consumes the one ticket it is handed, so a later call has nothing to consult.

### Compile probe (staleness cannot compile) — recorded verbatim

Probe planted in `solve_cycle.rs`'s test module (a second `run_epoch(ticket, ...)`), compiled
through the recipe `just check-rust-default` (exit 101), then reverted byte-for-byte:

```
error[E0382]: use of moved value: `ticket`
    --> crates/engine/degenbot-bot/src/arb_engine/solve_cycle.rs:3088:13
     |
3079 |         let ticket = crate::arb_engine::test_harness::cycle_ticket(&[]);
     |             ------ move occurs because `ticket` has type `solve_cycle::CycleTicket`, which does not implement the `Copy` trait
3080 |         let _ = engine.cycle.run_epoch(
3081 |             ticket,
     |             ------ value moved here
...
3088 |             ticket,
     |             ^^^^^^ value used here after move
     |
note: consider changing this parameter type in method `run_epoch` to borrow instead if owning the value isn't necessary
    --> crates/engine/degenbot-bot/src/arb_engine/solve_cycle.rs:1323:17
     |
1321 |     pub(crate) fn run_epoch(
     |                   --------- in this method
1322 |         &mut self,
1323 |         ticket: CycleTicket,
...
error: could not compile `degenbot-bot` (lib test) due to 1 previous error
```

The same probe is documented as a `// fn probe(...)`-style module-doc example (a ```text fence in
`solve_cycle.rs`: the retired `cycle.admission_draw_zero` read = E0609, a fabricated
`CycleTicket { .. }` = E0603 private field, the second `run_epoch(ticket, ..)` = E0382).

### Runtime red path (consume-once) — flipped once, recorded verbatim, reverted byte-for-byte

Flip: one extra `let _probe_second_take = work.paths.take_ticket();` inside
`EngineStages::on_solve`, driven by
`just test-rust-nextest "-E 'test(admission_draw_zero_shed_preserves_pending_new_paths)'"`:

```
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 763 filtered out; finished in 0.00s

  stderr ───

    thread 'arb_engine::tests::admission::admission_draw_zero_shed_preserves_pending_new_paths' (3653742) panicked at crates/engine/degenbot-bot/src/bot_core/stage_handlers.rs:217:14:
    the Resolved row's cycle ticket rides exactly one Solved pass
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

────────────
     Summary [   0.013s] 1 test run: 0 passed, 1 failed, 4755 skipped
        FAIL [   0.007s] (1/1) degenbot-bot arb_engine::tests::admission::admission_draw_zero_shed_preserves_pending_new_paths
error: test run failed
error: recipe `test-rust-nextest` failed with exit code 100
```

The flip was reverted byte-for-byte (`grep -c _probe_second_take` -> 0) and the take-once guard
is also pinned permanently by `the_resolved_row_takes_its_ticket_exactly_once` (a
`#[should_panic(expected = "the Resolved row's cycle ticket")]` unit test in `solve_cycle.rs`).

## Counts delta (baseline hqylek run 2)

| # | target | before | after |
|---|---|---|---|
| C1 | `engine.cycle.*` reference occurrences, tree-wide | 204 | **203** |
| C2 | `engine.cycle.*` reference occurrences in `arb_engine/tests/*.rs` | 160 | **155** |
| C3 | `pub(crate)` fields on `SolveCycle` (HEAD vs tree) | 36 | **33** |
| C4 | `pub(crate)` fields on `SolveCycleShared` | 13 | 13 |
| C5 | `pub(crate)` fields on the new `CycleTransients` / `CycleTicket` | n/a | **0 / 0** (3 + 2 private fields) |
| C6 | non-comment CODE reads of the privatized stash fields outside the owning module | 10 | **0** (the one remaining hit is the `test_harness` door call) |

> **Corrected 2026-10-10 (post-epic audit).** C1/C2 above count *module-path mentions* —
> doc links, imports, and calls to public methods (`solve_all`/`run_epoch`/`cursor`/`results`)
> — not white-box reads. Verified state at epic close: **0** test reads of `.transients.*`
> fields (the only 15 refs are `solve_cycle.rs` using its own private struct), **0** reads
> through the `#[cfg(test)]` doors at `mod.rs:2712/:2717`, **0** remaining `test_net_delta`
> references (the field itself is gone — `CycleTicket` absorbed it), and **0** test pokes of
> `SolveCycleShared`'s public internals (`pool_cache`, `wallet_deltas`, `pool_allocations`,
> `token_read_totals`, `addresses_read_totals`, `admission_targets`). The `arb_engine/tests/*`
> references drive the sanctioned `drawn.with_ticket(CycleTicket::is_empty)` shape.
> **The "203-site" test migration this task deliberately left is a phantom — the landed end
> state is already the clean endpoint. There is no follow-up work.**

Counting definitions (so a later task's numbers are comparable): a "reference occurrence" is one
`rg -o 'engine\.cycle\.[a-z_0-9]*'` match; a "field" is one `^    (pub(crate) )?name: ` line inside
the struct's body. The task body's "39 + 28 shared" baseline is a wider count than this one (my
parse of the same HEAD file gives 36 + 13; the delta, not the absolute, is the comparable unit).

`SolveCycle` field ledger: `admission_draw_zero` DELETED, `last_arm` / `pending_new_paths` /
`paths_same_state_this_cycle` privatized (folded into `CycleTransients`), `transients` added as a
`pub(crate)` field so the engine's construction literal can stand the cold stash up — 36 - 4 + 1 = 33.

## Deliberately left

- **The 100+ white-box `engine.cycle.*` sites are NOT swept.** The migration is opportunistic as
  briefed: 160 -> 155 in `arb_engine/tests/*.rs` (only where a site had to change anyway — the
  `pending_new_paths` reads and row peeks went through `test_harness` doors / `with_ticket`; the
  `path_resolved` / `results` / `path_status` / `solve_all` re-indexing is untouched).
- **The `run_epoch` / `run_solve_cycle` call sites DID have to change** (the ticket is a parameter,
  so `&[AffectedKey]` cannot be passed): 34 mechanical sites (17 `run_epoch` + 14 `run_solve_cycle` +
  3 harness/internal) across
  `tests/{arb_solving,admission,curve_and_spans,registration_and_delivery,lane_unify,detached_merge}.rs`
  and `boot_stamp.rs`, each wrapped as `test_harness::cycle_ticket(<the same keys>)`. No test's
  asserted behaviour changed.
- **`engine.cycle.cursor` stays `pub(crate)`.** Its four read accessors collapsed to ONE value door
  (`coordinates()`), but privatizing the cursor FIELD would force a ~69-site advance-sweep through
  new doors — the forced sweep the brief explicitly does not want.
- **`pending_new_paths` is not drained into the ticket.** Draining it at the draw would DROP the
  eager-registration carry on a shed cycle (the F2 contract: a draw-zero shed must leave the eager
  path for the NEXT cycle), so the carry stays a register->cycle stash and only its reads moved
  behind the doors.
- Nothing else: no Python surface, no stub, no ergo state, no commit.

## Gate exits (captured directly, foreground)

- `just rebuild-if-stale` -> EXIT 0 (installed 2231 = receipt 2231, `extension is fresh`)
- `just test-rust-nextest` -> EXIT 0 (4727 passed, 29 skipped in 10.3s) — re-run after the clippy fixes
- `just test-rust` (cargo, doc-tests included) -> EXIT 0
- `just lint-rust-check` -> EXIT 0 (clippy `--deny warnings` + one-impl-block + no-inner-allow + cli-shell-purity)
- `just lint-python-check` -> EXIT 0 (ruff + ty)
- `uv run --no-sync pytest tests/test_ffi_boundary.py` -> EXIT 0 (8 passed; re-run on the final sources)
- `just gen-stubs --check` -> EXIT 0 ("committed stubs match generator output" — surface did not move)
- `just fmt-check` (rustfmt) -> EXIT 0; `just fmt-check-python` (ruff format) -> EXIT 0
- `just lint-comment-hygiene` -> EXIT 0
- `just check-rust-default` -> EXIT 0 (the compile authority used for the probe, exit 101 with the probe in)

Two hygiene catches on my own new comments were fixed in place (bare `TICKET` / `BORROW` / a stray
caps token) — no census waiver, no lexicon entry added.

## Protection-strength check (guard rail c)

No guard weakened. The draw verdict lost every reader but the epoch's consume; a cycle can no longer
be driven from a stale key slice (the parameter is the ticket); siblings can no longer read the
latch/carry (private contents); the cursor exposes ONE reading instead of four pokes. The only new
`pub(crate)` surface is the `transients` field, whose contents stay module-private.

## Tree / foreign edits

- Files written by this chunk: the 17 files in `git diff --stat` plus this result doc. No commit, no
  `.ergo/plans.jsonl` edit, no ergo state change.
- Foreign modifications observed: NONE (the working tree was quiet all run; `git status` shows only
  this chunk's files).

## PENDING

None. (The manager's ladder — whole-workspace gates and the commit — is the manager's.)

## Manager review — verdict PASS (2026-10-10)

Independently verified: the `admission_draw_zero` field is gone (only the module-doc E0609 probe
and one behaviour-test name reference it); the `pending_new_paths`/`same_state_this_cycle` doors are
`#[cfg(test)]` over module-private fields; `rg '\.transients'` outside `solve_cycle.rs` = ZERO
outside reads; `block_cursor.rs` has exactly one `-> BlockCoordinates` accessor. The take-once
`Cell<Option<CycleTicket>>` adaptation (forced by `on_solve(&self, ...)` borrow semantics) keeps the
loud-failure property; the `#[should_panic]` permanent pin is the right shape. Counts table accepted
as honest: the no-sweep half-step shows near-flat site counts BY DESIGN (36 -> 33 `pub(crate)` fields,
transient/ticket fields 0 public). Manager battery `/tmp/pc9-gates/`: pre-push 0, gen-stubs 0,
comment-hygiene 0.
