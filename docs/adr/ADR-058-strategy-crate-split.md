# ADR-058: The strategy crate split — submission is mechanism, `degenbot-strategy` owns the plane

**Status: accepted** (2026-09-21). Records the landed extraction (commits
442527b36, 5daa9d670, 8888d85fc, c8c56aab0) and the vocabulary ruling that
preceded it (b3a3d91f5). Basis: ADR-055 (pending-transaction strategy seams),
ADR-054 (frame evidence seams), ADR-057 (the strategy host), ADR-019
(strategy-vs-engine), and the executed design. The crate sources remain the
last word on what the code does today.

## Context

Before the split, `degenbot-submission` held two different things at once: the
submission channel (signer, dispatcher, monitor, ledger, submit) and the
backrun strategy that used it — the reaction pipeline, the frame feed, the
anchored DFS, gap quarantine, and the pending-tx driver all lived beside the
signer. The backrun arm's decision layer and loop also straddled
`degenbot-bot`. Nothing owned "concrete strategies" as a concept, so the
umbrella crate was the only place a pure-Rust consumer could find one.

"Strategy" was also overloaded: `ExecutionStrategy` named the four-part
executor-contract seam, and `PendingTxStrategy` named the source-side reaction
contract, so the word could mean any of three different things.

## Decision

### D1 — Submission is mechanism only

`degenbot-submission` owns the channel and nothing strategy-shaped. Its
surface is the signer, fee pricing, transaction params, the bundle target, the
dispatcher, the monitor, the submission ledger, and `submit`; it names no
strategy. A caller hands it a candidate and a `SubmissionTarget` and reads a
`SubmitOutcome`.

### D2 — `degenbot-strategy` owns the plane and the concrete compositions

The strategy-plane crate (442527b36) is the home for concrete strategy
compositions and the vocabulary they compose over. It is pyo3-free. The
backrun arm then moved in (5daa9d670): `backrun.rs` and `backrun_engine.rs`
from `degenbot-bot`; `backrun_strategy.rs`, `backrun_driver.rs` plus its
subtree, `anchored_dfs.rs`, `frame_pipeline.rs`, `pending_tx.rs`,
`market_context.rs`, `gap_probe.rs`, `gap_quarantine.rs`, and
`gap_quarantine_journal.rs` from `degenbot-submission`.

The dependency direction is `strategy → capability crates`. Capability
implementations stay in their own crates and are re-exported here, never
moved. No capability crate depends on `degenbot-strategy`; only the umbrella
`degenbot` crate, `degenbot-python`, and (by the same allowance) any operator
binary may.

### D3 — Vocabulary rulings (b3a3d91f5)

The bare word "strategy" was reserved for the top-level composition only:

- `ExecutionStrategy` → `ExecutionAdapter` (module `strategy.rs` →
  `adapter.rs`). The Python lift needed no class rename of its own: its
  pyclass (`PyPayloadComposer`, Python-visible `PayloadComposer`) was never
  named after the seam, and the trait appeared in the lift only in doc
  comments (renamed with the trait). The four-part Encode / Probe / Assess /
  Fee seam (ADR-025 D2) is an executor-contract adapter, not a strategy.
- `PendingTxStrategy` → `PendingTxReaction`. The pending-transaction pipeline
  is the source-side contract behind backrun-type strategies, not a strategy.
- **Strategy** names a top-level kind of profit opportunity, composed as
  values over six capability slots: source, infrastructure, calculation,
  encoder, simulator, submission.

### D4 — Per-ecosystem types, composition over sub-traits

The backrun arm split into two distinct types (8888d85fc): `MevblockerBackrun`
and `TxpoolBackrun`. Both parameterize the same reaction machinery (frame feed,
anchored discovery, decide gate, simulation, dispatch) by composition; they
differ only in the value of their `SubmissionSlot`:

- `Mevblocker { bundle_url, private_url }` — an `eth_sendBundle` auction on the
  MEVBlocker searcher WebSocket, with the raw-broadcast fan-out leading with
  the private endpoint before the read-provider fallback.
- `PublicFanOut { relays }` — public-mempool relay fan-out with the read
  provider as fallback.

There is no `Backrun` sub-trait. Distinct ecosystems get distinct types;
shared reaction capabilities are mixed in by composition. Both compositions
are independently activatable and can run in one `StrategyHost` process.

