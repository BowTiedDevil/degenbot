# Spike: revm/AlloyDB composition API + cold-miss latency

Epic: `3OMYOB` ("In-process EVM execution (revm)"). Spike task: `QGJGWI`.

This spike verifies the three unverified claims from
[`in-process-evm-execution-revm-reth-ethrex-feasibility.md`](in-process-evm-execution-revm-reth-ethrex-feasibility.md)
that the deeper-integration design (option B: `Bot` as a revm `Database`) rests
on, and produces the measured cold-miss latency number that decides option A
(seeded `CacheDB`) vs option B (`Bot`-as-`Database`).

## 1. Version + feature pin

revm `v41.0.0` builds cleanly against the degenbot workspace's resolved alloy
versions (verified by a scratch crate `cargo fetch` + `cargo build`):

| Dep | revm's requirement | degenbot lock resolution | OK? |
|---|---|---|---|
| `alloy-provider` | `2.0.0` (`default-features = false`) | `2.2.0` | ✓ (`^2.0`) |
| `alloy-eips` | `2.0.0` | `2.2.0` | ✓ |
| `alloy-transport` | `2.0.0` | `2.2.0` | ✓ |
| `alloy-primitives` | `1.5.2` (`^1.5.2`) | `1.6.1` | ✓ |
| `alloy-consensus` | `2.0.0` | `2.2.0` | ✓ |

**Minimum feature set for degenbot's Cancun/Prague targeting** (the `revm`
umbrella crate's `[features]`):

```toml
[dependencies]
revm = { version = "41.0.0", default-features = true, features = ["alloydb", "asyncdb"] }
```

- `default-features = true` brings `std` + `secp256k1` (ecrecover precompile) +
  `c-kzg` (point evaluation) + `blst` (BLS12-381) + `portable` (no ISA assumptions)
  + `tracer`. **All mainnet precompiles are enabled by default — keep them.**
- `alloydb` = `["database/alloydb"]` → enables `AlloyDB` (RPC-backed
  `DatabaseAsyncRef`).
- `asyncdb` = `["database-interface/asyncdb"]` → enables `DatabaseAsyncRef` +
  `WrapDatabaseAsync` (the bridge from async `AlloyDB` to sync `Database[Ref]`).
- **`dev` is NOT needed.** `dev` enables `optional_balance_check` /
  `optional_no_base_fee` (relaxes mainnet invariants). The "fund owner 100 ETH
  out of nowhere" override uses explicit `CacheDB::insert_account_info` (the
  owner is genuinely credited in the `CacheDB`), which keeps the sim a faithful
  mainnet execution — no balance-check bypass. Keep `dev` off.

