# Executor command grammar

> **Status:** production (epic `463V2C`, ADR-029). The grammar is the sole
> command-stream encoder for every 2/3-hop arbitrage path the bot executes.
>
> **Code:**
> - `rust/crates/degenbot-executor/src/grammar.rs` — the production entry points.
> - `rust/crates/degenbot-executor/src/grammar_shape.rs` — the per-family Plan builders + `derive_shape`.
> - `rust/crates/degenbot-executor/src/grammar_ledger.rs` — the axis types + `LedgerValidator`.
> - `rust/crates/degenbot-executor/src/composers.rs` — `PathInfo`/`HopInfo`, `EncodeOptions`, the top-level `encode_cmd_stream`, and the `config_for_options` axis→config builder.
> - `rust/crates/degenbot-executor/src/config.rs` — the `execute(commands, config)` `uint256` config packing.
>
> **Decision record:** [ADR-029](../adr/ADR-029-executor-command-grammar-axes.md).

## What the grammar is

The `cmd_executor` contract is a tiny on-chain VM. It consumes a **command
stream** — a compact byte-encoded list of instructions (`V2_SWAP_COMPACT`,
`V3_SWAP_COMPACT`, `V4_SWAP_COMPACT`, `V4_UNLOCK`, `ERC20_TRANSFER`,
`V2_SWAP_CALC`, `V4_TAKE_DELTA`, `V4_SETTLE`, …) — and executes them against
the real Uniswap V2/V3/V4 pools. An arbitrage path is worthless without a
*correct* command stream that nets every pool's invariants while leaving the
executor with a profit.

The **grammar** is the Rust model that turns a solver result (an ordered set of
pools + the optimal input + per-hop outputs) into those bytes. It is a
**grammar** in the sense that a small, well-typed surface (axes + a `Plan` tree
+ a validator) generates the entire byte family — replacing ~35 previously
hand-written per-family encoders. Byte-parity with the retired hand-written
adapters is held by the golden-master suites (`tests/composers_parity.rs`,
`tests/composers_3hop_parity.rs`).

## Why it exists (the bugs the old model could not see)

The old model keyed a family by exactly **one** axis: the hop-protocol tuple
`(V2|V3|V4)^n`. Every other concern — where the stream's capital comes from,
where its profit goes, which hop wraps which `unlock`/callback, and the
delta/orderings that make a stream valid — was **implicit and hand-written
into each adapter**. That single axis failed to express the invariants, and two
real bug classes escaped into production:

1. **`D0` — take-before-credit (`v2_v2_v4` / `v2_v4_v4`).** The old adapter
   emitted `V4_TAKE_COMPACT(WETH)` **before** any swap created a positive
   PoolManager WETH delta, so real v4-core reverted with `"D0"`
   (`require(cur > 0)`). The root cause was a *funding-source* decision
   baked invisibly into the adapter — the leading V2 hop should have been
   self-funded, not flash-funded.

2. **Terminal-V2 über-draw (`2PT5HH` / path-182449).** A terminal exact-out
   `V2_SWAP_COMPACT` over-drains 1 wei and reverts with `UniswapV2: K`. The
   fix is the *terminal-V2 pre-fund rule*: pre-grant the pair its input, then
   `V2_SWAP_CALC` (swap from whatever the feeder delivered). That is an
   ordering invariant the old grammar had no vocabulary to express.

Byte-parity alone could not catch these: it derives expected bytes from the
same `enc_*` primitives the composer uses, so it is **self-referential** and
blind to ordering defects. The grammar fixes the root cause by making the
invariants **unrepresentable** — a `LedgerValidator` rejects any stream that
violates **credit-before-debit within a ledger**, *before* the bytes ever reach
the contract.

## The four design axes (ADR-029 D1)

A family is no longer a bare protocol tuple. It is a **shape class** over
independent dimensions:

| Axis | Type | Who chooses | Today's honored values |
|------|------|-------------|------------------------|
| **hop-protocol sequence** | `Vec<Prot>` (V2/V3/V4) | the solver (path) | all 2/3-hop Uniswap combinations + all-V2 any-N |
| **funding source** | `FundingSource` | strategy/operator, **per-path, runtime** | `InPathFlash` (default), `SelfFund`, `PmLedger` (pure-V4), modeled: `ExternalLender`, `Erc6909BurnToSettle` |
| **profit capture** | `ProfitCapture` | strategy/operator, **per-path, runtime** | `Custody` (default), `Erc6909`, `Native`, `SweepToAddress`; modeled: `Owner`, `BalancerVault` |
| **builder bribe** | `Bribe` | strategy/operator, **per-path, runtime** | `None` (default), `Some { bips, recipient_idx }` |

