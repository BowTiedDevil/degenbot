# ADR-100: Progressive custom-adapter validation — a declared interface, explicit cases, execution-backed certification

**Status: accepted.** Design only: this change adds no production code. It refines
[ADR-025](ADR-025-execution-strategy-seam.md) and
[ADR-098](ADR-098-executor-agnostic-production-execution.md). A foreign adapter's
encoder output becomes checkable against a declaration the operator writes, at
three increasing strengths, and behaviour stays proven only by simulation.

## Context

ADR-098 made a foreign contract a first-class execution target: an adapter's
Encode part returns one complete `TransactionRequest` (`to` + `data` + `value`),
the engine simulates exactly that, and the adapter's Assess part interprets the
observation. Everything the generic layer knows about the adapter's calldata is
that it is bytes. Nothing checks that those bytes are the call the adapter meant
to make.

That gap is now measurable, because the failure modes are already visible in the
shapes this repository ships:

- **A hand-packed payload cannot be checked at all.** The Rust sample
  (`rust/examples/degenbot-execution-sample`) builds
  `selector || optimal_input || final_output || offset || length || hop_outputs[]`
  by hand, from 32-byte words it constructs itself. Its expected-bytes corpus is
  the only thing tying those words to
  `execute(uint256,uint256,uint256[])`. A word misplaced, a `u128` widened
  wrongly, a dynamic offset computed against the wrong head length — each of
  those produces bytes that are *not a call to that function*, and nothing in
  the seam can distinguish them from a valid but different call.
- **A wrong selector is indistinguishable from a capability failure.** The
  engine executes the bytes; a contract that reverts because the selector is
  unknown produces a failed simulation exactly like a contract that refuses an
  unsupported path. The two need different operator reactions and currently
  arrive as the same verdict.
- **A wrong target is caught late and loosely.**
  `simulate_path_on_evm_seam` records a `target-mismatch` bucket after encode,
  and only for the deployment's own declared target.
- **A wrong native value is not checked at all.** The engine sends whatever
  `value` the adapter returned.

The contract side of the same gap is already solved, and the shape of the
solution is the model for this ADR. `tests/dispatch/test_custom_executor_contract.py`
certifies a foreign adapter by *executing* compiled Vyper
(`contracts/vyper/custom_executor_probe.vy`): the adapter encodes `record(uint256)`,
the engine runs the installed runtime, and the verdict's gross figure is the
declared `last_value()` probe delta — so which function ran and what argument it
stored are both evidence. Its decoy case keeps the same read and encodes
`decoy()` instead, proving a wrong selector cannot impersonate a certified run.
That pattern is correct and should stay the last word; it is also minutes of
engine work per case, which is why it cannot be the *first* check an adapter
author meets.

Meanwhile both language samples hand the encoder a stand-in for the core's own
intake: the Python example composes against `types.SimpleNamespace` with the
right attribute names, so the documented path never exercises the real
`SolveResult`/`PathInfo` the seam hands a production callback.

Three constraints bound any answer:

- **Registration stays pure.** Registering a deployment validates identities,
  owner authority, gas limit, and declared code, and reads nothing from the chain
  (ADR-098 D3). It must not acquire a hook that calls an operator's callback or
  talks to a provider.
- **The default is zero-overhead.** An adapter that declares nothing keeps
  today's exact behaviour and cost, and no existing adapter changes.
- **No universal metadata.** ADR-098 D5 and
  [ADR-099](ADR-099-executor-capability-set-commitments.md) D8 settled that a
  foreign contract exposes no `version()`, no release tag, no capability
  commitment, and no manifest. A validation facility that reintroduced a
  version or capability scheme would re-open that decision through the back
  door.

## Decision

### D1 — Three progressive tiers, one home, one direction of dependency

Validation is opt-in in three tiers. Each tier adds evidence; no lower tier
implies a higher one, and the highest tier cannot be replaced by any
combination of the ones below it.

| Tier | What the operator declares | What it proves | When it runs |
|---|---|---|---|
| **0 — none** (default) | nothing | that the engine simulated the bytes the adapter returned | today's path, unchanged |
| **1 — contract interface** | the contract's ABI JSON plus optional target, selector, and native-value policies | the encoder's output is a *declared call to this contract*: declared selector, arguments that decode against the declared types, allowed target, allowed value | every `encode`, once the declaration is attached |
| **2 — validation cases** | explicit `PathInfo` + amount + expected-transaction literals, one per behaviour | the encoder produces *that exact call* for *that exact path*, across the dimensions the cases cover | an explicit call by the author or a CI test; never at registration |
| **3 — execution-backed certification** | a compiled contract (tracked source + declared runtime or deployed code) and its expected evidence | the deployed code *does what the calldata says* | CI and the fork lane; never in a production process |

The facility lives in `degenbot-execution`, beside the seam it checks — the same
crate as `ExecutionAdapter`, `TransactionRequest`, and `ComposeError`, in a new
`interface` module. It does not live in `degenbot-executor` (the built-in command
executor is one adapter, ADR-098 D1), it does not live in the engine (simulation
owns mechanics, ADR-019), and it does not live in the Python binding (Python
states policy data, ADR-025 D3).

The dependency direction does not change: `degenbot-execution` gains no
`degenbot-executor` edge, and
`architecture_gates::foreign_execution_sample_carries_no_concrete_executor_edge`
already refuses that leak transitively through the sample's dependency tree. No
new crate is introduced.

### D2 — The neutral declaration: standard ABI JSON plus three optional policies

A **contract interface** is a value that declares one contract's callable
surface and the transaction policy the adapter promises to obey. It is written
in the one artifact every Solidity/Vyper toolchain already emits, plus three
independent policies that are each optional:

