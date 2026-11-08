# ADR-098: Executor-agnostic production execution — `cmd_executor` is one adapter

**Status: accepted (implementation in progress).** Refines ADR-025 and ADR-033: the
execution-strategy seam becomes the production interface for both the built-in
command executor and foreign executors, with neutral opportunity types and no
generic dependency on the command-executor implementation.

## Context

An MEV operator needs degenbot's pool state, solver, exact-block simulation,
revert diagnostics, access-list generation, signing, nonce management, relay
submission, and receipt monitoring. The operator's execution contract is the
part most likely to be bespoke: its calldata schema, funding and custody model,
profit observation, deployment lifecycle, and versioning policy are not general
facts degenbot can prescribe.

ADR-025 established Encode / Probe / Assess / Fee as the foreign-adapter seam,
but the canonical pipeline still embodies the developer's `cmd_executor`:

- `degenbot-execution` imports `PathInfo` and `HopInfo` from
  `degenbot-executor`, so a foreign adapter must depend on the implementation it
  is meant to replace.
- `degenbot-arbitrage` calls `encode_cmd_stream` directly and hardcodes the
  WETH/ETH/ERC6909 balance vector, bypassing the adapter's Encode and Assess
  decisions.
- Batch simulation carries one session-global executor address, owner, runtime
  artifact, PoolManager, and WETH identity.
- The foreign examples prove encoding, but not production simulation or
  submission.

Treating the current command stream as the generic interface would obligate
foreign consumers to inherit its protocol versions, capability metadata,
release manifests, Vyper artifacts, and future migration policy. Immutable
contracts make that especially harmful: an operator's deployed address can
outlive the repository version that inspired it.

## Decision

### D1 — The generic execution crate depends on no concrete executor

`degenbot-execution` is a foundation-layer, pyo3-free seam. Neutral opportunity
descriptors (`PathInfo`, `HopInfo`, and their family payloads) live beside the
execution interface, not in `degenbot-executor::composers`. The dependency is
`degenbot-executor → degenbot-execution`; the reverse edge is deleted. There is
one set of path types, with no compatibility duplicates.

`SolvePathResult` remains owned by `degenbot-pools`. `EncodeRequest`,
`EncodeContext`, command grammar, opcode encoders, packed command config,
warmup-slot math, and Vyper artifact selection remain `cmd_executor`
implementation details.

The dependency rule applies to production call paths as well as Cargo metadata:
a generic simulation or submission module must not call the built-in encoder or
import its assessment model. The built-in depends on generic simulation and
submission capabilities, never the reverse.

### D2 — Encode produces a transaction; Assess interprets an observation

The generic Encode result is one complete `TransactionRequest`:

```text
to: Address
data: Bytes
value: U256
```

An adapter may use any ABI or byte-stream schema and may keep any private
protocol version, capability, release, or deployment state it needs. A simple
encoding-only adapter can retain a small `PayloadComposer` interface, but the
production boundary preserves target and value as well as calldata.

The engine owns simulation mechanics: declared pre/post probe reads, transaction
execution, gas and return data, traces, captured events, and access lists. It
returns a typed `ExecutionObservation`. The adapter's Assess decision converts
that observation into economic success, gross profit, and diagnostics. Common
sum-of-deltas and return-value shapes may be defaults; neither is mandatory.
Fee pricing remains the defaulted pricing half of Assess rather than a fifth
execution concern, so an adapter's priced result — its priority fee, net,
threshold, and verdict — is returned unchanged unless the deployment explicitly
opts into the settlement strategy's market-aware re-pricing. The generic seam
never infers that choice from the adapter's type.

The production scope is **one opportunity → one transaction**. A contract may
batch internally. Multi-transaction strategy choreography and a free-form
consumer-owned simulation loop are outside this seam.

The deployment's final verdict travels with the result. Generic dispatch joins
it with its own gas floor, so a failed assessment is never gas-profitable or
submitted even when its gross and net are positive; such a result stays
visible as a diagnostic. A declared code override is installed only at the
address the adapter's own transaction targets, so a mismatch is refused rather
than simulated.

### D3 — Production simulation is adapter-driven end to end

The composition root injects execution adapters. Foreign and built-in adapters
then cross the same production simulation path: `Opportunity` → Encode → engine
observation → Assess → generic dispatch/submission policy.

Block-wide facts (provider, block environment, fee samples, Multicall3) are
separate from deployment facts (adapter, target, owner, declared runtime and
warmup). The settlement seven-call balance bundle and all WETH/PoolManager
assessment assumptions leave generic simulation. The built-in executor reports
its own receipt (D7), so that bundle is retired rather than preserved as a
second assessment path: the built-in adapter declares no probe reads and
assesses the `execute` return data. Receipt-vs-seven-call parity is pinned by
`degenbot-arbitrage/tests/execution_receipt_parity.rs`.

Actual deployed code is the production simulation authority. An adapter may
supply declared code/storage/balance overrides for development or an undeployed
build; injected code is never silently substituted for a different deployed
implementation and cannot enable live submission.

Python consumers continue to meet the same seam through the thin PyO3 lift.
Python may author a foreign Encode blob and declared probe/assessment policy;
it does not acquire a second simulation implementation or a Python mirror of
the built-in command composer.

### D4 — Deployment identity is generic; release identity is adapter-owned

