# ADR-105: Block-anchored event-sourced chain state

**Status: proposed** (2026-11-23; ergo epic `YMSQIZ`, scoping task `6GTIG7`).

Builds on [ADR-041](ADR-041-block-epoch-pipeline.md) (one stage machine over cheap reads), [ADR-068](ADR-068-updater-golden-capture-replay.md) (recorded updater coherence), [ADR-070](ADR-070-updaters-are-the-sole-db-writers.md) (updater-owned writes), and [ADR-104](ADR-104-typed-eventhub-intake-and-engine-session-stream.md) (typed head intake). Those decisions remain foundations; this ADR supplies the chain identity, position, receipt ownership, and projection persistence they do not currently unify.

## Context

Twelve fixes during 2026-11-15 through 2026-11-22 addressed recurring mechanisms rather than isolated arithmetic mistakes: reads at the wrong block, replacement entities without branch-local generations, transaction-wide ownership scans, late relationship resolution, divergent running state, missing log identity, competing head sources, and telemetry whose ancestry was reconstructed at consumers. Appendix E maps every fix to the invariant that now prevents its mechanism.

The current tree already has useful pieces: `Epoch` and `BlockContext`, core `CanonicalHead`, RPC `BlockLineage`, simulation `ReadAnchor`, `EventOwnership`, `AaveChunkEvent`, `ChunkSubstrate`, pool `ReorgJournal`s, typed eventhub heads, and substantial cassette/receipt goldens. They do not yet form one contract. In particular, Aave persists a number-only cursor and performs resolution RPC inside its DB write transaction; live work carries several independent block-number fields; exact hashes exist only on some side paths.

## Decision

### D1 — `ChainAnchor` is the one exact block identity

`degenbot-core` owns dependency-neutral chain coordinates. `ChainAnchor` contains `(chain_id, number, hash, parent_hash)`. `TxRef` and `LogRef` qualify transaction and log positions by that anchor; `CallRef` adds a trace call path for trace-backed receipts.

Existing core `CanonicalHead`, RPC `BlockLineage`, and simulation `ReadAnchor` converge on this identity. `Epoch` remains the live session's invalidation sequence, and telemetry `Block lineage` remains solve identity plus exporter context. Header metadata stays separate from identity. A canonical head names current branch membership, not finality.

Unresolved observations may carry only a number. They must resolve to a `ChainAnchor` before entering a canonical snapshot, projection, or downstream work item. Missing or contradictory block, transaction, and log identity is an ingress error, never a zero default or dropped row.

### D2 — Canonical ingress proves range and receipt completeness

Every historical projection unit includes the complete header chain for its range, including empty-log blocks, and validates parent linkage before its cursor can advance.

Filtered logs identify candidate Aave transactions. The updater then obtains complete receipts for those transactions. Receipt-only and trace-backed transactions are different types: call paths exist only on `TracedTransaction`. Call-level evidence is required initially for liquidations and later for any resolver whose ownership cannot be proved from receipts alone.

A trace join validates ordered log count, address, topics, and data against the complete receipt. A partial join cannot degrade into receipt-only interpretation. Tracing every transaction is not required.

### D3 — Block snapshots and Projection views are different read positions

An anchored block snapshot serves external chain reads at one exact block. It exposes no independent block argument and no `latest` mode. Live pool `StateView` retains ADR-041's cheap-read representation; no full chain-state clones or per-transaction RPC snapshots are introduced.

A post-block snapshot means post-block state. Its parent snapshot means pre-block state. A `ProjectionView` represents protocol state at an exact transaction/event position by reading preceding state plus ordered deltas. It advances forward without cloning the whole projection.

Every resolver read is classified:

- **Position-insensitive block fact:** later transactions in the block cannot change its interpretation, so the anchored post-block snapshot may serve it.
- **Position-sensitive state:** only the Projection view or explicit trace/EVM-replay evidence may serve it. An ordinary post-block `eth_call` is not evidence at an earlier transaction or log.

### D4 — Resolution and reduction interleave in canonical event order

For each protocol-defined event boundary, Aave performs:

```text
resolved input at current ProjectionView
    → pure reduction into CanonicalDelta
    → apply that delta to the ProjectionView
```

