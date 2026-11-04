# ADR-091: Retire revm completely — evm2 as the sole EVM dependency

**Status:** implemented (R0–R5 landed). revm is removed from every manifest and the lockfile; the Tier-3 suites, the oracle/harness/capture labs, and the wave-2/wave-3 cassette generators run on evm2 with zero divergence (the cassettes regenerate byte-identical).

## Context

ADR-089 made evm2 the only engine on production paths and retained the revm
dependency lib-level for the `oracle`/`harness`/`capture` lab modules — "the
Tier-3 truth source until their deliberate migration decision." This ADR is
that decision, made with evidence ADR-089 did not have.

Two facts changed the calculus:

1. **The Tier-3 center of gravity moved to the live node.** The live-node
   parity harnesses (batch-quote bench, phantom parity, black-box parity,
   dynamic-fee census) prove evm2 == chain to the wei on real pools with real
   state at anchors. reth is the independent interpreter now — more
   independent than revm ever was, because it is the production network's
   actual EVM.
2. **What Tier-3's independence actually is.** The offline Tier-3 suites
   (canonical solc-compiled Uniswap/Curve/Balancer bytecode vs the Rust
   white-box) derive their independence from the *reference bytecode*, not
   from the interpreter executing it. The same bytecode runs identically
   under evm2; swapping the interpreter does not weaken the oracle.

What revm retention actually cost: a second EVM integration to version,
track, and keep compatible across every toolchain and dependency bump —
permanent drag in exchange for a second interpreter opinion whose role the
live node now covers better.

## Decision

Retire revm completely. evm2 is the sole EVM dependency. The port sequence:

- **R1** — trivial severances: production `revm::primitives::TxKind`
  (literally alloy's type via revm's re-export) → `alloy::primitives::TxKind`;
  stale crate metadata.
- **R2** — an offline-DB fixture adapter over `evm2::evm::InMemoryDB`
  (deploy-bytecode / set-storage / set-balance conveniences matching the
  `CacheDB<EmptyDB>` pattern the Tier-3 tests use), making the ports
  mechanical.
- **R3** — the Tier-3 suites (math `compute_swap_step`, pools Curve/Balancer/V2)
  ported to evm2 with canonical bytecode, fuzz vectors, and assertions
  unchanged. A divergence against revm-era vectors is an evm2 semantic bug —
  surfaced, never masked.
- **R4** — the lab cluster (`oracle`/`harness`/`capture` and the
  executor-grammar lab): audited per module and either ported via R2 or
  retired per ADR-087. Investigation machinery with no production consumer
  retires rather than being ported on principle.
- **R5** — the dependency removal: revm out of every manifest and the
  lockfile, all lanes green.

## The recorded trade-off

The ported offline oracles lose a second interpreter opinion: an evm2 bug
affecting both an oracle execution and production identically would be
invisible to them. The mitigation is structural, not procedural: the
live-node harnesses (evm2 vs reth) are the independent-interpreter tier, and
this decision binds the project to keeping them strong. The white-box math
itself remains independent of evm2 (it is not executed by an EVM at all), so
Tier-3's core guarantee — our Rust math reproduces the canonical Solidity
libraries — is untouched.

## Consequences

- One EVM integration to maintain; the revm version-churn tax (which reth's
  own migration demonstrated) ends.
- The Tier-3 suites keep their canonical-bytecode guarantee under the engine
  production actually runs — their interpreter fidelity claim *strengthens*.
- reth-as-chain is the sole independent truth anchor; the live-node
  harnesses become load-bearing test infrastructure, not optional tooling.

Supersedes the lab-retention clause of ADR-089.
