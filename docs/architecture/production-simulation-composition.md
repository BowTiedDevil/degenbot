# Production simulation composition — block facts, injected deployment

The canonical simulation path is adapter-driven (ADR-098 D2/D3). This page is
the **migration record** for the cutover that removed the direct
`encode_cmd_stream` call and the hardcoded WETH/ETH/ERC6909 settlement vector
from the production seam, and it states what a consumer must now supply.

## The shape

```text
SimulateContext {
    block: SimulationBlock { provider, base_fee_next, current_block,
                             block_timestamp, block_priority_fees },
    deployment: &SimulationDeployment,
}
```

- **Block-wide facts** (`SimulationBlock`) are one set per simulated block: the
  provider the engine's cold-miss fallback reads through, the block
  environment, and the fee samples. Every candidate in a fan-out shares them.
- **Deployment facts** (`SimulationDeployment`) are one per execution
  deployment: its `ExecutionAdapter` (Encode / Probe / Assess), the account
  whose key sends the opportunity's transaction, the top-level gas limit, any
  **declared code override**, and the **settlement warmup** it declares for
  WETH/PoolManager. A foreign contract declares no settlement identity at all
  (`settlement() == None`) and crosses the same path.

The seam order in `degenbot-arbitrage::simulate_path_on_evm` is
`adapter.encode` → `degenbot-simulation::simulate_transaction` (the engine:
declared probes, the transaction, gas, return data, trace, swap capture, access
list) → `adapter.assess` → the deployment's fee policy → the strategy's
categorization. Nothing in `degenbot-arbitrage::simulator` or `dispatch` calls
`encode_cmd_stream`, `config_for_options`, or the settlement calldata builders;
the built-in `cmd_executor` deployment is one `CmdExecutorBinding` in
`degenbot-arbitrage::cmd_executor`.

### Fee sizing is a deployment policy, adapter-owned by default

The adapter's Assess carries Fee as its pricing half (ADR-025 D2), so a
deployment returns its adapter's `ExecutionResult` unchanged — priority fee,
net, threshold, and verdict — unless it explicitly says otherwise:

```text
SimulationDeployment::new(adapter, owner, gas_limit, code, settlement)
    .with_fee_sizing(FeeSizing::MarketAware)   // the ONLY opt-in
```

- `FeeSizing::AdapterOwned` (the default) — the adapter's own Fee decision is
  the result. A foreign adapter's custom priority fee, threshold, and net
  survive the canonical fan-out untouched.
- `FeeSizing::MarketAware` — the deployment re-prices the adapter's gross figure
  with the settlement strategy's `compute_priority_fee` and the dispatch gas
  floor (`simulator::price_gross`). `CmdExecutorBinding::{deployment, injected}`
  select it, which is how the built-in settlement path keeps its market-aware
  sizing without the generic seam inferring anything from the adapter's type.

The seam never inspects the adapter: there is no adapter-type branch and no
`cmd_executor` branch in the generic simulator.

### The assessment verdict travels to dispatch

`SimResult::passed` carries the deployment's FINAL assessment verdict verbatim;
dispatch never re-derives it. `SimResult::is_submittable()` joins that verdict
with the dispatch gas floor (`passed && net >= MIN_PROFIT_NET`), and
`DispatchOutcome` categorizes with it:

- `gas_profitable` — submittable results only: the deployment's assessment
  passed and the net clears the floor.
