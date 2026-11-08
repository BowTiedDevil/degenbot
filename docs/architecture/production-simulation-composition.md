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

## Built-in economics

The built-in binding reproduces the settlement pipeline's bytes and arithmetic
exactly, and the golden fixtures pin them:

- the same command stream and packed config for the session's `EncodeOptions`,
  and the same one deployed executor as the target with a zero native value;
- **no probe reads**: the contract measures its own profit and returns
  `ExecutionReceipt { gross_profit, bribe_paid }` from
  `execute(bytes,uint256)`, so the built-in adapter assesses that return data
  (ADR-098 D7) as the transaction's only sequence step. It decodes exactly two
  ABI words; absent, short, or trailing bytes are an `AssessError::Evidence` /
  `AssessError::Decode` refusal, never a zero-profit success, and a
  `bribe_paid > gross_profit` receipt is inconsistent evidence rather than a
  saturated zero;
- the same measured gas basis: `gas_used` is the top-level `execute` call's own
  gas, and the retired probe reads were separate sequence steps that never
  entered it. The success-path golden's recorded `gas_used` therefore moved only
  with the executor stub's receipt-return code (+30 gas), and its `net_profit` /
  `priority_fee` with it;
- the same gross figure: `gross_profit − bribe_paid`, i.e. retained after the
  bribe and before gas. The receipt's `gross_profit` is the selected-domain
  increase measured BEFORE the bribe, while the retired seven-call bundle
  measured all three legs AFTER the whole transaction — in mode 2 the bribe
  showed up on the physical legs and the gross on the ERC6909 leg, so the
  subtraction reproduces the same total. `docs/execution-strategy.md` and
  `rust/crates/engine/degenbot-arbitrage/tests/execution_receipt_parity.rs`
  pin the parity per profit mode, bribe shape, and malformed payload;
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
| Python `SimulateContext(provider, executor_owner, executor_address, weth_address, pool_manager_address, multicall3_address, inject_code, injected_address=None, executor_runtime_path=None, contracts_dir=None)` | `SimulateContext(provider, executor_owner, executor_address, weth_address, pool_manager_address, inject_code, injected_address=None, executor_runtime_path=None, contracts_dir=None)` — `multicall3_address` left the keyword set, so passing it raises `TypeError: unexpected keyword argument`; every other input keeps its position | the built-in assesses the `execute` receipt and reads no Multicall3 balances, so the address had no reader left; no Python name is added in its place |
| `CmdExecutorBinding::new(owner, executor, weth, pool_manager, multicall3, opts)` | `CmdExecutorBinding::new(owner, executor, weth, pool_manager, opts)` | the binding no longer declares a Multicall3 identity: its adapter declares no probe reads, so the `getEthBalance` leg retired with the balance bundle |
| `SimulateContext::override_params()` / `encode_context()` | `SimulationDeployment::override_params()`; the encode context is the built-in binding's | the engine override projection and the encode intake are deployment facts |
| `DispatchCandidate.opts`, `SimulatePath.opts` | `CmdExecutorBinding::new(..., opts)` / `with_opts(opts)` | one authority for the cmd config axes (the injected deployment), never a per-candidate copy |
| `SimulatePath::encode_request()` | gone — the seam hands the adapter `PathInfo` + `ComposerInputs` (`SimulatePath::expected_outputs_vec` / `consumed_inputs_vec`) | Encode belongs to the adapter |
| `SimulationOverrideParams { owner, inject_code, injected_address, runtime_bytecode, warmup, weth_address, pool_manager_address }` | `SimulationOverrideParams { owner, code: Option<DeclaredCode>, settlement: Option<SettlementWarmup> }` | declared code is an explicit override (production runs deployed code); the settlement warmup is a declaration, not a required pair of addresses |
| `PrefetchFacts { weth_address, pool_manager_address, warmup }` | `PrefetchFacts { settlement: Option<SettlementWarmup> }` | the same declaration drives the executor-side prewarm words; a deployment with no settlement identity derives only path words |
| `warmup: WarmupSlots` computed by the driver shell | `CmdExecutorBinding::{deployment, injected, load_runtime}` | warmup math + runtime-artifact selection are the built-in binding's |
| fee sizing implied by the settlement strategy for every deployment | `SimulationDeployment::fee_sizing()` / `with_fee_sizing(FeeSizing)`; `AdapterOwned` is the `new` default, `MarketAware` is the built-in binding's explicit opt-in | Fee belongs to the adapter's Assess; only the deployment that chooses market-aware sizing is re-priced |
| `SimResult { …, net_profit, … }` with no verdict | `SimResult { …, passed, … }` + `SimResult::is_submittable()`; `DispatchOutcome::gas_profitable` holds only submittable results | the deployment's assessment verdict survives to dispatch/submission instead of being re-derived from the net |
| `CmdExecutorBinding::injected(address, runtime) -> SimulationDeployment` | `CmdExecutorBinding::injected(address, runtime) -> Result<SimulationDeployment, InjectionAddressMismatch>` | the built-in adapter encodes to its executor, so a declared override at any other address would simulate code the transaction never runs; the Python `SimulateContext` refuses a mismatch with `ValueError` |
| `degenbot_arbitrage::SimulateContext` constructed by `ExecutorRuntime` flat fields | `ExecutorRuntime.binding` + `ExecutorConfig.deployment` | the batch executor injects one deployment per session |
| `SimulationDeployment::new(adapter, owner, gas_limit, code, settlement)` | `SimulationDeployment::new(deployment_id, adapter, owner, gas_limit, code, settlement)`; `CmdExecutorBinding::{deployment, injected}` take the same `DeploymentId` first | a deployment has no unnamed form: every candidate it executes is keyed by its operator-assigned identity (ADR-098 D4). A single-deployment session names it once at `ExecutorConfig::BUILTIN_DEPLOYMENT_ID` (`1`) |
| `DispatchCandidate.path_id`, `SimulatePath.path_id`, `SimResult.path_id`, `SimFailure.path_id`, `PayloadRow.path_id`, `BatchOutcome.path_id`, `SubmitCandidate.path_id`, `SubmitRecord::{Submitted, Skipped}.path_id` (`u64`) | `.candidate_id: degenbot_execution::CandidateId { path_id, deployment_id }` (`Copy + Hash + Ord`; `.path_id()` / `.deployment_id()`; `Display` = `path@deployment`) | one solver path evaluated at two deployments is two candidates; a record keyed by the path alone collapses them. The solver's own path numbering is preserved inside the composite |
| post-target joins, failure/staleness attribution, and the suppression ledger keyed by `path_id` | the same joins keyed by `CandidateId`; `PathSuppression::{record_success, record_failure, is_suppressed, is_in_suppressed_set, discard}` take a `CandidateId`, and `discard_path(path_id)` is the one path-scoped operation (the engine removes paths, not candidates) | a failure at one deployment says nothing about another, and a staleness/failure record must name WHICH deployment it attributed |
| `PayloadRow::to_candidate` / `RawResult::to_candidate(path_info)` | `RawResult::to_candidate(path_info, deployment_id)`; `AssemblyInputs.deployment_id` | a raw solver row is path-keyed (the solver has no deployment in view), so the candidate identity is minted where the row is bound to the session's deployment |
| `merge_payload_results(payloads, resolver, executor_address)` | `merge_payload_results(payloads, resolver, executor_address, deployment_id)`; the Python seam passes `ExecutorConfig::BUILTIN_DEPLOYMENT_ID` itself and gains no new argument | the standalone merge joins rows into ONE deployment's submit lane, so a payload carrying another deployment's identity is refused BEFORE the resolve/join with the same `AssemblyError` `assemble_batch` raises (actual + expected deployment named, `path_id` preserved) instead of being re-stamped onto the built-in executor |
| Python `SubmitCandidate(path_id=42, …)` | `SubmitCandidate(candidate_id=CandidateId(path_id=42, deployment_id=1), …)`; a bare `path_id=` keyword raises `TypeError` | the constructor takes the composite identity the sim record carries, never a bare path that cannot name its deployment |
| Python sub-records: failure render dicts, inline-sim payload dicts, payload verdicts, submit-record wire dicts, and the `[sim-diag]` JSON line carried a top-level `path_id` | each carries ONE nested `candidate: {"path_id", "deployment_id"}` mapping; `typed_submit_record` and the payload parser raise `DegenbotValueError`/`ValueError` on the retired path-only shape | a wire record that names only a path cannot be joined back to a deployment's submit lane; the identity is one value, not two keys that can disagree |
| Python `BatchOutcome.path_id` / `FailureDetail.path_id` / `PayloadVerdict.path_id` | `BatchOutcome.candidate_id` / `FailureDetail.candidate_id` / `PayloadVerdict.candidate_id` (the FFI `CandidateId` pyclass, with `.path_id` / `.deployment_id`) | the typed accessor exposes the composite; a renderer that needs the path id reads the path half |
| Python `Dispatcher.{record_success, record_failure, is_suppressed, discard_path, total_suppressed}` (`path_id: int`) | gone from the Python surface; the batch executor is wired with the ledger's `Arc` instead | the ledger is candidate-keyed and the pipeline owns its mutators (the sim fan-out records outcomes, the assembly reads the skip projection, the session discards a removed path's entries); a Python caller could only have passed a path id, which no longer identifies an entry |
| Rust `RawResult`, `PathResolver`, `path_info_by_id` / `path_infos`, path registration/discovery/crawl receipts | unchanged — still keyed by the solver `path_id` | these name an OPPORTUNITY before any deployment is chosen; minting a deployment identity there would invent the target selection this chunk does not perform |
| `SimFailure { weth_before, weth_after, eth_before, eth_after, erc6909_before, erc6909_after }`; `FailBuckets::record_no_profit(..., weth_before, weth_after, eth_before, eth_after, erc6909_before, erc6909_after)` | the same records without the balance legs: bucket, `fail_index`, revert data, reverting frame, captured/reverted swaps, call trace | nothing measures those legs once the built-in declares no probes; a field no producer can fill is a fabricated zero, and the receipt's `gross_profit` is the measurement that replaced them |
| the Python failure-detail dict keys `weth_before` / `weth_after` / `eth_before` / `eth_after` / `erc6909_before` / `erc6909_after`, and the `[sim-bals]` debug line rendered from them | no balance keys and no `[sim-bals]` line; `[sim-trace]`, `[sim-logfull]`, and `[sim-revswaps]` are unchanged | the keys had no producer left, so the renderer would print a delta over fabricated zeros |
| `execute(bytes,uint256)` simulated at sequence step 3, bracketed by the WETH / ETH / ERC6909 reads at steps 0-2 and 4-6 | step 0 — the transaction is the sequence's only step, so an `execute` revert reports `fail_index = 0` (`tests/standalone_parity/fixtures/inspector_cafebabe_revert.json`), and a custom adapter's transaction sits at its own declared read count (`SimulationCapture::failing_step`) | no probe read brackets the built-in transaction; positions come from the adapter's declaration, which for the built-in is empty |