Resolution may use anchored chain evidence and read-side projection access outside the DB write transaction. Reduction performs no RPC or SQL. The transaction or chunk's resulting deltas remain atomic at persistence even though the Projection view advances at event granularity.

This preserves distinct positions within one transaction: a discount pre-pass may intentionally read transaction-start state while a later operation reads the relationship established by an earlier configuration event.

### D5 — Receipt ownership is total over the complete receipt

Every receipt log receives exactly one terminal classification: an Aave operation owner, a recognized non-operation role, or `OutOfScope`. Duplicate ownership and an unrecognized log where the protocol model expects coverage are refusals.

`OutOfScope` means deliberately irrelevant, not unknown. Liquidation collateral ownership uses `CallRef` and the trace-backed execution tree rather than user/asset similarity or a transaction-wide log window. Operation builders consume the ownership ledger; they do not rescan and pair the transaction independently.

### D6 — Entity generations are logical, branch-local versions

Proxy implementations, aToken/vToken relationships, discount tokens, GHO links, and endpoints have stable logical entity keys and generation-qualified versions. A generation is identified by its canonical activation event and carries the address, implementation/revision facts appropriate to that entity.

A replacement creates a new generation at its exact event position. It does not rewrite the interpretation of earlier operations. A same-height alternative branch activates a different version, not a globally decremented or reused generation. Epoch sequence and a contract's reported `REVISION()` remain different concepts.

Caches that depend on chain state or entity versions key on the full `ChainAnchor` and relevant generation rather than a block number or whichever entity is currently active.

### D7 — Canonical deltas are the only projection write product

Reduction produces typed, validated `CanonicalDelta`s. `AaveChunkEvent` is the starting vocabulary, migrated to carry exact position and generation references rather than adding a parallel write model.

Recovery applies retained validated deltas forward. Replay acceptance independently resolves and reduces canonical facts and requires byte-identical deltas. Neither path implements inverse protocol arithmetic or inverse SQL semantics.

`degenbot-aave` owns delta meaning, encoding, and the mapping to Aave rows. `degenbot-db` owns schema, constraints, and transactional storage primitives without depending back on Aave.

### D8 — Projection persistence is per market and hash-aware

Each market owns its Projection cursor, canonical delta journal, and relational checkpoints. A cursor identifies its terminal `ChainAnchor` and expected parent linkage. Current projection, journal entry, optional checkpoint, and cursor advance commit atomically against the expected parent.

One market's recovery cannot rewrite another market. Number-only `last_update_block` ceases to be the canonical completeness claim. Schema evolution follows the Rust-owned path; no Alembic branch or parallel compatibility schema is added.

### D9 — Reorgs restore a common ancestor and replay forward

A head mismatch triggers common-ancestor restoration, not an ordinary forward advance. Live work from the previous `Epoch` cannot publish, and persistence rejects an unexpected-parent commit.

Aave restores the nearest retained per-market checkpoint preceding the ancestor and applies retained deltas forward. Journals are mandatory at every committed projection unit. Checkpoint cadence is a measured storage/recovery trade-off and cannot affect correctness.

Retention aligns with the existing session reorg window. A deeper reorg requires explicit recovery from a known checkpoint or market reset rather than unbounded production trace retention or hidden repair. Existing pool `ReorgJournal`s remain the live-pool mechanism behind Chain-anchor-aware identity; one physical storage mechanism is not imposed across protocols.

### D10 — Live lineage changes facts, not solver policy

The shared head source carries `ChainAnchor`. `BlockContext`, cheap-read `StateView`, result batches, candidates, executor work, and telemetry retain the solve anchor through asynchronous hand-offs.