**alloy deps for the PyO3-wrapper / standalone examples** (the `alloy` umbrella
does not expose `rpc-types-state` as a feature; use the granular crates, matching
revm's own `examples/uniswap_v2_usdc_swap/Cargo.toml`):

```toml
alloy-sol-types = { version = "1.5.2", features = ["std"] }
alloy-eips = "2.0"
alloy-provider = { version = "2.0", default-features = true }
alloy-primitives = { version = "1.5", default-features = false }
```

## 2. Layered composition API — THE unverified claim, resolved

### 2.1 `Either` does NOT do first-wins fallback (dead end for option B)

`revm::database_interface::either::Either<L, R>` impls `DatabaseRef`, but the
dispatch is **fixed-variant**, not fall-through-on-miss:

```rust
// revm-database-interface-41.0.0/src/either.rs (condensed)
fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
    match self {
        Self::Left(db) => db.basic_ref(address),   // always L
        Self::Right(db) => db.basic_ref(address),  // always R
    }
}
```

`Either::Left(BotStateDb)` returns `None` for an untracked contract and **never
tries `AlloyDB`**. So `Either` is "pick one of two DBs for the whole sim," NOT
"tracked first, RPC fallback." **Reject the `Either` candidate from the
feasibility doc.**

### 2.2 `CacheDB<ExtDB: DatabaseRef>` IS the canonical first-wins fallback (option A)

`CacheDB` (in `revm-database/src/in_memory_db.rs`) is the override-then-backing
composition. Its `DatabaseRef` impl:

```rust
fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
    match self.cache.accounts.get(&address) {
        Some(acc) => Ok(acc.info()),              // override wins
        None => self.db.basic_ref(address),        // fall through to backing
    }
}
fn storage_ref(&self, address: Address, index: StorageKey) -> Result<StorageValue, Self::Error> {
    match self.cache.accounts.get(&address) {
        Some(acc_entry) => match acc_entry.storage.get(&index) {
            Some(entry) => Ok(*entry),             // override wins
            None => self.db.storage_ref(address, index),  // fall through
        },
        None => self.db.storage_ref(address, index),
    }
}
```

So **option A** is the canonical revm pattern:

```rust
type InProcessDb = CacheDB<WrapDatabaseAsync<AlloyDB<Ethereum, DynProvider>>>;

let alloy_db = WrapDatabaseAsync::new(AlloyDB::new(provider, BlockId::from(block))).unwrap();
let mut cache_db: InProcessDb = CacheDB::new(alloy_db);
// seed overrides:
cache_db.insert_account_info(owner, AccountInfo { balance: one_eth, ..Default::default() });
cache_db.insert_account_storage(weth, balance_of_slot, U256::from(42))?;
// execute:
let mut evm = Context::mainnet().with_db(&mut cache_db).build_mainnet();
let res = evm.transact_one(TxEnv::builder().caller(owner).kind(TxKind::Call(weth)).data(calldata.into()).value(U256::ZERO).build().unwrap())?;
```

**This compiles + executes against a mainnet RPC** — verified by the 50-line PoC
(`/tmp/revm-spike/src/main.rs`, archived in this spike's record). Override honored
(`balanceOf` returned the inserted `42`, not the on-chain value, proving the
override-wins path fired and no RPC was issued for that slot).

### 2.3 Option B (`BotStateSnapshot: DatabaseRef`) — custom impl, ergonomic, no Mutex

For option B (engine state *is* the `Database`, `AlloyDB` cold-miss fallback),
write a hand `DatabaseRef` impl (NOT `Either`):

```rust
// in degenbot-evm (depends on degenbot-bot + revm)
pub struct BotStateDb<ExtDb: DatabaseRef> {
    snapshot: BotStateSnapshot,           // &Bot typed-state read view
    fallback: ExtDb,                      // WrapDatabaseAsync<AlloyDB<…>>
}

impl<ExtDb: DatabaseRef> DatabaseRef for BotStateDb<ExtDb> {
    type Error = ExtDb::Error;
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.snapshot.basic(address)? {       // typed-state encode-on-demand
            Some(info) => Ok(Some(info)),          // tracked → served from engine state
            None => self.fallback.basic_ref(address),  // untracked → RPC
        }
    }
    // storage_ref, code_by_hash_ref, block_hash_ref: same fall-through shape
}
```

- `DatabaseRef` is `&self` (vs `Database`'s `&mut self`) — **no `Mutex` needed.**
  `WrapDatabaseAsync<AlloyDB>` impls `DatabaseRef` via `&self` (it blocks on the
  async runtime internally), so the fallback borrows cleanly.
- Composes under `CacheDB<BotStateDb<WrapDatabaseAsync<AlloyDB>>>` for sim-scoped
  overrides *on top of* engine-state-as-DB:
  ```
  EVM transact → CacheDB (sim-scoped overrides)
                    → BotStateDb (engine typed state, encode-on-demand)
                    → WrapDatabaseAsync<AlloyDB> (RPC fallback for untracked)
  ```
- This is the move that delivers the standalone-Rust-core constraint: a `cargo
  add degenbot` consumer drives `Bot` (state owner) → `Bot` *is* the EVM's
  `Database` → in-process sim with no Python and no RPC-for-tracked-state.

### 2.4 Single-source-of-truth invariant (reaffirmed)

The typed fields in `Bot`/`V2PoolState`/`V3PoolState`/`V4PoolState` remain the
sole truth. `BotStateSnapshot::basic` / `storage` encode typed fields to EVM
slots **on demand** — no long-lived encoded copy. `CacheDB` above it caches only
for one simulation's duration. Same shape as a DB view: one underlying relation,
many projected access patterns. No drift.

## 3. Cold-miss latency profile (real numbers, local mainnet node)

PoC: `CacheDB<WrapDatabaseAsync<AlloyDB>>` against `http://host.containers.internal:8545/`
(chainId 1, mainnet, block ~25.6M). 7-call vector shape: 3 balanceOf-style reads
across 3 distinct cold contracts (WETH9, Multicall3, V4 PoolManager), each touched
multiple times (pre/post), mirroring the degenbot-simulation 7-call vector's
cold-account surface.

| Path | Wall-clock | RPC count | Notes |
|---|---|---|---|
| In-process 7-call, **COLD** (3 new contracts) | **8374 µs** | 9 (3 × `basic_async_ref`, each = 3 concurrent RPCs) | First sim of a block |
| In-process 7-call, **WARM** (persistent `CacheDB`) | **442 µs** | 0 | Every subsequent sim in the block |
| `eth_simulateV1` (1 call, batched) | **1415 µs avg** (583–4265) | 1 | The round-trip the in-process path replaces |
| Single `eth_getStorageAt` RTT | **496 µs** | 1 | Per-slot RPC baseline |

**Cold→warm speedup: 18.9×.** A warm in-process sim (0 RPC) is 3.2× faster than a
single batched `eth_simulateV1` RTT, and pays zero RPC for every subsequent sim in
the block's fan-out.

### 3.1 Cold-miss RPC count per block

The 7-call vector touches **3 constant contracts** (WETH9, Multicall3,
PoolManager) + the executor + path-specific pools:

- The **first sim of a block** warms the 3 constant contracts (8374 µs, 9 RPCs,
  once). The executor bytecode is injected via `insert_account_info` (never RPC).
- **Path-specific pools**: the engine OWNS their state (reserves, `slot0`,
  liquidity, tick-data). Their *code* is the same contract (V2 pair / V3 pool)
  reused across paths → warmed once. Under option A, pool *storage* is
  `insert_account_storage`'d from typed state per block; under option B, served
  on demand from typed state. Either way: **0 RPC for tracked pool state**.
- **Residual cold touches**: path-specific intermediate tokens not seen in prior
  blocks (1 token = 1 `basic_async_ref` = 3 concurrent RPCs ≈ 1 RTT). Rare after
  the first block warms the common tokens.

**Verdict on the decision rule** ("if cold-miss RPC count is <5/block after
seeding, option A wins; if >20/block, option B is justified"): **cold-miss RPC
count after seeding is <5/block → option A wins on the latency criterion.** The
latency numbers do NOT force option B's complexity.

### 3.2 The fork (raised to the operator — see §6)

The latency criterion says option A. But option B is architecturally cleaner
(no per-pool-per-block slot enumeration — `BotStateDb` answers any tracked slot
on demand; the ADR-003 "Bot is the EVM's `Database`" payoff; matches the "be
ambitious, encapsulate as much EVM machinery as we can" steer). Latency is
identical between A and B (both in-memory after first read); the differentiator
is architectural cleanliness + ambition, not performance. **This is a real fork
that the operator must decide** — see §6.

## 4. `code_by_hash` panic safety

`AlloyDB::code_by_hash_async_ref` **panics** ("This should not be called, as the
code is already loaded"). This is safe because `AlloyDB::basic_async_ref`
**eagerly loads code** and sets `code_hash = code.hash_slow()` in the returned
`AccountInfo`:

```rust
// revm-database-41.0.0/src/alloydb.rs
async fn basic_async_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
    let (nonce, balance, code) = tokio::join!(…, get_balance(…), get_code_at(…));
    let code = Bytecode::new_raw(code?.0.into());
    let code_hash = code.hash_slow();
    Ok(Some(AccountInfo::new(balance, nonce, code_hash, code)))  // code carried inline
}
async fn code_by_hash_async_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
    panic!("This should not be called, as the code is already loaded");
}
```

The EVM never calls `code_by_hash` when `basic` already provided `code` inline.
**`BotStateSnapshot::basic` (option B) must do the same** — return full
`AccountInfo { code }` with a correct `code_hash` for tracked contracts (the
injected executor bytecode is a `Bytes` arg; WETH9/V2/V3/V4 runtime bytecodes
are either known or fetched once + cached). Untracked contracts fall to
`AlloyDB`, which eagerly loads. **Confirmed: as long as every `basic*` path
eagerly loads code, the `code_by_hash` panic is unreachable.** No safe path
triggers it.

## 5. `State` journal for access-list emission (retire `eth_createAccessList`)

The `ExecuteEvm::transact` method (NOT `transact_one`) returns the post-execution
state, which carries the touched address + slot set:

```rust
// revm-handler-41.0.0/src/api.rs
fn transact(&mut self, tx: Self::Tx)
    -> Result<ExecResultAndState<Self::ExecutionResult, Self::State>, Self::Error>;

// revm-context-interface-41.0.0/src/result.rs
pub struct ExecResultAndState<R, S = EvmState> {
    pub result: R,            // ExecutionResult (gas, output, success/revert)
    pub state: S,             // EvmState — the touched set
}
pub type ResultAndState<H = HaltReason, S = EvmState> = ExecResultAndState<ExecutionResult<H>, S>;

// revm-state-41.0.0/src/types.rs
pub type EvmState = AddressMap<Account>;     // Address → Account
pub type EvmStorage = StorageKeyMap<EvmStorageSlot>;  // slot key → slot

// revm-state-41.0.0/src/account.rs (Account)
pub struct Account { pub info: AccountInfo, pub storage: EvmStorage, pub status: AccountStatus, … }
// Account::mark_touch() / AccountStatus flags the touched accounts.
```

**Access-list emission:** iterate `result.state.iter()` (an `AddressMap<Account>`);
for each `Account` with touched storage, collect the `storage.keys()` into an
`AccessListItem { address, storage_keys }`. No `eth_createAccessList` RPC. The
API surface is `ResultAndState.state` (an `AddressMap<Account>`), iterating
`.storage` per account for slot keys. `transact_one` discards the state — use
`transact` for the access-list path.

```rust
// degenbot-evm::access_list (sketch)
pub fn emit_access_list_from_state(state: &EvmState) -> AccessList {
    AccessList::from(state.iter().filter_map(|(addr, acc)| {
        let keys: Vec<B256> = acc.storage.keys().map(|k| B256::from(*k)).collect();
        (!keys.is_empty()).then(|| AccessListItem { address: *addr, storage_keys: keys })
    }).collect())
}
```

`OnStateHook` (`revm-database-interface::state_hook`) is the commit-hook variant
(`fn on_state(&mut self, state: EvmState)`) — useful if you want the state
*before* commit, but for access-list emission post-`transact` the `ResultAndState`
return is the cleaner API.

## 6. The fork: option A vs option B (raised to the operator)

The spike's checkpoint requires raising the option-A-vs-B decision before
implementing `degenbot-evm`:

- **Option A (seeded `CacheDB<WrapDatabaseAsync<AlloyDB>>`)** — latency-sufficient
  (<5 cold RPCs/block after warming), less code (no `DatabaseRef` impl, no
  storage-layout mapping, no `degenbot-bot` dep), but requires enumerating every
  pool slot per pool per block to `insert_account_storage` into the `CacheDB`
  (the `degenbot-executor` warmup logic already knows the slots; risk: a pool in
  the path but unseeded → silent RPC cold-miss, not divergence).
- **Option B (`BotStateDb: DatabaseRef` over engine state, `AlloyDB` fallback)** —
  architecturally cleaner (no slot enumeration; answers any tracked slot on
  demand; the ADR-003 "Bot is the EVM's `Database`" payoff; matches the "be
  ambitious, encapsulate as much EVM machinery as we can" steer), identical
  latency, but more code (custom `DatabaseRef` impl + storage-layout mapping for
  every pool type + `code_by_hash` eager-load invariant + the `BotStateSnapshot`
  type + crate dep on `degenbot-bot`) and commits deeper to ADR-003/014/016.

**Recommendation (spike):** Option B. The latency numbers do not *force* it, but
the operator's "be ambitious" steer + the architectural cleanliness (no
enumeration, no staleness for tracked pools, the standalone-core payoff) + the
diagnostic-sharing leverage (§"Share `BotStateSnapshot` with the diagnostic
recompute path") make it the move that encapsulates the most EVM machinery
correctly. Option A's per-pool slot enumeration is a recurring maintenance hazard
and a latent cold-miss source; option B closes it structurally.

**Operator decision required** (the standing instruction: stop at forks you
cannot resolve). This is unresolvable autonomously because the latency data says
A suffices while the ambition/architecture steer says B — the two pull opposite
directions on a one-time architectural commitment. Spike stops here; the
`degenbot-evm` crate-skeleton task (`4ZFZFF`) is unblocked for the shared
scaffolding (revm dep wiring, module stubs) regardless of the choice, but the
`BotStateSnapshot: DatabaseRef` task (`EGMSNS`) + the state-override adaptor's
backing shape (`RBCQTQ`) wait on this decision.

## 7. PoC reference

`/tmp/revm-spike/src/main.rs` (scratch, not committed to a crate) — ~80 lines.
Builds revm 41.0.0 + alloy 2.2.0, constructs `CacheDB<WrapDatabaseAsync<AlloyDB>>`
against the local mainnet node, inserts a fake owner-fund + WETH `balanceOf`
override, executes the 7-call vector cold + warm, prints the latency table in §3.
Representative output (block 25584325):

```
7-call cold (3 distinct contracts cold): 8374 µs
7-call warm (all code cached, 0 RPC):     442 µs
cold→warm speedup: 18.9×
eth_getStorageAt (1 RPC round-trip):     496 µs
cold-miss RPCs/block after warm: 0 (3 contracts warmed once, reused)
```
