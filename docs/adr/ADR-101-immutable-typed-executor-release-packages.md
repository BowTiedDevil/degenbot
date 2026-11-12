# ADR-101: Immutable typed executor release packages

**Status: accepted (implemented).** Refines
[ADR-098](ADR-098-executor-agnostic-production-execution.md) D5 and carries
[ADR-099](ADR-099-executor-capability-set-commitments.md)'s independent
capability commitment into a complete release-lifecycle policy.
`cmd_executor/2` is the official tracked starting point. The abandoned TGUZCT
deployment is not a member of this release series and acquires no compatibility
adapter.

## Context

The built-in executor already pairs a validated manifest with a
`ReleaseEncoding`, checks deployment metadata, and binds a release's own
artifact. Those mechanisms establish that a release exists; they do not make its
lifecycle invariant:

- `CmdStreamV1Encoding` is a mutable implementation shared by the current
  release, while `ReleaseRegistry::production()` is a hand-written list whose
  omissions are discovered only when code calls it.
- `CmdExecutorBinding::adapter()` selects that encoding directly and declines
  capability planning. Production can therefore bypass the release policy that
  `BoundCmdExecutor` implements.
- Recompiling the tracked Vyper source and replacing its artifacts is a valid
  working-tree operation whether or not the author bumped the release. The
  compiler-vs-use gate proves agreement with the current source, not that the
  previous release was preserved.
- Release tag, call-encoding version, protocol version, and capability set are
  separate facts. A single "version bump" rule for all four would either cause
  unnecessary redeployments or permit wire/artifact drift under an unchanged
  identity.

The abandoned deployment illustrates the failure mode. Its runtime exactly
matches the TGUZCT artifact at `f54f4bd6`, including a scalar `uint256` execution
result. Current code emits and decodes the two-word `ExecutionReceipt`. Mapping
that deployment to either metadata release would be a fabricated compatibility
claim, not release selection.

Three forces shape the solution:

1. **Deployed contracts are immutable.** A semantic change is a new
   implementation and a new address, never an edit to the meaning of an existing
   deployment.
2. **Off-chain conformance fixes are real.** An encoder can be wrong while the
   deployed contract and its declared interface are already correct. Requiring a
   new contract for every such correction would be operationally absurd.
3. **Rust cannot prove Vyper behavior.** Type-level pairing is valuable, but
   source/artifact CI and execution-backed evidence remain necessary.

## Decision

### D1 — Version axes move independently, by rule

The release tag identifies one immutable implementation package. The call
encoding identifies its command stream, packed config, and `execute` ABI with
receipt. Capability IDs identify features within that encoding. The protocol
version identifies the neutral engine's dispatch compatibility. None is inferred
from another.

| Change | Release tag | Call encoding | Capability IDs/hash | Protocol |
|---|---|---|---|---|
| Documentation or tests only | unchanged | unchanged | unchanged | unchanged |
| Off-chain-only compatible planning change | unchanged | unchanged | unchanged | minor may increase |
| Encoder conformance fix (D6) | unchanged | unchanged | unchanged | unchanged |
| Compiler or runtime-bytecode change | **next tag** | unchanged if the wire is unchanged | unchanged | unchanged |
| Metadata getter change | **next tag** | unchanged | changes only if the set changes | unchanged |
| Command-stream, config, or receipt change | **next tag** | **next version** | update for affected capabilities | usually unchanged |
| Capability removal or semantic change | **next tag** | bump when the wire changes | **new ID; old ID retired; new hash** | usually unchanged |
| Neutral engine cannot dispatch old and new under one model | **next tag** | usually bumped | as required | **next major** |

Protocol minor versions are for backward-compatible off-chain planning and
dispatch changes only. They do not track contract changes. Capability IDs remain
ascending, unique, non-zero, and never reused; a removed capability leaves a gap.

### D2 — A release is one immutable package

Each `degenbot-executor/releases/vN/` package owns its contract source, manifest,
compiler artifacts, encoding implementation, and normative goldens. The package
is the unit of review and certification: no part can be upgraded independently
and still call itself the same release.

