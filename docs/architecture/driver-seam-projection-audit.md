# Driver-seam projection audit: the five one-line projections

**Status: audited, no change.** `EngineDriver`'s seam keeps its five projections (`core`, `verify`, `pool_id_for_identity`, `snapshot_seed_block`, `set_snapshot_seed_block`), defended by [ADR-093](../adr/ADR-093-session-lifecycle-and-consumer.md) D1's As-shipped note. The bar this audit was given: delete a projection and let its callers reach the home through `stages()` / `core()` instead — which pays only when the caller already holds one of those handles at the call site. `verify` fails the bar structurally (no other home owns a verify lifecycle), and the other four each carry a read/write *discipline* — a `LockSite` claim plus the guard-inside-`py.detach` rule — that a caller reaching through `stages().core()` would have to restate. One candidate has the strict shape (`core`: a pure forward with no rule in it); it is priced below and not taken, because deleting a `pub fn` on `EngineDriver` is a public-Rust-API deletion the session-cleanup plan licenses nowhere, and ADR-075's reopen condition (drift costing repeated incidents) is not met.

## What the five answer from

ADR-093 D1 ("As shipped") records twenty-one public seam members, five of which are one-line projections of state another home owns. Each answers from a home that already owns the state:

| Projection | Definition | Answers from | The home's own door |
|---|---|---|---|
| `core` | `arb_engine/driver.rs:619` | `EngineStages::core` | `arb_engine/engine_stages.rs:266` |
| `verify` | `arb_engine/driver.rs:638` | the driver's OWN `VerifyLifecycle` field | constructed in `assemble` from `stages.core()` (`driver.rs:591`, `:600`) |
| `pool_id_for_identity` | `arb_engine/driver.rs:666` | `BotState`, a `LockSite::Registration` read (`:669`) | `degenbot-substrate/src/pool_lookup.rs:481` |
| `snapshot_seed_block` | `arb_engine/driver.rs:676` | `BotState`, a `LockSite::Pump` read (`:680`) | `concentrated_liquidity_orchestration.rs:2942` |
| `set_snapshot_seed_block` | `arb_engine/driver.rs:685` | `BotState`, a `LockSite::Pump` write (`:689`) | `concentrated_liquidity_orchestration.rs:2558` |

Paths in the tables are relative to `rust/crates/` — `engine/degenbot-bot/src/`, `foundation/degenbot-substrate/src/`, `shells/degenbot-python/src/`, `facade/degenbot/src/` — and `rust/examples/` for the example.

A note the audit had to establish first: of the five, only four are projections in the strict sense. `verify` is the composition's own door. The verify lifecycle is built in `assemble` (`driver.rs:600`) and installed nowhere else — `EngineStages` carries no verify member at all (`grep -c verify arb_engine/engine_stages.rs` → 0), and no `Bot` accessor reaches it: the `Bot`-static `set_verify_*` adapters are deleted (direct-verify slice), so every caller configures the home through its own `config()` / `configure()` door over a `verify()` handle.

## The caller inventory

Every call site of the five in the workspace, with the handle each caller holds at that site and what the narrowed seam would give it. `driver()` in the shell is `EngineSession::driver() -> &Arc<EngineDriver>` (`engine/degenbot-bot/src/bot_core/session_boot.rs:234`); `PyArbEngine` holds `self.stages` (`shells/degenbot-python/src/bot/engine/mod.rs:63`) beside it.

### `core` — three call sites, one test file, zero production callers

| Call site | The handle in hand | Narrowed form |
|---|---|---|
| `engine/degenbot-bot/src/arb_engine/tests/session_paths.rs:506` | `first: Arc<EngineDriver>`, deriving `first.stages()` one line above (`:503`) | `first.stages().core()` |
| `.../tests/session_paths.rs:508` (same test) | same | `first.stages().core()` |
| `.../tests/session_paths.rs:581` | a local `stages: Arc<EngineStages>` cloned at `:578` | `stages.core()` — the local is already there |

