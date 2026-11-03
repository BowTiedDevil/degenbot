//! The simulation engine — the in-process revm EVM handle + its DB stack.
//!
//! ADR-019 D4/D7 (decision R — strategy/engine separation): this crate owns
//! the **engine** (`BlockSimHandle`, the settle verb [`SimSession::run`], the
//! layered DB, `apply_simulation_overrides`, `AccessListCollector`,
//! `WarmCodeCache`). The **strategy** — the 7-call pre/post-balance bundle,
//! `compute_priority_fee`, `decode_balance`, `SimResult`,
//! `dispatch_profitable_results`, `SimulateContext`, `SimulatePath`,
//! `FailBuckets`, the calldata builders — relocated to the
//! `degenbot-arbitrage` crate, which drives the engine through
//! [`BlockSimHandle::session`].
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
/// AL collector, warm cache). The settlement-arbitrage strategy relocated to
/// `degenbot-arbitrage` (ADR-019 D4/D7, decision R).
pub mod sim;

/// The executor grammar harness: deploy the real `cmd_executor` +
/// synthesized pools into a fresh revm `CacheDB`, run a path's
/// [`encode_cmd_stream`](degenbot_executor::composers::encode_cmd_stream)
/// payload through `execute()`, and report whether it executes + which pools
/// it touched. The missing third correctness tool — byte parity can't prove
/// runtime behavior, and live sim needs a captured mainnet path. See
/// [`harness`](crate::harness) for the API.
///
/// Behind the non-default `test-harness` feature: the lab has no production
/// consumer, so a shipped crate root is the engine + fixture driver + capture
/// surface only.
#[cfg(feature = "test-harness")]
pub mod harness;

/// Contract-agnostic in-process revm **fixture driver** — deploy a pinned
/// contract artifact, run staged calls, seed storage slots, drive a call,
/// classify the verdict (Revert vs Halt), and read back output + logs. The
/// shared spine of the tier-3 on-chain oracles and any per-contract (user)
/// investigation harness. See [`oracle`] for the API.
pub mod oracle;

/// The wave-2 golden-capture generator seam (the wave-2 capture program, ADR-068 approach
/// C): a node-free JSON-RPC chain over the fixture EVM whose served answers
/// are real frame-execution products, wrapped by the recorder's canonical
/// writer to manufacture cassettes without a node. See [`capture`] for the
/// API.
pub mod capture;

pub use oracle::{
    call_bytes, decode_error_string, deploy, load_foundry_creation_bytecode, native_balance_of,
    new_fixture_evm, parse_foundry_creation_bytecode, read_address, seed_slots, selector,
    set_code_size_limits, set_disable_nonce_check, set_native_balance, transact, FixtureEvm,
    TxSpec, Verdict,
};
// Re-export the engine surface at the crate root for ergonomic access + for
// the strategy crate (`degenbot-arbitrage`) + the PyO3 wrapper. The
// strategy value types (`SimResult`, `SimulateContext`, `FailBuckets`, …)
// now live in `degenbot-arbitrage`.
pub use sim::capture::{CallFrame, CallTrace, CapturedSwap, FrameOutcome, SwapFamily};
pub use sim::evm2::bot_state_db::BotStateDb;
pub use sim::evm2::frame_replay::{Evm2CountingFrameDb, Evm2ReplayOutcome, Evm2ScratchEvm};
pub use sim::evm2::handle::{Evm2BlockSimHandle, Evm2ProductionBlockDb, Evm2ScratchDb};
pub use sim::evm2::state_override::apply_simulation_overrides;
pub use sim::evm2::v4_black_box::{
    decode_quote_return, leg_state_slots, quote_calldata, quote_exact_in, resolve_pool_manager,
    V4BlackBoxQuote, V4BlackBoxQuoter, V4Leg, V4QuoteError, V4QuoterConfig,
};
pub use sim::evm2::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
pub use sim::{
    divergence_probe, BaseFeeSource, FrameRpcCounter, HotSlotStore, ReplayFrameError, ReplayableTx,
    RunVerdict, ScratchBlock, SequenceReplayError, SimulationOverrideParams, StorageMemo,
    StorageMemoCache,
};
