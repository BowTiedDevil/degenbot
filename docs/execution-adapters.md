# Execution adapters: plugging in your own contract

degenbot owns the machinery around execution: pool and token state, solving,
exact-block simulation, revert diagnostics, access lists, signing, nonces, relay
submission, and receipt monitoring. What it cannot know is your execution
contract — its calldata schema, its funding model, and how its profit should be
measured.

An **execution adapter** is the small piece of code that closes that gap. It
does two things:

- turn one solved opportunity into exactly one transaction — `to`, `data`, and
  the native `value` sent with it;
- say whether that transaction, as simulated, made money.

Everything else — running the exact-block simulation, submitting the transaction,
watching the receipt — stays with the engine. Gas pricing stays with the adapter
by default: `FeePolicy`, or a custom `assess`, sets the priority fee. A
deployment may re-price above that interface instead, and the built-in
settlement deployment does — it applies its own market-aware fee sizing to the
adapter's gross figure.

Pick the path that matches what you run:

| You run… | What to do |
|---|---|
| the bundled `cmd_executor` contract | nothing. The built-in adapter is registered for every session, and the [built-in release policy](#the-built-in-adapter-and-release-discovery) is opt-in on top of it |
| your own contract — immutable, upgradeable, or freshly written | write an adapter (Rust trait, or a Python calldata function) and register it as a deployment |

Your own contract needs **none** of degenbot's built-in machinery: no command
grammar, no `release_id()`/`protocol_major()`/`call_encoding_version()`/
`capabilities_hash()` getter, no release manifest, no capability commitment, and
no entry in the release registry. All of that exists for the bundled
`cmd_executor` alone, and so does every step of the `cmd_executor/2` migration and
verification workflow: a custom adapter is verified by its own simulation, not by
metadata screens.

## One opportunity, one transaction

A solved opportunity is one path with solver-driven amounts. The engine executes
the one transaction your adapter encodes, then hands back evidence for your
adapter to interpret. There is no simulation loop to write and no
multi-transaction choreography to arrange — a contract that batches internally
does so inside its own call.

| Type | What it is |
|---|---|
| `ExecutionAdapter` | the trait you implement in Rust (the Python binding implements it for a Python callable) |
| `PathInfo` | neutral hop descriptors: pool address, tokens, direction, family payload. No settlement identity anywhere |
| `ComposerInputs` | the solver-driven amounts: `optimal_input`, `hop_outputs`, `consumed_inputs`, plus `opts`. Integer fixed-point `u128`, never floats |
| `TransactionRequest` | `{ to, data, value }` — the complete transaction, with `value` preserved verbatim to simulation and submission |
| `ProbeSpec` | a declared read to snapshot before and after your transaction: a contract call carrying its **complete calldata**, or an account's own native balance |
| `ExecutionObservation` | the evidence the engine captured: the transaction, `status`, `gas_used`, `base_fee_next`, `return_data`, `probe_reads` in declared order, and `events` |
| `ExecutionResult` | your verdict: `gross_profit`, `net_profit`, `gas_used`, `base_fee_next`, `priority_fee`, `passed`, `deltas` |
| `AssessOptions` | the built-in assessment shape (`rule`) and the `min_net_profit` a pass requires |
| `FeePolicy` | priority-fee pricing: the built-in market-percentile default, or an absolute override |

A read the engine could not take is **absent** (`None`), never a fabricated
zero. An assessment that needs absent evidence fails with
`AssessError::Evidence` rather than pricing a guess.

## Encode, Probe, Assess, Fee

Two of the four parts are yours, one is declared data, one is a defaulted
pricing decision.

- **Encode** — you write it. `encode(&PathInfo, &ComposerInputs) ->
  Result<TransactionRequest, ComposeError>`. Any ABI, any byte format, any
  target, any native value.
- **Probe** — you declare it, the engine runs it. `probe_spec() -> &ProbeSpecs`
  returns the pre/post reads to snapshot; the engine runs them, warms the cache
  and access list, and reports the readings on the observation.
- **Assess** — you own the interpretation; the interface supplies a default. The
  default is `AssessOptions { rule, min_net_profit }` priced by
  `fee_policy()`: `gross = Σ probe deltas` (or `rule:
  AssessRule::ReturnData` for the first 32-byte return word), then `net = gross
  − gas_used × (base_fee_next + priority_fee)`, `passed = net ≥ min_net_profit`.
- **Fee** — `FeePolicy { market_percentile, override_priority_fee }`. Not a
  separate part: `net` is defined in terms of the pricing policy, so a custom
  `assess` reuses `ExecutionResult::price(..)` and keeps the same pricing
  arithmetic.

Override `assess` when your contract reports profit in its own shape — a custom
return struct, an event, a credit-ledger read, the receipt the bundled executor
returns. Keep

```rust
ExecutionResult::price(gross, deltas, observation, min_net_profit, &fee_policy)
```

as the last step. That helper carries the terminal-status invariant: a
**reverted** observation prices to zero gross, zero net, and `passed = false`
whatever your decoder found in the payload, with the observed gas and fee facts
preserved for diagnostics. A revert moved no value, so no return bytes or event
can be profit — which is why no adapter, default or custom, can pass a revert.

`Ok(passed = false)` is an economic decline. `Err(AssessError)` means the
evidence was missing or undecodable, so no verdict was reached; the engine
records it as a decline carrying the reason and keeps running.

## Encoding only, or a full adapter?

Most adapters are encoding-only: one contract, one stable target, one stable
native value, and a profit shape the built-in gate already understands.

| Your situation | Use |
|---|---|
| target and `value` are constant, and `Σ probe deltas` (or return data) is your profit figure | `PayloadComposer` + `PayloadAdapter` |
| target or `value` varies per opportunity (several contracts, a value sized from the solution) | implement `ExecutionAdapter::encode` yourself |
| your contract's profit needs a custom decode (bespoke event, receipt struct, ERC6909 ledger) | implement `ExecutionAdapter` and override `assess` |

`PayloadAdapter` is the explicit binding between calldata and the transaction it
executes — calldata alone cannot carry a target or a value:

```rust
// Bind calldata to the transaction it executes.
let adapter = PayloadAdapter::new(MyComposer { executor: my_contract }, my_contract)
    .with_value(U256::ZERO)                         // native value sent with the call
    .with_probes(my_probes)                         // declared pre/post reads
    .with_assess_options(AssessOptions::default())  // assessment rule + threshold
    .with_fee_policy(FeePolicy::default());         // priority-fee pricing
```

The result implements the full `ExecutionAdapter` interface, so it reaches the
production path exactly as a hand-written adapter does.

## A Rust adapter

Add the `degenbot-execution` crate plus `alloy` for `Address`/`Bytes`/
`U256`; the sample crate records the exact versions this tree builds against.

```rust
use alloy::primitives::{Address, Bytes, U256};
use degenbot_execution::{
    AssessOptions, ComposeError, ComposerInputs, ExecutionAdapter, FeePolicy, PathInfo,
    PayloadAdapter, PayloadComposer, ProbeSpec, ProbeSpecs, SolveResult, TransactionRequest,
};

struct MyComposer { executor: Address }

// Encode: your calldata for your contract. `build_call` is your own encoder.
impl PayloadComposer for MyComposer {
    fn compose(&self, _path: &PathInfo, inputs: &ComposerInputs<'_>)
        -> Result<Bytes, ComposeError>
    {
        let final_out = inputs.hop_outputs.last().copied()
            .ok_or_else(|| ComposeError::encode("path has no hops"))?;
        Ok(build_call(inputs.optimal_input, final_out))
    }
}

// Probe: declared data, not code. A call carries COMPLETE calldata
// (selector + ABI arguments); a native read observes the account itself.
let probes: ProbeSpecs = vec![
    ProbeSpec::call("WETH", weth, weth_balance_of_calldata(my_executor)),
    ProbeSpec::native_balance("ETH", my_executor),
];

let adapter = PayloadAdapter::new(MyComposer { executor: my_executor }, my_executor)
    .with_value(U256::ZERO)
    .with_probes(probes);

// A solved path -> ONE complete transaction ({ to, data, value }).
let transaction: TransactionRequest = adapter.compose_view(&path, &solve_result)?;
assert_eq!(transaction.to, my_executor);
```

When the encode decision is dynamic, implement the trait directly. Only `encode`
and `probe_spec` are required; `assess_options`, `fee_policy`, and `assess` have
working defaults:

```rust
impl ExecutionAdapter for MyStrategy {
    fn encode(&self, path: &PathInfo, inputs: &ComposerInputs<'_>)
        -> Result<TransactionRequest, ComposeError>
    {
        let data = PayloadComposer::compose(&self.composer, path, inputs)?;
        // Target and native value are this opportunity's decision.
        Ok(TransactionRequest::new(self.executor, data, self.value))
    }

    fn probe_spec(&self) -> &ProbeSpecs { &self.probes }
}
```

Both fragments are simplified: `build_call`, `weth_balance_of_calldata`, `path`,
and `solve_result` belong to your code. The complete, compilable reference is
the `degenbot-execution-sample` crate
(`rust/examples/degenbot-execution-sample`) — a foreign `SimpleExecutor`
strategy implementing all four parts, with its own recorded expected-bytes
corpus:

```bash
cargo run -p degenbot-execution-sample --example foreign_consumer
```

## A Python adapter

Python supplies the same two things: a calldata callable and the binding the
calldata cannot carry. The Python binding turns them into the core adapter, so
no Rust is required to add a contract — and no second simulation or assessment
engine is introduced.

```python
from degenbot._ffi.execution import PayloadAdapter, abi_encode_call

SIMPLE_EXECUTOR = "0x7777777777777777777777777777777777777777"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

def compose_simple_executor(result):
    """Encode: `SolveResult -> bytes` for YOUR contract (integer amounts)."""
    hop_outputs = list(result.hop_outputs)
    return abi_encode_call(
        "execute(uint256,uint256,uint256[])",
        [result.optimal_input, hop_outputs[-1], hop_outputs],
    )

adapter = PayloadAdapter(
    compose_simple_executor,
    to=SIMPLE_EXECUTOR,             # the contract the call targets
    value=0,                        # native value sent with the call (wei)
    probes=[
        # A contract-call read carries COMPLETE calldata: selector + ABI args.
        {"label": "WETH", "to": WETH,
         "data": abi_encode_call("balanceOf(address)", [SIMPLE_EXECUTOR])},
        # A native-account read observes the account's own ETH balance.
        {"label": "ETH", "account": SIMPLE_EXECUTOR},
    ],
    min_net_profit=0,               # the gate threshold, in wei
)
```

`value`, `probes`, and `min_net_profit` are optional; the default assessment is
the built-in sum-of-deltas gate with market-percentile pricing. Python states
policy data — the encoding, the binding, the declared reads, the threshold. A
custom assessment interpreter is a Rust-side `assess` override.

That fragment is simplified (the `SolveResult` arrives from the engine, not from
a literal). The runnable, self-checking script is
`examples/execution_strategy_foreign.py`, which asserts the foreign selector:

```bash
uv run python examples/execution_strategy_foreign.py
```

## Validating your adapter

Writing an adapter is one step; knowing it encodes the call you meant is
another. degenbot offers three progressive checks, each opt-in, each proving
more than the one below it (ADR-100):

| Tier | What you declare | What it proves | When it runs |
|---|---|---|---|
| **0 — none** (default) | nothing | that the engine simulated the bytes the adapter returned | today's path, unchanged |
| **1 — contract interface** | the contract's ABI JSON, plus optional target, selector, and native-value policies | the encoder's output is a *declared call to this contract*: declared selector, arguments that decode against the declared types, allowed target, allowed value | every `encode`, once you attach the declaration |
| **2 — validation cases** | explicit path + amount + expected-transaction literals, one per behaviour | the encoder produces *that exact call* for *that exact path*, across the dimensions the cases cover | an explicit `check_cases(...)` / `run_cases(...)` call; never at registration |
| **3 — execution-backed certification** | a compiled contract and its expected evidence | the deployed code *does what the calldata says* | CI and the fork lane; never in a production process |

No lower tier implies a higher one, and the default costs nothing: an adapter
that declares nothing keeps exactly today's encode path — no wrapper, no decode,
no new field, no registration change. Validation is a **checker**, not a plugin
framework and not a manifest: a contract declares one callable surface, never a
version, a release tag, or a capability list (ADR-099 D8).

### The declaration (tier 1)

A contract interface is standard ABI JSON plus three independent, optional
policies:

```json
{
  "abi": [ { "type": "function", "name": "execute", "stateMutability": "nonpayable",
             "inputs": [ {"name": "optimal_input", "type": "uint256"},
                         {"name": "final_output", "type": "uint256"},
                         {"name": "hop_outputs", "type": "uint256[]"} ],
             "outputs": [ {"name": "", "type": "bool"} ] } ],
  "selectors": ["execute(uint256,uint256,uint256[])"],
  "target": "0x7777777777777777777777777777777777777777",
  "value": {"kind": "zero"}
}
```

| Field | Required | Meaning |
|---|---|---|
| `abi` | yes | a standard Solidity **ABI JSON array**. Only `type: "function"` entries are the callable surface; `constructor`, `event`, `error`, `fallback`, and `receive` entries are accepted and ignored, so a generated artifact pastes in whole |
| `selectors` | no | a signature allow-list drawn from `abi`. Present, the encoded selector must be one of them; absent, any function the ABI declares is allowed |
| `target` | no | the one address this interface's transactions must name. Omitted means address-agnostic — one declaration may describe deployments of one contract |
| `value` | no | the native-value policy. Omitted means unchecked: native value is a funding decision the deployment owns, not a fact the ABI states |

`value` takes exactly one of four forms:

| Form | Meaning |
|---|---|
| `{"kind": "any"}` (the default when `value` is omitted) | no check |
| `{"kind": "zero"}` | `value == 0` — a flash-funded contract |
| `{"kind": "max", "max_wei": "123"}` | `value <= max_wei`, a decimal string (u256; never a JSON number) |
| `{"kind": "exact", "exact_wei": "123"}` | `value == exact_wei` |

Five rules make the declaration checkable rather than decorative:

1. **Selectors come from the ABI, not from arithmetic here.** Each is
   `keccak256(canonical signature)[..4]` as Alloy computes it, so a declaration
   and a toolchain cannot disagree about what a signature means.
2. **A declaration must contain at least one function.** A surface that declares
   none could never validate anything, so it is refused instead of silently
   passing.
3. **Two functions may not share a selector.** A 4-byte collision is refused at
   construction, because the declaration would otherwise be ambiguous about
   which argument types a given calldata must decode against.
4. **An allow-listed signature must exist in `abi`.** Naming a function the ABI
   does not declare is a typo to refuse, not an implicit second declaration.
5. **Argument types are the ABI's own.** Each parameter resolves to an Alloy
   type; nothing in the declaration names a degenbot type, and nothing in it
   describes a build or a future.

A **Rust** adapter declares from the same `sol!` types that produce its calldata,
so the encoder and its declaration cannot drift — the sample crate does exactly
this:

```rust
alloy::sol! {
    interface SimpleExecutor {
        function execute(uint256 optimal_input, uint256 final_output, uint256[] hop_outputs)
            external returns (bool);
    }
}

use degenbot_execution::{ContractInterface, PayloadAdapter, ValidatedAdapter, ValuePolicy};

// `with_target` and `with_selectors` are fallible; `with_value_policy` is not.
let interface = ContractInterface::from_signatures([SimpleExecutor::executeCall::SIGNATURE])?
    .with_target(my_executor)?
    .with_value_policy(ValuePolicy::Zero);

// Attach it by wrapping: every other adapter member is forwarded unchanged.
let adapter = ValidatedAdapter::new(
    PayloadAdapter::new(MyComposer { executor: my_executor }, my_executor),
    interface,
)?;
```

`ContractInterface::from_abi_json(...)` is the other construction route, for an
adapter that ships its ABI artifact rather than a `sol!` declaration. The wrapper
**forwards and never repairs**: `encode` and `compose_view` run the inner adapter
and then validate the returned transaction unchanged, while `probe_spec`,
`assess_options`, `fee_policy`, and `assess` are the inner's own — custom
overrides included. Nothing is coerced, zeroed, or re-encoded, because repair
would invent a transaction the adapter did not produce.

A **Python** adapter declares with one keyword; the binding does the rest
(serializes the `abi` array, parses the addresses, resolves the policy, and
stamps the target from its own `to`, since one binding encodes to one address):

```python
adapter = PayloadAdapter(
    compose_execute,
    to=MY_EXECUTOR,
    value=0,
    interface={
        "abi": EXECUTOR_ABI,                                    # standard ABI JSON
        "selectors": ["execute(uint256,uint256,uint256[])"],
        "value": {"kind": "zero"},
        # "target" is omitted: the binding stamps its own `to`
    },
)
```

Construction is **pure data validation**: it never invokes your compose callable,
never reads the chain, and never touches a provider. A malformed ABI, an empty
surface, a duplicate selector, an unknown allow-listed signature, a bad policy, a
declared `target` that disagrees with `to`, or a declared probe to your own
contract that the declaration does not describe is a `ValueError` there, before
any deployment exists. Checking a declaration is not registration:
`SimulationDeployment` gains no validation hook, and a validated adapter arrives
as an ordinary adapter.

### The cases (tier 2)

A **validation case** is a literal that says: for *this* path, with *these*
amounts, the adapter must produce *that* transaction — or decline.

```python
CASES = [
    {
        "name": "two_hop_v2_execute",
        "hops": [
            {"family": "v2", "pool_address": "0x1111…1111", "token0": "0xC02a…",
             "token1": "0xA0b8…", "fee": 30, "zfo": True},
            {"family": "v2", "pool_address": "0x2222…2222", "token0": "0xA0b8…",
             "token1": "0xC02a…", "fee": 30, "zfo": False},
        ],
        "amounts": {"optimal_input": 10**18,
                    "hop_outputs": [10**18, 121 * 10**16],
                    "consumed_inputs": [10**18, 10**18]},
        # Decoded argument expectations, in declaration order.
        "expect": {"kind": "transaction",
                   "selector": "execute(uint256,uint256,uint256[])",
                   "arguments": [10**18, 121 * 10**16, [10**18, 121 * 10**16]]},
    },
    {
        "name": "three_hop_path_declines",
        "hops": [ …three hops… ],
        "amounts": {"optimal_input": 10**18, "hop_outputs": [ … ],
                    "consumed_inputs": [ … ]},
        "expect": {"kind": "decline"},
    },
]

reports = adapter.check_cases(CASES)
assert all(report.passed for report in reports), [r.violations for r in reports]
```

| Part | Rule |
|---|---|
| `name` | unique within the list; the report key and the message prefix |
| `hops` | the **full** per-family hop shape, not the solver's report descriptor. `v2`/`v3` carry `pool_address`, `token0`, `token1`, `fee`, `zfo`; `v4` carries `pool_manager_address`, `pool_id_hex`, `currency0`, `currency1`, `fee`, `tick_spacing`, `hook_address`, `zfo` |
| `amounts` | `optimal_input`, plus `hop_outputs` and `consumed_inputs` with exactly one entry per hop; every amount at or below the seam's `int128` bound |
| `expect.kind` | `"transaction"`, then `selector` is required and `target`/`value` default to the binding's own; or `"decline"` |
| `expect.arguments` | decoded argument values in declaration order — the produced calldata is decoded against the declared types and compared |
| `expect.calldata` | an exact calldata golden (hex), mutually exclusive with `arguments` |

A case needs no chain, because its path **is** data: each `hops` entry becomes a
real `HopInfo` variant, the `amounts` become a real `ComposerInputs`, and the run
calls the same `encode` production calls. There is no pool lookup, no `eth_call`,
no engine, no registration, and no provider parameter to pass — which is also why
a malformed case (wrong arity, an amount above the bound, an unknown family tag)
is refused before the first encode instead of being encoded against.

`check_cases` is explicit and synchronous. In Rust, `run_cases(&adapter,
&interface, &cases)` returns one `CaseReport` per case; in Python the method
returns reports carrying `name`, `passed`, and `violations`. A case that expected
a transaction but received a decline (or the reverse) fails as a refusal; a case
whose declared evidence disagrees fails as a named mismatch. Both are data, so one
run reports every failing case.

### Declines and violations

Two refusals look alike and mean opposite things.

| Refusal | Where it comes from | What happens |
|---|---|---|
| `ComposeError::Unsupported` — Python: raise `UnsupportedPath` | your adapter's routine capability decline ("this path is outside what my contract can encode") | `FirstCompatible` advances to the next target; nothing is suppressed or failed |
| `ComposeError::Validation(ContractViolation)` | the encoder's output disagrees with the declaration you attached | a **hard** encode failure: the walk stops for that candidate, the simulation records `SimVerdict::Error`, and no submit row is produced — offering the same path to another target would hide the defect rather than fix it |
| `ComposeError::Encode` — Python: any other exception | your encoder failed outright | the same hard failure |
| `DeclarationError` | the declaration, or a case document, is unusable | a loud refusal where it is read: `ValueError` in Python |
| `ContractViolation::CaseExpectation` | the produced transaction disagrees with a case's declared evidence | that case fails by name; the other cases still run |

`AssessError` is untouched: validation never fabricates economic evidence, and a
refusal is never priced.

`ContractViolation` names the field that disagreed — `Calldata` (payload shorter
than a 4-byte selector), `Selector` (not declared, or not allow-listed),
`Arguments` (bytes that do not decode against the declared types, or are not
their canonical encoding), `Target`, `NativeValue`, and tier 2's
`CaseExpectation`. `DeclarationError` names what made the declaration unusable —
`AbiJson`, `EmptyInterface`, `DuplicateSelector`, `UnknownSelector`,
`UnparsableSignature`, `ZeroTarget`, `InvalidValuePolicy`, `Probe`, and tier 2's
`CaseDocument`, `CaseArity`, `CaseAmount`.

Argument checks are strict: the payload must be the **canonical** encoding of its
arguments, so trailing bytes and a non-canonical dynamic offset are refused
rather than tolerated. The decoder exposes no strict mode, so exactness is checked
by re-encoding the decoded values and comparing — which is what keeps a padded
payload distinguishable from the declared call.

### What validation costs

| Tier | Added per-encode work |
|---|---|
| 0 (default) | none — no wrapper exists, no decode runs, no field is added to any adapter |
| 1 | one 4-byte selector lookup in the compiled table, then one argument decode (and the canonical re-encode comparison) over the bytes after the selector. No keccak, no provider, no ABI re-parse: the declaration is compiled once, at construction |
| 2 | none on any production path — cases run only when a caller asks |
| 3 | engine work, in CI and the fork lane only |

Tier 1 targets development, CI, and operators who accept one decode per
transaction in exchange for refusing a broken call before paying for its
simulation. There is no `validation` cargo feature and no configuration key: the
facility is inert until a declaration is constructed.

The maintained starting point is `examples/execution_adapter_template.py` — ABI
declaration, one transaction case with decoded argument expectations, one decline
case, and the assertions that keep it self-checking:

```bash
uv run python examples/execution_adapter_template.py
```

## Certifying against the compiled contract

Tiers 1 and 2 read the calldata your adapter produced, and both cost no
simulation. That is also their limit: a declaration that describes garbage
calldata passes exactly as a faithful one does, because behaviour lives in code
and a declaration never runs it — **a violation is dispositive, a pass is a
screen** (ADR-100 D8).

Certification is the tier that runs the code. It installs a tracked contract's
own compiled runtime as a deployment's declared code (development only, so no
live submission — see [Registering a deployment](#registering-a-deployment)), or
executes the code actually deployed at the target (the `just test-fork` lane),
and drives it through the same session → batch → candidate lane production uses,
so the adapter's encode, the engine's mechanics, and the adapter's assess all run
exactly as they will live. What the verdict then carries is named per kind:

| Evidence | What it proves |
|---|---|
| receipt / probe deltas — `gross_profit` is the declared read's post-minus-pre | the call moved the state or balances the adapter named |
| the contract's own public getters, declared as probes | *which* function ran and *what* it stored — calldata inspection cannot show either |
| return data / captured events | the contract reported the figure the assessment decoded |
| submit-plan rows | the certified candidate reached submission, or was correctly skipped |

Two requirements make certification an instrument rather than a formality:

- **Name the evidence in the assertion.** The verdict assertion reads the probe
  delta or return data it claims. "The transaction succeeded" proves a call ran;
  it does not prove the call did the job.
- **Keep a negative control in the same module, and pin the asymmetry.** The
  decoy case keeps the identical read set and declaration and encodes a different
  function: the verdict must report that function's own movement and never the
  marker. Its counterweight is a declaration over a runtime that refuses to store
  the marker: the same calldata decodes and the case run passes, and execution
  fails anyway. One control without the other cannot separate "the evidence
  proves the call" from "the evidence proves something".

Registration checks and certification are different instruments and stay apart:
registration answers "may this deployment run" and never simulates;
certification answers "does its code do what its calldata says" and never
registers. A certification never runs at registration, and a registration never
waits on a simulation.

## Registering a deployment

An adapter says *how* to encode and assess. A **deployment** says *where* and
*as whom*:

| Fact | Meaning |
|---|---|
| `DeploymentId` | one deployment's operator-assigned identity: degenbot never interprets the number |
| the adapter | `Arc<dyn ExecutionAdapter + Send + Sync>` in Rust; in Python the `PayloadAdapter` above |
| owner | the account whose key sends the transaction — the `from`, and the account the engine funds in the simulated block |
| target | the `to` of the transaction your adapter encodes. A deployment fact, so a fan-out stamps each candidate's own address |
| `gas_limit` | the top-level gas limit the opportunity's transaction may burn |
| declared code | optional runtime bytecode installed at the target in the simulated block — **development only** |
| settlement warmup | WETH9/`PoolManager` slots to warm, as `SettlementWarmup { weth_address, pool_manager_address, slots }`; `None` when the contract uses neither settlement identity |

Two identities matter, and they are different facts:

- `DeploymentId` names one deployment.
- `CandidateId { path_id, deployment_id }` names one solver path at one
  deployment. It is the key for every post-target record, so the same path at
  two addresses can never collide. Its `Display` is `path@deployment`.

Registration refuses three things loudly, at construction rather than per block:
an empty registry; two entries claiming one `DeploymentId`; and a deployment
whose owner is not the account the session signer holds (it could be simulated
but never signed). Simulate a target you can sign for.

`target` must also be the address your adapter actually encodes, because
`SimulationDeployment::target()` is what the submit join stamps. An adapter that
encodes away from its declared target is refused for that candidate
(`target-mismatch`): the submitted row's `to` and its calldata must describe the
same call.

Declared code is explicit development data, and development-only: it installs
the runtime at the deployment's target and nothing else, never silently
substituting for a different deployed implementation. While code injection is
enabled, live submission is skipped on every chain — the submit lane declines
the broadcast rather than send a call to code that exists nowhere but the
simulated block.

### Rust

```rust
use std::sync::Arc;
use degenbot_arbitrage::{DeploymentRegistry, SimulationDeployment};
use degenbot_execution::DeploymentId;

let foreign = Arc::new(SimulationDeployment::new(
    DeploymentId::new(2),      // operator-assigned; must be unique in the registry
    Arc::new(adapter),         // your ExecutionAdapter
    owner,                     // the session signer's account
    my_executor,               // the `to` your adapter encodes
    1_000_000,                 // gas limit
    None,                      // declared code: None = simulate the deployed contract
    None,                      // settlement warmup: neither WETH nor PoolManager used
));

let registry = DeploymentRegistry::new(vec![Arc::clone(&foreign)])?;   // order = fan-out order
registry.validate_signing_authority(owner)?;
```

In a full session the registry is built for you. The built-in settlement
deployment is always the first entry (identity
`ExecutorConfig::BUILTIN_DEPLOYMENT_ID`, `1`), and your deployments are
registered after it in the order you supply. Given the session's
`ExecutorRuntime` and its resolved config verdict:

```rust
use degenbot_arbitrage::TargetSelection;
use degenbot_batch_executor::ExecutorConfig;

runtime.additional_deployments.push(Arc::clone(&foreign));  // after the built-in entry
runtime.target_selection = TargetSelection::AllTargets;     // or FirstCompatible (default)
let config = ExecutorConfig::from_verdict(&verdict, runtime)?;   // validates identities + owners
```

### Python

```python
from degenbot.dispatch import (
    SimulationDeployment,
    build_batch_executor,
    builtin_deployment_id,          # the built-in settlement deployment's identity
)

foreign = SimulationDeployment(
    2,                              # deployment_id: distinct from every other, including builtin_deployment_id()
    adapter,                        # the PayloadAdapter above
    "0x1111111111111111111111111111111111111111",  # owner: the session signer's account
    SIMPLE_EXECUTOR,                # execution target: the `to` your adapter encodes
    1_000_000,                      # gas limit
    # code=b"\x60\x00…",            # optional declared runtime (undeployed build only)
)

executor = build_batch_executor(
    context=sim_ctx,
    dispatcher=dispatcher,
    engine=engine,
    signer=signer,
    submit_provider=provider,
    operator_nonce=nonce,
    dry_run=True,
    deployments=[foreign],          # None / [] is the one-entry built-in session
    target_selection="first_compatible",
)
```

Python's `SimulationDeployment` takes no settlement argument, so a Python
deployment cannot declare the optional settlement warmup. Reach for the Rust
constructor only when you want to declare that warmup; trading through WETH9 or
`PoolManager` itself needs no Rust. Fee sizing stays adapter-owned, so your
adapter's `ExecutionResult` — priority fee, net, threshold, verdict — reaches the
caller unchanged. Registration refusals arrive as ordinary `ValueError`s (a
duplicate identity, an owner the signer does not hold, a zero gas limit), never a
panic. A driver that owns its own pipeline factory names the same list on
`ExecutorInputs.deployments` and `ExecutorInputs.target_selection`.

## Target selection

Which deployments a solved path is offered to is a separate policy value, not an
encoder opinion. None of it changes a one-deployment session: one entry selects
itself under either policy, and the comparison never happens.

**`FirstCompatible` (the default)** walks the registry in order and executes at
the first deployment whose adapter accepts the path.

- A path an adapter cannot encode — `ComposeError::Unsupported`, the routine
  capability decline — advances to the next target. Nothing is suppressed or
  failed.
- A hard encoding failure (`ComposeError::Encode`) ends the walk for that
  candidate: another target would not make an encoding bug disappear.
- Acceptance happens once. The accepted `TransactionRequest` travels to
  simulation on the candidate, so `encode` runs once per examined target.
- Targets the walk never reached are reported as retired by the fan-out
  (`DeploymentFanOutOutcome::retired`) and are never encoded, simulated, or
  submitted — so they have no simulation record and no submit row.
- A revert, an unprofitable result, or a failed assessment cannot select a later
  target — selection finished before any of them ran.

**`AllTargets` (explicit fan-out)** offers every path to every registered
deployment. Each deployment runs its own lane with its own candidates, and the
per-batch cap applies per lane, so one target's volume cannot starve another's.
Lanes are isolated: a refusal, a revert, or a failed assessment at one target
stays that target's row, and suppression/divergence/fee-on-transfer state is
keyed by `CandidateId`, so a failure at one address says nothing about the same
path at another. Records are grouped by deployment in registration order.

```python
build_batch_executor(..., target_selection="all_targets")   # the explicit fan-out
```

An unknown spelling is a `ValueError`, never a silent default.

## The built-in adapter and release discovery

The bundled `cmd_executor` adapter has two levels of configuration. They are
independent; a foreign adapter uses neither.

### Explicit current release (the default)

`CmdExecutorBinding::deployment(id)` binds the session's own `cmd_executor`
identities to the release this build ships as *the current one*
(`cmd_executor/2`). Nothing is read from the chain: the session already names the
contract, the consumer is offline-safe, and the encoder is chosen by
construction.

```rust
use degenbot_arbitrage::{CmdExecutorBinding, SimulationDeployment};
use degenbot_execution::DeploymentId;
use degenbot_executor::composers::EncodeOptions;

let binding = CmdExecutorBinding::new(
    owner,
    executor,       // the deployed cmd_executor
    weth,
    pool_manager,
    EncodeOptions::default(),   // the cmd config axes
);

let deployment: SimulationDeployment = binding.deployment(DeploymentId::new(1));
```

Sessions are built this way for the built-in deployment, which is why a
`cmd_executor` operator needs no extra setup.

### Opt-in validated discovery

Discovery asks the deployment what it is, so a contract running a release this
build explicitly ships — including an older one — can be bound to that release's
own encoder instead of the current one. A tag this build's registry does not hold
is refused, so an unknown newer release never runs on a guessed encoder. The
contract declares four constants, each one `eth_call`:

| Getter | Answers |
|---|---|
| `release_id()` | the release tag, e.g. `cmd_executor/2` |
| `protocol_major()` | the command-protocol generation: majors mean incompatible command streams, so a build that does not know a major cannot encode for it |
| `call_encoding_version()` | the version of the whole call encoding — stream, packed config, and `execute` ABI together |
| `capabilities_hash()` | the commitment to the capability ID list it supports (the list itself is in the release manifest) |

Discovery is opt-in and explicit about both halves — the bounded registry of
releases *this build* dispatches, and (for a getterless contract) the operator's
own legacy mapping:

```rust
use degenbot_arbitrage::release_registry::ReleaseRegistry;
use degenbot_arbitrage::{BoundCmdExecutor, CmdExecutorBinding};
use degenbot_execution::DeploymentId;

let registry = ReleaseRegistry::production()?;   // this build's bounded set: validated manifests + encodings
let binding = CmdExecutorBinding::new(owner, executor, weth, pool_manager, EncodeOptions::default());

// Reads the four getters, classifies the answers, resolves the tag, validates it
// against that release's manifest, and binds the release's complete encoding.
let bound = BoundCmdExecutor::discover(binding, &provider, None, &registry).await?;

let deployment: SimulationDeployment = bound.deployment(DeploymentId::new(2));
// bound.release().release_id()  — the validated release
// bound.source()                — Declared(metadata) or Legacy { release }
```

A contract compiled before the getters existed answers nothing, and silence
names nothing: it must be named explicitly. The mapping is the operator's
statement of fact, and it is still checked against the registry:

```rust
use degenbot_arbitrage::release::{LegacyReleaseEntry, LegacyReleaseMapping, ReleaseId};

let legacy = LegacyReleaseMapping::new(&[LegacyReleaseEntry {
    address: executor,
    release: ReleaseId::from_ascii_tag("cmd_executor/2")?,
}]);

let bound = BoundCmdExecutor::discover(binding, &provider, Some(&legacy), &registry).await?;
```

The bound adapter also plans capabilities: a path whose encoding needs an ID the
release does not declare is a `ComposeError::Unsupported` decline for that
candidate, which under `FirstCompatible` lets the next target take the path. A
known unsupported path is therefore declined before simulation or submission,
rather than encoded and left to revert on chain; a revert for any other reason is
still possible, and stays an ordinary failed assessment.

Every failure to establish identity is typed, and **none of them defaults to the
newest release**:

| Situation | Refusal |
|---|---|
| a getter could not be read (timeout, dropped connection, rate limit) | `BindingError::Node` — a broken endpoint proves nothing about the deployment |
| no getter answered, and no legacy entry names the address | `BindingError::Unknown` |
| the declared (or mapped) tag is not in this build's registry | `BindingError::UnknownRelease { tag, registered }` |
| only some getters answered, or an answer is malformed | `BindingError::Decode` |
| the declaration disagrees with its own manifest (major, encoding version, capability commitment) | `BindingError::Mismatch` |

Discovery is a Rust binding decision. Neither Python nor session configuration
performs it: there is no config key for it, and the built-in session deployment
stays explicitly bound to the current release. To run a release you validate,
build the deployment with `BoundCmdExecutor` and register it — including, for an
undeployed build of that release, `bound.injected(id)`, which installs the
release's own artifact and refuses any other bytes.

## Upgrading the built-in contract

The bundled `cmd_executor` is immutable, so its versioning is *declared*, not
inferred — and it evolves without touching the generic interface. A future major
ships one manifest plus one encoding in a new build; a deployment declaring a
major or tag that build does not dispatch is refused at binding rather than
guessed at. The manifest schema, the capability catalog, the artifact
verification, the ordered `cmd_executor/2` migration and verification workflow,
and the migration policy for getterless deployments live in
[the executor release reference](executor/releases.md).

## Reference

Detailed material behind this guide:

- [Your execution strategy, not degenbot's](execution-strategy.md) — the full
  interface: observation semantics, assessment rules, the production simulation
  leaf, and the complete consumer-visible signature table
- [Production simulation composition](architecture/production-simulation-composition.md)
  — block facts beside deployment facts, the registry fan-out, fee sizing, and
  the assessment verdict's route to submission
- [The executor's user guide](executor/user-guide.md) — the bundled contract
  itself: command grammar, balance semantics, entrypoints
- [Executor releases](executor/releases.md) — declared release identity, the
  manifest, capability IDs, and migration policy
- Rust sample: `rust/examples/degenbot-execution-sample` (a foreign
  `SimpleExecutor` strategy, with its own recorded payload corpus)
- Python sample: `examples/execution_strategy_foreign.py` — the same foreign
  payload, with a declared interface and its recorded corpus run as a
  validation case over a real `PathInfo`
- Maintained template: `examples/execution_adapter_template.py` — ABI
  declaration, one transaction case with decoded argument expectations, one
  decline case, no provider
- Compiled-contract certification:
  `tests/dispatch/test_custom_executor_contract.py` compiles
  `contracts/vyper/custom_executor_probe.vy`, installs it as a foreign
  deployment's declared runtime, and proves which of its functions ran by
  reading its own public getters as the verdict's evidence. The module is the
  worked example of the two requirements above: `custom_executor_reverting.vy`
  is the declaration's twin (same `record(uint256)` ABI, no storage), the decoy
  case is the negative control, and a third case pins the submit lane's refusal
  of declared code with no broadcast configured

The design rationale for a generic execution interface with adapter-owned
versioning is [ADR-098](adr/ADR-098-executor-agnostic-production-execution.md).
The three validation tiers, the declaration document, the case shape, and the
classification of a violation as a hard encode failure are
[ADR-100](adr/ADR-100-progressive-custom-adapter-validation.md).