```json
{
  "abi": [ { "type": "function", "name": "execute", "stateMutability": "nonpayable",
             "inputs": [ {"name": "optimal_input", "type": "uint256"},
                         {"name": "final_output", "type": "uint256"},
                         {"name": "hop_outputs", "type": "uint256[]"} ],
             "outputs": [ {"name": "", "type": "bool"} ] } ],
  "target": "0x7777777777777777777777777777777777777777",
  "selectors": ["execute(uint256,uint256,uint256[])"],
  "value": {"kind": "zero"}
}
```

| Field | Required | Meaning |
|---|---|---|
| `abi` | yes | a standard Solidity **ABI JSON array**. Only `type: "function"` entries are part of the callable surface; `constructor`, `event`, `error`, `fallback`, and `receive` entries are accepted and ignored |
| `target` | no | when present, the transaction's `to` must equal it. Omitted means address-agnostic — one declaration may describe deployments of one contract |
| `selectors` | no | a signature allow-list drawn from `abi`. When present, the encoded selector must be one of them; when absent, any function the `abi` declares is allowed |
| `value` | no | the native-value policy. Omitted means unchecked — native value is a funding decision the deployment owns (ADR-098 D2), not a fact the ABI states |

`value` takes exactly one of four forms:

| Form | Meaning |
|---|---|
| `{"kind": "any"}` (default when `value` is omitted) | no check |
| `{"kind": "zero"}` | `value == 0` — a flash-funded contract |
| `{"kind": "max", "max_wei": "123"}` | `value <= max_wei`, decimal string (u256; never a JSON number) |
| `{"kind": "exact", "exact_wei": "123"}` | `value == exact_wei` |

Rules that make the declaration checkable rather than decorative:

1. **Selectors come from the ABI, not from arithmetic in this repository.**
   Each function's 4-byte selector is `keccak256(canonical signature)[..4]` as
   Alloy computes it (`alloy::json_abi::Function::selector`), so a declaration
   and a toolchain cannot disagree about what a signature means. `degenbot`
   never hashes a signature string itself here.
2. **A declaration must contain at least one function.** An empty surface can
   never validate anything, so it is refused instead of silently passing.
3. **Two functions may not share a selector.** A 4-byte collision is refused at
   construction (`DeclarationError::DuplicateSelector`) rather than resolved by
   ordering: the declaration would otherwise be ambiguous about which argument
   types a given calldata must decode against.
4. **An allow-listed signature must exist in `abi`.** Naming a function the ABI
   does not declare is a typo to refuse, not an implicit second declaration.
5. **Argument types are the ABI's own.** Each parameter resolves to an Alloy
   `DynSolType`; nothing in the declaration names a degenbot type.
6. **The declaration carries no version, no release tag, and no capability
   list.** It describes one contract's surface, nothing about its build or its
   future.

### D3 — Rust declares the same interface from `sol!`, and encodes with it

A pure-Rust adapter has a better source of truth than ABI JSON: its `sol!`
declarations. Alloy's `sol!` generates a call struct whose `SIGNATURE`,
`SELECTOR`, and argument types are the same facts the ABI JSON would state, and
whose `abi_encode()` emits `selector || abi.encode(args)` in canonical form. So
the Rust-facing constructor is:

```rust
alloy::sol! {
    interface SimpleExecutor {
        function execute(uint256 optimal_input, uint256 final_output, uint256[] hop_outputs)
            external returns (bool);
    }
}

let data = Bytes::from(SimpleExecutor::executeCall {
    optimal_input: U256::from(inputs.optimal_input),
    final_output: U256::from(final_output),
    hop_outputs: inputs.hop_outputs.iter().copied().map(U256::from).collect(),
}.abi_encode());

let interface = ContractInterface::from_signatures([SimpleExecutor::executeCall::SIGNATURE])?
    .with_target(my_contract)
    .with_value_policy(ValuePolicy::Zero);
```

Two consequences follow, and both are part of the decision:

- **Typed encoding replaces hand-packed words in the Rust sample.** The sample's
  manual word layout retires in favour of `executeCall::abi_encode()`; its
  recorded expected-bytes corpus is unchanged, because `sol!` emits the same
  canonical bytes the hand-packing was imitating. That equality is the
  acceptance criterion for the rewrite, and it is the reason the rewrite is a
  no-op for every committed golden.
- **The encoder and the declaration cannot drift.** Deriving the interface from
  the same `sol!` type that produces the calldata removes the failure mode where
  a hand-written ABI JSON and a hand-written encoder disagree; a Rust adapter
  need not ship ABI JSON at all.

The generic crate gains no encoder: `ContractInterface` checks calldata and
produces none. `degenbot-executor`'s command-stream encoders are untouched.

### D4 — `ValidatedAdapter` is an optional wrapper that forwards, never repairs

Tier 1 attaches by wrapping, so the seam's trait gains no method and no default
implementation changes:

```rust
pub struct ValidatedAdapter<A> { /* inner: A, interface: ContractInterface */ }

impl<A> ValidatedAdapter<A> {
    /// Attach the declaration. Fallible because the inner adapter's declared
    /// probes are checked against the same interface once, here.
    pub fn new(inner: A, interface: ContractInterface) -> Result<Self, DeclarationError>;
    pub fn interface(&self) -> &ContractInterface;
    pub fn into_inner(self) -> A;
}
```

Forwarding semantics, exhaustively:

