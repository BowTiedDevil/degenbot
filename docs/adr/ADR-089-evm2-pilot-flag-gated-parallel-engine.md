# ADR-089: The evm2 pilot — a flag-gated parallel engine behind the simulation seam

## Status

Accepted (2026-10-30).

## Context

revm is the sole simulation executor (ADR-019): the per-block `BlockSimHandle`
over the `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>>`
stack, the inspector-driven swap capture and revert attribution, and the
Tier-3 differential oracles that treat EVM execution as the truth source for
the hand-written swap math. revm 43 is pinned workspace-wide.

reth 2.9.0 replaced revm with `evm2` (alloy-rs/evm2, DaniPopes' revm
successor: a tighter interpreter, claimed up to 2× revm, alloy-primitives
native, runtime feature flags where revm uses compile-time features). evm2 is
unpublished on crates.io — reth pins rev `f93318f120f6300599b21170d6c7bef81ed63d87`
— and carries an explicit "MSRV may be updated at any time" policy against
this workspace's toolchain pin (1.98.1) and certified MSRV (1.97).

The engine/strategy seam (ADR-019 D4/D7) contains the swap: revm is named
inside `degenbot-simulation`'s engine and the `degenbot-arbitrage` strategy's
trait bounds, nowhere else in production. Two constraints shape the approach:

- **Publish contract.** The workspace publishes crates; `cargo publish`
  rejects git-only dependencies. An optional git dep carrying a `version`
  key keeps packaging legal (cargo strips the git source at publish).
- **Truth-source risk.** The Tier-3 oracles assume the EVM is ground truth.
  Swapping the truth source without a differential gate would devalue every
  oracle that runs on it.

The pilot's first differential run already caught a real divergence class:
revm 43's `CfgEnv` caches a `gas_params` table, so assigning `cfg.spec`
directly leaves the default spec's gas schedule active (an EIP-7623 floor
appeared at Cancun). evm2 derives its gas schedule from the spec at
construction and cannot drift this way.

## Decision

**evm2 enters as an opt-in parallel implementation behind the `evm2` cargo
feature on `degenbot-simulation`, gated at every layer by a differential
parity test against revm, and promoted only by a hard cutover that removes
the flag.**

1. One feature, one home: `degenbot-simulation/evm2` gates the
   `src/sim/evm2/` module and its test targets. No default lane enables it;
   no other crate gains an evm2 dependency during the pilot.
2. The pin follows reth 2.9.0's rev (`f93318f`), with `version = "0.1.0"`
   on the dependency so `cargo publish` can strip the git source. Bumping
   the pin is a deliberate, reviewed event (MSRV and API churn ride along).
3. Only pure-Rust precompile backends are enabled (no GMP/MCL/aws-lc native
   builds), matching the revm dependency's backend shape and keeping the
   devcontainer build contract intact.
4. The migration mechanism is the differential gate
   (`tests/evm2_pilot.rs`): the same transaction through both engines must
   produce identical status, gas, output, and storage writes. Every layer
   ported to evm2 (state-forwarding DB, warm code cache, async RPC
   fallback, inspectors) extends the gate before it extends the engine.
5. revm stays the Tier-3 truth source and the production engine until the
   gate covers every engine capability the strategy uses, including the
   live fork-truth legs. The cutover is hard: the flag, the revm
   dependency, and this ADR's pilot scaffolding retire together.
6. `evm2-jit` enters the pilot behind the stacked `evm2-jit` feature
   (amended 2026-10-30; this clause initially read "out of scope"). It is a
   fail-soft hybrid: `JitInterpreterRunner` falls back to the interpreter
   when no compiled program exists, so the differential gate covers the JIT
   with no new divergence surface. Its costs stay out of every default lane:
   LLVM 23.1 exactly (the F45 devcontainer's `llvm-devel`), and the
   unreleased inkwell git rev re-pinned at the workspace root (cargo's
   `[patch]` is not transitive; dropped at publish like reth's copy).

## Rejected alternatives

**Wait for a crates.io release, then big-bang the swap.** Rejected: the
release has no date, reth's production mileage on the pinned rev is the
best de-risking signal available now, and a big-bang swap would land
without the layer-by-layer parity evidence the gate accumulates.

**Track evm2 HEAD instead of reth's rev.** Rejected: reth's rev carries
production validation; HEAD adds churn without adding signal.

**Parallel implementations as a habit.** This project defaults against
parallel implementations. The exception stands because the truth-source
risk is asymmetric: a silent execution divergence corrupts the solver's
accuracy oracle, not just a runtime path. The flag has a recorded deletion
condition (D5), not an open-ended lifetime.

## Consequences

- `rust/Cargo.lock` gains evm2 and its dependency tree; the default build
  neither compiles nor downloads them (optional dep).
- The revm-side pin inside the gate documents the `gas_params` caching
  footgun (`set_spec_and_mainnet_gas_params`, not `cfg.spec = ...`); the
  same trap applies anywhere the engine sets a spec directly.
- CI lanes are unchanged; the pilot runs on demand via
  `just test-rust-nextest -p degenbot-simulation --features degenbot-simulation/evm2`.
- Downstream crates (`degenbot-arbitrage`, facade, Python shell) see no
  surface change during the pilot; the strategy's `ExecuteEvm` trait bounds
  keep naming revm until the cutover.
