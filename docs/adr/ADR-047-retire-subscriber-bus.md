# ADR-047: Retire the ADR-006 D4 subscriber bus — compile is the guard for retired modules

**Status: accepted** (2026-09-12; architecture-review candidate #3, grilled + settled; epic `Y4VMWH` — bridge tail `967d1e81b` (T1), Rust seam `5aace566a` (T2)).

## Context

ADR-006 D4 introduced a per-state-subject publisher/subscriber bus (the "subscriber
bus"): `LogDispatcher` owned a decoder registry plus a
`Weak<dyn PoolStateSubscriber>` registry keyed by `pool_id`; after the core write
lock released, it notified every live subscriber of the affected pool, and the engine
implemented `PoolStateSubscriber::on_pool_state_updated` to dirty the `pool_id` in its
own per-engine set. A Python bridge (`PySubscriberAdapter` / `PySubscription` /
`register_subscriber` plus a dedicated `subscriber-drainer` OS thread) let Python
callbacks subscribe to the same fan-out.

Two later decisions drained the bus of its consumers:

- **ADR-041** (epic `MROOY7`) retired `EngineSubscriber` and moved dirty-tracking to
  the `EpochDelta` ledger; solve triggering became the `StageHandlers` hooks driven
  inline by the pump. The bus's only non-test consumer path — the engine subscriber —
  disappeared.
- The surviving consumers were the in-tree test fakes and an uncalled Python bridge
  (CONTEXT.md recorded the `_ffi.subscriber` submodule as "0 production callers;
  test-only"). No production consumer ever appeared.

The dead seam was not free. On **every applied forward log** the hot path carried a
`parking_lot::Mutex<HashMap<\u2026>>` lock in `LogDispatcher`, plus a
`NOTIFY-MISS` warn when a state apply found no subscriber attached
(`log_dispatcher.rs`). With zero production subscribers the warn fired on the hot path
to nobody. Worse, the notify channel was a *second* dirt-recording mechanism: dirt is
already owned by the `EpochDelta` ledger, so the notify path was redundant with the
authoritative owner.

## Decision

### D1 — Hard-retire the bus and its Python bridge

Delete the whole tail, hard, no shims and no feature flag (AGENTS.md):

- **Rust bus**: `PoolStateSubscriber`, `LogDispatcher::subscribe`, `LogDispatcher::notify`,
  the `subscribers: Mutex<HashMap<u64, Vec<Weak<dyn PoolStateSubscriber>>>>` field, and
  the `NOTIFY-MISS` warn. Dispatch becomes decode → apply → record delta, with no
  subscriber branch; an unregistered-pool apply is the only miss.
- **Python bridge**: `bot/subscriber.rs` in full — `PySubscriberAdapter`,
  `PySubscription`, `register_subscriber`, the queue, and the `subscriber-drainer`
  OS thread + its init/shutdown — its `c_api` registration, the `_ffi` `.pyi` stubs,
  and the test-only surface (`tests/fakes/subscribers.py`,
  `tests/test_pubsub_seam_parity.py`, `tests/types/test_subscriber_rust_routing.py`).

The deletion left the suite green: the `EpochDelta` ledger already covered the dirt
the bus tests asserted, so no replacement test was needed.

### D2 — `notify_pool_state_changed` → `record_pool_state_changed`

The `Bot` method is renamed to `record_pool_state_changed`. `record` names the sole
remaining meaning — append the changed pool to the `EpochDelta` ledger. `notify`
described a fan-out that no longer exists.

### D3 — ADR-006 D4 is superseded in part

The D4 orchestrator rows — `Bot` / `BotState` / `LogDispatcher` / `BlockPump` /
`ReorgCoordinator` — remain accurate. The D4 "per-state-subject publisher/subscriber
event bus" (the solve-notification protocol in ADR-006's Deferred section) is
**retired**; ADR-006 records the partial supersession.

### D4 — SETTLED POLICY: no resurrection guards for retired modules

**Resurrection-guard tests are rejected.** Once a module is deleted, **the compiler is
the guard**: any use of the retired name is a compile error, covering every consumer,
typed, at build time. A source-scan test only greps text; it guards nothing a deleted
module could violate.

**Source-scan tests are for LIVE invariants only** — rules a still-present code path
could silently violate without a compile error. The `no-pyo3-in-cores` scan is the
model: "core crates stay pyo3-free" can be violated by adding a dependency in another
crate, which compiles fine, so the scan is the only guard. A deleted module cannot
violate anything and needs no guard.

Concretely: do **not** add tests asserting `PoolStateSubscriber` / `subscribe` /
`PySubscription` / `register_subscriber` / `notify_pool_state_changed` do not exist.
Compile covers it. Future architecture reviews must not re-suggest resurrection guards
for retired modules.

## Considered options rejected

- **Keep the bus for a future second state-subject type or second engine.** Rejected:
  sample-of-one machinery (ADR-045's replay-harness rule) — reintroduce when the second
  adapter actually exists, not speculatively.
- **Feature-flag the bus for a parallel implementation.** Rejected: AGENTS.md hard
  cutover; there is no consumer to keep.
- **Keep the Python bridge as public API.** Rejected: 0 production callers, test-only;
  an API with no user.
- **Add a resurrection-guard scan for the retired names.** Rejected by D4 — compile is
  the guard; scans guard live invariants only.
- **Keep the `notify_pool_state_changed` name.** Rejected by D2 — it names fan-out
  that no longer exists.

## Consequences

- The per-log hot path drops the subscriber `Mutex` lock and the `NOTIFY-MISS` warn;
  dispatch has no subscriber branch.
- Dirt has exactly one owner: the `EpochDelta` ledger. `record_pool_state_changed` is
  the sole write.
- No Python-visible API change for users: the deleted bridge was an un-homed
  `_ffi.subscriber` with no companion home and no production callers.
- Hard cutover; the build receipt was verified after the Rust edits (`5aace566a`).
- The retired names and the rename are recorded in CONTEXT.md's retired-name
  discipline.

## Related

- **ADR-006** (Bot as the per-chain orchestrator) — D4's subscriber bus is superseded
  in part; the orchestrator rows stand.
- **ADR-041** (block-epoch pipeline) — retired `EngineSubscriber` and seated dirt on
  the `EpochDelta` ledger, removing the bus's production consumer.
- **ADR-045** (solve-cycle extraction) — the sample-of-one seam rule invoked above.
- **ADR-046** (StageHandlers / PumpControl) — the "which layer's vocabulary" test the
  retire-by-vocabulary call follows.
- The `no-pyo3-in-cores` scan — the live-invariant scan the settled policy contrasts
  against.
- **ADR-055** (pending-transaction strategy seams) — Phase B schedules a scoped
  `EventHub` (pending-tx/newHeads intake with subscription fan-out to registered
  strategies). This is NOT the retired bus resurrected: intake-scoped, typed at the
  subscription site, no generic broadcast surface for cores. The D4 retirement
  stands.
- Ergo epic `Y4VMWH` (candidate #3); parent `MROOY7`.
