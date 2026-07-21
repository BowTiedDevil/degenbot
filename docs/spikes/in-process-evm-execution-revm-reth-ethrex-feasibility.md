# Spike: In-process EVM execution (revm / reth / ethrex) feasibility

Research into bringing transaction-envelope execution + result inspection
**in-process** (no `eth_simulateV1` / `eth_call` JSON-RPC round-trip) into the
degenbot Rust core, so that value-capturing transaction simulation is faster and
self-contained. The three candidate engines surveyed at the user's request:
[bluealloy/revm](https://github.com/bluealloy/revm),
[paradigmxyz/reth](https://github.com/paradigmxyz/reth),
[lambdaclass/ethrex](https://github.com/lambdaclass/ethrex).

## TL;DR

- **revm is the clear fit — ship it.** revm (`v41.0.0`, MIT, audited by Guido
  Vranken) is the canonical Rust EVM, already used by Reth/Foundry/many L2s. It
  is a *library* (not a node), zero-pyo3-compatible, and — critically — it ships
  an **`alloydb` feature** providing `AlloyDB<N, P>`: an async `Database` that
  fetches account/code/storage from any alloy `Provider` at a pinned `BlockId`.
  Wrapped in `CacheDB<WrapDatabaseAsync<AlloyDB<…>>>` it is, byte-for-byte, the
  in-process equivalent of `eth_simulateV1` with `stateOverrides`. revm uses
  `alloy-primitives 1.5` + `alloy-consensus/eips/provider 2.0`, matching
  degenbot's pinned `alloy = "^2.0"` — no version skew.
- **The exact pattern degenbot needs already ships as a revm example:**
  `examples/uniswap_v2_usdc_swap` builds `CacheDB::new(WrapDatabaseAsync::new(AlloyDB::new(provider, BlockId::latest())))`,
  then `insert_account_storage(...)` + `insert_account_info(...)` for the
  state-overrides (fund the caller, fake a WETH balance), then drives
  `balanceOf` / `transfer` / `swap` through `Context::mainnet().with_db(&mut cache_db).build_mainnet().transact_one(TxEnv{…})`.
  That maps 1:1 onto `degenbot-simulation::build_simulation_state_overrides`
  (owner funded 100 ETH, injected executor + runtime bytecode, warmup slots,
  WETH9 `balanceOf` override) + the 7-call simulate vector in `simulate_one`.
- **reth is a node, not a simulator.** Its `reth-revm` crate is only glue that
  adapts *reth's own MDBX snapshot store* into a revm `Database` (the `database`
  module: "glue code for integrating reth database into revm's Database"). It is
  the right answer for a *different* question — "execute against a local reth
  archive snapshot with zero RPC" — which is a heavier, optional, archive-node
  backend, not the in-process replacement for `eth_simulateV1`.
- **ethrex is the wrong differentiator for this use case.** ethrex ships its own
  ZK-native EVM (`levm`, in `crates/vm/levm`) and a `vm` crate exposing `Evm` /
  `VmDatabase` / `DynVmDatabase`. Its differentiation is multi-prover ZK rollup
  proving (SP1/RISC0/TEE), *not* in-process RPC-backed simulation tooling. It
  has no equivalent of revm's `AlloyDB` (an on-demand RPC-backed `Database`); we
  would have to build that adapter ourselves. `levm` is far younger and less
  battle-tested than revm. Do not adopt for simulation.

## Recommendation

Add a new pyo3-free core crate **`degenbot-evm`** (+ extend the umbrella `pub
use degenbot_evm;`) wrapping **revm with the `alloydb` feature**. It owns the
in-process execution path and produces the **same `SimulationResult` shape**
`simulate_one` already consumes, so the `degenbot-simulation` dispatch leaf can
swap `dispatch::simulate_v1` (JSON-RPC `eth_simulateV1`) for
`degenbot_evm::simulate_in_process` behind a single call-site change. The
state-override construction (`build_simulation_state_overrides`) and the revert
classification (`classify_revert`) are reused unchanged — revm returns the same
revert bytes / `Panic(0x11)` selectors the classifier already keys on.

The big, bot-specific win — and the reason this is worth doing rather than just
calling revm directly — is a **persistent `CacheDB` seeded from Rust-owned
state**. `AlloyDB` falls back to RPC for any account/code/storage the `CacheDB`
lacks; left naive, an in-process revm sim of a 7-call arb vector touching many
contracts could issue *more* RPC round-trips than one `eth_simulateV1`. The bot
already owns, in Rust, exactly the state the EVM touches:

| State | Already-Rust owner | Pre-load into `CacheDB` as |
|---|---|---|
| V2 pool reserves | `V2PoolState` in `Bot` (`bot_core`) | `insert_account_storage(pool, RESERVE0/1 slots)` + `insert_account_info(pool)` |
| V3/V4 `slot0`/`liquidity`/tick net/gross | `V3PoolState.tick_data` + scalars in `Bot` | `insert_account_storage` for `slot0`, `liquidity`, tick-map slots |
| PoolManager ERC6909 balances | the warm-up slot math in `degenbot-executor` | `insert_account_storage(pm, balanceOf slot)` |
| WETH9 `balanceOf` slots | the warm-up slots (`WarmupSlots`) | `insert_account_storage(weth, balanceOf slot)` |
| Executor runtime bytecode | already a `Bytes` arg to the simulate leaf | `insert_account_info(addr).code = bytecode` |

So for the hot path, the EVM never RPC-round-trips for the pools it just solved
inside the same block — the dominant cost of naive `AlloyDB` is paid once per
*new* contract, then amortised across every candidate in the block. This is the
performance case for ownership (the Architecture Vision's "Rust is the engine"
goal pays off concretely here: the in-process simulator is fed by the engine,
not by re-fetching the engine's own state).

## How it lands in the three-layer architecture (ADR-005)

| Layer | Where | Holds |
|---|---|---|
| **Rust core `degenbot-evm`** | `rust/crates/degenbot-evm/src/` | `revm` + `AlloyDB` + `CacheDB`; `EvvmSimulator { db: CacheDB<…>, spec: SpecId }`; `simulate_in_process(ctx, path) -> SimulationResult`; the `CacheDB`-seeding adapters above. **Zero `pyo3`** (gated `just check-no-pyo3-in-cores`). |
| **PyO3 wrapper** | `rust/crates/degenbot-python/src/simulation/evm.rs` | `#[pyfunction]` only: arg extract → GIL release → `simulate_in_process` → wrap `SimulationResult` as the same `PySimResult` the Python driver already reads. |
| **Python driver** | `examples/eth_backrun_v2_v3_v4_rust.py` | One-line swap: `simulate_v1(...)` → `simulate_in_process(...)` (or a `--sim=rpc|evm` flag for dual-path A/B). No new state on the Python side. |

**Standalone-Rust-core constraint (ADR-005 §4.1):** `cargo add degenbot`
reaches in-process sim via `pub use degenbot_evm;`. revm's only "heavy"
default deps are the precompiles (`c-kzg`, `blst`, `secp256k1`), all
feature-gated and matching the EVM spec the bot targets (Cancun/Prague). No
pyo3, no Python in the build graph. ✓

**Dual-path coverage (ADR-005 §4.2):** add a Tier-2 parity pair — same canonical
fixture, driven through `eth_simulateV1` (RPC) and through
`simulate_in_process` (revm), asserting identical gross/net/gas + revert
classification. revm is *the* reference EVM that geth/erigon `eth_simulateV1`
parity is measured against, so confidence in agreement is high; the parity test
pins the seam (block-env, state-override merge, gas accounting).

## revm facts established (primary sources)

- **Repo:** `github.com/bluealloy/revm`, branch `main`, umbrella crate `revm`
  `v41.0.0`. README: "Revm is a highly efficient and stable implementation of
  the EVM… audited by Guido Vranken (#1 Ethereum Bug Bounty)." Used by Reth,
  Foundry, Optimism, Base, Scroll, RISC0, Succinct.
- **Workspace `Cargo.toml`** declares members under `crates/`: `revm`,
  `primitives`, `interpreter`, `precompile`, `database`, `database/interface`,
  `bytecode`, `state`, `context`, `context/interface`, `handler`,
  `inspector`, `statetest-types`, `ee-tests`, plus `examples/` (including
  `uniswap_v2_usdc_swap`, `uniswap_get_reserves`, `erc20_gas`,
  `contract_deployment`, `database_components`, `my_evm`).
- **`crates/revm/Cargo.toml` features** (verified): `default = ["std",
  "secp256k1", "portable", "tracer", "c-kzg", "blst"]`. Relevant opt-ins:
  - `alloydb = ["database/alloydb"]` — enables `AlloyDB` (the on-demand
     RPC-backed `Database`).
  - `asyncdb = ["database-interface/asyncdb"]` — enables
     `DatabaseAsyncRef` / `WrapDatabaseAsync` (required to wrap `AlloyDB`
     behind the synchronous `Database` the EVM `transact_*` paths take).
  - `serde`, `tracer`, `dev` (relaxes balance/gas-limit checks — useful for
     state-override-style sims where the caller is prefunded out of thin air),
  - precompile gates `c-kzg`/`blst`/`secp256k1`/`bn`/`gmp`/`p256-aws-lc-rs`.
- **`crates/database/src/lib.rs`** re-exports `AlloyDB`, `CacheDB`,
  `State`/`StateBuilder`, `BundleState`, `CacheState`, `PlainAccount`,
  `StorageWithOriginalValues`. `CacheDB<DB>` is the standard "in-memory
  overrides on top of a backing `Database`" wrapper — i.e. the `stateOverrides`
  object in revm form.
- **`crates/database/src/alloydb.rs`** — `pub struct AlloyDB<N: Network, P:
  Provider<N>> { provider, block_number: BlockId }`. Implements
  `DatabaseAsyncRef`: `basic_async_ref` →
  `provider.get_transaction_count` + `get_balance` + `get_code`; `code_by_hash`
  via `get_code`; `storage_async_ref` via `get_storage_at`. All pinned to
  `block_number`. This is the "fetch live state on cache miss" primitive.
- **`examples/uniswap_v2_usdc_swap/src/main.rs`** — the canonical usage:
  ```rust
  let provider = ProviderBuilder::new().connect(rpc_url).await?.erased();
  let alloy_db = WrapDatabaseAsync::new(AlloyDB::new(provider, BlockId::latest())).unwrap();
  let mut cache_db = CacheDB::new(alloy_db);
  // stateOverrides:
  cache_db.insert_account_storage(weth, hashed_slot.into(), one_ether);
  cache_db.insert_account_info(account, AccountInfo { balance: one_ether, .. });
  // execute a call in-process:
  let mut evm = Context::mainnet().with_db(&mut cache_db).build_mainnet();
  let out = evm.transact_one(
      TxEnv::builder().caller(..).kind(TxKind::Call(token))
          .data(encoded.into()).value(U256::ZERO).build().unwrap()
  );
  ```
  → this is `eth_call` + stateOverrides + balanceOf read, in-process, no
  JSON-RPC framing. The README's headline API (`Context::mainnet().with_block(block).build_mainnet(); evm.transact(tx)`)
  is the full-tx (envelope + fee charge + nonce) form, which is what
  "simulate the actual execution of a value-capturing transaction" needs.
- **`crates/database/interface/src/lib.rs`** — the `Database` trait
  (`basic` / `code_by_hash` / `storage`), `DBErrorMarker`, `auto_impl::auto_impl`
  on `&mut, Box` (so `Arc<dyn Provider>` style usage works). `empty_db`,
  `either`, `state_hook`, `try_commit` adapters ship in-tree.

## reth facts established

- **Repo:** `github.com/paradigmxyz/reth`, `main`. "Production-ready Ethereum
  execution layer client… built with Rust, Alloy, revm, Foundry." Apache+MIT.
  Reth 2.0 (April 2026), Storage V2 default. It is a *node*, not a library you
  embed for simulation.
- **`crates/revm/src/lib.rs`** — `reth-revm` is a thin glue crate: modules
  `cached` ("Cache database that reads from an underlying `DatabaseRef`"),
  `database` ("glue code for integrating reth database into revm's
  `Database`"), `cancelled`, `witness` (execution-witness generation). It
  re-exports `revm` itself. It is only useful if you have *reth's local store*
  to feed it — i.e. you run a reth archive node and point the bot's EVM at its
  MDBX snapshot. That gives zero-RPC in-process execution against a local
  snapshot, at the cost of running + coupling to a reth node.
- Other relevant crates: `crates/evm` (reth's execution orchestration),
  `crates/storage` (reth's DB). Both are node-scale, not simulation-tools-scale.

### reth disposition

**Optional heavy backend, not the primary path.** The usable reth surface for us
is `reth-revm::database` — an adapter that turns a reth snapshot store into a
revm `Database`. That is strictly more powerful than `AlloyDB` (no RPC at all)
but requires a co-located reth node (MDBX on disk). Recommendation: design the
new `degenbot-evm` crate around the `Database` trait so a future
`degenbot-evm-reth` adapter is a drop-in (the trait boundary is the seam), but
do **not** take a reth dependency for the first cut. revm + `AlloyDB` +
Rust-state-seeded `CacheDB` already eliminates the `eth_simulateV1`/`eth_call`
round-trip the user wants gone, without a reth node obligation.

## ethrex facts established

- **Repo:** `github.com/lambdaclass/ethrex`, `main`. "Minimalist, stable,
  modular, fast, ZK native implementation of the Ethereum protocol in Rust."
  Dual L1/L2 (L2 = multi-prover ZK rollup: SP1, RISC Zero, ZisK, OpenVM; proofs
  aggregated via Aligned Layer).
- **`crates/vm/`** — `lib.rs` exposes `Evm`, `BlockExecutionResult`,
  `TxGasBreakdown`, `TxStatus`, `EvmError`, `DynVmDatabase`, `VmDatabase`,
  `PrecompileCache`, `intrinsic_gas_*`. Backends: `backends/levm` (its own EVM)
  + `backends/mod.rs`. **`crates/vm/levm/`** is a full separate EVM
  implementation ("ethrex levm") with its own `bench`/`runner`. There is
  **no** in-tree equivalent of revm's `AlloyDB` — no "fetch state from a JSON-RPC
  provider on demand" `VmDatabase` implementation ships in the `vm` crate; it
  expects a `vm_db` backed by ethrex's own storage / a state provider, not an
  RPC node.

### ethrex disposition

**Do not adopt for simulation.** ethrex's reason-to-exist is ZK proving of block
execution, not in-process RPC-backed call simulation. Its EVM (`levm`) is
younger than revm and oriented to zkVM proving (datum structures chosen for the
prover, not for raw execution speed or `eth_simulateV1`-shape ergonomics). It
would require us to write the on-demand RPC-backed `VmDatabase` ourselves —
exactly what revm ships out-of-the-box as `AlloyDB`. ethrex is only interesting
if degenbot ever wants to *prove* a captured transaction (e.g. trustless backrun
verification on an L2) — out of scope for this feature.

## Design option B (deeper): make `Bot` *be* a revm `Database`

The baseline recommendation above (option A) seeds a `CacheDB` from Rust-owned
state before each block's sim fan-out. This is a mechanical, low-risk port that
absorbs `eth_simulateV1` and is the recommended first cut.

There is a tighter, first-class variant worth recording as a design option.
It touches ADR-003 (Bot as single state owner) + ADR-014/ADR-016 (pool-state
deepening, reorg rollback) and should be evaluated against the triage rubric in
`docs/migration-guides/three-layer-transition.md` before being chosen — it is
**not** part of the primary recommendation, but is the move that makes the
in-process sim non-degenerate for a standalone Rust consumer and unifies the
snapshot state-read axis.

### The boundary: revm-state abstracts the *snapshot state-read axis* only

revm's `Database` trait (`crates/database/interface/src/lib.rs`) is three reads:
`basic(address) → AccountInfo`, `storage(address, index) → U256`,
`code_by_hash(hash) → Bytecode` (plus `block_hash`). `CacheDB` is the
in-memory-overrides-over-a-backing-`Database` composition. So the abstraction
revm-state buys is: "what is account X / slot Y at block B, with overrides" —
a snapshotted EVM state read. It does **not** abstract streams, writes, receipts,
fee history, or log filtering:

| Chain-data concern in degenbot | revm-state can own it? | Why / why not |
|---|---|---|
| Pool state reads (V2 reserves, V3/V4 `slot0`/`liquidity`/tick-data) | **Yes** | These *are* storage slots; `Database::storage(pool, slot)` answers them. |
| `balanceOf` / allowances / arbitrary storage reads during sim | **Yes** | What `AlloyDB` + `CacheDB` exist for. |
| `eth_simulateV1` / `eth_call` | **Yes (absorbed)** | The in-process `transact`. |
| `eth_createAccessList` | **Yes (absorbed)** | revm computes warming internally; emit from the `State` journal. |
| Bootstrap snapshots / verification tick-map fetches | Partial | One-shot bulk reads; `Database` is point-query-shaped, so Multicall3 stays better for *bulk*. |
| `newHeads` / `logs` **stream subscriptions** (the pump) | **No** | revm has no notion of "notify me when state changes." State is a snapshot, not a stream. |
| `eth_sendRawTransaction` (submission) | **No** | revm executes in-process; it does not submit to the network. |
| `eth_getTransactionReceipt` | **No** | Historical receipts are not in revm's domain (revm produces results, doesn't store chain history). |
| `eth_feeHistory` (p10/p50 percentiles) | **No** | Chain-wide fee statistics; not account/storage state. |
| Log filtering over a block range | **No** | revm produces logs *during execution*; it doesn't serve "give me logs matching this filter historically" — that's `eth_getLogs`. |
| Multicall3 batched reads | **No** | An on-chain contract; an RPC concern (though a `Database` impl *could* execute Multicall3 in-process via revm — a separate optimization). |

**Reframing.** `AlloyProvider` does not disappear; it re-specializes. Today it
is "the thing everything reads chain data through." After this, it is "the thing
that handles live-network concerns":

- The **pump's dual WS subscription** (`newHeads` + `logs`) — streams, not
  snapshots. Unchanged.
- **Submission** (`eth_sendRawTransaction`) + receipt polling — writes.
  Unchanged (`degenbot-submission`).
- **`eth_feeHistory`** — fee statistics, not account state. Unchanged.
- **`eth_getLogs`** backfill (on pump timeout / empty-block suspicion) — log
  filtering. Unchanged.
- **Bootstrap / verification bulk reads** — Multicall3 batching stays the
  better tool for *bulk* point reads than a `Database`'s one-slot-at-a-time shape.
- **The RPC fallback** inside `AlloyDB` for untracked contracts. So
  `AlloyProvider` is *inside* the `degenbot-evm` stack as the cold-miss path,
  even as it is also the pump/submission path. One typed provider, two consumers.

The net: three orthogonal axes, no overlap, no mirror — one `AlloyProvider`
(live network: streams/writes/receipts/fees/logs + cold-miss fallback), one
revm-state layer (snapshot state-reads: sim + inspection), one engine
(event-driven mutable state owner, primary seed for the revm-state layer).

Notable: in degenbot's *hot path*, `AlloyProvider`'s per-block state-read role
is already small — the pump is event-driven (it mutates Rust-owned state from
decoded `Sync`/`Swap`/`Mint`/`Burn`/`ModifyLiquidity` events; it does not re-fetch
pool state each block). `AlloyProvider`'s state-read role today is bootstrap +
verify + simulate. This redesign absorbs the **simulate** slice and optionally
the **verify** slice; bootstrap stays Multicall3; the pump stays WS. So
"abstract away `AlloyProvider`" really means "abstract away the *point state
reads*," which is most of the post-bootstrap state-read surface.

### The move: `BotStateSnapshot` implements `DatabaseRef`, `AlloyDB` is the cold-miss fallback

The tighter shape implements revm's `DatabaseRef` directly on a
`BotStateSnapshot` view of the engine, so the engine state *is* a revm
`Database` and `AlloyDB` falls back only for contracts the engine doesn't track:

```
… EVM transact → CacheDB (sim-scoped overrides)
                    → BotStateSnapshot  (engine typed state, encode-on-demand)
                    → AlloyDB           (RPC fallback for untracked contracts)
```

`BotStateSnapshot::storage(pool, RESERVE0_SLOT)` reads the typed
`V2PoolState.reserves.0` and ABI-encodes it into the 32-byte word the EVM
expects. For V3/V4: `slot0`/`liquidity`/tick-bitmap/`ticks(i32)` slots encoded
on demand. For PoolManager ERC6909 + WETH9 `balanceOf`: the exact slot layouts
`degenbot-executor` already encodes for warmup. For the hot path (pools the
engine tracks), `AlloyDB` is never consulted — zero RPC, in-process, fed by the
engine's own mutated state. For auxiliaries (WETH9 when not overridden,
Multicall3, untracked tokens, the injected executor's immutables), `AlloyDB`
fetches and `CacheDB` caches.

This is the move that delivers the standalone-Rust-core constraint concretely:
a `cargo add degenbot` consumer drives `Bot` (state owner) → `Bot` *is* the EVM's
`Database` → in-process sim with no Python and no RPC-for-tracked-state.

### The single-source-of-truth invariant (the critical one)

The trap is double-bookkeeping: if `Bot` owns typed pool state *and* a `CacheDB`
mirrors EVM-slot-encoded copies of the same state, a `Sync` event must update
both or they drift — and the AGENTS.md "do not introduce a mirror of
Rust-owned state" smell fires *inside* Rust. The correct shape avoids it:

- **The typed fields in `Bot`/`V2PoolState`/`V3PoolState`/`V4PoolState` remain
  the single source of truth.**
- **`BotStateSnapshot` is a *read view*** that impls `DatabaseRef` by encoding
  typed fields to EVM slots **on demand**. No long-lived encoded copy.
- **`CacheDB` above it caches only for one simulation's duration** — a scoped,
  short-lived cache of encoded slots that dies with the snapshot. Mutation
  coherence is trivial because the snapshot is immutable for the sim's lifetime
  (the engine is at block N; the sim runs against block N).

Same shape as a DB view: one underlying relation (the typed fields), many
projected access patterns (EVM-slot reads). No denormalization, no drift.

### Where it lives (crate dependency direction)

Do **not** make `degenbot-bot` (the engine/state owner) depend on revm — that
drags the EVM into the engine crate, and the engine's job is event-driven state
mutation, not EVM execution. The clean split:

- The `DatabaseRef` impl for bot state lives in **`degenbot-evm`** (the new core
crate), which depends on `degenbot-bot` (for the typed state types) + `revm`.
The engine crate stays revm-free.
- `degenbot-bot` exposes a `pool_state_snapshot()` or
`BotStateSnapshot::from(&bot)` constructor (it already exposes
`v2_pools_snapshot()` / `v3_pools_snapshot()` / `v4_pools_snapshot()` — verified
in the V3/V4 recompute-feasibility spike). `degenbot-evm` consumes those and
impls `DatabaseRef`.

Dependency direction: `degenbot-evm → degenbot-bot + revm`, never the reverse.
The engine never knows revm exists; the EVM crate adapts the engine's state
into revm's interface — the same adapter shape as the reth-revm glue, but over
`Bot`'s typed state instead of reth's MDBX store.

### The storage-layout mapping surface

`BotStateSnapshot::storage(pool, slot)` must map each slot index back to the
right typed field *and match the deployed contract's storage layout exactly*.
This is a real, owned surface — but not new work: `degenbot-executor` already
encodes these layouts for the warmup-slot overrides (WETH9 `balanceOf`@3,
PoolManager ERC6909 `balanceOf`@4, V2 reserves, V3 `slot0`/`tick_bitmap`/
`ticks`). The `DatabaseRef` impl reuses that same layout knowledge; a layout
drift between the impl and the contracts = silent sim divergence, so a parity
test against `eth_getStorageAt` for a fixture pool set is the guard.

### ADR touchpoints

This option touches:

- **ADR-003** (Bot as single state owner) — `Bot`-as-`Database` is a natural
extension; the engine's typed state *is* the EVM's state, directly.
- **ADR-014 / ADR-016** (pool-state deepening layer, reorg rollback traits) —
the `DatabaseRef` impl is a *read view* over the same deepened state; the reorg
journal applies underneath it, so a snapshot taken at block N is automatically
coherent with rollback.
- **ADR-005 standalone-core** — `Bot`-as-`Database` is the move that makes the
standalone Rust consumer's in-process sim *non-degenerate* (seeded, not
RPC-cold).

### Open design questions for option B (defer to a follow-up spike)

1. **Layered composition API.** revm ships `either` (`database-interface::
   either`) for composing two databases, and `CacheDB` wraps a backing
   `Database`. The exact chaining of *overrides > engine state > AlloyDB* —
   whether it is `CacheDB<Either<BotStateDb, AlloyDB>>` or a custom trait impl
   — needs a 30-minute spike to pin the precise API surface. The pieces exist;
   the exact nesting's composability without an adapter is not yet verified.
2. **`code_by_hash` safety.** `AlloyDB` panics on `code_by_hash` because `basic`
   eagerly loads code. `BotStateSnapshot::basic` must do the same — return full
   `AccountInfo { code }` for tracked contracts (the injected executor bytecode
   is already a `Bytes` arg; WETH9/V2/V3/V4 runtime bytecodes are either known or
   fetched once + cached). Untracked contracts fall to `AlloyDB`.
3. **Scope of `BotStateSnapshot`.** Should it carry *all* tracked state (V2 +
   V3 + V4 pools, full tick maps), or be a lazy view backed by
   `Arc<RwLock<Bot>>` reads? Full snapshot per-block is cheaper than it sounds
   (the engine already builds mixed-path state per-block), and a snapshot is
   immutable + coherent with reorg rollback. A lazy view risks lock contention
   under the `JoinSet` fan-out. Lean: snapshot per block, immutable for the sim
   fan-out.
4. **Does the diagnostic recompute path (`diagnostic.rs`) consume
   `BotStateSnapshot` too?** If yes, the revm-state layer becomes the *shared
   read interface* for sim + diagnostic + a future standalone inspector — the
   real "first-class abstraction" payoff. Worth scoping explicitly.

## Risk & caveats

1. **Naive `AlloyDB` can be slower than one `eth_simulateV1`.** Each cold
   account/code/storage access = one RPC call. A 7-call arb sim touching N
   contracts with M storage slots could be 5–30 RPC round-trips vs one batched
   simulate. **Mitigation (the whole point of the `degenbot-evm` crate):** keep
   a `CacheDB` alive across the per-block fan-out and seed it from the
   Rust-owned engine state (reserves, tick maps, warmup slots, injected
   bytecode) before the first sim of a block. After the seed, in-block sim is
   RPC-free for the hot contracts. **Measure this** in a Tier-2 fixtured
   comparison (RPC simulate vs in-process, same fixture, same block) before
   flipping the default.
2. **Block-env parity.** The existing `SIMULATE_BLOCK = Pending` +
   `SIMULATE_BLOCK_ID` const (with the documented genesis-rejection bug) shows
   simulation is sensitive to block-env details. revm gives explicit
   `BlockEnv { number, timestamp, basefee, gas_limit, beneficiary, … }`
   control — *better* than the simulateV1 `"pending"` tag (no node-dependent
   flattening), but the parity test must pin the same env the Python oracle
   used (`base_fee_next`, the pump's block timestamp/number).
3. **`StateOverride` vs `CacheDB` merge semantics.** `build_simulation_state_overrides`
   documents an explicit-balance-wins merge (the executor's 10-ETH override
   must not be clobbered by the warmup's residual balance). `CacheDB`'s
   `insert_account_info` / `insert_account_storage` are last-write-wins — the
   port must preserve the same explicit-balance-wins ordering or the WETH9
   `balanceOf` final value drifts. The §4.2 state-override parity corpus
   (`inject_code` True/False × warmup on/off) carries over unchanged.
4. **Gas accounting fidelity.** revm gas matches mainnet (it's the reference).
   But `eth_simulateV1` aggregates the simulated block's gas usage against the
   parent block's gas limit (the genesis bug above). In-process revm computes
   per-call `gas_used` directly — closer to reality than the node's aggregation
   edge cases. The 1.5× `GAS_SAFETY_MARGIN` + `INITIAL_EXECUTE_GAS` constants
   stay; they consume the same `gas_used` field of `SimResult`.
5. **Precompiles + spec pin.** Pin `SpecId` to the live fork (Cancun/Prague)
   and enable the matching precompiles (`c-kzg`, `blst`, `secp256k1`). The
   `dev` feature's `optional_balance_check` / `optional_no_base_fee` are
   tempting for the "fund the owner with 100 ETH out of nowhere" override, but
   prefer the explicit `insert_account_info` path so the sim stays a faithful
   mainnet execution (the owner is genuinely credited in the `CacheDB`, not via
   a balance-check bypass).
6. **`degenbot-fork` (anvil subprocess) coexistence.** The anvil fork path
   (`degenbot-fork`) stays valid for integration tests that need a *real* node
   (receipts, mempool, `evm_mine`). In-process revm does not subsume it; it
   subsumes the hot-path `eth_simulateV1` simulation. Document the split on
   `degenbot-fork`'s lib.rs so the anvil path is not mistaken for the sim path.

## Open questions for the planning task (ergo)

- Where does the `CacheDB` live across the per-block fan-out? Candidate: the
  `SimulateContext` (currently borrows `&AlloyProvider`) gains an owned
  `EvmSimulator` whose `CacheDB` is reset + reseeded per block (or per
  reorg-window). Concurrency: `simulate_one` is fanned out over a `JoinSet`
  with a `Semaphore` cap (`dispatch_profitable.rs`) — the `CacheDB` is `&mut`,
  so either thread it behind a `Mutex` (hot contention) or clone-per-task
  (cheap-ish: `CacheDB` clones the override map, not the backing `AlloyDB`).
  The clone-per-task shape matches the existing `SimulateContext: Clone` model.
- Do we keep `eth_simulateV1` as a fallback / parity oracle, or delete the RPC
  dispatch leaf once in-process parity is green? Per the no-backwards-compat
  directive, prefer delete-after-parity; keep the typed `AlloyProvider::eth_simulate_v1`
  surface (it's a generic RPC primitive, not simulation-specific) but remove the
  `degenbot-simulation::dispatch::simulate_v1` orchestration if revm wins.
- Does the in-process path subsume `eth_createAccessList` (the EIP-2930 warmup
  optimization)? revm computes access-list warming internally; we can read the
  touched addresses/slots from the `State` journal post-`transact` and emit an
  access list for the *submitted* tx without a separate RPC. Potential second
  round-trip elimination.

## Suggested ergo epic shape

1. **Spike → `degenbot-evm` crate skeleton** — revm `alloydb`+`asyncdb`+`std`
   feature set, `EvvmSimulator` wrapping `CacheDB<WrapDatabaseAsync<AlloyDB<…>>>`,
   spec pinned, builds + lints under `just check-no-pyo3-in-cores`.
2. **State-override adaptor** — port `build_simulation_state_overrides` output
   → `CacheDB` `insert_account_*` calls (explicit-balance-wins preserved).
   §4.2 parity fixture reused verbatim.
3. **`simulate_in_process`** — execute the 7-call vector (or the execute() call
   + balance reads) in-process; return the existing `SimulationResult`/
   `SimResult` shape. Reuse `classify_revert`.
4. **Rust-state seeding** — `CacheDB` seeders reading `V2PoolState` reserves,
   `V3PoolState`/`V4PoolState` slot0+liquidity+tick-data, `WarmupSlots`. (This
   is where the Architecture Vision's "Rust owns all state" payoff lands.)
5. **Tier-2 dual-driver parity** — `rust/crates/degenbot/tests/parity_evm_sim.rs`
   + `tests/standalone_parity/test_evm_sim_dual_driver.py`, same fixture as the
   existing `parity_*` pairs, asserting gross/net/gas + revert-bucket equality
   between the RPC simulate and the in-process sim.
6. **PyO3 wrapper + driver swap** — `degenbot-python/src/simulation/evm.rs`;
   `examples/eth_backrun_v2_v3_v4_rust.py` `--sim=rpc|evm` flag; A/B in dry-run.
7. **(stretch) access-list emission** from the revm `State` journal, retiring
   the `eth_createAccessList` dispatch.

## Sources (primary)

- revm README: `https://github.com/bluealloy/revm/blob/main/README.md`
- revm workspace + crate manifests: `…/revm/blob/main/Cargo.toml`, `…/crates/revm/Cargo.toml`
- revm `AlloyDB`: `…/crates/database/src/alloydb.rs`
- revm `Database` trait + `CacheDB` re-exports: `…/crates/database/src/lib.rs`, `…/crates/database/interface/src/lib.rs`
- revm in-process Uniswap swap example: `…/examples/uniswap_v2_usdc_swap/src/main.rs`
- reth README: `https://github.com/paradigmxyz/reth/blob/main/README.md`
- reth-revm glue: `…/reth/blob/main/crates/revm/src/lib.rs`
- ethrex README: `https://github.com/lambdaclass/ethrex/blob/main/README.md`
- ethrex `vm` crate: `…/ethrex/blob/main/crates/vm/lib.rs`, `…/crates/vm/levm/`