The Python `SimulateContext` pyclass builds the built-in binding and loads the
artifact through it: `multicall3_address` is refused at construction
(`TypeError`), and the rest of the constructor's order is unchanged. The other
retired Python inputs fail at the boundary they cross, never silently:

- the failure record's six balance keys and the retired **path-only wire
  shape** (a top-level `path_id` on the failure, inline-sim payload, verdict,
  and submit-record dicts) — the record parsers read the nested `candidate`
  mapping instead (`DegenbotValueError` from `candidate_id_from_mapping`; the
  inline-sim payload parser raises `ValueError`), so a record that names only a
  path is an error rather than a guessed deployment. The `[sim-diag]` line
  carries the same nested mapping;
- the `Dispatcher` mutators `record_success` / `record_failure` /
  `is_suppressed` / `discard_path` / `total_suppressed` — absent from the class,
  so a caller that reaches for one fails with `AttributeError` instead of
  passing a path id the candidate-keyed ledger can no longer resolve.

One Python name IS added: the `CandidateId` pyclass (`path_id` +
`deployment_id`, `Display` = `path@deployment`) that the record accessors
return and the submit constructor takes.

## Out of scope here

Deployment identity propagation is part of this record set: every post-target
record carries `CandidateId`, and the standalone payload merge validates the
expected deployment. What remains outside it is heterogeneous execution — the
deployment registry, multi-target fan-out, and first-compatible selection — and
version-capability policy. A session still constructs exactly one deployment,
and `executor_address` on the batch-executor config remains its single-target
join stamp.

## Evidence honesty note

A probe read that yields no single ABI word — a revert, a halt, an empty
return, or a multi-word return — is an **absent** reading, never a fabricated
zero (`degenbot-simulation::sim::production`). The built-in adapter's evidence
is the `execute` receipt: an absent or malformed one is
`AssessError::Evidence` / `AssessError::Decode`, which the settlement seam
categorizes as `balance-decode`. A declared-code fixture therefore needs no
stand-in code at WETH9 / PoolManager / Multicall3; it needs an executor stub
that returns the two receipt words.