**Funding source is an economic knob**, not a build-time constant. Self-fund =
cheaper gas for small opportunities (no flash-callback overhead, no
flash-repay transfer); InPathFlash = access to outside capital for large ones.
The operator selects it per path; the grammar honors it (the all-V2 SelfFund
path emits chained `V2_SWAP_CALC` with a top-level pre-fund; the InPathFlash
path emits `V2_SWAP_COMPACT` with the callback repay).

## How it replaces the hand-written encoders

### The old model: 35 bespoke adapters, no shared invariant

Before this epic, every `(protocol)^n` family had its own hand-written
`encode_cmd_*` body. Adding a new DEX meant writing a fresh adapter for **every
position × every neighbor × every funding × every capture** cell — the
combinatorial explosion the grammar kills. Worse, the ordering invariants
(credit-before-debit, terminal-V2 pre-fund) lived invisibly inside each body;
nothing enforced them generically, so the two bug classes above escaped.

### The new model: Plan tree → bytes + validator, one representation

Each family's ledger decisions are authored as an execution-ordered,
callback-nested **`Plan` tree** (ADR-029 D4 (iii), task `BP7KIR`). The nesting
**is** execution order: a `FlashSwap`'s `callback` step fires when the swap
runs (depth-first walk); a `V4Unlock`'s `inner` runs inside the unlock callback.
Two consumers derive from the **same** Plan:

- **the encoder** — `plan_to_bytes(&plan, &at) -> Vec<u8>` walks the tree
  depth-first and emits the `enc_*` byte primitives (callback subtrees
  serialized as their `FlashSwap`'s callback payload).
- **the validator** — `plan_to_ledger_ops(&plan) -> Vec<LedgerOp>` walks the
  tree depth-first (same order) and projects it to the `LedgerOp` IR that
  `LedgerValidator` enforces.

One representation, no drift, no reordering, no per-family trace duplication.

The `PlanStep` variants are the **declarative ledger facts** (which ledgers a
step touches, in which direction, for how much, with the address-table index
already resolved): `FlashSwap`, `Erc20Transfer`, `V2SwapCalc`, `V4Unlock`,
`V4Swap`, `V4TakeDelta`, `V4TakeCompact`, `V4Settle`/`SettleDelta`/`SettleAll`,
`V4Sync`, `V4Batch`, `V4Mint`, `WethDeposit`/`WethWithdraw`,
`NativeTransfer`, `SelfFund`. Each step carries BOTH the resolved
address-table index (for the byte encoder) AND the currency/pool address
(for the `LedgerOp` projection) so the two consumers never diverge.

Per-family `build_v2v3_plan` / `build_v3v2_plan` / `build_v3v3_plan` /
`build_v2v4_plan` / … author the Plan tree for that family's shape class;
`derive_shape` dispatches to them and returns the encoded bytes.

### The ledger validator — where the invariants live

`LedgerValidator` is a stateful walker over the `LedgerOp` IR. It models five
accounting locations (ADR-029 D2, an **open set**, never a closed enum):

- `Erc20(token)` — the executor's own balance per token (incl. WETH). Extended
  by V2/V3 flash swaps and self-fund; consumed by `Erc20Transfer`.
- `Native` — the executor's native ETH balance.
- `Pm(token)` — the PoolManager delta (positive = PM owes executor).
- `Erc6909(token)` — the executor's held PM claim per currency.
- `PairHandoff(pool)` — tokens deposited into a V2 pair but not yet in reserves.
- `External(&'static str)` — (extension) a Balancer Vault / Aave lender, via
  the `BalanceLedger` trait.

It rejects — on the **first** violating op, before any byte is emitted — any
stream that:

- **`D0`:** debits `PM[currency]` (a `V4_TAKE*`/`V4_MINT*`) before a prior swap
  left `PM[currency] ≥ amount` (`ValidationError::TakeBeforeCredit`). This is
  the pre-fix `v2_v2_v4`/`v2_v4_v4` bug, now unrepresentable.
- **terminal-V2 über-draw:** issues a `V2_SWAP_CALC` for a pair that was never
  seeded (`ValidationError::SwapCalcBeforeCredit`). This is the `2PT5HH`
  / path-182449 class, now unrepresentable.
- **flash-repay-before-credit:** an `Erc20Transfer` debiting the executor's
  `Erc20[currency]` before a prior flash extended it
  (`ValidationError::Erc20TransferBeforeCredit`). Surfaced by the V2/V3
  flash-credit chain — byte-parity cannot see this ordering defect.
- **flash-debt-unpaid:** a flash debt left outstanding at `finish()`
  (`ValidationError::FlashDebtUnpaid`) — the V2/V3 analogue of the V4
  "every delta nets to zero by callback end" invariant.
- **PM-delta-nonzero:** a `V4_UNLOCK` closed with a nonzero `PM[currency]`
  delta (`ValidationError::PmDeltaNonzero`) — the V4 master invariant.
- **native-transfer-before-credit:** a native pay-in debiting `Native` before a
  `WethWithdraw` or native V4 take produced it
  (`ValidationError::NativeTransferBeforeCredit`).

### Why this is superior to the hand-written adapters

| Property | Old (hand-written adapters) | New (grammar + Plan + validator) |
|----------|----------------------------|-----------------------------------|
| **invariants enforced** | implicit, per-body — invisible | explicit, generic — the `LedgerValidator` over declarative facts |
| **bug surface** | every adapter can re-introduce D0 / über-draw | the validator rejects them *before bytes exist* |
| **testability of the ordering property** | per-family spot-checks only | one generic validator, exhaustively testable over every `(protocol × funding × capture × bribe)` combination |
| **adding a DEX family** | a fresh adapter for every `(position × neighbor × funding × capture)` cell — combinatorial explosion | **one** axis value + one `BalanceLedger` impl + per-protocol mechanics — additive, no cross-matrix fan-out (ADR-029 D6) |
| **byte-parity's role** | the *only* gate — self-referential, blind to ordering | a weak cross-check; the runtime matrix (actual execution through the contract) is the source of truth (D5) |
| **surface area** | ~35 bespoke `encode_cmd_*` bodies | one `derive_shape` dispatcher + `build_*_plan` authors + one encoder + one validator |

## The hybrid split (ADR-029 D4)

The grammar is **not** a magic data-DSL. It is a deliberate hybrid:

- **declarative coupling/ledger facts** — the `PlanStep` tree: which ledgers a
  step touches, its forward currency, and its coupling role at each boundary.
  These are *data* the validator reasons over generically.
- **per-protocol mechanics** — the `enc_*` byte primitives and callback wiring
  (`V2_SWAP_COMPACT` payload layout, `V4_UNLOCK` nesting) are *code* behind
  the encoder. Solidity callback return-wiring is imperative; faking it as
  data would be its own bug farm (the fully-declarative option ADR-029 rejects).

This split is the choice **most testable for the ordering property**: a generic
validator over declarative facts makes "bad command streams impossible to
write" testable, while imperative mechanics stay where they belong (per-protocol
code, testable in isolation).