- `gas_unprofitable` — returned results that did not pass: below the gas floor,
  or declined by the deployment's own verdict. A gross-positive failed
  assessment stays visible here (the "every gross-profitable path stays
  visible" policy) and never becomes a submit candidate.

## Built-in economics that did NOT move

The built-in binding reproduces the settlement pipeline's bytes and arithmetic
exactly, and the golden fixtures pin them:

- the same three declared reads in the same declared order — WETH9
  `balanceOf(executor)`, Multicall3 `getEthBalance(executor)`, PoolManager
  ERC6909 `balanceOf(executor, weth_id)` — executed as steps 0-2 and 4-6 around
  the `execute(bytes,uint256)` transaction at step 3, so the reported
  `fail_index` taxonomy (pre-balance / execute / post-balance) is unchanged;
- the same command stream and packed config for the session's `EncodeOptions`,
  and the same one deployed executor as the target with a zero native value;
- the same gross figure, `(weth + eth + erc6909)_after − (…)_before`;
- the same market-aware priority fee and gas floor, applied **once** in
  `simulator::price_gross` through the shared `ExecutionResult::price` helper —
  reached because `CmdExecutorBinding` declares `FeeSizing::MarketAware` (ADR-098
  D7 puts gas profitability and operator margins off-chain, above the adapter's
  Assess — the seam's `FeePolicy` documents itself as the stand-in for this
  strategy-side math, and a foreign deployment keeps its own `FeePolicy`);
- the same dispatch policy: bounded fan-out, staleness, suppression, thin
  margin, revert diagnostics, ordered submission, single target.

## Consumer-visible migrations

| Old surface | New surface | Why |
|---|---|---|
| `SimulateContext { provider, executor_owner, executor_address, weth_address, pool_manager_address, multicall3_address, inject_code, injected_address, runtime_bytecode, warmup, base_fee_next, current_block, block_timestamp, block_priority_fees }` | `SimulateContext { block: SimulationBlock { provider, base_fee_next, current_block, block_timestamp, block_priority_fees }, deployment: &SimulationDeployment }` | one simulated block's facts beside one deployment's facts (ADR-098 D3) |
| `SimulateContext::override_params()` / `encode_context()` | `SimulationDeployment::override_params()`; the encode context is the built-in binding's | the engine override projection and the encode intake are deployment facts |
| `DispatchCandidate.opts`, `SimulatePath.opts` | `CmdExecutorBinding::new(..., opts)` / `with_opts(opts)` | one authority for the cmd config axes (the injected deployment), never a per-candidate copy |
| `SimulatePath::encode_request()` | gone — the seam hands the adapter `PathInfo` + `ComposerInputs` (`SimulatePath::expected_outputs_vec` / `consumed_inputs_vec`) | Encode belongs to the adapter |
| `SimulationOverrideParams { owner, inject_code, injected_address, runtime_bytecode, warmup, weth_address, pool_manager_address }` | `SimulationOverrideParams { owner, code: Option<DeclaredCode>, settlement: Option<SettlementWarmup> }` | declared code is an explicit override (production runs deployed code); the settlement warmup is a declaration, not a required pair of addresses |
| `PrefetchFacts { weth_address, pool_manager_address, warmup }` | `PrefetchFacts { settlement: Option<SettlementWarmup> }` | the same declaration drives the executor-side prewarm words; a deployment with no settlement identity derives only path words |
| `warmup: WarmupSlots` computed by the driver shell | `CmdExecutorBinding::{deployment, injected, load_runtime}` | warmup math + runtime-artifact selection are the built-in binding's |
| fee sizing implied by the settlement strategy for every deployment | `SimulationDeployment::fee_sizing()` / `with_fee_sizing(FeeSizing)`; `AdapterOwned` is the `new` default, `MarketAware` is the built-in binding's explicit opt-in | Fee belongs to the adapter's Assess; only the deployment that chooses market-aware sizing is re-priced |
| `SimResult { …, net_profit, … }` with no verdict | `SimResult { …, passed, … }` + `SimResult::is_submittable()`; `DispatchOutcome::gas_profitable` holds only submittable results | the deployment's assessment verdict survives to dispatch/submission instead of being re-derived from the net |
| `CmdExecutorBinding::injected(address, runtime) -> SimulationDeployment` | `CmdExecutorBinding::injected(address, runtime) -> Result<SimulationDeployment, InjectionAddressMismatch>` | the built-in adapter encodes to its executor, so a declared override at any other address would simulate code the transaction never runs; the Python `SimulateContext` constructor keeps its signature and refuses a mismatch with `ValueError` |
| `degenbot_arbitrage::SimulateContext` constructed by `ExecutorRuntime` flat fields | `ExecutorRuntime.binding` + `ExecutorConfig.deployment` | the batch executor injects one deployment per session |

The Python `SimulateContext` pyclass keeps its constructor signature: it builds
the built-in binding and loads the artifact through it. `degenbot_rs` exposes no
new/removed Python name.

## Deliberately unchanged: built-in receipt assessment

The built-in contract's own `ExecutionReceipt { gross_profit, bribe_paid }`
decode (ADR-098 D7) is **not** wired here. The seven-call balance bundle remains
the built-in assessment evidence until receipt parity is proven (task XTKNA6);
this chunk only moved that bundle onto the seam as declared
`ProbeSpec` data. Deployment identity / multi-target fan-out (DKQZWP) and
version-capability policy (OZCAUU) are likewise out of scope: `executor_address`
on the batch-executor config is still the single-target join stamp.

## Evidence honesty note

A probe read that yields no single ABI word — a revert, a halt, an empty
return, or a multi-word return — is an **absent** reading, never a fabricated
zero (`degenbot-simulation::sim::production`). The built-in adapter reports such
an observation as `AssessError::Evidence`, and the settlement seam categorizes
it as `balance-decode`. Fixtures that model the settlement bundle must deploy
code at WETH9 / PoolManager / Multicall3, as the chain does; the parity
fixtures do.