A bot may hold multiple heterogeneous execution deployments. Neutral
`DeploymentId` and `CandidateId { path_id, deployment_id }` identity travels
through composition, simulation, outcome joins, and submission so the same path
at two addresses cannot collide.

Generic identity includes chain, address, owner, and operator-facing routing
identity. It does not include `cmd_executor` protocol versions, capabilities,
release manifests, or runtime-artifact conventions. Target-selection policy is
separate from target lookup: an operator may prefer deployments in order or
explicitly fan out, and a refusal at one deployment does not suppress another.

A deployment and an implementation release are different facts. Two addresses
may run the same release; one address is permanently bound to its deployed
implementation.

### D5 — `cmd_executor` versioning is an optional built-in policy

The `cmd_executor` adapter may discover and validate its own protocol version,
capability set, and release ID; select a matching encoder and artifact; and
bind multiple deployments independently. Unknown versions or releases fail at
binding rather than defaulting to the newest encoder. Capability-aware planning
covers the complete call encoding — stream, config, and execute ABI — and a
missing capability produces a documented fallback or decline.

None of those getters, manifest fields, or support policies is part of the
generic execution interface. A foreign contract need not expose metadata or
participate in degenbot's release process. There are no proxy requirements and
no upgradeability assumptions.

### D6 — The public acceptance seams are consumer, production simulation, and built-in regression

The three confirmed test surfaces are:

1. **Rust consumer seam.** A foreign crate implements and drives an adapter
   without depending on `degenbot-executor`. This catches dependency and
   interface leakage, not the foreign contract's own correctness.
2. **Production simulation seam.** A foreign `TransactionRequest` uses the same
   simulation machinery as the built-in and receives adapter-defined Assess.
   This catches direct-encoder and fixed-balance-bundle coupling, not relay
   delivery.
3. **Built-in regression seam.** Existing `CmdExecutorAdapter` behavior and
   command-stream golden bytes remain stable during extraction. This catches
   built-in protocol drift, not genericity by itself.

The acceptance proof for genericity is a foreign contract with no
`version()`, `capabilities()`, `release_id()`, WETH, PoolManager, or command
stream using the production path. The acceptance proof for independent
versioning is two defined `cmd_executor` deployments selecting different
encoders beside that foreign target.

### D7 — The built-in executor returns an explicit `ExecutionReceipt`

The built-in contract owns its profit measurement and returns
`ExecutionReceipt { gross_profit, bribe_paid }`. `gross_profit` is the selected
WETH-equivalent balance-domain increase after all commands and before bribe
payment; `bribe_paid` is the actual native ETH transferred to the bribe
recipient, including any available-balance cap. Built-in Assess decodes these
values from simulation return data, computes retained profit as gross minus
bribe, and applies gas and fee policy exactly once.

`profit_mode` selects the accounting domain: physical WETH + native ETH (1) or
PoolManager ERC6909 WETH claims (2). Every ordinary `execute` measures and
reports; there is no successful unchecked mode whose zero return means
“unmeasured.” Config bits 32–255 become `min_profit`, replacing the ignored
`expected_value`. Execution requires both a non-decreasing selected balance and
`gross_profit >= min_profit`. Off-chain policy packs this word from a typed
`ExecutionPolicy` and still owns gas profitability and operator margins.

Sweep mode is not an execution profit mode. Simple physical withdrawals remain
owner-only `withdraw` operations; any command-driven claims sweep uses a
separate owner-only entrypoint with no profit or bribe semantics. Modes 0 and 3
are invalid in the new protocol.

Simulation return data is assessment evidence, not a transaction-receipt fact:
Ethereum receipts do not preserve it. The on-chain non-decrease and minimum
profit guards therefore remain live protection after simulation.

The contract source, Ape tests, and Vyper toolchain are first-class root-project
concerns rather than a nested uv/Ape project. The pinned Vyper compiler and
compile-vs-use artifact gates remain authoritative.

## Consequences

- `degenbot-execution` moves from `crates/engine` to `crates/foundation` and
  becomes the neutral home for opportunity descriptors. The workspace and
  capability/dependency records change with it.
- `degenbot-executor` becomes the optional built-in command-executor adapter;
  the umbrella may re-export it, but generic crates cannot require it.
- Generic transaction observation replaces assumptions that profit means a
  WETH/ETH/ERC6909 delta. The settlement model remains available as a built-in
  adapter with unchanged golden behavior.
- Candidate identity and simulation/submission target data must become
  deployment-aware before one bot can safely fan out to multiple contracts.
- Foreign adapters can evolve their contracts and deployment policy without a
  degenbot release. Built-in protocol majors can likewise evolve without
  changing the generic interface.

## Alternatives considered

- **Make the command stream versioned but keep it as the generic interface.**
  Rejected: it solves compatibility only for this contract and forces every
  foreign consumer to inherit its schema and release policy.
- **Add a generic capability/version enum covering every executor.** Rejected:
  degenbot cannot enumerate features of contracts it knows nothing about.
  Capability discovery belongs inside each adapter.
- **Offer a free-form simulation callback.** Rejected by ADR-019's separation:
  the engine owns execution mechanics; adapters own declared observations and
  assessment, not the sim loop.
- **Standardize an upgradeable executor proxy.** Rejected: immutable
  deployments are first-class, and upgradeability cannot be retrofitted or
  imposed on operators.