### What "derived" means here (clarified by the `6ZIE5X` decision)

The `derive_*` family emitters are **Plan-derived bytes**: each family's stream
is produced by building a `Plan` then encoding it (`plan_to_bytes`), and the
validator gates the Plan. They are **not** data-driven byte synthesis from a
single `ShapeClass` descriptor over every family — the V2/V3 2-hop slice is
genuinely `ShapeClass`-driven (`derive_2hop`), while the V4-involving families
are authored per-family `build_*_plan` functions whose Plan is then validated
by the same generic gate. The deliverable (D4) is the **generic validator
proving ordering from declarative facts**; emitters are per-protocol mechanics.

### What was retired

The cutover/`debug_assert` oracle and the ~32 proven adapter functions it
guarded were retired in `WAYDTL` once `derive_shape` covered every family
byte-identically. The single retained hand-written emitter is `v2_v2_v2` (the
all-V2 3-hop layout — a deliberate routing split: `encode_cmd_stream` routes
any-N all-V2 to the N-hop speedrail first; the 3-hop layout is structurally
distinct, reached only via the test-only `encode_cmd_3_hop` entry). There is
**no hand-written backstop**: a family either derives via `derive_shape` or it
does not encode.

## The runtime matrix is the source of truth (ADR-029 D5)

Correctness is judged by **actual execution through the on-chain contract**:
the runtime harness (`rust/crates/degenbot-simulation/tests/harness_declarative.rs`)
runs the production encoder methods, executes the stream in revm, and asserts
`actual_delta == predicted` exactly. The matrix **also validates every produced
Plan through the ledger validator** (`Plan → LedgerOp` depth-first walk, then
`LedgerValidator::validate_full`) and asserts the ordering invariants hold.

Byte-encoding is an implementation detail handled by the encoder methods the
matrix calls, so a future executor revision (new commands, new byte layout) is
absorbed by those methods without re-validating bytes. A future generic
`ShapeClass`→Plan walker (`6ZIE5X` branch (a), deferred) may build on
Plan-authored families; per-family Plan authoring is sufficient to deliver D4/D5
today.
