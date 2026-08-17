# ADR-018: Tracked Debt — `bot_core`↔`solvers` Fusion

**Status: accepted (debt tracking).** This ADR records an acknowledged
architectural debt and its extraction trigger. No code change ships with it;
it exists so the fusion is **tracked** rather than silently baked in. Recorded
for epic `2PTFMZ` (task `JVRGO4`).

## Context

`degenbot-bot`'s `lib.rs` states that `bot_core` (the `BotState`
single-owner state, decoders, reorg journal, verifier, pump) and `solvers`
(the Möbius solvers + the `ArbitrageEngine` path/solver/dispatch layer) are
a **mutually coupled pair** — ~30 cross-references each way:

- `BotState` needs `IntHopState` / `IntV3TickRangeSequence` / decoders from
  `solvers` to re-derive solve-ready hop states at resolve time.
- The `ArbitrageEngine` needs `BotState` / `V3PoolState` / `TickInfo` /
  `PoolStateSubscriber` from `bot_core` to read/mutate pool state through
  the shared `Arc<RwLock<BotState>>` (ADR-003's engine-then-core lock order).

ADR-003 explicitly refuses to extract a `LiquidityMap` generic against this
sample-of-one, so the two live in one crate (`degenbot-bot`) rather than
behind an artificial shared-trait seam.

### The standalone-Rust-core cost

The fusion entangles the engine with `BotState`, so a standalone Rust
consumer (`cargo add degenbot`) that wants **only the V2/V3/V4 solve math**
cannot get it without dragging in the full `BotState` state machine **plus**
`degenbot-rpc` (the WS/IPC/HTTP provider plumbing), `degenbot-db` (the
SQLite schema + snapshot readers), `tokio` (the pump's async runtime),
`rayon` (the parallel solve fan-out), and `dashmap` (the concurrent
registry). This violates the spirit of ADR-005's standalone-Rust-core
constraint for the **solve** surface specifically — though the pure
swap-math leaves (`degenbot-v2-math`, `degenbot-concentrated-liquidity-math`,
`degenbot-balancer-math`, `degenbot-curve-math`, `degenbot-solidly-math`)
**are** already standalone and reachable without `degenbot-bot`. The gap is
the `ArbitrageEngine` composition layer that sits between the pure math
leaves and a standalone consumer.

## Decision

**Track the fusion as debt with an explicit extraction trigger.** Do not
extract a `solvers-core` crate or a `LiquidityMap` trait today — the
sample-of-one ruling still holds. Instead:

1. **Record the trigger.** Extraction happens when a **second engine
   family** joins — e.g. an `AaveLiquidationEngine`, or a split
   `SolidlyEngine` from a future Solidly rename/separation. At that point
   the "engine reads `BotState`" coupling is no longer sample-of-one (two
   engines need the same state-reading surface), and a shared trait becomes
   justified. Until then, the cross-references are the cost of one engine
   family and one state owner co-evolving.
2. **Record the sketch.** When the trigger fires, extraction would look
   like one of:
   - A `solvers-core` crate holding the `ArbitrageEngine` + the
     `IntHopState` / `IntV3TickRangeSequence` intake, depending on a
     `LiquidityMap` trait (the trait ADR-003 deferred) rather than on
     `BotState` concretely. `degenbot-bot` then implements `LiquidityMap`
     for its `V2/V3/V4` state maps; a standalone consumer can use
     `solvers-core` against its own `LiquidityMap` impl without `BotState`.
   - Or, if the trait proves premature, a `solvers-core` crate that takes
     `BotState`-shaped state **by generic parameter** (`ArbitrageEngine<S>`)
     so `degenbot-bot` is one `S = BotState` instantiation and a standalone
     consumer another.
3. **Keep the `lib.rs` doc honest.** The `lib.rs` module doc now references
   this ADR (rather than just stating ADR-003's refusal), so a reader
   landing on the fusion knows it is **tracked debt with a trigger**, not
   an accidental coupling.
4. **Record the disposition in the rubric.**
   `docs/migration-guides/three-layer-transition.md` gains an entry
   applying the triage rubric to `degenbot-bot::solvers`: disposition
   `partial` (the solve math is Rust-owned but not reachable standalone),
   trigger = second engine family.

## Consequences

- **No code change today.** The fusion stays; the cross-references stay;
  the crate-split target stays deferred. This ADR is the tracking
  artifact, not the extraction.
- **The standalone solve-surface gap is documented.** A standalone consumer
  wanting the `ArbitrageEngine` today must take `degenbot-bot` (and its
  transitive `degenbot-rpc` / `degenbot-db` / `tokio` / `rayon` / `dashmap`
  deps). The pure swap-math leaves **are** standalone; the engine layer is
  not. This is the known, tracked cost.
- **The trigger is named, not vague.** "Second engine family" is the
  signal; until then, extraction would be premature per the
  sample-of-one ruling.
- **No regression on the stateful topology.** ADR-003's engine-then-core
  lock order, ADR-004's typed TickMap seam, and ADR-006's per-chain `Bot`
  orchestrator are unaffected. This ADR is about the **crate boundary**
  between engine and state, not the runtime topology.

## References

- ADR-003 — `Bot` as the single Rust state owner (the refusal to extract a
  `LiquidityMap` generic against a sample-of-one; engine-then-core lock
  order).
- ADR-005 — Polars-inspired three-layer FFI (the standalone-Rust-core
  constraint the `solvers-core` extraction would restore for the solve
  surface).
- `rust/crates/degenbot-bot/src/lib.rs` — the module doc referencing this
  ADR.
- `docs/migration-guides/three-layer-transition.md` §"Dispositions" — the
  `partial` entry for `degenbot-bot::solvers` with the trigger noted.
- ergo `2PTFMZ` (epic) / `JVRGO4` (this task).