### D5 — The plane contract is extracted by subtraction

`strategy_plane.rs` (c8c56aab0) holds the surface the three concrete strategies
demonstrably share and nothing else: `StrategyName` (the facet name, the host
registration id, and the one `select()` path) and `trait Strategy { const NAME }`.
A capability slot with one consumer stays out of the plane. The hosted boot
registers via `StrategyName::ALL` in plane order, and the config resolves the
settlement broadcast endpoints through the same plane surface instead of a
second config copy.

Settlement keeps a **special-case ledger** in `settlement.rs`, cited as the
open-work map rather than normalizing the cases away:

- **Pump self-driving.** The engine pump owns the settlement block clock, so
the host registers no spawn factory and `start_driving` skips it; generalizing
the pump is ADR-018's on-demand work.
- **Unconditional `Configured` registration.** The hosted boot registers the
  settlement facet as configured even when `strategy.settlement.active` is
  false; the runner resolves its live-mode gate after the engine exists.
- **Process-global submission lane.** The settlement `NonceLane` is installed
  process-wide, because the settlement seam is the Python-driven arm of the one
  hosted process.
- **Sole channel registrar.** `EngineChannelHandles::register_on` is the
  closure `StrategyHost::mint` hands the exclusive `&mut Hub`.
- **ADR-057 head-liveness coupling.** The hosted head feed runs from the
  Python settlement consumer's accepted-header clock, guarded on hosted
  activity.

### D6 — Seam learnings from the exercise

What actually moved differed from the plan:

- **Tests and fixtures moved with their modules.** `trace_*` tests and
  `backrun_lane_e2e` (from `degenbot-bot`) joined `anchored_dfs`,
  `frame_pipeline`, `admit_probe`, the v3/v4 window suites, `frame_liveness`,
  and `quarantine_journal_roundtrip` under the new crate.
  `hosted_driver_ambient_runtime` and `mock_third_family` **stayed** in
  `degenbot-submission`, because they exercise `strategy_host` plus the
  submission surface only. Two out-of-tree seams were blindly repointed to the
  moved paths: the `degenbot-config` env-read allowlist test and the
  `frame_oracle` capture fixture path.
- **A recorded conflict, resolved by carve-out.** Gating settlement
  registration on `strategy.settlement.active` would have broken the pinned
  default-boot enable behavior (the runner resolves its live relay posture
  after the engine exists, and a dry-run boot must still enable the arm). The
  unconditional-`Configured` carve-out was kept and documented in
  `settlement.rs` rather than silently changed.
- **The config hard cut.** `strategy.backrun` was replaced by
  `strategy.mevblocker_backrun` and `strategy.txpool_backrun` typed facets at the
  single declaration site, with env spellings
  `DEGENBOT_STRATEGY_MEVBLOCKER_BACKRUN_*` / `DEGENBOT_STRATEGY_PEER_BACKRUN_*`
  and `StrategyName` becoming `Settlement | MevblockerBackrun | TxpoolBackrun`.
  There are no aliases and no legacy parsing; readiness legality is per facet
  (MEVBlocker bid mode requires `key_file` and `mevblocker_url`). The retired
  single-arm `strategy.name` selector stays undeclared, pinned by
  `strategy_arm_selector_is_retired`.

## Consequences

- A pure-Rust consumer depends on `degenbot-strategy` for concrete strategies;
  `degenbot-submission` no longer names one.
- Adding a strategy family is one composition type, one config facet, and one
  registration. The plane contract grows only when a shared slot gains a second
  consumer.
- The retired names (`ExecutionStrategy`, `PendingTxStrategy`) and the old
  crate homes survive only in ADR bodies and the historical record.

## Related

- **ADR-055** — the pending-transaction seams the reaction contract serves.
- **ADR-057** — the strategy host that runs the compositions.
- **ADR-054** — the frame evidence seams the reaction stages consume.
- **ADR-019** — strategy vs engine; the six-slot composition is the strategy
  side of that split.
- **ADR-025** — the execution seam the adapter renamed from; the adapter is one
  of the six slots.
- **ADR-026** — settlement / backrun terminology.
- `CONTEXT.md`, `docs/architecture/strategy-seams.md`,
  `docs/architecture/adding-a-strategy.md`.