Once accepted, its contract source, manifest, artifacts, and normative interface
goldens are append-only. Semantic work begins as the next sequential package.
There is no `latest` directory or alias whose contents silently move. One
explicit composition-root selection names the production release.

The initial package is `v2`. The earlier metadata `/1` remains source history,
not a production compatibility obligation. TGUZCT and its scalar receipt are not
backfilled into the registry.

### D3 — Sealed release types make incomplete registration a compile error

Each release has a sealed Rust type associating its encoding and receipt
implementations with its manifest and artifact. The bounded production registry
enumerates those types exhaustively. Adding a release without its required
registration and associated pieces fails compilation rather than becoming a
runtime surprise.

Type erasure, where needed by the generic engine, happens only after the
release-specific pair is complete. The generic `ExecutionAdapter` seam remains
free of release tags, capability commitments, manifests, and Vyper artifacts.

This deliberately trades a small amount of per-release registration code for
locality and compiler-checked completeness. Fully generated release packages are
not required; generation may later remove repetitive metadata declarations if
that proves worthwhile.

### D4 — Production must use the release-bound adapter

The production shortcut that constructs `CmdStreamV1Encoding` directly with no
capability planning is removed. Production adapter construction goes through the
same validated release binding as metadata-driven consumers. Capability planning
is therefore present wherever the built-in adapter encodes a candidate.

An explicit composition-root release selection is configuration, not discovery.
Unknown, incomplete, or mismatched releases fail closed. There is no fallback to
the newest encoding, no alternate receipt decoder, and no abandoned-contract
legacy mapping.

### D5 — CI carries the immutability and artifact proofs Rust cannot carry

A deterministic release ratchet rejects modification, deletion, or tag reuse for
accepted packages. A new package must have the next sequential tag and the full
required package contents. A negative probe mutates a frozen package and proves
the gate fails; the clean tree proves the same gate passes.

The existing pinned-compiler gate remains authoritative for source/artifact
agreement. Release certification additionally requires:

- manifest and on-chain metadata agreement;
- recomputed capability commitment;
- exact calldata and strict receipt-shape goldens;
- capability planning through the public release-bound adapter;
- representative execution against the release's own injected artifact;
- refusal of unknown releases, foreign artifacts, and unregistered definitions.

A gate that merely recompiles the current source is not a release-preservation
proof. The append-only ratchet supplies that missing half.

### D6 — Conformance fixes are the narrow exception to semantic version churn

An off-chain change is a **conformance fix** only when the deployed contract and
its declared semantics were already correct and the change brings the encoder or
other off-chain implementation into agreement with them. It keeps the release
and call-encoding identities and adds focused regression plus execution-backed
proof.

A conformance fix may not change the contract, manifest, capability meaning, or
accepted interface to fit the implementation. When intent is unclear, the change
is a new release, not a conformance fix. The exception is an explicit review
decision; the ratchet does not pretend to infer intent.

### D7 — Deployment certification remains separate and mandatory

A green package is not proof of a deployment. Each new immutable deployment must
verify its creation input, runtime bytes and immutables, metadata declaration,
and one representative production-path simulation before submission is enabled.
A matching capability commitment remains a screen, never proof of behavior.

## Consequences

- A contract or declared-interface change requires a new release and deployment;
  an off-chain conformance fix does not necessarily require either.
- Existing releases can remain certified without becoming production targets.
  Operational support is an explicit registry decision, not a compatibility
  layer inferred from old addresses.
- Release work concentrates in one package and its public certification seams,
  rather than being scattered through encoder, receipt, artifact, and deployment
  code.
- CI can reject an unversioned semantic edit deterministically, while execution
  evidence remains the final authority on what contract code actually does.

## Acceptance

1. `cmd_executor/2` is the explicit tracked baseline; no TGUZCT compatibility
   path or latest-release fallback exists.
2. Incomplete typed registration fails at compile time or the release gate.
3. Editing a frozen contract, manifest, artifact, or normative golden without the
   next sequential release fails deterministically.
4. Production built-in encoding uses release-bound capability planning.
5. Valid `/2` calldata remains unchanged except for separately reviewed
   conformance fixes with runtime proof.
6. The full Rust and Python suites pass. Contract deployment is outside this
   ADR's implementation scope.