| Member | Behaviour under the wrapper |
|---|---|
| `encode` | runs the inner `encode`, then validates the returned `TransactionRequest` against the interface before returning it |
| `compose_view` | runs the **inner's** `compose_view` (so an inner override of that projection is never bypassed), then validates |
| `probe_spec` | returned unchanged (the same `&ProbeSpecs`, borrowed from the inner) |
| `assess_options` | returned unchanged |
| `fee_policy` | returned unchanged |
| `assess` | returned unchanged — the inner's assessment, custom overrides included, with `ExecutionResult::price`'s terminal-status invariant intact |

A validation failure never mutates the transaction: the wrapper either returns
the inner's complete request unchanged or returns an error. There is no coercion
of `to`, no zeroing of `value`, and no re-encode. Repair would invent a
transaction the adapter did not produce, which is exactly what the seam exists
to prevent.

Both `encode` entry points are validated because both reach production
(`compose_transaction` calls `encode`; a consumer may call `compose_view`), and a
wrapper that validated only one of them would be a hole a foreign adapter could
fall into by choosing the other.

### D5 — Typed failures, and where each one fires

Two enums carry every refusal. Both are data (no boxed errors, no panics), both
are `Debug + Error + PartialEq + Eq`, and both render a message naming the
offending field.

**`DeclarationError` — the declaration is unusable.** Produced where a
declaration is read (construction, or a case document being turned into typed
values). Reading a declaration invokes no operator callback and performs no I/O.

| Arm | Fires when |
|---|---|
| `AbiJson { detail }` | the text is not a standard ABI JSON array |
| `EmptyInterface` | the ABI declares no function |
| `DuplicateSelector { selector, first, second }` | two declared functions share a 4-byte selector |
| `UnknownSelector { signature }` | an allow-listed signature is not in `abi` |
| `UnparsableSignature { signature }` | a `sol!`-route signature string does not parse |
| `ZeroTarget` | a declared target is `Address::ZERO` |
| `InvalidValuePolicy { detail }` | an unknown `kind`, a missing or non-decimal `max_wei`/`exact_wei`, or a value above u256 |
| `Probe { label, detail }` | a declared probe that targets the interface's own contract names an undeclared selector, or carries argument bytes that do not decode against its declared types |
| `CaseDocument { case, detail }` | a case document is malformed (unknown `family`, missing per-family hop field, unparseable address, both or neither of `arguments`/`calldata`, unknown `expect.kind`) |
| `CaseArity { case, detail }` | a case's `hop_outputs` or `consumed_inputs` length differs from its hop count |
| `CaseAmount { case, field }` | a case amount is negative or above the seam's int128 bound (`i128::MAX`) |

**`ContractViolation` — the encoder's output disagrees with the declaration.**
Produced per encode (tier 1) and per case comparison (tier 2), so both tiers
speak one vocabulary.

| Arm | Fires when |
|---|---|
| `Calldata { detail }` | the payload is shorter than a 4-byte selector |
| `Selector { selector }` | the selector is not declared (or not in the allow-list) |
| `Arguments { signature, detail }` | the bytes after the selector do not decode against the declared parameter types |
| `Target { declared, encoded }` | a declared target disagrees with the request's `to` |
| `NativeValue { policy, encoded }` | the request's `value` violates the declared policy |
| `CaseExpectation { case, field, expected, actual }` | a case's declared evidence disagrees with what the encoder produced (tier 2 only) |

`ContractViolation` reaches the seam's error type as one new `ComposeError`
variant:

```rust
/// The encoded transaction violates the interface its adapter declared.
#[error("declared-interface violation: {0}")]
Validation(ContractViolation),
```

**Its classification is hard, and that classification is the decision.**
`ComposeError::Unsupported` is the routine capability decline that lets a
first-compatible walk examine the next target; `ComposeError::Encode` is that
candidate's own unrecoverable failure. A validation violation belongs with the
second: an adapter that produces calldata its own declaration rejects is broken,
and offering the same path to another target would hide the defect rather than
fix it. Concretely:

- `selection::PreparedEncoding::from_compose_result` maps it to
  `PreparedEncoding::Failed(detail)`, so the walk stops for that candidate.
- `simulator::simulate_path_on_evm_seam` maps it to the existing hard-error
  path: `SimVerdict::Error` with the existing `SIM_PRE_ENCODE` error reason. No
  new telemetry vocabulary is introduced.
- A submission row is never produced for it.

The engine's own `target-mismatch` bucket stays exactly as it is — the last line
for an adapter that declares no interface. When a target *is* declared, the same
defect is caught at encode with the stricter classification, which is the
stricter behaviour the operator opted into.

`AssessError` is untouched. Validation never fabricates economic evidence, and
a refusal is never priced.

### D6 — Hot-path cost, and why the default pays none of it

The encode path runs per solved path per block, inside the fan-out, so the cost
model is part of the decision:

| Tier | Added per-encode work |
|---|---|
| 0 (default) | none. No wrapper exists, no decode runs, no field is added to any adapter |
| 1 | one `[u8;4]` selector lookup in a compiled table, then one `DynSolType` decode over the bytes after the selector. No re-encode, no keccak, no provider, no ABI re-parse |
| 2 | none on any production path: cases run only when a caller asks |
| 3 | engine work, in CI and the fork lane only |

The declaration is **compiled once, at construction**: `ContractInterface`
retains a selector-keyed table of `(signature, parameter types)` and drops the
ABI JSON. Per-encode validation allocates only the decoded argument values
(proportional to the argument count) and releases them immediately; it never
retains state, never caches per path, and never touches the journal or the
provider.

Full ABI decoding therefore never happens on a path that did not ask for it. The
ADR records the cost deliberately: tier 1 is intended for development, CI, and
operators who accept one decode per transaction in exchange for refusing a
broken call before paying for its simulation. No gate requires a microbenchmark,
because the tier-0 path is byte- and cost-identical to today and tier 1 is
opt-in.

