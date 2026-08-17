# Executor grammar — usage guide

> **Companion to:** [executor-command-grammar.md](executor-command-grammar.md)
> (the *why* and *how it's built*) and [ADR-029](../adr/ADR-029-executor-command-grammar-axes.md)
> (the decision record). This page is the *how to use it*.
>
> **Crate:** `degenbot-executor`. All examples compile against
> `rust/crates/degenbot-executor/`.

## The production entry point: `encode_cmd_stream`

The bot builds a command stream for an arbitrage path with one function:

```rust
use degenbot_executor::composers::{
    encode_cmd_stream, config_for_options, EncodeOptions, HopInfo, PathInfo, V2HopInfo,
};

let cmd_bytes = encode_cmd_stream(
    &path.path_info,            // the ordered hops (PathInfo)
    path.optimal_input,         // entry capital (u128)
    &path.hop_outputs,          // per-hop forward outputs (solver-computed)
    &path.consumed_inputs,      // per-hop executable input (CL-clamped)
    ctx.executor_address,      // the cmd_executor contract
    ctx.pool_manager_address,   // the Uniswap V4 PoolManager
    ctx.weth_address,           // WETH9
    path.opts,                  // EncodeOptions — the per-path axes
);
```

`encode_cmd_stream` routes the path:

- **all-V2 any-N** → `grammar_shape::derive_all_v2` — the any-N flash-borrow +
  chained `V2_SWAP_CALC` Plan layout (one producer: the `facts_of_all_v2` arm
  of `build_walk`; the distinct all-V2 3-hop layout and the N-hop speedrail
  were deleted in `4JOWO5`, collapsing onto this layout).
- **every other 2/3-hop family** → `grammar::encode_grammar` →
  `grammar_shape::derive_shape` — one `recognized_key` gate, then the single
  facts-driven `build_walk` pipeline (`facts_for` → `derive_plan` → the shape
  modules → `LedgerValidator` gate → `plan_to_bytes`). Again: **no
  hand-written backstop.**

`encode_cmd_3_hop` survives only as a `#[doc(hidden)]` test shim over
`encode_grammar` — it produces no distinct byte layout.

Returns `None` if the family is unknown or any `enc_*` step fails (e.g. a
`u128` that does not fit the contract's `uint96`/`int128` range — guard with
`fits_int128` upstream).

## The per-path axes: `EncodeOptions`

`EncodeOptions` carries the runtime economic choices the strategy/operator
makes **per path** (ADR-029 D1). All default to inactive/custody:

```rust
use degenbot_executor::grammar_ledger::{
    Bribe, FundingSource, ProfitCapture,
};

let opts = EncodeOptions {
    erc6909_profit: false,                      // legacy alias for capture=Erc6909
    use_v4_batch: false,                         // bundle pure-V4 swaps in one PM extcall
    funding: FundingSource::SelfFund,            // executor holds the entry WETH
    capture: ProfitCapture::Erc6909,             // mint profit as an ERC6909 claim
    bribe: Bribe::Some { bips: 50, recipient_idx: 0 }, // 0.5% to block.coinbase
};
```

| Field | Type | Honored today |
|-------|------|---------------|
| `funding` | `FundingSource` | yes — `SelfFund` on V2-led paths; `InPathFlash` default. Modeled on others. |
| `capture` | `ProfitCapture` | yes — routes to `check_mode` via `config_for_options`. |
| `bribe` | `Bribe` | yes — routes to `bribe_bips`/`recipient_idx` via `config_for_options`. |
| `erc6909_profit` | `bool` | legacy alias; forces `ProfitCapture::Erc6909` via `resolve_axes`. |
| `use_v4_batch` | `bool` | yes — `V4_BATCH` for pure-V4 paths (single PM extcall). |

`resolve_axes(opts)` collapses the legacy `erc6909_profit` bool into `capture`
(backwards-compatible: `erc6909_profit: true` forces `Erc6909` regardless of
the `capture` field; leaving it `false` and setting `capture` directly is
honored).

## The config builder: `config_for_options`

`execute(commands, config)` takes a packed `uint256` config. Build it from the
same `EncodeOptions`:

```rust
let execute_config = config_for_options(path.opts, U256::ZERO);
```

`config_for_options` reads the full axis set:

- `capture` → `check_mode` (ADR-029, U3WVLL):
  - `Custody`/`Native`/`Owner`/`BalancerVault` → `check_mode=1` (WETH+ETH
    combined-balance assert — **active by default**, the on-chain money-loss
    protection).
  - `Erc6909` → `check_mode=2` (ERC6909 WETH claim).
  - `SweepToAddress` → `check_mode=3` (SWEEP — defeats the assert for the rare
    "send accumulated profit to another address" case).
- `bribe` → `bribe_bips` + `bribe_recipient_idx` (`None` = (0, 0); `Some{bips,
  recipient_idx}` is forwarded — `recipient_idx 0` = `block.coinbase`).
- `expected_value` is **IGNORED** (kept in the signature for ABI compat): the
  U3WVLL contract fix made the executor read its **own** combined balance at
  start+end, so the operator no longer supplies the pre-tx balance.

> **Defect history (U3WVLL).** The old default was `check_mode=0` — the profit
> assert was *skipped by default*, a footgun that let a money-losing tx execute
> silently. `config_for_options` makes `check_mode=1` the default; a
> money-losing production path now reverts on-chain. The sweep opt-in
> (`check_mode=3`, task `767TN5`) is the sole explicit defeat path.

For low-level packing (or to bypass the axis builder), see
`degenbot_executor::encoders::pack_config(check_mode, expected_value,
bribe_bips, bribe_recipient_idx)` in `config.rs`.

## The ABI wrap: `encode_execute_call`

Wrap the command stream in the `execute(bytes, uint256)` ABI call:

```rust
use degenbot_executor::composers::{encode_execute_call, EncodedCall};

let call: EncodedCall = encode_execute_call(
    ctx.executor_address,
    &cmd_bytes,
    execute_config,
).expect("ABI encode");
// call.to / call.data / call.value — ready for submission.
```

## Putting it together — the production call site

This is exactly what `degenbot-arbitrage::simulator` does (task
`Q35IJN`):

```rust
// 1. Encode the cmd_executor command stream (YQORTM).
let cmd_bytes = encode_cmd_stream(
    &path.path_info,
    path.optimal_input,
    &path.hop_outputs,
    &path.consumed_inputs,
    ctx.executor_address,
    ctx.pool_manager_address,
    ctx.weth_address,
    path.opts,
)?;
// 2. Build the axis-aware config (Q35IJN).
let execute_config = config_for_options(path.opts, U256::ZERO);
// 3. Wrap in execute(bytes, uint256).
let execute_calldata = encode_execute_call(ctx.executor_address, &cmd_bytes, execute_config)?;
```

See `rust/crates/degenbot-arbitrage/src/simulator.rs` (~`build_execute_tx`)
for the full production path, including the pre/post balance reads that prove
profitability.

## Building a `PathInfo`

A `PathInfo` is the ordered hop descriptors:

```rust
use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};

let path = PathInfo::new(vec![
    HopInfo::V2(V2HopInfo {
        pool_address, token0_address, token1_address, fee, zfo,
    }),
    HopInfo::V3(V3HopInfo {
        pool_address, token0_address, token1_address, fee, zfo,
    }),
    HopInfo::V4(V4HopInfo {
        pool_manager_address, pool_id_hex,
        currency0_address, currency1_address,
        fee, tick_spacing, hook_address, zfo,
    }),
]);
```

The `HopInfo` variants carry exactly the fields the encoder needs — pool
address, direction (`zfo` = zero-for-one), fee, and the token0/token1 (or
currency0/1) addresses. `V3HopInfo.fee` is informational (V3 fees are encoded
in the pool address). `V4HopInfo` carries the PoolManager address, the
`pool_id_hex`, the pool-key fields (`fee` uint24, `tick_spacing` int24,
`hook_address`), and the currencies. `v4_input_is_native` / `v4_output_is_native` classify the native-ETH
representation gap that `CurrencyBridge` resolves at a V4↔X boundary.

## The currency bridge (native-ETH ↔ WETH)

V4 tracks native ETH and WETH as **distinct** delta currencies. When a path's
hop A outputs one and hop B's input expects the other, an explicit
`WETH_DEPOSIT` (wrap) or `WETH_WITHDRAW` (unwrap) must bridge the gap inside
`V4_UNLOCK` before hop B runs:

```rust
use degenbot_executor::composers::CurrencyBridge;

let bridge = CurrencyBridge::at_boundary(output_currency_a, input_currency_b);
if bridge.needs_bridge() {
    let (take_idx, settle_idx) = bridge.bridge_indices(weth_idx, native_idx);
    // emit V4_TAKE_COMPACT(take_idx → SELF) + (WETH_DEPOSIT | WETH_WITHDRAW)
}
```

`CurrencyBridge::None` (both sides agree) needs no bridge. The walker emits
the bridge steps automatically at V4↔X boundaries from the facts' currency
pairing (the `V4TakeCompact` + `WETH_DEPOSIT`/`WETH_WITHDRAW` sequence — see
`grammar_shape::v4_bridge_steps`, used by the shape modules).

## Validating a Plan directly (the invariant gate)

The validator is the **reason** the grammar exists — it makes the two bug
classes unrepresentable. Author/build a Plan, project it, and validate:

```rust
use degenbot_executor::grammar_ledger::{LedgerValidator, LedgerOp};

let mut v = LedgerValidator::default();
for op in &ops {               // ops: Vec<LedgerOp> from plan_to_ledger_ops(&plan)
    v.push(*op)?;              // fails on the FIRST credit-before-debit violation
}
v.finish()?;                   // every flash debt must be repaid
```

or the all-in-one convenience:

```rust
v.validate_full(&ops)?;        // push all + finish
```

The error variants name the invariant that fired:

| `ValidationError` | The invariant | The bug class it kills |
|-------------------|--------------|------------------------|
| `TakeBeforeCredit` | `PM[currency] ≥ amount` before a `take`/`mint` | pre-fix `v2_v2_v4` / `v2_v4_v4` |
| `SwapCalcBeforeCredit` | pair seeded before `V2_SWAP_CALC` | `2PT5HH` / path-182449 über-draw |
| `Erc20TransferBeforeCredit` | `Erc20[currency] ≥ amount` before a debit | V2/V3 flash-repay-before-credit |
| `FlashDebtUnpaid` | flash debt zero at `finish()` | unpaid in-path flash |
| `PmDeltaNonzero` | every `PM[cur]` nets to zero at `V4UnlockEnd` | unresolved PM delta |
| `NativeTransferBeforeCredit` | `Native ≥ amount` before a native pay-in | native-settle gap |

For external ledgers (the additive-capability proof, ADR-029 D6), construct
the validator with the external ledgers and the
`ExternalFlash`/`ExternalRepay` ops route to them via the `BalanceLedger`
trait:

```rust
use degenbot_executor::grammar_ledger::{LedgerValidator, ExternalLedger};

let v = LedgerValidator::default()
    .with_external_ledgers(vec![ExternalLedger::default()]); // index 0 = a Balancer-shaped Vault
```

## The Plan tree (advanced — supporting a new family)

A new family is **no longer authored as code per family**. The authoring
surface is:

1. **Add the facts row in `grammar_walker::facts_for`** — the per-hop
   `HopFacts`: protocol variety, direction (`zfo`), pool/fee/spacing data,
   the coupling axes the family exercises (`out_dest`, `repay`, and — only
   where a consumer shape body reads it — `terminal_form` /
   `repay_mechanism` / `seed_delivery`).
2. **If the enclosure is new, teach a shape body.** The 3-hop topology rules
   (`rule_walk_v2v3` / `rule_walk_v4_led` / `rule_walk_v2v3_v4_mixed` in
   `grammar_walker/shapes/three_hop.rs`) derive the nesting from the facts'
   debt-flow threading (rules 1–3, stated on the walker fns in
   `grammar_walker/shapes/three_hop.rs`). A genuinely new DEX
   primitive lands as one constructor in `grammar_walker::mod mechanics`.
3. **The validator is the gate.** `derive_shape_detailed` runs the
   `LedgerValidator` on every produced Plan; an ordering defect is a fatal
   `Rejected` (ADR-030), never silently dropped — so the new family either
   produces an invariant-clean Plan or produces nothing. Byte-identity /
   execution grounds come from the golden suites + the revm runtime matrix.

The Plan tree is an execution-ordered, callback-nested tree whose leaves
carry BOTH the resolved address-table index (for the byte encoder) and the
currency/pool address (for the `LedgerOp` projection). Inspecting one
directly:

```rust
use degenbot_executor::grammar_plan::{plan_to_bytes, plan_to_ledger_ops};
use degenbot_executor::grammar_walker::build_walk;

let (preamble, plan, at) = build_walk(&path, &inputs)?;
// Same Plan → two consumers, no drift:
let mut bytes = preamble;
bytes.extend_from_slice(&plan_to_bytes(&plan, &at));
let ops: Vec<_> = plan_to_ledger_ops(&plan);   // feed to LedgerValidator
```

A `PlanStep::FlashSwap { callback, auto_repay, .. }` nests its callback subtree
(the bytes that fire when the swap runs); `auto_repay=true` models the
empty-callback V2/V3 flash whose `in_currency` the contract auto-pays from the
executor's balance at callback-end. `V4Unlock { inner, .. }` nests its unlock
callback and emits `V4UnlockEnd` after it. Depth-first walk = execution order.

The full `PlanStep` variant set lives in
`rust/crates/degenbot-executor/src/grammar_plan.rs`; the axis vocabulary a
new family can consume is documented on the axis enums in
`rust/crates/degenbot-executor/src/grammar_walker.rs` (`TerminalForm`,
`RepayMechanism`, `SeedDelivery` — each field's doc comment states when it is
set and who consumes it) and in the `CONTEXT.md` walker glossary.

## Where to look next

- **The architecture doc:** [executor-command-grammar.md](executor-command-grammar.md) — the *why* and the invariant model.
- **The decision records:** [ADR-029](../adr/ADR-029-executor-command-grammar-axes.md) (the axes, the hybrid, the additive proof), [ADR-030](../adr/ADR-030-derivation-outcome-tri-state.md) (the derive tri-state), [ADR-031](../adr/ADR-031-executor-plan-walker.md) (the facts-driven walker).
- **The walker lineage:** [executor-walker-spike.md](executor-walker-spike.md) (the spike that de-risked the schema) and ADR-031's Resolution paragraph (the T1–T6 decomposition record; the standalone ledger doc was removed in the stale-docs cleanup `71ec78b2`).
- **The V4 ledger rules:** [`grammar_ledger.rs`](../../rust/crates/degenbot-executor/src/grammar_ledger.rs) — the PM net-zero-at-unlock-close invariant (the V4 master rule, stated on the V4 ledger ops).
- **The Plan-tree decision:** [executor-command-grammar.md](executor-command-grammar.md) §"What 'derived' means here (the `6ZIE5X` decision, realized)" — why the Plan tree (mechanism (iii)) over byte-decoding / `enc_*`-instrumentation.
- **The model record:** [ADR-029](../adr/ADR-029-executor-command-grammar-axes.md) (the axes + open-ledger model) and [executor-command-grammar.md](executor-command-grammar.md) (the realized architecture that the model plan doc preceded).
- **The tests of record:** `tests/composers_parity.rs`, `tests/composers_3hop_parity.rs` (golden-master byte-parity), `rust/crates/degenbot-simulation/tests/harness_declarative.rs` (the runtime matrix), and the `rule_walker_shadows_*` unit tests in `grammar_walker/shapes/three_hop.rs` (post-cutover pinning of the current enclosure per topology-rule group).
