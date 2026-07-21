//! The hot-path in-process simulation entry point.
//!
//! Executes the 7-call vector (pre-balances → `execute()` → post-balances) via
//! revm `transact_one`, returning the same `SimResult` shape
//! `degenbot-simulation::simulate_one` produces, so the dispatch leaf can swap
//! `dispatch::simulate_v1` for this behind a single call-site change. Reuses
//! `classify_revert` — revm returns the same revert bytes / `Panic(0x11)`
//! selectors the classifier already keys on.
//!
//! # Filled by task `JHGLF4`.
//!
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §3 for the
//! measured latency profile (cold 8374 µs / 9 RPCs; warm 442 µs / 0 RPC; 18.9×
//! speedup vs cold).

/// Execute the 7-call simulate vector in-process, returning the same
/// `SimResult` shape `degenbot-simulation::simulate_one` yields.
///
/// Orchestration: int128 guard → encode (`encode_cmd_stream`,
/// `degenbot-executor`) → `execute()` calldata wrap
/// (`payload::wrap_execute_calldata`) → apply state overrides (the adaptor) →
/// `transact_one` per call → parse balance diffs → compute gross/net profit +
/// the market-aware priority fee → return `SimResult`.
///
/// Block-env parity: revm gives explicit `BlockEnv { number, timestamp,
/// basefee, gas_limit, beneficiary }` — pin the same env the Python oracle used
/// (`base_fee_next`, the pump's block timestamp/number).
///
/// # Filled by task `JHGLF4`.
#[allow(clippy::missing_errors_doc)]
pub fn simulate_in_process() -> Result<SimulationOutcome, SimulationError> {
    // TODO(JHGLF4): port simulate_one's 7-call orchestration into revm
    // transact_one. Return the same SimResult fields the dispatch leaf reads.
    todo!("JHGLF4: port simulate_one into in-process revm transact")
}

/// The in-process simulation outcome — mirrors `degenbot-simulation::SimResult`
/// (the fields the dispatch leaf + driver read). Filled by task `JHGLF4`.
#[derive(Debug, Clone)]
pub struct SimulationOutcome {
    /// The arbitrage path identifier (mirrors `SimResult.path_id`).
    pub path_id: u64,
    /// Gross profit in wei (pre-gas): (weth_after + eth_after + erc6909_after)
    /// - (weth_before + eth_before + erc6909_before).
    pub gross_profit: alloy::primitives::U256,
    /// Net profit in wei (post-gas).
    pub net_profit: alloy::primitives::U256,
    /// The `execute()` gas used (revm per-call, no `eth_simulateV1`
    /// block-aggregation edge case).
    pub gas_used: u64,
    /// The market-aware priority fee (mirrors `compute_priority_fee`).
    pub priority_fee: alloy::primitives::U256,
    /// The encoded `execute()` calldata submitted to the executor.
    pub execute_calldata: alloy::primitives::Bytes,
    /// The EIP-2930 access list emitted from the revm `State` journal
    /// (filled by [`crate::access_list::emit_access_list_from_state`]).
    pub access_list: alloy::rpc::types::eth::AccessList,
    /// The number of hops in the arbitrage path.
    pub hop_count: usize,
}

/// Errors raised by `simulate_in_process`.
#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// The int128 guard rejected a path payload (mirrors the int128-bucket).
    #[error("Int128 guard rejected the path payload")]
    Int128Guard,
    /// The `execute()` call reverted (classified via `classify_revert`).
    #[error("Execute reverted: {0}")]
    Revert(String),
    /// The simulation found no profit (the no-profit bucket).
    #[error("No profit on path {path_id}")]
    NoProfit {
        /// The arbitrage path identifier.
        path_id: u64,
    },
    /// A revm `transact` execution failure (not a revert — an EVM error).
    #[error("revm transact failed: {0}")]
    Transact(String),
}
