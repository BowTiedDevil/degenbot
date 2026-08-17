//! The simulation engine — the in-process revm EVM handle + its DB stack.
//!
//! ADR-019 D4/D7 (decision R — strategy/engine separation): this crate owns
//! the **engine** (`BlockSimHandle`, the layered DB, `apply_simulation_overrides`,
//! `AccessListCollector`, `WarmCodeCache`). The **strategy** — the 7-call
//! pre/post-balance bundle, `compute_priority_fee`, `decode_balance`,
//! `SimResult`, `dispatch_profitable_results`, `SimulateContext`,
//! `SimulatePath`, `FailBuckets`, the calldata builders — relocated to the
//! `degenbot-settlement-strategy` crate, which drives the borrowed `&mut evm`
//! the engine exposes via [`BlockSimHandle::evm_mut`].
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-simulation` reaches the in-process engine surface with
//! zero `pyo3` dependency (ADR-005 standalone-core).

// Solidity/EVM identifiers (balanceOf, stateDiff, ERC6909, WETH9, PoolManager,
// …) are ubiquitous in this crate's docs; allow the pedantic doc-markdown lint
// to match the peer core crates.
#![expect(clippy::doc_markdown)]

/// The in-process revm engine + its DB stack (the EVM handle, overrides,
/// AL collector, warm cache). The backrun strategy relocated to
/// `degenbot-settlement-strategy` (ADR-019 D4/D7, decision R).
pub mod sim;

/// The executor grammar harness (UQOAHA): deploy the real `cmd_executor` +
/// synthesized pools into a fresh revm `CacheDB`, run a path's
/// [`encode_cmd_stream`](degenbot_executor::composers::encode_cmd_stream)
/// payload through `execute()`, and report whether it executes + which pools
/// it touched. The missing third correctness tool — byte parity can't prove
/// runtime behavior, and live sim needs a captured mainnet path. See
/// [`harness`](crate::harness) for the API.
pub mod harness;

/// Contract-agnostic in-process revm **fixture driver** — deploy a pinned
/// contract artifact, run staged calls, seed storage slots, drive a call,
/// classify the verdict (Revert vs Halt), and read back output + logs. The
/// shared spine of the tier-3 on-chain oracles and any per-contract (user)
/// investigation harness. See [`oracle`] for the API.
pub mod oracle;

pub use oracle::{
    call_bytes, decode_error_string, deploy, load_foundry_creation_bytecode, native_balance_of,
    new_fixture_evm, parse_foundry_creation_bytecode, read_address, seed_slots, selector,
    set_code_size_limits, set_disable_nonce_check, set_native_balance, transact, FixtureEvm,
    TxSpec, Verdict,
};
// Re-export the engine surface at the crate root for ergonomic access + for
// the strategy crate (`degenbot-settlement-strategy`) + the PyO3 wrapper. The
// strategy value types (`SimResult`, `SimulateContext`, `FailBuckets`, …)
// now live in `degenbot-settlement-strategy`.
pub use sim::evm::{
    apply_simulation_overrides, divergence_probe, emit_access_list_from_state, AccessListCollector,
    BlockEvm, BlockSimHandle, BotStateDb, CallFrame, CallTrace, CallTraceHandle,
    CallTraceInspector, CapturedSwap, FrameOutcome, ProductionBlockDb, SimInspector,
    SimulationOverrideParams, SwapEventCaptureHandle, SwapEventCaptureInspector, SwapFamily,
    WarmCodeCache, WarmCodeCacheInner, WARM_CODE_CACHE_TTL_BLOCKS,
};