`core` is the only one of the five whose callers all hold `stages()` and whose body states no rule: it is `self.stages.core()` verbatim (`driver.rs:620`). The irony worth recording: `every_engine_operation_is_reached_at_the_stage_seam` (`:576`) reaches the core through the driver while holding the stages arc.

### `verify` — fourteen call sites, none holding a verify home

| Call site | The handle in hand |
|---|---|
| `engine/degenbot-bot/src/arb_engine/driver.rs:1767` (test helper `live_crawl_for_test`) | `Arc<EngineDriver>` |
| `.../arb_engine/crawl/controller/tests.rs:503` (crawl fixture) | `Arc<EngineDriver>` — its `stages` arc was moved INTO the driver at `:498` |
| `shells/degenbot-python/src/bot/registration_lifecycle.rs:331`, `:338`, `:351` | `&Arc<EngineDriver>`, passed from `self.session.driver()` (`:388`) and from the `Bot` twin's `pump_state()` (`:408`) |
| `shells/degenbot-python/src/bot/pump.rs:52`, `:75` (the verify-config install wrappers the `set_verify_*` pymethod pairs and the `start` ritual delegate to) | `&Arc<EngineDriver>`, passed from `self.session.driver()` and the `Bot` twin's `pump_state()` |
| `shells/degenbot-python/src/bot/pump.rs:271`, `:288`, `:293` (the adapter's fail-loud error-path tests) | `Arc<EngineDriver>` |
| `examples/settlement_bot/src/main.rs:850`, `:930`, `:932`, `:942` (the verify-home configuration + the retry merge + the admission's verify handle) | `driver: Arc<EngineDriver>` (`:756`), which derives `driver.stages()` in the same boot block |

Narrowed form: **none exists**. `stages()` cannot answer — the verify home has no member on `EngineStages` — so "moving" `verify()` would first have to relocate the verify home into another type, which is a D4 reshaping, not a seam narrowing. The callers are not missing a handle; the destination is absent.

### `pool_id_for_identity` — two call sites, both the shell

| Call site | The handle in hand | Narrowed form |
|---|---|---|
| `shells/degenbot-python/src/bot/engine/identity.rs:46` (`pool_id_for_pool`) | `self.session.driver()`; the same `PyArbEngine` also holds `self.stages` (`mod.rs:63`) and the sanctioned `with_core` (`mod.rs:121`) | `self.with_core(py, \|s\| s.pool_id_for_identity(&identity))` answers the same id — but stamps `LockSite::Python` where the projection stamps `LockSite::Registration` (`driver.rs:669`) |
| `.../engine/identity.rs:65` (`pool_id_for_v4_pool`) | same | same |

The caller holds the handle, so the shape is narrowable; the cost is the claim. The projection says "this read is a registration read", and `with_core`'s stamp is the GIL-boundary site. Getting both would need a `with_core_at(LockSite)` helper — one new sanctioned accessor to delete one projection.

### `snapshot_seed_block` / `set_snapshot_seed_block` — the driver's own session, plus tests

| Call site | The handle in hand | Narrowed form |
|---|---|---|
| `engine/degenbot-bot/src/arb_engine/driver.rs:1003` (`backfill_gap`, the resume's `S+1..W` read) | `self.bot` and `self.stages` | the same read, restating `LockSite::Pump` |
| `.../arb_engine/driver.rs:1495`, `:1497`, `:1507` (unit test `snapshot_seed_block_roundtrips_through_the_shared_core`) | `EngineDriver` | `driver.stages().core()` / `driver.bot()` plus the lock site |
| `.../arb_engine/driver.rs:1496`, `:1506` (same test, the write) | `EngineDriver` | the write door is the driver's or the FFI's; `BotState` has no `Bot`-side setter |
| `.../arb_engine/tests/session_lifecycle.rs:456` (backfill-gap failure test) | `Arc<EngineDriver>` | same |

No production consumer outside the driver's own `backfill_gap`, and the Python-visible getter/setter pair already bypasses the projection entirely: `shells/degenbot-python/src/bot/engine/mod.rs:229-231` and `:248-253` go through `with_core` / `with_core_mut`, which is what ADR-093 D1's As-shipped note records ("the FFI's engine getter goes through the shared core rather than through the driver"). The parallel `Bot::snapshot_seed_block` (`bot_core/lifecycle.rs:52`) reads the same state with `LockSite::Orchestrator` — two readers naming their own site over one value, not two copies of it.

## Decision

**No change.** The conditional fails on its own terms:

- `verify` — fourteen call sites (nine production, five test), no alternative home to move it to. Neither a caller-side handle nor a stages-side member exists, so the projection is not redundancy laundered through the driver; it is the only door, and the deleted `Bot`-static setters only ever borrowed it.
- `pool_id_for_identity`, `snapshot_seed_block`, `set_snapshot_seed_block` — the callers do hold `stages()`/`bot()`, but what they would have to restate is the part the projection owns: which `LockSite` the read/write is claimed under, and (in the shell) that the guard is acquired inside `py.detach`. Deleting the door spreads that rule to every caller to save a one-liner.
- `core` — the one strict candidate; priced below.

The seam's own history is the other half of the disposition: ADR-093 D3 already made `EngineStages` the direct engine interface by subtraction (ergo KKUMXS deleted nine forwarding members), ADR-075's supersession record defers to that, and ADR-075's reopen condition — drift between the mirrored surfaces costing repeated incidents — has not fired. No incident is attributable to any of the five.

## The one strict candidate, priced

`EngineDriver::core()` is `self.stages.core()` and nothing more: no state, no lock-site choice, no discipline. Deleting it would take the seam from twenty-one members to twenty, retarget three test call sites (`session_paths.rs:506`, `:508`, `:581` → `stages.core()`), and require:

- a `rust/CHANGELOG.md` `Removed` row (ADR-088's hard-cutover license — the method is `pub` on a type the umbrella re-exports, `facade/degenbot/src/lib.rs:59`);
- an ADR-093 D1 As-shipped note recount (twenty-one → twenty; the five projections become four);
- a decisions-table row in `docs/adr/README.md` in the same change.

It is not taken here: the session-cleanup plan's "Explicitly NOT proposed" section states "no public surface deletions" and licenses exactly two in the pass (`reset_divergence_tally`, and T1.4's dropped FFI door), so a third public surface deletion this pass is scope the plan does not carry. Recorded rather than taken, so a future item that does license it starts from the three call sites above.

## Receipts

Reproducible from the repository root (build artifacts excluded):

```shell
grep -rn '\.core()' --include=*.rs rust/crates rust/examples | grep -v '/target/'
grep -rn '\.verify()' --include=*.rs rust/crates rust/examples | grep -v '/target/'
grep -rn 'pool_id_for_identity' --include=*.rs rust/crates rust/examples | grep -v '/target/'
grep -rn 'snapshot_seed_block' --include=*.rs rust/crates rust/examples | grep -v '/target/'
grep -c  verify rust/crates/engine/degenbot-bot/src/arb_engine/engine_stages.rs   # 0
grep -rn 'driver\.core()\|driver\.stages()' --include=*.rs rust/crates rust/examples
```

`EngineDriver::core`'s three call sites are the only hits for a receiver named `driver`/`first` at a `core()`; the other `.core()` hits in the tree are `EngineStages::core` (`engine_stages.rs:266`), the shell's `self.stages.core()` (`mod.rs:126`, `:143`, `:155`; `solve.rs:119`, `:147`), and test fixtures that hold an `EngineStages` (`clamp_merge_worker_tests.rs:169`, `:270`, `:456`; `crawl/unit/tests.rs:434`).