There is no `validation` cargo feature. A feature would fork one crate's public
surface and add a lane to the feature matrix to guard code that is inert unless a
declaration is constructed; `alloy`'s default feature set already provides
`json_abi` and `dyn_abi`, so the module adds no dependency edge.

### D7 — Validation cases own a real `PathInfo` and real amounts

A **validation case** is a literal that says: for *this* path, with *these*
amounts, the adapter must produce *that* transaction (or decline).

```python
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
    "expect": {"kind": "transaction",
               "selector": "execute(uint256,uint256,uint256[])",
               "arguments": [10**18, 121 * 10**16, [10**18, 121 * 10**16]]},
}
```

| Part | Rule |
|---|---|
| `name` | unique within a case list; it is the report key and the message prefix |
| `hops` | the **full** `HopInfo` shape per family, not the `SolveResult.hop_descriptors` report shape. `v2`/`v3`: `pool_address`, `token0`, `token1`, `fee`, `zfo`. `v4`: `pool_manager_address`, `pool_id_hex`, `currency0`, `currency1`, `fee`, `tick_spacing`, `hook_address`, `zfo` |
| `amounts` | `optimal_input` (u128), `hop_outputs`, `consumed_inputs` (lists of u128). Both lists must have exactly `len(hops)` entries |
| `expect.kind` | `"transaction"` (then `selector` required, `target`/`value` defaulting to the binding's own, `arguments` **or** `calldata` optional) or `"decline"` (the adapter must refuse with `ComposeError::Unsupported`) |
| `expect.arguments` | decoded argument values in declaration order; the produced calldata is decoded against the interface's declared types and compared |
| `expect.calldata` | an exact calldata golden (hex); mutually exclusive with `arguments` |

**How a Python case gets a real path with no reimplementation and no chain
I/O.** The binding builds the core's own values from the literal:

1. each `hops` entry becomes a real `degenbot_execution::HopInfo` variant
   (`V2`/`V3`/`V4`), and the list becomes a real `PathInfo`;
2. `amounts` becomes a real `ComposerInputs` (the borrowed u128 bundle
   `encode` takes), narrowed against the same `i128::MAX` bound production
   applies;
3. `run_cases` calls `ExecutionAdapter::encode(&path, &inputs)` — the entry
   point production uses (`compose_transaction`) — so a Python callback receives
   the `SolveResult` view the seam already projects from exactly those two
   arguments (`solve_result_from_path_inputs`).

No part of that crosses a network boundary. A case declares its addresses as
literals; there is no pool lookup, no `eth_call`, no engine, no database, and no
path registration. That is the deliberate answer to "where does a path come
from": **from the case, as data**, because a case that fetched its own path
would be a hidden RPC in a validation helper, and a case that borrowed an
engine's path registry would make the check depend on session state. A recorder
that emits case literals from a live solved path is a separate, later decision;
today cases are written by hand.

Two informational bounds the plan fixes rather than leaves open:

- The view the encode seam projects carries `path_id = 0` (only `compose_view`
  carries a real path id), so a case cannot assert a path id and must not try.
- Case comparison reuses tier 1's checks plus `CaseExpectation`, so a case can
  never pass while the same transaction would fail the interface check.

`run_cases` returns one `CaseReport` per case — `name` plus
`Result<(), CaseFailure>`, where `CaseFailure` is `Refused(ComposeError)` (the
adapter declined, or failed, when the case expected a transaction, or produced
one when it expected a decline) or `Mismatch(ContractViolation)`. A malformed
case document is a `DeclarationError` raised before anything is encoded; an
adapter mismatch is data, so a caller can report every failing case in one run.

### D8 — Execution-backed certification is the only behaviour proof

Tiers 1 and 2 check the encoder against its own declaration. They cannot check
the contract, and the asymmetry is the same one ADR-099 D7 records for
capability metadata: **a violation is dispositive, a pass is a screen.** An
operator who writes an ABI that matches their garbage calldata passes tier 1
exactly as a faithful one does. Behaviour lives in code, so only execution
reaches it.

Tier 3 is therefore the pattern this repository already runs, named and
generalized rather than invented:

1. **Source of the code under test.** Either a tracked contract source compiled
   by the pinned toolchain into a runtime installed as the deployment's declared
   code (`SimulationDeployment(code=runtime)` — development only, submission
   skipped, ADR-098 D3), or the code actually deployed at the target read from
   the chain (the `just test-fork` lane). Both are execution-backed; the first
   proves behaviour in CI with no endpoint, the second proves the deployment an
   operator actually runs.
2. **The production path.** The certification drives the same session → batch →
   candidate lane as production, so the adapter's encode, the engine's
   mechanics, and the adapter's assess all run exactly as they will live.
3. **Expected evidence, named per kind:**

| Evidence | What it proves |
|---|---|
| receipt/probe deltas (`gross_profit` = declared post-minus-pre read) | the call moved the balances/state the adapter named |
| the contract's own public getters as declared probes | *which* function ran and *what* it stored — calldata inspection cannot show this |
| return data / captured events | the contract reported the figure the assessment decoded |
| submit-plan rows | the certified candidate reached (or correctly skipped) submission |
4. **A negative control in the same test module.** The existing decoy case
   keeps the identical read set and encodes the wrong function: the verdict must
   report the decoy's own one-bit movement, not the marker. A certification
   without a negative control cannot distinguish "the evidence proves the
   call" from "the evidence proves something".

Registration checks and certification are different instruments and are kept
apart: registration answers "may this deployment run", certification answers
"does its code do what its calldata says". A certification never runs at
registration, and a registration never waits on a simulation.

### D9 — Registration invokes no callback and performs no chain I/O

Made explicit because "validate at registration" is the obvious wrong answer:

- Attaching an interface parses a declaration and checks the adapter's *declared
  probe list* (data). It does not call the adapter's encode callback, does not
  read the chain, and does not touch a provider.
- `SimulationDeployment` and `build_batch_executor` gain no validation hook, no
  interface field, and no new refusal. A validated adapter arrives as an
  ordinary `Arc<dyn ExecutionAdapter + Send + Sync>`, because validation is a
  property of the adapter, not of the deployment.
- Validation cases run only when a caller invokes them, synchronously, over
  literals. They are not a registration step, not a config key, and not part of
  a batch.
- The only place an adapter callback runs is the production encode path and an
  explicit case run — both of which the caller asked for.

### D10 — Python's optional surface

`PayloadAdapter` gains exactly one keyword argument, so a Python adapter's
construction stays one expression:

```python
from degenbot._ffi.execution import PayloadAdapter, abi_encode_call

adapter = PayloadAdapter(
    compose_simple_executor,
    to=SIMPLE_EXECUTOR,
    value=0,
    probes=PROBES,
    min_net_profit=0,
    interface={
        "abi": EXECUTOR_ABI,                                  # standard ABI JSON
        "selectors": ["execute(uint256,uint256,uint256[])"],
        "value": {"kind": "zero"},
        # "target" is omitted: the binding stamps its own `to`
    },
)

reports = adapter.check_cases(CASES)     # explicit; raises ValueError when no interface
assert all(r.passed for r in reports), reports
```

Rules:

1. **`interface` is keyword-only, optional, and additive.** Omitted, the binding
   is byte-identical to today (same fields, same lift, same encode path).
2. **The document is decomposed by the binding, not by the user.** The binding
   serializes the `abi` array to JSON text (the ABI is the only part that needs
   a standard serializer), parses addresses, resolves the value policy, and
   stamps `target` from the binding's `to`. A declared `target` that disagrees
   with `to` is a `ValueError` naming both, because one binding encodes to one
   address.
3. **Construction is pure data validation.** Malformed ABI JSON, an empty
   surface, a duplicate selector, an unknown allow-listed signature, a bad
   policy, or a declared probe to the adapter's own contract that the interface
   does not declare — each is a `ValueError` at construction, before any
   deployment exists. The compose callable is never invoked here.
4. **Per-encode validation runs inside the pyclass's own `encode`**, by calling
   the same `ContractInterface::validate` the Rust wrapper calls. The lifted
   adapter a deployment carries is a plain copy of the pyclass, so a validated
   Python adapter has exactly one validation site.
5. **`check_cases` is explicit, synchronous, and offline.** It requires a
   declared interface (tier 2 sits above tier 1 by construction) and returns
   `list[CaseReport]` — `name`, `passed`, and a `violations` list of rendered
   messages; a malformed case document raises `ValueError` first.
6. **Stub propagation is part of the change, not a follow-up.** Every new
   keyword, property, method, and exception is described in the binding's doc
   comments, so `just gen-stubs` regenerates `src/degenbot/_ffi/execution.pyi`
   with the new surface and `tests/ffi/test_ffi_stub_drift.py` stays green.
   `tests/helpers/seam.py` gains the `CaseReport` alias so test code keeps
   reaching `_ffi` through one door (ADR-013).
7. **Python still authors no encoder and no assessment.** `abi_encode_call`
   remains the only encoding helper; the interface is a checker. A custom
   assessment interpreter stays a Rust-side `assess` override.

### D11 — What this deliberately is not

- **Not a manifest, and not a version or capability scheme.** No release id, no
  protocol major, no encoding version, no capability list, no commitment, no
  scheme tag, no registry. A metadata-free contract (ADR-099 D8) remains first
  class: it simply declares no interface.
- **Not a capability model.** "This path is outside what my contract can encode"
  stays the adapter's own `ComposeError::Unsupported` decision (ADR-098 D5). The
  interface never infers capability from a selector set.
- **Not a plugin framework.** No trait object registry, no dynamic
  registration, no lifecycle, no per-adapter configuration tree. Two public
  types (`ContractInterface`, `ValidatedAdapter`), two enums, one case type, and
  one report type.
- **Not a declarative encoder.** The declaration never maps path fields to call
  arguments; the adapter's code does that. A declaration that *did* map them
  would be a DSL, and no generic mapping exists for path-dependent payloads.
- **Not an assessment change.** `AssessOptions`, `FeePolicy`, `assess`, and
  `ExecutionResult::price` are untouched; the wrapper forwards the inner's
  assessment verbatim.
- **Not an extension of the built-in executor.** `cmd_executor` keeps its own
  release, manifest, and golden-byte policy. Nothing here imports
  `degenbot-executor`, and the built-in adapter needs no declaration.

## Consequences

- An adapter author gets a cheap first check (does my calldata decode as the
  call I declared?) that costs no simulation, and a second check (does it encode
  *this* path to *that* call?) that costs one `encode` call and no chain access.
- A broken foreign encoder fails at encode with a typed, named violation instead
  of paying for a simulation that reverts and reporting an ambiguous verdict.
- `ComposeError` gains one variant, so every exhaustive match over it acquires
  one arm: `degenbot-arbitrage`'s `selection.rs` and `simulator.rs` today. The
  variant is a public type-shape change and needs the full-feature lint lane.
- The Rust sample's hand-packed encoder retires in favour of `sol!`, keeping its
  recorded corpus byte-identical; the Python example stops composing against
  `SimpleNamespace` and runs a real case instead.
- Adapters that declare nothing are untouched: no new trait method, no new
  required field, no wrapper, no decode, no registration change, no
  configuration key.
- Four documentation surfaces grow one section each (adapter guide, strategy
  reference, glossary, template), and the certification module gains the
  interface-declaring variant of the test it already runs.

## Alternatives considered

| Alternative | Why it does not fit as well |
|---|---|
| **Make the ABI declaration the encoder** (map amounts to arguments declaratively) | That is a plugin DSL: path-dependent payloads (fees, directions, per-family argument shapes, V4 pool identity) cannot be expressed by a generic mapping, so the DSL would grow until it became a programming language — and the adapter's code, which already exists, would be reimplemented beside it. |
| **Require a declaration for every adapter** | Breaks the zero-overhead default and the "existing adapters work unchanged" rule, and charges every operator a decode for a check that a mismatch screen, not a proof, anyway. |
| **Validate inside the engine's registration** | Registration is deliberately pure (identity, owner, gas, declared code) and runs with no provider and no adapter callback. A validation hook there would call operator code at startup and could fail a session on an encoder defect that only one path triggers. |
| **Validate in `assess`, from the simulated calldata** | Too late: the simulation has already been paid for, and it conflates assessment evidence (economic) with encoder conformance (structural). It would also make every assessment decode calldata it does not need. |
| **Exact-bytes goldens only** (tier 2 without tier 1) | A golden pins one input and says nothing about the next path, and a byte diff cannot distinguish "this is not an ABI call at all" from "this is a different valid ABI call" — the distinction an author needs. |
| **A generic version/capability manifest for custom executors** | Settled against by ADR-098 D5 and ADR-099 D8: degenbot cannot enumerate the features of contracts it knows nothing about, and immutable deployments outlive the documents that describe them. |
| **A separate `degenbot-execution-validate` crate** | One more crate for two types and one decode, and a second place to keep the seam's value types in sync. The module is inert without a declaration. |
| **A `validation` cargo feature** | Forks one crate's public surface and adds a feature-matrix lane to guard code that costs nothing when unconstructed; `json_abi`/`dyn_abi` already arrive through `alloy`'s default features. |
| **Have cases fetch their own path from an engine or a provider** | That is the hidden chain I/O and the session-state coupling this ADR refuses; a case is data, and a recorder is a separate decision. |

## Acceptance

The design is implemented when these pass. Each name is an acceptance
criterion and fails on the defect it describes; the workstream that owns it is
in parentheses.

**Tier 0 — the default is untouched (`degenbot-execution`, `degenbot-arbitrage`)**

1. `an_adapter_without_a_declaration_encodes_byte_identically` — the same
   adapter, unwrapped, produces the same `to`/`data`/`value` constants the
   existing suites pin, and no decode runs. (D4MM42)
2. `an_adapter_written_before_this_adr_meets_the_unchanged_trait` — the
   existing `degenbot-execution` foreign-adapter and seam suites run without
   edits, so a tier-0 adapter compiles and runs against the same trait, and
   `SimulationDeployment`/`DeploymentRegistry` gain nothing. (D4MM42)

**Tier 1 — declared interface, per-encode checks (`degenbot-execution`)**

3. `a_conforming_transaction_passes_its_declared_interface` — a `sol!`-encoded
   call validates against the interface derived from the same declaration.
4. `an_undeclared_selector_is_a_validation_violation` — hand-packed bytes whose
   selector is not in the ABI produce `ContractViolation::Selector`.
5. `argument_bytes_that_do_not_decode_against_the_declared_types_are_refused` —
   a truncated dynamic array (bad offset/length) produces
   `ContractViolation::Arguments` naming the signature.
6. `a_target_policy_naming_another_contract_is_refused` — encoded `to` differs
   from the declared target.
7. `the_native_value_policy_refuses_zero_max_and_exact_differently` — the three
   arms, including an `any` policy that does not check.
8. `the_compiled_selector_table_refuses_a_duplicate_selector` —
   `DeclarationError::DuplicateSelector`, not a silently chosen winner.
9. `an_interface_declared_from_sol_signatures_matches_its_abi_json_declaration`
   — the two construction routes agree on selectors and parameter types (the
   drift gate for adapters that ship no ABI JSON).
10. `a_declared_probe_to_the_adapters_own_contract_must_be_declared` — a probe
    naming an undeclared selector is `DeclarationError::Probe`; a probe to a
    *different* contract (WETH) is left alone.

**Wrapper semantics and classification (`degenbot-execution`, `degenbot-arbitrage`)**

11. `the_wrapper_forwards_every_assessment_half_unchanged` — `probe_spec`,
    `assess_options`, `fee_policy`, and a custom `assess` override return
    exactly what the inner adapter returns.
12. `the_wrapper_forwards_compose_view_so_an_inner_override_is_not_bypassed` —
    the inner's `compose_view` runs and its output is validated.
13. `a_validation_failure_is_a_hard_encode_failure_not_a_capability_decline` —
    `PreparedEncoding::Failed`, the walk stops, the simulation records
    `SimVerdict::Error` with `SIM_PRE_ENCODE`, and no submit row exists, in
    `degenbot-arbitrage`'s suite. (D4MM42)
14. `a_capability_decline_still_advances_to_the_next_target` — the untouched
    contrast case: `ComposeError::Unsupported` behaviour is unchanged under a
    declared interface. (D4MM42)

**Tier 2 — validation cases (`degenbot-execution`, Python binding)**

15. `a_case_hands_the_encoder_a_real_path_info_and_real_amounts` — the case's
    hops arrive as `HopInfo::{V2,V3,V4}` with every per-family field
    (`fee`, `zfo`, `tick_spacing`, `pool_id_hex`, `hook_address`) and its amounts
    as `ComposerInputs`; a two-hop V2 case reproduces the sample's recorded
    corpus. (D4MM42 + FXU3XU)
16. `a_case_whose_adapter_declines_the_path_reports_the_decline` —
    `expect.kind == "decline"` accepts `ComposeError::Unsupported` and rejects a
    produced transaction. (D4MM42)
17. `a_case_with_a_mismatched_calldata_golden_names_the_case_and_the_diff` —
    `ContractViolation::CaseExpectation` carrying the case name. (D4MM42)
18. `a_malformed_case_is_refused_before_any_encode_runs` — wrong arity, an
    amount above `i128::MAX`, an unknown family tag: `DeclarationError`, and a
    counting adapter records zero `encode` calls. (D4MM42 + FXU3XU)
19. `case_validation_takes_no_provider_and_reads_no_chain` — the case path has
    no provider parameter at all, and a Python case run makes no provider call
    (a counting fake provider stays at zero). (FXU3XU)

**Python surface (`tests/dispatch/test_custom_executor_interface.py`)**

20. `test_payload_adapter_accepts_an_interface_declaration` — construction
    succeeds, the declaration is not echoed back, and the adapter encodes as
    before.
21. `test_payload_adapter_refuses_a_malformed_interface` — bad ABI JSON, an
    empty surface, a duplicate selector, an unknown allow-listed signature, a
    bad value policy, and a declared target that disagrees with `to` are each a
    `ValueError` naming the problem.
22. `test_registering_a_validated_adapter_invokes_no_callback` — constructing
    `PayloadAdapter(..., interface=...)` and a `SimulationDeployment` around it
    leaves a counting compose callable at zero calls.
23. `test_an_interface_violation_reaches_the_verdict_as_an_encode_error` — a
    production-path batch whose adapter encodes an undeclared selector records
    the hard encode failure and produces no submit row.
24. `test_check_cases_requires_a_declared_interface` — `ValueError`, not a
    silent no-op.
25. `test_the_interface_surface_is_in_the_generated_stub` —
    `just gen-stubs` output carries the keyword, the method, and `CaseReport`;
    the stub-drift gate passes. (FXU3XU)

**Tier 3 — execution-backed certification**

26. `test_a_certified_adapter_encodes_the_call_the_compiled_contract_answers` —
    the existing certified case, now with a declared interface: the verdict's
    gross is the `last_value()` probe delta and the declaration passed on the
    same encode. (5LEZFY)
27. `test_a_decodable_interface_is_not_a_behaviour_proof` — the honest
    asymmetry: a declaration that decodes fine over a contract that reverts is
    caught by execution, not by tier 1. (5LEZFY)
28. `test_declared_code_stays_refused_for_live_submission` — re-pinned for the
    certification deployment: injection still skips broadcast. (5LEZFY)

**Docs and template (R5ZW5S)**

29. `examples/execution_adapter_template.py` runs offline end-to-end: it
    declares an interface, runs two cases (one pass, one decline), and asserts
    both reports — no provider, no engine, no fixture.
30. `docs/execution-adapters.md`, `docs/execution-strategy.md`, and
    `GLOSSARY.md` name the three tiers, the declaration, the case shape, and the
    failure table with no contradiction against this ADR.

## Implementation plan

Four workstreams, in this order. Each names its files, its API surface, and what
makes it done; nothing here needs a further design decision.

### D4MM42 — Rust core and sample

**Scope.** The whole tier-1 and tier-2 facility in `degenbot-execution`, the
`ComposeError` variant and its two classification sites, and the sample's
migration to `sol!` encoding with a declared interface.

**Files.**

- `rust/crates/foundation/degenbot-execution/src/interface.rs` (new):
  `ContractInterface` (`from_abi_json`, `from_signatures`, `with_target`,
  `with_selectors`, `with_value_policy`, `target`, `value_policy`,
  `selectors`, `validate`, `validate_probes`), `ValuePolicy`,
  `DeclarationError`, `ContractViolation`, `ValidatedAdapter`
  (`new`/`interface`/`into_inner` + the forwarding `ExecutionAdapter` impl),
  `ValidationCase`, `CaseAmounts`, `Expected`, `ExpectedTransaction`,
  `ExpectedCall`, `CaseReport`, `CaseFailure`, `run_cases`.
- `rust/crates/foundation/degenbot-execution/src/lib.rs`: `pub mod interface;`
  plus re-exports of the new public names.
- `rust/crates/foundation/degenbot-execution/src/payload.rs`:
  `ComposeError::Validation(ContractViolation)` and its doc.
- `rust/crates/foundation/degenbot-execution/src/adapter.rs`: doc note that the
  wrapper forwards `compose_view` (no trait change).
- `rust/crates/foundation/degenbot-execution/Cargo.toml`: no dependency change;
  `alloy`'s default features already expose `alloy::json_abi` and
  `alloy::dyn_abi` (verified: `alloy` default → `essentials` → `contract` →
  `dyn-abi`/`json-abi`). Do not add `serde`/`serde_json`: name no serde type,
  render `JsonAbi::from_json_str`'s error with `to_string()`.
- `rust/crates/engine/degenbot-arbitrage/src/selection.rs`: one arm —
  `ComposeError::Validation(detail)` → `PreparedEncoding::Failed(detail.into())`
  (i.e. treat it exactly like `Encode`).
- `rust/crates/engine/degenbot-arbitrage/src/simulator.rs`: both matches gain the
  same hard-error arm (`SimVerdict::Error`, `SIM_PRE_ENCODE`, `ProviderError`
  with the rendered violation).
- Tests: `degenbot-execution/tests/interface.rs` (acceptance 3–12, 15–18),
  `degenbot-arbitrage/tests/validation_classification.rs` (13–14).
- `rust/examples/degenbot-execution-sample/src/lib.rs`: replace `to_word`/manual
  word layout with `alloy::sol!` `executeCall::abi_encode()`; add
  `SimpleExecutorComposer::interface()` built from
  `executeCall::SIGNATURE`; keep the recorded corpus constant **unchanged** and
  add `typed_encoding_reproduces_the_hand_packed_corpus`; wrap the strategy in
  `ValidatedAdapter` in one documented example call site.

**Done when.** `just test-rust-nextest -p degenbot-execution`,
`-p degenbot-arbitrage`, and `-p degenbot-execution-sample` pass;
`just check-rust-consumer` passes; `just lint-rust-check` passes (the
`ComposeError` variant is a public shape change).

### FXU3XU — Python FFI surface

**Scope.** The `interface=` keyword, per-encode validation inside the pyclass,
`check_cases` with the case-document translation, stub propagation, and the
Python-side tests. Depends on D4MM42.

**Files.**

- `rust/crates/shells/degenbot-python/src/execution/mod.rs`:
  `PyPayloadAdapter` gains `interface: Option<ContractInterface>`; `new` grows a
  keyword-only `interface=None`; a `#[pyclass] PyCaseReport` (getters `name`,
  `passed`, `violations`); an `ExecutionAdapter::encode` that calls
  `ContractInterface::validate` when the field is `Some`; a `check_cases`
  method that decomposes each case dict (hops per D7's per-family table,
  amounts narrowed against `i128::MAX`, `expect` per D7) into `ValidationCase`s
  and returns `Vec<PyCaseReport>` from `run_cases`. The `abi` array is
  serialized with the stdlib `json` module from Rust; addresses go through
  `crate::address_utils::parse_address`; `target` defaults to the binding's
  `to` and a disagreeing declaration is a `ValueError` naming both.
- Doc comments on every new keyword, getter, and method (they feed
  `just gen-stubs`), including the "no callback, no chain I/O at construction"
  guarantee and the `ValueError` cases.
- `src/degenbot/_ffi/execution.pyi`: regenerated by `just gen-stubs`, committed
  as produced.
- `tests/helpers/seam.py`: export `CaseReport` beside `PayloadAdapter`.
- `tests/dispatch/test_custom_executor_interface.py` (new): acceptance 20–24.
- A counting compose callable and a counting provider live in
  `tests/helpers/` if reused; do not duplicate them in the new module.

**Done when.** `just dev` then `just gen-stubs` produce the committed stub, and
`just test-python-noise-gate '' tests/dispatch/test_custom_executor_interface.py
tests/ffi/test_ffi_stub_drift.py` and `just lint-rust-check` pass.

### 5LEZFY — explicit certification

**Scope.** Execution-backed certification as an acceptance surface: the
certified case declares an interface, the asymmetry test exists, and the
classification is proven through the production path. Depends on D4MM42 (and
reads naturally after FXU3XU, since the certified adapter is a Python one).

**Files.**

- `tests/dispatch/test_custom_executor_contract.py`: declare `interface=` on the
  certified `PayloadAdapter` (acceptance 26); add
  `test_a_decodable_interface_is_not_a_behaviour_proof` (a declaration that
  decodes over a contract which reverts — the marker is not stored and no submit
  row appears); re-pin `test_declared_code_stays_refused_for_live_submission`
  for the certified deployment (28). Reuse `_CannedChainNode`,
  `probe_runtime`, and `_one_batch` unchanged.
- `rust/crates/engine/degenbot-arbitrage/tests/validation_classification.rs`:
  read-only for this workstream — D4MM42 owns the file. If the classification
  test is not yet in the arbitrage suite when this work starts, stop and report
  it rather than duplicating the assertion.
- `docs/execution-adapters.md`: the evidence table for tier 3 (D8's four
  evidence rows) and the negative-control requirement.

**Done when.**
`just test-python-noise-gate '' tests/dispatch/test_custom_executor_contract.py`
passes with the new and the existing cases, and every verdict assertion names
the probe/return evidence it reads.

### R5ZW5S — template and docs

**Scope.** The copy-pasteable authoring path and the documentation surfaces.
Depends on FXU3XU for `check_cases` and on 5LEZFY for the certification text.

**Files.**

- `examples/execution_adapter_template.py` (new): the three-tier skeleton — an
  ABI JSON constant for the author's own contract, a `PayloadAdapter` with an
  `interface=`, two cases (a transaction case with decoded-argument
  expectations and a decline case), and a `main()` that asserts both reports.
  Offline: no provider, no engine, no fixture, no RPC URL.
- `examples/execution_strategy_foreign.py`: retire the `SimpleNamespace`
  stand-in — run `adapter.check_cases([...])` against a real declared `PathInfo`
  instead, so the shipped example exercises the seam's own intake.
- `docs/execution-adapters.md`: a "Validating your adapter" section (three
  tiers, the declaration document, the case shape, the failure table, the
  cost/opt-in statement, the certification pattern and its asymmetry), plus an
  ADR-100 link in "Reference".
- `docs/execution-strategy.md`: add the new names to the consumer-visible
  signature table (`ContractInterface`, `ValuePolicy`, `ValidatedAdapter`,
  `ValidationCase`, `CaseReport`, `ComposeError::Validation`) and one line in
  "What is NOT on offer" stating that validation is a checker, not a plugin
  framework.
- `GLOSSARY.md`: `Contract interface`, `Validation case`, `Certification`
  entries (one or two sentences each, `_Avoid_` lines for "schema", "manifest",
  "plugin").

**Done when.** `just lint-markdown-check` and `just lint-comment-hygiene` pass,
and `just py python examples/execution_adapter_template.py` exits zero with its
self-check assertions intact.