Executor work distinguishes `based_on` (the solve anchor) from `execution_block` (the consumer's execution clock). Existing solver eligibility and stale-batch handling remain unchanged. Submission targeting remains executor policy, not a block-pipeline decision.

Exact anchors travel in work values, structured logs, and span attributes. Prometheus never labels by block hash, transaction hash, call path, or entity generation.

### D11 — Failure classes stay distinct

The updater distinguishes retryable transport failure, unsupported/incomplete node evidence, deterministically invalid canonical evidence, protocol-resolution/reduction refusal, and post-application chain divergence. Only genuinely transient classes retry. Every failure rolls back the affected projection unit without cursor advancement; contradictory evidence is not retried as though transient.

### D12 — Rust owns every projection; existing module homes remain

- `degenbot-core`: chain coordinates and dependency-neutral identity.
- `degenbot-rpc`: live and recorded acquisition, exact reads, headers, receipts, traces, and cassette transport.
- `degenbot-aave`: ownership, resolution, reduction, delta meaning, and per-market projection driver.
- `degenbot-db`: constraints, cursors, journals, checkpoints, and atomic storage primitives.
- `degenbot-substrate`: `Epoch`, live `StateView`, and existing pool journals.

Python may request historical ranges by number and receive primitive projections. Only Rust constructs validated anchors, receipts, snapshots, resolved inputs, deltas, and cursors. No Python projection implementation is added.

### D13 — Golden replay and measured performance are acceptance surfaces

Existing scenarios and expected semantic outputs remain the oracle. Cassettes evolve to carry header chains, complete receipts, and selective traces. Legacy-format reading is confined to fixture migration and disappears after regeneration; production does not retain two formats or updaters.

Each supporting commit has a permanent red-first case. Additional cases cover chunk independence, same-transaction replacement, same-height reorg, mixed-anchor reads, duplicate ownership, incomplete evidence, future-state leakage, and checkpoint recovery.

The replay bench measures RPC rounds/bytes, trace acquisition, DB write-lock hold, replay runtime, and journal/checkpoint storage. Budgets are derived from the baseline rather than invented during scoping.

## Non-goals

- A generic event-sourcing framework, event bus, or new framework crate.
- Full chain-state clones per block or transaction, or universal transactional RPC snapshots.
- One async trait spanning RPC, SQL, and synchronous pool reads.
- Rewriting unrelated pool protocols or replacing their existing journals.
- Tracing every transaction indiscriminately.
- Solver eligibility, stale-batch, revalidation, or submission-targeting policy changes.
- A Python receipt parser, projection, generation model, or recovery implementation.
- Protocol semantics inferred solely from types: Aave math and behavior still require recorded receipts and on-chain differential evidence.

## Consequences

Mixed-anchor reads, ambiguous log ownership, unversioned replacement drift, and number-only cursor commits become construction or validation failures rather than consumer conventions. Aave resolution moves outside the DB write transaction, while the Projection view preserves read-your-own-writes. Recovery has one forward semantics, and live telemetry can name the exact chain position of asynchronous work.

The cost is additional header/receipt evidence, selective traces, and bounded journal/checkpoint storage. Those costs are measured before optimization. Correctness does not depend on a particular checkpoint cadence or trace-retention policy.

## Alternatives considered

- **Block-only snapshots.** Rejected: post-block RPC state cannot represent two replacements within one block or changes within one transaction.
- **Transactional RPC snapshots for all state.** Rejected as unnecessary machinery: protocol projections supply the needed positions; raw position-sensitive EVM state uses explicit trace/replay evidence only when required.
- **Inverse deltas or inverse SQL.** Rejected: inverse protocol semantics would duplicate the forward model and its failure modes.
- **Whole-database checkpoints.** Rejected: an Aave market is the projection owner; unrelated markets must not share its recovery.
- **Permanent hash-less degrade.** Rejected for canonical work: it preserves same-height reorg ambiguity.
- **Operation-family parallel implementations.** Rejected: migration replaces internal seams across operation families instead of maintaining two semantic pipelines.

## Migration and dependency order

1. Consolidate chain coordinates and exact-head validation.
2. Add anchored header/read acquisition and evolve replay evidence.
3. In parallel, propagate live anchors and build complete Aave receipt ownership.
4. Introduce Projection views and interleaved resolution/reduction across all Aave operation families.
5. Add per-market cursors, journals, checkpoints, and schema constraints.
6. Add bounded checkpoint replay and common-ancestor recovery.
7. Delete superseded seams and run semantic, performance, scoped live-replay, and full `just test` acceptance.

The detailed implementation graph lives in ergo epic `YMSQIZ`. Each production change replaces one internal seam; replay-only prototypes are permitted before that seam's cutover, not as a second long-lived updater.

## Invariant ownership

| Layer | Owned invariants |
|---|---|
| Types | Exact identities, snapshot pinning, read-position classification, total receipt ownership, resolved relationships, generation-qualified references, valid delta construction |
| Database | Block/transaction/log referential identity, branch-position uniqueness, generation-specific relationships, expected-parent cursors, atomic projection/journal/checkpoint commits |
| Replay acceptance | Protocol semantics, chunk independence, same-block ordering, reorg recovery, future-state leakage, byte-identical deltas |

# Appendix A — Current type inventory and admitted ambiguity

| Existing surface | Current fact | Ambiguity the target removes |
|---|---|---|
| `degenbot-core::canonical_head::CanonicalHead` | Number, hash, parent hash | No chain ID; not yet the shared identity across Head, Epoch, simulation, DB, and telemetry |
| `degenbot-substrate::epoch::{Epoch, BlockContext}` | Block number plus rewind sequence and metadata | Sequence identifies a local timeline, not chain content |
| `degenbot-rpc::provider::BlockLineage` | Number, hash, parent hash | Duplicate block-lineage type outside the core identity |
| `degenbot-simulation::sim::storage_memo::ReadAnchor` | Number plus optional hash | Hash-less mode cannot distinguish a same-height branch |
| `degenbot-eventhub::head::Head` | Number plus optional metadata | No block or parent hash |
| `degenbot-core::block_notification::BlockNotification` | Number and execution metadata | Consumer clock has no exact anchor |
| `degenbot-bot::arb_engine::ResultBatch` | `solve_block`, metadata, results, telemetry lineage | Solve identity is number-only and separate from the session clock |
| `degenbot-batch-executor::executor::BatchWork` | `current_block`, execution facts, `BlockLineage` | Execution clock and solve identity can be confused; target names them separately without changing policy |
| `degenbot-bot::telemetry::BlockLineage` | Solve number plus optional exporter context | Exact telemetry ancestry is fixed, but chain identity remains number-only |
| Aave `ChunkSpan` / `RevisionMemo` | Numeric span and `(implementation, selector, block)` memo key | No branch hash in the read key |
| `aave_fetch::sort_logs_by_block_and_index` / merge dedup | `(block_number, log_index)` ordering and identity | No chain, block hash, transaction, or call identity; missing fields receive sentinel ordering |
| `run::process::TxGroup` | Transaction hash, numeric block, borrowed logs | Missing transaction hash is dropped; missing block number becomes zero |
| `ScaledTokenEvent` / `Operation` | Transaction-local `log_index` and borrowed raw logs | Public operation shapes do not carry exact chain or call identity |
| `EventOwnership` | One classification per decoded scaled-token log | Not a total receipt ledger; keyed by `u64`, not `LogRef` |
| `LiquidationWindow` | Log range after the previous `LiquidationCall` | Proves a window heuristic, not the actual call path |
| `AaveChunkEvent` | Resolved IDs and precomputed writes; no RPC in its apply core | No chain position, activation provenance, or branch-local generation identity |
| `ChunkSubstrate` | Chunk prefetch plus dirty/write overlay | Strong Projection-view seed, but currently coupled to the chunk DB transaction |
| `AaveMarketRow.last_update_block` | Optional numeric cursor | Cannot assert which branch is complete or reject an unexpected-parent commit |
| Pool `ReorgJournal` | Existing forward/restore journal by block | Remains the pool mechanism; needs exact anchor context rather than replacement |

# Appendix B — Aave provider-read and DB-lineage census

## Run and activation reads

| Site | Current anchor and role | Target disposition |
|---|---|---|
| `run/activate.rs::fetch_market_id` | `eth_call(..., None)` at latest for market name | Resolve and record an exact activation anchor; prove the fact position-insensitive or use an explicitly pinned read |
| `run/activate.rs::fetch_activation_metadata` | `get_block_number` then ERC-20 metadata at that preview head | Same: exact resolved head, not an ambient latest read |
| `run.rs::run_aave_update_driver` | `get_block_number` only when `to_block` is absent | Range selection only; resolve the selected terminal header into a `ChainAnchor` |
| `run/fetch.rs::bootstrap_pool_contracts` | Address-provider logs over a bootstrap range | Validate complete header/log identity; unrelated proxy roles are explicitly out of scope |
| `run/fetch.rs` bootstrap revision reads | Each `ProxyCreated` log's own block since `4a31d79d6` | Anchored snapshot at that exact event block and branch |
| `run.rs` main and supplemental `eth_getLogs` passes | Chunk range, plus newly discovered tokens, discount generations, and replacement endpoints | Replace repeated heuristic coverage with generation-aware acquisition over the same complete header chain and full candidate receipts |

## Per-transaction resolution and verification reads

| Site | Current anchor and role | Target disposition |
|---|---|---|
| `config_dispatch/context.rs::read_revision*` | Current transaction's numeric block; memo includes implementation, selector, number | Full Chain anchor and entity generation; classify implementation revision as position-insensitive only with evidence |
| `config_dispatch/asset_config.rs` | `getConfiguration`, EIP-1967 slots, token metadata, and oracle source at current transaction block | Split position-insensitive implementation facts from position-sensitive proxy/asset state; no unproved post-block reads for earlier positions |
| `config_dispatch/discount.rs` | `getDiscountPercent` at transaction block; `balanceOf` at `block_number - 1` | Explicit parent Chain anchor for pre-block balances; Projection view for intra-block state; post-apply refresh resolves the generation at its event position |
| `config_dispatch/context.rs::dispatch_transaction` | Transaction-start GHO read, discount pre-pass, config apply, post-config GHO read | Event-interleaved resolve/reduce against Projection views preserves both intentional positions |
| `verify.rs::verify_*` | `scaledBalanceOf` and `getPreviousIndex` at numeric `chunk_end` | Exact terminal anchor; verification remains a chain-truth check, not a second state implementation |

`run/apply.rs` currently performs no RPC and no ABI decode. `AaveChunkEvent` already carries resolved IDs and precomputed balance deltas to that apply core. This is the primary extraction point for Canonical deltas rather than a reason to invent another write pipeline.

## Current DB transaction shape

```text
number-only market cursor
  → optional current-head range selection
  → bootstrap and chunk RPC outside the DB lock
  → open chunk DB transaction
      → ChunkSubstrate prefetch
      → per transaction
          → GHO/revision/discount RPC
          → config resolution and immediate apply
          → operation parsing against connection + overlay
          → operation events buffered/applied
      → touched/full verification RPC at chunk_end
      → numeric cursor advance and commit
```

The write transaction therefore covers resolution and verification RPC as well as writes. `ChunkSubstrate` correctly provides read-your-own-writes through its overlay, but it lives inside that transaction. The target moves acquisition and resolution outside the write transaction, retains the overlay as the Projection view, and shortens the DB transaction to validated deltas, journal/checkpoint evidence, verification policy, and cursor commit.

Aave has no equivalent of the pool branch journal: its durable recovery is numeric resume or explicit market reset. The per-market hash cursor, journal, and checkpoint fill that gap.

# Appendix C — Receipt identity and ownership lineage

```text
Alloy Log with optional identities
  → numeric (block_number, log_index) sort and dedup
  → transaction-hash grouping, with missing hashes dropped
  → TxGroup { numeric block, logs }
  → config dispatch
  → ScaledTokenEvent { transaction-local log_index, raw Log }
  → EventOwnership over decoded scaled logs
  → operation builders plus assigned HashSet<u64>
  → Operation with borrowed logs
  → AaveChunkEvent without source position
  → numeric-cursor relational apply
```

The target lineage is instead:

```text
complete header chain + complete receipt (+ selective trace)
  → validated ChainAnchor / TxRef / LogRef / CallRef
  → total ReceiptOwnership
  → event-position resolve → pure reduce → ProjectionView advance
  → CanonicalDelta with exact source and entity generation
  → atomic per-market journal + projection + cursor
```

# Appendix D — Live block and work lineage

```text
accepted header
  → shared eventhub Head { number, optional metadata }
  → StageMachine Epoch { number, rewind seq }
  → BlockContext { Epoch, metadata }
  → ResultBatch { solve_block, metadata, telemetry BlockLineage }
  → Session BlockNotification { number, metadata } (independent consumer clock)
  → BatchWork { current_block, execution facts, telemetry BlockLineage }
  → simulation ReadAnchor { number, optional hash }
  → submission and spans
```

Separately, core `CanonicalHead` and the BAL path already verify number/hash/parent lineage. This proves the required header facts are available; they are not yet propagated through the primary work types.

The target carries `ChainAnchor` with the accepted head and solve product, retains the independent execution clock under that name, and leaves stale-batch behavior unchanged. `BlockLineage` continues to carry exporter context, now pointing at the same exact solve anchor.

# Appendix E — Commit-to-code evidence

| Commit | Evidence and root mechanism | Preventing invariant |
|---|---|---|
| `035ce3da2` — ignore unrelated proxy deployments | `config_dispatch/mod.rs` had treated every unknown `ProxyCreated` id as fatal; `aave_market_reset.rs` added the incentives-proxy case. Event presence was confused with a tracked role. | Role-scoped resolution and explicit `OutOfScope`; only an unrecognized log where the model expects coverage refuses. |
| `4a31d79d6` — read bootstrap revisions at event blocks | `run/fetch.rs` previously passed `from_block` to every proxy revision read; the fixture split Pool and Configurator events across blocks. | Every read is anchored to the exact event block and branch, never the chunk start. |
| `c4dadacf9` — track discount token generations | `run.rs` fetched only the chunk-start token and later added every uncovered generation; `write/gho.rs` invalidated balances denominated in the replaced token. | Generation-qualified token state and caches; post-replacement reads resolve the generation active at that position. |
| `21c03b43a` — track endpoint replacements during updates | `build_fetch_spec` froze endpoints; supplemental passes were needed for mid-chunk replacements and DB refreshes for later chunks. | Endpoint generations plus generation-aware acquisition at exact positions, with no run-start identity snapshot. |
| `d617fdce8` — resolve GHO links before operation parsing | `dispatch_transaction` needed a post-config GHO read for a same-transaction `ReserveInitialized` → Mint while the discount pre-pass intentionally remained transaction-start. | Event-interleaved resolution/reduction over Projection views makes both positions explicit. |
| `f801633f4` — synchronize bad-debt running state | A `DebtPositionReset` reached the output buffer while a later same-transaction GHO event read a stale running-state tuple. `emit_bad_debt_reset` now updates both. | One Canonical delta and Projection-view advance updates reset and subsequent read state atomically. |
| `3e08816b6` — a liquidation owns its own collateral sub-call | `LiquidationWindow` was added after same-user/same-collateral liquidations shared legs; the real 218-log receipt regression is in `operations_parser/receipt_repro.rs`. | Trace-backed `CallRef` ownership binds every collateral log to its actual liquidation call. |
| `4a431b43f` — assign transfer event ownership once | Multiple builders re-derived ERC20/BalanceTransfer and zero-companion pairing; `EventOwnership` centralized classification and typed duplicate/ignore roles. | Total ReceiptOwnership over `LogRef`, assigned once before operation reduction. |
| `f6098bb6b` — enforce Aave token relationships | Reads previously used zero-address fallbacks or dropped unresolved joins; required underlying/vToken relationships now produce `MissingRow`. | Generation-qualified database relationships and typed missing-relationship refusals; absent-before-initialization remains distinct from corruption. |
| `8cdab21b0` — reject logs without block numbers | Backfill had skipped missing numbers or supplied chunk/zero defaults; whole-batch validation now precedes any apply. | Canonical ingress requires complete identity for every log before any projection or cursor movement. |
| `c0f273201` — share the pump head source | Hosted sources and the pump could own competing head watches; the fix declared one pump-owned source and refused an unregistered boot. | One typed head source carrying one exact `ChainAnchor` to every consumer. |
| `cf7e9cfae` — carry exact block lineage | A global number-keyed span map and nearest-previous fallback could parent work to another block. `BlockLineage` now travels with the batch through fanout and submission. | Solve anchor plus exporter context travels as a work value through every asynchronous hand-off. |

# Appendix F — Live capability probes

Scoping probed the available archive node, identified as `reth/v2.11.0-b9abbcc/x86_64-unknown-linux-gnu`:

- `eth_call` with `{ blockHash, requireCanonical: true }` at the captured liquidation block succeeded.
- `debug_traceTransaction` with `callTracer` and `withLog: true` succeeded for the captured liquidation transaction.
- Recursive trace traversal yielded 218 logs, matching the 218 receipt logs in `tests/fixtures/aave_receipts/liquidation_collateral_ownership_26458346.json`.

These probes establish that the two enabling capabilities exist for the highest-risk captured case. They do not replace the implementation's receipt/trace join tests, per-method capability checks, or broader corpus evidence.
