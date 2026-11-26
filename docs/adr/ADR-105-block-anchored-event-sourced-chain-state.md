# ADR-105: Block-anchored event-sourced chain state

**Status: accepted (implemented).** (2026-11-23; ergo epic `YMSQIZ`, scoping task `6GTIG7`, certification task `ONNSDV`; post-cutover cleanup and hardening epic `FUXOFN`, certification task `4C2R5N`.) The implemented shape, the seams retired by the cutover, the deliberate number/tag lanes, and the deferred choices are stated in [Implemented shape and seam disposition](#implemented-shape-and-seam-disposition-2026-11-23).

Builds on [ADR-041](ADR-041-block-epoch-pipeline.md) (one stage machine over cheap reads), [ADR-068](ADR-068-updater-golden-capture-replay.md) (recorded updater coherence), [ADR-070](ADR-070-updaters-are-the-sole-db-writers.md) (updater-owned writes), and [ADR-104](ADR-104-typed-eventhub-intake-and-engine-session-stream.md) (typed head intake). Those decisions remain foundations; this ADR supplies the chain identity, position, receipt ownership, and projection persistence they do not currently unify.

## Context

Twelve fixes during 2026-11-15 through 2026-11-22 addressed recurring mechanisms rather than isolated arithmetic mistakes: reads at the wrong block, replacement entities without branch-local generations, transaction-wide ownership scans, late relationship resolution, divergent running state, missing log identity, competing head sources, and telemetry whose ancestry was reconstructed at consumers. Appendix E maps every fix to the invariant that now prevents its mechanism.

The current tree already has useful pieces: `Epoch` and `BlockContext`, core `CanonicalHead`, RPC `BlockLineage`, simulation `ReadAnchor`, `EventOwnership`, `AaveChunkEvent`, `ChunkSubstrate`, pool `ReorgJournal`s, typed eventhub heads, and substantial cassette/receipt goldens. They do not yet form one contract. In particular, Aave persists a number-only cursor and performs resolution RPC inside its DB write transaction; live work carries several independent block-number fields; exact hashes exist only on some side paths.

_Scoping-time state, recorded before the cutover: the current shape is [Implemented shape and seam disposition](#implemented-shape-and-seam-disposition-2026-11-23) below._

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

The pre-commit verification gates follow the same rule: the chain truth they compare against is acquired during resolution, in the same batch shapes the gates already used, and the transaction only compares that truth against the rows it is about to commit. A verification-enabled chunk therefore holds the write lock across no read either, and a divergence still rolls the chunk back.

Every resolver read is classified:

- **Position-insensitive block fact:** later transactions in the block cannot change its interpretation, so the anchored post-block snapshot may serve it.
- **Position-sensitive state:** only the Projection view or explicit trace/EVM-replay evidence may serve it. An ordinary post-block `eth_call` is not evidence at an earlier transaction or log.

Simulation reads state the same position on the wire. The solve anchor is the read position: an identified anchor's account, storage, code, balance, and nonce reads name its hash with canonical membership required (the one Exact read pin), and an unresolved anchor is an explicit ingress observation whose reads state its height — a resolved identity never converts back to a height. Blockhash lookup is position-sensitive in its own right: a requested number resolves against the solve anchor's proven header ancestry, only the EVM's own window rule (measured from the executing block) may answer zero, and missing or contradictory ancestry is a read failure rather than an unrelated canonical block.

### D4 — Resolution and reduction interleave in canonical event order

For each protocol-defined event boundary, Aave performs:

```text
resolved input at current ProjectionView
    → pure reduction into CanonicalDelta
    → apply that delta to the ProjectionView
```

Resolution may use anchored chain evidence and read-side projection access outside the DB write transaction. Its relational starting point is one synchronously loaded chunk seed holding every fact resolution may ask for; resolution itself receives no connection, performs no SQL, and refuses an unanticipated dependency instead of issuing a lazy query. Reduction performs no RPC or SQL and is exposed as one reducer interface over the whole transaction. The transaction or chunk's resulting deltas remain atomic at persistence even though the Projection view advances at event granularity.

This preserves distinct positions within one transaction: a discount pre-pass may intentionally read transaction-start state while a later operation reads the relationship established by an earlier configuration event.

Identity creation follows the same split. The row identities a delta names — users, positions, ERC-20/asset rows, contract and market rows — are resolved during that out-of-transaction resolution phase, idempotently, so every delta reaching persistence carries settled ids and the write transaction performs no identity lookup of its own. A chunk that rolls back therefore leaves its resolution-created identity rows behind while the cursor does not move; the rows are inert (nothing references them until the retried chunk commits), the retry's lookups return the same ids, and no duplicate row can appear. Retiring such rows is not a recovery obligation.

### D5 — Receipt ownership is total over the complete receipt

Every receipt log receives exactly one terminal classification: an Aave operation owner, a recognized non-operation role, or `OutOfScope`. Duplicate ownership and an unrecognized log where the protocol model expects coverage are refusals.

`OutOfScope` means deliberately irrelevant, not unknown. Liquidation collateral ownership uses `CallRef` and the trace-backed execution tree rather than user/asset similarity or a transaction-wide log window. Operation builders consume the ownership ledger; they do not rescan and pair the transaction independently.

### D6 — Entity generations are logical, branch-local versions

Proxy implementations, aToken/vToken relationships, discount tokens, GHO links, and endpoints have stable logical entity keys and generation-qualified versions. A generation is identified by its canonical activation event and carries the address, implementation/revision facts appropriate to that entity.

A replacement creates a new generation at its exact event position. It does not rewrite the interpretation of earlier operations. A same-height alternative branch activates a different version, not a globally decremented or reused generation. Epoch sequence and a contract's reported `REVISION()` remain different concepts.

Caches that depend on chain state or entity versions key on the full `ChainAnchor` and relevant generation rather than a block number or whichever entity is currently active.

### D7 — Canonical deltas are the only projection write product

Reduction produces typed, validated `CanonicalDelta`s. `AaveChunkEvent` was the starting vocabulary; it migrated into the delta vocabulary's configuration family (`ConfigurationEffect`), whose deltas carry exact position and generation references, and its separate apply entrypoint retired into the one delta writer. One vocabulary, one write product.

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
- `degenbot-decoders`: neutral Aave event-log topic and payload recognition — the signature, the indexed fields, and every decoded field, for both operation events and non-operation reports. Emitter roles, operation ownership, whether a market tracks an emitter, and which revision introduced a report stay with `degenbot-aave`; the decoder leaf never depends on it.
- `degenbot-aave`: ownership, resolution, reduction, delta meaning, and per-market projection driver.
- `degenbot-db`: constraints, cursors, journals, checkpoints, and atomic storage primitives.
- `degenbot-substrate`: `Epoch`, live `StateView`, and existing pool journals.

Python may request historical ranges by number and receive primitive projections. Only Rust constructs validated anchors, receipts, snapshots, resolved inputs, deltas, and cursors. No Python projection implementation is added.

### D13 — Golden replay and measured performance are acceptance surfaces

Existing scenarios and expected semantic outputs remain the oracle. Cassettes evolve to carry header chains, complete receipts, and selective traces. Legacy-format reading is confined to fixture migration and disappears after regeneration; production does not retain two formats or updaters.

Each supporting commit has a permanent red-first case. Additional cases cover chunk independence, same-transaction replacement, same-height reorg, mixed-anchor reads, duplicate ownership, incomplete evidence, future-state leakage, and checkpoint recovery.

The replay bench measures RPC rounds/bytes, trace acquisition, DB write-lock hold, replay runtime, and journal/checkpoint storage. Budgets are derived from the baseline rather than invented during scoping.

The deterministic counters those suites and the bench assert live in one committed, machine-readable replay-surface authority, and the run under measurement is never their source.

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

## Implemented shape and seam disposition (2026-11-23)

The cutover landed; this section states what the tree now carries, decision by
decision, so the ADR can be read against code instead of intent.

- **D1** — `degenbot_core::canonical_head` owns `ChainAnchor` (chain id, number,
  hash, parent hash; `ChainAnchorError` refuses a zero or repeated parent) with
  `TxRef`, `LogRef`, and `CallRef` qualified by it. RPC `get_block_lineage_at` and
  `resolve_terminal_anchor` return the same identity; the provider's duplicate
  `BlockLineage` is gone. Simulation's `ReadAnchor` and bot telemetry's
  `BlockLineage` carry the resolved anchor and key on it.
- **D2** — `degenbot_rpc::chain_evidence` acquires a complete `HeaderChain`
  (`from_anchors` refuses gaps, broken links, and same-height substitutions),
  `ProjectionRange` places logs by their exact identity, `degenbot_rpc::tx_evidence`
  binds complete receipts and joins selective traces (`CallRef` paths) to them,
  and `degenbot_aave::updater::evidence` refuses a filtered log that disagrees
  with its receipt in any identity fact.
- **D3** — `BlockReads` is the hash-pinned read view (`{blockHash,
  requireCanonical}`, `ExactReadError` separating an unsupported capability from a
  failed read) and the one production read view. The chunk's SQLite write
  transaction covers no acquisition: `ChunkSeed::load` reads the chunk's complete
  relational starting point under the caller's connection guard,
  `ChunkResolver::resolve` runs with no connection at all, and `persist_chunk`
  opens the transaction — so verification truth is acquired before the lock and
  only compared inside it.
- **D4** — `run/resolver.rs` interleaves resolve → reduce → advance against
  `ProjectionView`, and the row identities a delta names are interned
  synchronously by `ChunkSeed::load` before the chunk's transaction opens, so
  resolution issues no SQL and persistence performs no identity lookup.
- **D5** — `operations_parser::ownership::ReceiptOwnership` is the total ledger:
  one terminal slot per receipt log (`LogOwner::{Operation, Role, OutOfScope}`),
  with `DuplicateOwnership`, `NonOperationClaim`, `UncoveredOperationLog`,
  `UnrecognizedLog`, and `LogOutsideReceipt` as refusals. A liquidation's
  collateral legs are owned by `CallOwnership` over the trace tree;
  `ParseError::MissingCallEvidence` refuses a receipt-only parse.
- **D6** — `GenerationEntity`/`GenerationRef` (activation-keyed, branch-local) seed
  from the chunk substrate and are journaled and checkpointed with the deltas; a
  same-block replacement activates a new generation rather than mutating one.
- **D7** — `CanonicalDelta` is the only write product, persisted by
  `apply_canonical_deltas_on_conn`. `AaveChunkEvent` and its per-family apply
  entrypoints are gone.
- **D8** — `aave_projection_cursors` (`ProjectionCursor{position, journal_sequence,
  range_end_block}`) plus the delta journal and relational checkpoints commit
  atomically under `CursorAnchor::{First, At, Unchanged}`, refusing an unexpected
  parent.
- **D9** — `run/recovery.rs` compares the cursor anchor's hash at its own height
  and, on a mismatch, restores the nearest retained per-market checkpoint and
  replays retained deltas forward; a deeper reorg names the market-reset path.
- **D10** — `SimulationBlock::based_on` is the solve anchor carried into dispatch,
  simulation, and the batch executor; telemetry records `chain_id`,
  `block_hash`, and `parent_block_hash` as span attributes only.
- **D11** — `LogIdentityRefusal`, `OwnershipRefusal`, `RecoveryError`,
  `ExactReadError`, `HeaderRefusal`, `ProjectionRefusal`, `BlockHashRefusal`,
  `IncompleteChunkSeed`, `ActivationMetadataError`, and `VerificationEvidenceError`
  keep the failure classes distinct.
- **D12** — the module homes are as listed; no Python projection implementation
  exists, and the primitive projections and generated stubs are the only Python
  surface. `degenbot_decoders::aave_event_decoder` decodes the operation events
  and `degenbot_decoders::aave_report_decoder` decodes the non-operation reports
  by signature alone: recognition is revision- and emitter-agnostic (a later
  compatible revision carrying a known report decodes identically), and an
  unknown topic fails closed at that neutral boundary instead of becoming a
  catch-all variant.
- **D13** — the replay bench and the scoped suites are the acceptance surfaces;
  their deterministic counters have ONE committed home (`replay_surface` below), and
  Appendix E's cases exist as named tests.

### Seams retired by this cutover

Every symbol below had zero callers in the workspace (compiler- and
grep-verified) because the canonical pipeline superseded it; deleting them is the
retirement (ADR-087), not a compatibility shim.

| retired | superseded by |
|---|---|
| `DegenbotDb::apply_scaled_token_mint` / `apply_scaled_token_burn` / `apply_scaled_token_transfer` and their `_on_conn` forms, plus the private read-then-write shell they shared | `apply_scaled_token_balance_delta_with_current_on_conn` (the chunk substrate supplies the current state; the `_burnScaled` clamp and the `last_index` max-with-prev rule live in `finish_scaled_token_balance_state`, and the clamp tests now drive that path) |
| `DegenbotDb::apply_reserve_data_updated` (+ `_on_conn`) | the chunk substrate's batched `flush_reserve_data_updates` |
| `DegenbotDb::fetch_user_gho_discount_by_address_on_conn` | the discount pre-pass reading the substrate/Projection view |
| `DegenbotDb::lookup_position_by_user_asset_on_conn` | the generation-aware substrate position lookups |
| `verify_stk_aave_balances_on_conn` / `verify_gho_discount_amounts_on_conn` | the D3 split: `acquire_verification_truth` in resolution, `compare_verification_truth_on_conn` in the transaction |
| the last doc prose naming `AaveChunkEvent` | the delta and decoded-event vocabulary |

### Retained, with the reason it is not a retirement candidate

- **Ingress-only number forms** — `sim::ReadAnchor::unresolved` and telemetry
  `BlockLineage::unattached` remain for an observation that resolved no header (a
  cold-start sweep, a probe, a lineage with no captured exporter context). ADR-105
  D1 admits a number-only *observation* and refuses it only at the canonical
  boundary; both forms fence rather than fabricate an anchor (a hashed anchor and
  a number-only one never share a memo).
- **Transaction-local working set** — the parser's assigned-log set and
  `EventOwnership` remain beside `ReceiptOwnership`. They carry information the
  ledger deliberately does not: logs a builder consumed as scaffolding without
  retaining them in the operation's fields, and the mirror/companion roles the
  ledger keeps a *role* unless an operation claims them (asserted on the real
  218-log receipt). A receipt *is* one transaction, qualified by its `TxRef`, so
  the key has no broader scope to widen to.
- **`aave_v3_markets.last_update_block`** — retired as the *completeness* claim
  (D8), which no code reads it as any more. It survives as the numeric *range*
  claim, and a chunk that journals nothing has no position to journal, so the
  exact cursor cannot advance for it.

### Deviations this ADR does not yet claim

None open at 2026-11-23. The two that stood at the ONNSDV cutover — height-pinned
configuration/verification reads (closed by the exact-read adoption below) and
driver prose that still described RPC under the chunk transaction (closed by
stating the resolve/persist split) — are retired. The cleanup epic's last
recorded non-claim, the replay-surface expectation consolidation, is closed by
[Replay-surface authority](#replay-surface-authority-2026-11-23).

The read lanes that intentionally stay number- or tag-addressed, with the reason
each is not a parallel path, are enumerated under
[Deliberate number/tag lanes](#deliberate-numbertag-lanes-not-exact-reads). None
of them is a production Aave read or an identified simulation read, and none is
described as an Exact read.

### Exact reads landed (2026-11-23)

Every production configuration, revision, discount, metadata, stkAAVE, and
verification read is hash-pinned; the former "height-pinned dispatch and
verification reads" deviation is retired.

- `BlockReads` is the one production read view: `call`, `code_at`, `storage_at`,
  `balance_at`, and `batch` (ONE Multicall3 `aggregate3` over the view's own
  anchor, the same envelope count the number-pinned batch had). It also
  implements `ContractRead`, whose block lane is a cross-check rather than a pin,
  so the ERC-20 read family runs over it unchanged — one implementation, two pin
  policies.
- A node that rejects the EIP-1898 shape is `ExactReadError::Unsupported` (D11):
  no path degrades to a number-addressed read, and the classification reads the
  node's shape markers only inside the `-32000` family, so a replay or harness
  gap in the JSON-RPC reserved band cannot masquerade as a missing capability.
- `resolve_parent_anchor` names the parent read position of an intentional
  pre-block observation (the stkAAVE balance seed): one header read, refused
  unless the served parent hashes to the child's own `parent_hash`. The seed
  therefore reads at an exact anchor, never at a number-only tag.
- `RevisionMemo` is keyed by `ChainAnchor`, so a same-height branch is a
  different key and no hit can cross an abandoned hash.
- Prefetch deltas this adoption adds (all outside the chunk's write transaction):
  one header read per non-final chunk (its verification gates pin the chunk's own
  terminal anchor), one per distinct block whose pre-block stkAAVE balance is
  read, and one per `ProxyCreated` block in the cold-boot bootstrap. The chunk
  transaction still covers no acquisition.

### BlockId read transports (2026-11-23)

The account, storage, and code reads state their read position as a `BlockId`
instead of an `Option<u64>`, so the exact and the height lanes are one
construction per operation and `BlockReads` builds no read params of its own.

- Public signatures (mechanical and source-breaking at the call site — a
  `BlockId` replaced the number parameter, so an `Option<u64>` argument becomes
  `map(BlockId::number)` and a bare `u64` becomes `BlockId::number(n)`):
  `get_code(&Address, Option<BlockId>)`, `get_balance(&Address,
  Option<BlockId>)`, `get_transaction_count(&Address, Option<BlockId>)`,
  `get_storage_at(&Address, U256, Option<BlockId>)`,
  `get_account_at(&Address, BlockId)`, `get_accounts_batch_at(&[Address],
  BlockId)`, `get_storage_batch_results_at(&[(Address, U256)], BlockId)`, and
  `get_storage_batch_at(&[(Address, U256)], BlockId)`.
- Wire behavior is unchanged in both existing lanes: `None` states the alloy
  builder's node-default `latest` tag (what the previous `None` branch issued),
  and `BlockId::Number` states the same height or tag word as before.
  `BlockId::Hash` states `{blockHash, requireCanonical: true}` — the EIP-1898
  identifier `BlockReads` pins — with the address in the corpus's lowercase
  form and the slot a minimal hex quantity, so a pinned read and its height
  twin differ in the identifier alone.
- `BlockReads::code_at`, `storage_at`, and `balance_at` hand that identifier to
  the transport and keep only the classification
  (`rejects_hash_pinned_shape` → `ExactReadError::Unsupported`, D11). No path
  degrades to a number-addressed read, and there is no parallel exact-read
  family left to drift from the height lane.
- `eth_call` is deliberately NOT on the `BlockId` lane yet: the recorded corpus
  carries `data` on the hash-pinned lane and `input` on the number lanes (the
  typed request builder emits `input`), so the pinned call and the typed call
  are two wire forms of one operation today. Moving `eth_call` onto the typed
  builder rewrites the recorded pinned corpus, which is its own change, not a
  mechanical migration.
- Batch member alignment and retries are untouched: members are the serial
  reads' own `(method, params)` pairs, returned one outcome per input identity
  in input order, and the whole round remains one "call" for the retry budget.
- Python surfaces keep `block_number: int | None`; the adapters convert at the
  `degenbot-rpc` boundary, so the generated `degenbot/_ffi` stubs are unchanged
  and their drift gate stays green.

### Simulation reads pinned by anchor (2026-11-23)

The fan-out's simulation stack now takes its read position from the solve
anchor end to end, so the value caches and the wire carry ONE identity.

- `Evm2BlockSimHandle::build` / `build_with_view` take a `ReadAnchor` in
  place of the block number. The block env's `number` and the scratch frame's
  next height are derived from it (`ReadAnchor::number`), so a height is never
  the read identity again. `AlloyAsyncDb::new` and
  `WarmCodeCache::with_owner` take the same anchor, and every read it issues
  passes through `ReadAnchor::block_id()`: an identified anchor states
  `{ blockHash, requireCanonical: true }` (`eth_getStorageAt`,
  `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode`, and the
  single- and multi-account batches), an unresolved anchor states its height.
- `ReadHub::drain_round` issues both batch kinds at `block_id()` — the hub's
  single-flight, coalescing, member counts, per-member failure attribution, and
  retry budget are unchanged; only the identifier's construction moved.
- `prefetch_paths_into_memo` / `prefetch_into_memo` / `prewarm_plan` take the
  anchor. The prewarm's ONE batch pins the same identifier the fallback issues,
  and the capture-learned decay is measured on `number()` because the hot-slot
  store carries identities, never values.
- `WarmCodeCache` entries are tagged with the anchor they loaded at. Freshness
  is the AGE (`anchor.number() - loaded.number()`) against the TTL, plus a
  same-height fence: at one height the entry is served only when its identity
  matches the read anchor's (equal hash for two identified observations,
  height-only for two unresolved ones). A NEGATIVE age is stale by construction,
  so a read that moves backward never reuses a row it read in the future it
  moved back from; a replacement block at the same height is likewise a cold
  position — it reads the node, never the abandoned branch's row — while the
  ordinary forward advance keeps the cross-block reuse the layer exists for.
- Dispatch threads the work item's solve anchor into the prefetch batch and the
  handle build (the delta/head generation already keyed on it), so the BAL
  selection, the memo, the hub, and the wire all name one block. The block
  environment, the scratch frame's `number + 1`, telemetry, and the solver's
  promotion stay number facts, unchanged.
- Callers that hold no resolved header pass `ReadAnchor::unresolved`: the
  Python inline-sim request (`InlineSimRequest` is number-only, so no
  fabricated hash is minted), the dry-run frame pipeline's live fork handles,
  and the live-RPC lab/bench harnesses that pin a height they never resolved.
  Those paths keep their previous wire behavior exactly. The scripted chain
  stubs the delta/retention suites drive honour the same rule on their side: a
  pin at a hash no recorded header or registration names is a hard RPC failure,
  so a stub's default word can never stand in for a block it cannot identify.
- Replay evidence gained the bridge this needs. `CassetteReplayTransport`
  indexes the ledger's own recorded `eth_getBlockByNumber` answers by hash, so a
  hash-pinned read that misses its exact key resolves to the height a recorded
  header names and is answered by that height's entry — the same read position,
  proven rather than assumed. A hash no recorded header names is a fixture gap
  (missing evidence, not a missing capability), a pin that does not require
  canonicality is refused (height evidence cannot identify a non-canonical
  branch), and recorded headers that contradict each other — one hash at two
  heights, or one height holding two hashes — are refused rather than resolved.
  `ServedSnapshot::hash_resolved` counts how much of a replayed read surface
  still depends on the bridge, which is the signal for re-recording a fixture at
  the exact hash. Only the five read methods take the bridge —
  `eth_getStorageAt`, `eth_getBalance`, `eth_getTransactionCount`,
  `eth_getCode`, and `eth_call` — because an account hydration is ONE batch of
  balance, nonce, and code whose members must answer together (the nonce is a
  served and bridged method, never a method-not-found in the middle of a
  hydration); every other method keeps its own key.
- Cassette provenance is unchanged: the committed corpus in
  `tests/fixtures/cassettes/` was not re-recorded for this change. Its read
  surface is the Aave updater's already hash-pinned reads (recorded as such) and
  log/header requests; no committed cassette carries a simulation read pinned by
  height, so no recorded entry became wrong or redundant. Structural comparison
  before/after: identical entry count, method histogram, and byte digest per
  file (the corpus drift gate `verify_cassette_bytes` still passes); semantic
  comparison: every replayed scenario's answers are unchanged, and the new
  replay cases prove a hash pin resolves to the recorded height's answer.
- `get_block_hash` issues no number-addressed read: BLOCKHASH answers from the
  solve anchor's proven ancestry, and the ancestry section below states the
  execution-height split, the refusals, and the measured RPC budget.

### Execution-aware BLOCKHASH ancestry (2026-11-23)

`sim/evm2/block_hash_ancestry.rs::BlockHashAncestry` replaces the retired
number-only full-block lookup on both async adapters (`AlloyAsyncDb` and
`ReadHubDb`). It carries two facts, not one:

- **The execution height** (`BlockEnv::number`) is the EVM's clock. The EVM zero
  rule is applied to it before any state or RPC: a number outside `u64`, the
  executing block, a future block, and a number more than `BLOCK_HASH_HISTORY`
  (256) below it all answer `B256::ZERO`. No request is issued and no earlier
  refusal is consulted.
- **The read anchor** is the read observation's own position, and evidence is
  proved downward from it only: ONE identity-only
  `chain_evidence::acquire_header_chain(target..=anchor.number())` (no block
  bodies), whose consecutive heights, parent links, and chain must agree and
  whose terminal header must BE the anchor. A deeper request extends the chain
  downward and must link to the already-proven floor's `parent_hash`, so a
  same-height replacement below the anchor is refused rather than served. A
  number inside the window but above the read anchor is
  `BlockHashRefusal::AboveAnchor`, never a fabricated answer.

`BlockHashAncestry::new(anchor)` executes at the anchor (the main per-block EVM
and the hub fan-out); `executing(anchor, execution)` is the explicit form, and
`Evm2BlockSimHandle::scratch_evm` uses it at `anchor.number() + 1` to match its
next-block `ScratchBlock` frame. An unresolved anchor inside the window fails
closed with `BlockHashRefusal::UnresolvedAnchor`: there is no identity to prove
against, so the node's current canonical block at that height is never served.
Deterministic refusals (`UnresolvedAnchor`, `AnchorMoved`, `AboveAnchor`,
`Evidence`) are sticky per adapter while provider failures stay retryable, and
proven headers are memoized per adapter, so a proof never outlives or crosses
its anchor. `AlloyAsyncDbError::BlockNotFound` and
`ReadHubDbError::BlockNotFound` retire with the full-body read; the ancestry
variant is the typed replacement.

RPC budget (measured offline and against the live archive node): EVM-zero
answers cost no dispatch; the main adapter's cumulative header reads at depths
[1, 2, 10, 255, 256] are [2, 3, 11, 256, 257]; a repeat request costs zero; a
next-block frame's depth-1 answer is the anchor itself, one read. Regressions:
`tests/block_hash_ancestry.rs` (in-window, zero-rule, replacement, deep
replacement, missing evidence, moved anchor, unresolved, wrong-height,
next-block window, above-anchor, memo scoping, the hub adapter, the EVM opcode,
and the production scratch-handle probe) plus the `live_*` fork-truth legs that
`just test-fork` runs against the reth archive node.

### Read evidence and fail-closed activation/verification (2026-11-23)

`degenbot_rpc::read_evidence::ReadEvidence<T>` is the typed outcome of one
exact read: `Value`, `Reverted`, `Malformed`, or `NoCode`. A transport fault
stays the outer `ExactReadError`. `BlockReads::{read_evidence, call_evidence,
code_present}` and `ReadEvidence::from_outcome` produce it, and
`erc20::read_exact` is the exact-block metadata walk; the generic
`ContractRead`/`erc20::read` family is untouched.

Activation resolves ONE `ChainAnchor` (the tip) and pins every market-name,
GHO-metadata, and code read to it. Market name and GHO `decimals()` are
required: a reverted, malformed, or code-less answer refuses the activation as
`RunError::ActivationMetadata`, and a partial failure persists no market row.
`GETMARKETID()` is attempted exactly once, and only after `getMarketId()`
reverted or was unsupported; a transport failure, `NoCode`, or malformed answer
propagates without the uppercase fallback. `name()`/`symbol()` may be NULL only
for a getter that reverted on a token holding code (an established
nonconforming token).

Verification refuses a reverted, short, or code-less read for a tracked
position, stkAAVE balance, or GHO discount as
`RunError::VerificationEvidence`; it never maps that evidence to zero. Code
presence is read once per distinct token, so the batch budget is unchanged, and
the DEAD/ZERO skips and the revision-deprecated GHO getter skip remain.
Regressions: the `read_evidence` unit suite (`a_short_payload_is_malformed_evidence_not_zero`,
`a_code_less_target_is_no_code_evidence`), `activate.rs`'s scripted suite (one
pin per activation read, no fallback on transport failure, one uppercase
attempt, no row on partial metadata), and `verify.rs`'s reverted/short
expected-zero refusals.

### Self-contained chunk seed and SQL-free resolution (2026-11-23)

`run/seed.rs::ChunkSeed::load` synchronously loads and interns the chunk's
whole relational starting point on the caller's connection: market families
(assets, contracts, GHO, plus the chunk-start asset and contract revisions),
the candidate asset-absence cache, reserve interning, the identity demand walk
over the chunk's decoded logs, the set-shaped user/position/config prefetch,
and the generation/checkpoint seed facts. `run/resolver.rs::ChunkResolver::resolve`
receives the seed, the candidate evidence, and the exact read views, holds no
`Connection`, issues no SQL, and returns ordered deltas, the advanced substrate,
touched users, and an optional checkpoint. A dependency the seed does not hold
is `RunError::IncompleteChunkSeed` naming the market and the entity, raised
before the chunk transaction opens and never replaced by a lazy query.
`run::process::persist_chunk` remains the only chunk writer.

`run/substrate.rs` is split by capability: seed loading (connection),
resolution reads (no connection), persistence (`*_for_apply`). Verification
follows the same split — `load_verification_positions` /
`load_verification_truth_rows` (SQL), `acquire_position_truth_at` /
`acquire_verification_truth_at` (header and batch reads, no connection
parameter), and `compare_position_truth` / `compare_verification_truth` (pure).
The production driver and the Python pre-commit seam both hold no guard across
RPC: they load, release the guard, acquire with no connection, then compare. One
public helper is outside that shape — `verify_touched_positions_on_conn` still
borrows the connection it is given across its own anchor and truth reads; it has
no production caller and is named under [Deferred choices](#deferred-choices).
The
typed seed refusal propagates through every operation path (token
classification, mint-to-treasury, liquidation patterns), so a hole cannot
become a zero address or a skipped aggregation. The reserve's two revision
reads share one Multicall3 pass again, with a single remaining miss keeping the
direct `eth_call` shape (a one-call `aggregate3` wrapper would change the
recorded request set).

Proofs: `resolver::tests::a_real_chunk_resolves_without_sql` (a production seed
over a real receipt resolves while the test holds the lock, producing both a
scaled-balance and a configuration delta), the stalled-provider gates
(`a_stalled_provider_does_not_block_an_unrelated_database_operation`,
`a_stalled_verification_acquisition_does_not_block_the_database`), the typed
propagation trio, and `the_two_reserve_revisions_share_one_multicall3_pass`.

### OperationReducer (2026-11-23)

`transaction_processor::OperationReducer::reduce_transaction` is the one
behavioral entry point `ChunkResolver` delegates to. It owns the market/GHO
context, liquidation-pattern state, substrate access, the
`ProjectionView`/`DeltaSink` interleave, and operation-family dispatch. The
refactor moved behavior, not parameters, and changed no storage format:
`DELTA_ENCODING_VERSION` remains `aave.canonical-delta/v1`, and no projection,
journal, checkpoint, or replay fixture moved. `ProcessTxError::Deferred` is
retired in favour of `ProcessTxError::InvalidOperation`; duplicate ownership
stays an `OwnershipRefusal`, and unresolved positions and stale generations
keep typed refusals. Regressions: the same-transaction
configuration-before/after and bad-debt-before-GHO ordering tests
(`a_configuration_delta_ahead_of_the_operation_advances_the_view_before_it`,
`a_bad_debt_write_off_advances_the_view_before_a_later_gho_operation`),
`projection::tests::the_reducer_module_holds_no_transport_or_storage_reach`,
and the replay, writer-parity, and recovery suites.

### Diagnostics (2026-11-23)

AAVE-TXTRACE reads the post-delta `ProjectionView` after the transaction's
operation reduction, so a position moved twice in one transaction reports its
settled end-of-transaction `(balance, last_index)`. It is emitted on the
explicit `degenbot::aave` DEBUG target, and the emission gate names that same
target, so enabling an unrelated target runs no scan. AAVE-PARSE-FAIL is one
bounded ERROR event on that target carrying `block`, `block_hash`, `tx`,
`tx_index`, and `err`; direct stderr is gone and the field names keep their
pre-cleanup spellings for existing narrowing scripts. Regressions:
`txtrace_reports_a_positions_same_transaction_updates`,
`txtrace_reports_the_state_the_transaction_settled`,
`txtrace_stays_dark_when_only_an_unrelated_target_is_enabled`, and
`parse_failures_state_the_exact_block_and_transaction`.

### Replay-surface authority (2026-11-23)

`tests/fixtures/replay_surfaces/manifest.json` (`degenbot.replay-surface/v1`)
is the one committed authority for the three primary captures: cassette and
span, RPC round trips and response bytes, SQL statements, and applied
pool/liquidity/event counts. `degenbot_rpc::replay_surface::ReplaySurface::load_committed`
is a strict loader (an unknown field, a foreign schema tag, a duplicate id, a
backwards span, and a kind/count mismatch all refuse), and every replay suite
plus `updater_replay_bench` compares observed traffic against it. No assertion
derives its expectation from the run it is checking. Current values: pool
7/5,510/16 with 1 pool and 1 liquidity apply, Aave 19/203,117/80 with 20
events, config 21/252,230/82 with 32 events.

The negative probe is one manifest edit: changing the pool entry's round trips
fails both the pool replay suite and the bench row with no second literal
edited, and restoring it passes with the observed counters equal again.
Historical measurement tables in `docs/updater-rpc-sql-survey.md` stay history;
the authority's table is a transcript of the manifest, not a second source.
Regressions: `replay_surface::tests`
(`the_committed_authority_matches_the_committed_cassettes` plus the refusal
cases) and every governed replay suite.

### Deliberate number/tag lanes (not Exact reads)

Each lane below is a documented read position, not a second implementation of
one read.

- **Generic `multicall3_batch` tag lanes** — the pool liquidity verifier, the
  connector index, and the ERC-20 read family's own transports address by block
  tag. `BlockReads` and this batch are two pin policies over one batch contract.
- **`eth_call` on the recorded-wire boundary** — the provider's typed request
  builder emits `input` on the number lanes while the recorded pinned corpus
  carries `data` on the hash lane, so `eth_call` keeps its documented separate
  boundary; `BlockReads::call` is the pinned view production uses.
- **Unresolved simulation ingress** — the Python inline-sim request (its
  `InlineSimRequest` is number-only), the dry-run frame-pipeline handles that
  resolved no header, and the live-RPC lab/bench harnesses that pin a height
  they never resolved. They are explicit `ReadAnchor::unresolved` observations,
  keep their previous wire behavior, never fabricate a hash, and fail closed on
  BLOCKHASH.
- **Provider `None`/number parameters** — `None` states the alloy builder's
  node-default `latest` tag and `BlockId::Number` states a height or tag word.
  These are ingress and administrative lanes, not simulation reads.
- **Telemetry `BlockLineage::unattached`** — a lineage with no captured exporter
  context is number-only by construction: D1 admits the observation and refuses
  it at the canonical boundary.
- **`aave_v3_markets.last_update_block`** — retired as the completeness claim
  (D8) and retained as the numeric range claim.

None of these is described as an Exact read, and no production Aave read or
identified simulation read uses one.

### Deferred choices

Recorded here as future choices, not open deviations:

- Stable cross-database entity keys: surrogate ids are compared by natural
  identity, and a cross-database key is a separate research project.
- Checkpoint cadence, journal retention, and storage-cost tuning; cadence
  cannot affect correctness (D9).
- Public helper deletion and broad public-path renames beyond the
  supersession-retired set above, explicitly including
  `verify_touched_positions_on_conn`: the unused public verification helper
  still borrows its connection across its own anchor and truth reads, so no
  caller should treat it as the pre-commit gate.
- A generic event-sourcing framework, event bus, or framework crate (non-goal).
- Solver-eligibility, stale-batch, revalidation, and submission-targeting
  policy changes (non-goal).
- Standalone cosmetic module splits.
- `eth_call` on the typed `BlockId` builder lane, which rewrites the recorded
  pinned corpus and is its own change.
- Migrating the wave-3/4/5 per-scenario expectations into the replay-surface
  authority: no duplicated consumer exists yet, and a broader fixture-framework
  rewrite is out of scope.
- Exact-read evidence for the unresolved simulation ingress sources: the Python
  inline-sim request and the live lab/bench harnesses would need a resolved
  header in their input shape first.

# Appendix A — Type inventory and admitted ambiguity at scoping

_Scoping snapshot, recorded before the cutover. The rows below state what the tree carried when this ADR was written; what it carries now is [Implemented shape and seam disposition](#implemented-shape-and-seam-disposition-2026-11-23)._

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
| `AaveChunkEvent` (now `ConfigurationEffect`) | Resolved IDs and precomputed writes; no RPC in its apply core | No chain position, activation provenance, or branch-local generation identity — removed: it is now a delta effect at an exact `LogRef` with an activation-keyed `GenerationRef`, persisted with the operation family |
| `ChunkSubstrate` | Chunk prefetch plus dirty/write overlay | Strong Projection-view seed, but currently coupled to the chunk DB transaction |
| `AaveMarketRow.last_update_block` | Optional numeric cursor | Cannot assert which branch is complete or reject an unexpected-parent commit |
| Pool `ReorgJournal` | Existing forward/restore journal by block | Remains the pool mechanism; needs exact anchor context rather than replacement |

# Appendix B — Aave provider-read and DB-lineage census at scoping

_Scoping snapshot, recorded before the cutover. The rows below state what the tree carried when this ADR was written; what it carries now is [Implemented shape and seam disposition](#implemented-shape-and-seam-disposition-2026-11-23)._

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
| `run/resolution.rs::TransactionResolution` | `ChunkContext::begin_dispatch` (transaction-start GHO read, discount pre-pass), the configuration stage (per event: `resolve_config_log` → substrate projection → generation seal → POOL repoint → delta reduction), and `resolve_operation_identity` (post-config GHO read) | Event-interleaved resolve/reduce against Projection views preserves both intentional positions |
| `verify.rs::verify_*` | `scaledBalanceOf` and `getPreviousIndex` at numeric `chunk_end` | Exact terminal anchor; verification remains a chain-truth check, not a second state implementation |

`run/apply.rs` performs no RPC and no ABI decode, and it is now the chunk's only writer: `apply_canonical_deltas_on_conn` persists the configuration family (`ConfigurationEffect` deltas) and the operation family (scaled-balance, reset, discount-refresh deltas) through one loop.

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

_Scoping snapshot, recorded before the cutover. The first timeline below states what the tree carried when this ADR was written; what it carries now is [Implemented shape and seam disposition](#implemented-shape-and-seam-disposition-2026-11-23)._

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
  → AaveChunkEvent without source position (now a positioned delta effect)
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

_Scoping snapshot, recorded before the cutover. The timeline and the "not yet propagated" verdict below state what the tree carried when this ADR was written; what it carries now is [Implemented shape and seam disposition](#implemented-shape-and-seam-disposition-2026-11-23)._

```text
accepted header
  → shared eventhub Head { number, optional metadata }
  → StageMachine Epoch { number, rewind seq }
  → BlockContext { Epoch, metadata }
  → ResultBatch { solve_block, metadata, telemetry BlockLineage }
  → Session BlockNotification { number, metadata } (independent consumer clock)
  → BatchWork { current_block, execution facts, telemetry BlockLineage }
  → simulation ReadAnchor { unresolved number | identified number + hash }
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
