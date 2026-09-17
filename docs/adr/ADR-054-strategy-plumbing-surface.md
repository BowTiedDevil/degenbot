# ADR-054: The strategy plumbing surface — ReplayOutcome, journal extractors, the planning workspace, and anchored discovery are the promoted seams

**Status: accepted** (2026-09-17; epic `HO76KB`, tasks `J4SSTF` → `Z3YEUO`: the frame-replay strategy was proven by the live soak report
`logs/backrun/replay_soak.md`, which this ADR closes over).

## Context

The pre-epic backrun pipeline decoded calldata: every strategy frame had to
*understand the wire* (routers, aggregators, batch settle orders, eth-flow
helpers...) — an unbounded-decode problem. The first extractor prototypes
lived in `degenbot_decoders::target_classifier`, the analytic post-target
estimator lived in `degenbot_bot::bot_core::post_target`, and the discover
stage fanned 2-hop drift cycles from a `QuoteFan`/connector-star surface.
The limit was structural: the wire carries MEV shapes the bot has to know
*about* without knowing the specifics — what pool a frame touches, with
what resulting state, and what cycle through those pools would still pay
after the frame.

The `HO76KB` epic rebuilt the hot path on four facts every lane consumes equally.
This ADR records the plumbing surface as promoted so the NEXT strategy
doesn't re-derive guarantees the merge already proves.

## Decision

### Seam 1 — `ReplayOutcome` carries the whole frame's failure/success

`degenbot_simulation::sim::evm::frame_replay::ReplayOutcome`. One type for
every possible result of having replayed a pending-tx frame: typed
post-states of touched pools, wall/rpc_reads evidence, and explicit failure
modes (`ReplayStatus`). A lane that observes an `Err`/`Incomplete`-variant
`ReplayOutcome` knows whether the frame died honestly (RPC failure,
nonce-gap) or simply was never actionable — the infamous "we closed our
eyes" pseudo-success doesn't exist on this surface.

Owner crate: `degenbot-simulation`. Producer: `degenbot-submission`'s frame
pipeline and any other lane feeding the workspace.

### Seam 2 — `extract_pool_post_states` = the journal is the only witness

`degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states`.
One function from `(ReplayOutcome, descriptors)` to typed
`PoolPostState`, keyed off the replay's journaled EV slots and nothing
else: reserve words, slot0/liquidity words, and V4's per-poolid state rows
flow from the EVM journal, and the extract stage reports a skip reason per
pool (`extract_skip` in the JSONL trace) instead of hallucinating a state.

The `slot_layout` module is the single source of truth for *where pool
state lives* on-chain — a lane that needed the count of pool-reserve-word
decoders from N=3 to N=1 in the first week of the epic.

### Seam 3 — The `PlanningWorkspace` is the only solver state a frame touches

`degenbot_submission::frame_pipeline` (`SidecarSolver` + the workspace
admission APIs in `degenbot_bot::sidecar_engine`). The contract is
**register with explicit state**: nothing in solve/discovery reads the
shared pool graph; a frame carries a private per-frame EVM whose chain
view feeds *this frame's* workspace admission and NOTHING else. The
consequence: concurrent frames can never cross-contaminate, and the
fleet's live dispatch keeps the true queue discipline the ADR-052-era
database had bought per-map.

Owner crates: `degenbot-bot` (engine) × `degenbot-submission` (frame
pipeline).

### Seam 4 — Anchored touched-set discovery replaces the star fan

`degenbot_submission::anchored_dfs` + `degenbot_submission::path_selection::solve_witnesses`.
On the frame's touched pools, the lane walks the connector-index multigraph
for **any depth-N cycle that closes in WETH** (`2-hop parity, 3-hop and
beyond by curve`). The old star fan (`two_hop_cycles`, `QuoteFan`,
normalization, per-frame quote ranking) is deleted outright — not kept
behind a flag — because the anchored lane is strictly the star's superset
by construction: every star cycle is a depth-2 anchored cycle, every quote
fan that could close through WETH also closes a WETH lexicographic
traversal if the walker is allowed to visit it.

Discovery evidence is committed per frame: `sideways steps are the node's
residual trees`, not side-band quotes; the **witness** records which
cycle's price at the frame state justified the envelope's compose.

## Hook-difficulty decisions made here

- **Scratch EVM isolation.** A frame's replay runs against a fresh EVM
  instance whose backend is the frame's own chain view; replaying a tx
  NEVER dirties the solver's shared `ScrapeDb`. The cost is one EVM
  scaffold per frame (~µs); the win is that relaying the SAME tx twice
  cannot compound state, and a buggy frame cannot poison a sibling frame.

- **Register with explicit state.** The solver workspace offers only
  `admit({family, pools} + explicit state)` — there is SECOND pool-state
  source a lane could ask for (the graph, the graph's caches). This makes
  the seeded `DB -> EVM` side directly auditable at `frame_pipeline.rs`'s
  single admission surface.

- **Slot-layout is one module.** No per-protocol "count the words"
  decoder scattered across strategies. The V2 reserves word, the V3
  slot0+liquidity pair, and the V4 poolid row each have exactly one
  home — both extractors and the workspace read through it.

- **Bid from profit, envelope against the fork view.** Compose≠decide:
  the lane computes `bid = floor(profit * share)` from the WETH-closed
  chain's profit and validates the compose via the on-chain `eth_callMany`
  envelope, not by replaying the bid inside the workspace. The two checks
  read *different* evidence so one bug can't flip both signs.

## How a third strategy consumes these seams — worked example: a liquidation dry-run

A lane "observe undercollateralized AAVE positions" consumes exactly each
seam in order, with no new machinery:

1. The frame pump hands pending-liquidation `ReplayOutcome`s (seam 1) —
   no liquidation-specific decode of calldata; a health-factor-touching
   frame is the wire selector `liquidationCall` that "touched a market" {
   the journal (seam 2) extracts the typed post-state of that market's
   debt-collateral record. The wire is understood as "pools whose state
   changed," the classic slot-layout query.
2. `admit_extracted` puts the affected market into a fresh frame's
   `PlanningWorkspace` (seam 3) — the workspace reads nothing from the
   arbitrator's long-lived state, so the lane needs no locks.
3. Anchored discovery (seam 4) enumerates cycles from the debt token
   through the collateral token; a solver-side ceiling prices each — the
   whole enumerator is "which markets are depth-2 WETH-closeable from this
   debt token," and nothing in the lane's code knows AAVE-specific words.
4. The liquidate-side contract call the lane emits goes through the same
   `eth_callMany` envelope verify each response epoch of composition uses.

The cost touched: none of the four seams. The lane's own specialised code
is "what sounds like a liquidation," (detect) and "why a profitable
relation between collateral and debt persists" (economics).

## Cross-references

- ADR-019 — the settlement/engine split this strategy's stage layering
  inherits.
- ADR-045 — the registry borrow contract; the strategy NEVER borrows from
  the registry because the discovery plane holds its own per-frame
  admission surface.
- ADR-051 — the rust-owned console: the lane's CLI face is a `cargo`
  binary, not a Python driver.
- AGENTS.md — "Rust is the engine; Python is a driver shell" is the
  interpretation under which these seams are promoted: every other
  strategy lives purely in `rust/crates/degenbot-submission` and
  `degenbot-bot`/`degenbot-simulation`, driving the strategy exclusively
  from this surface.
- `logs/backrun/replay_soak.md` — the live evidence that these are the
  observed contracts, not the hoped-for ones: 59 live frames, zero fake
  conversions, latency budget with >99% slot headroom at p95.
