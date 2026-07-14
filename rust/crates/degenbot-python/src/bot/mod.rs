//! Per-domain binding-crate modules.
//!
//! `engine/` is the first inhabitant; step 6 of the binding-layer reorg
//! relocates the other `bot` / `bot::pool` wrappers alongside.
//! (ergo UG6FKN task WXHGOH.)

pub mod deployments;
pub mod dex_identity;
pub mod engine;
pub mod pool;
pub mod pump;
pub mod py_bot_io;
pub mod subscriber;
pub mod token;

// === PyBot (moved from the former root `py_bot.rs`) ===
//
// `PyO3` wrappers for `BotState` — thin Python handle over Rust-owned state.
// Implements the Polars-inspired three-layer topology (ADR-005): Rust [`BotState`]
// core → `PyBot` `#[pyclass]` wrapper holding `Arc<parking_lot::RwLock<BotState>>`
// → Python `BotState` session. `PyLiquidityPool`/`PyErc20Token` clone the same
// `Arc` so many Python handles reference one Rust-owned `BotState`.
// See: `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.

use crate::prelude::*;
use std::sync::Arc;

use alloy::primitives::Address;

use crate::bot::engine::{
    hex_string_to_pool_id, map_register_v2_err, map_register_v3_err, map_register_v4_err,
    SpecViolationError,
};

/// Narrow a Python-supplied `U256` reserve to `U112` (the on-chain `uint112`
/// width), raising `SpecViolationError` if bits ≥ 112 are set.
fn narrow_reserve(
    value: alloy::primitives::U256,
    field: &'static str,
) -> PyResult<alloy::primitives::aliases::U112> {
    degenbot_pools::spec_bounds::narrow_v2_reserve(value, field)
        .map_err(|sv| SpecViolationError::new_err(format!("{sv}")))
}
use crate::bot::pool::PyLiquidityPool;
use crate::bot::token::PyErc20Token;
use degenbot_bot::bot_core::PoolTickCoverage;
use degenbot_bot::bot_core::{
    Bot, RegisterAerodromeV2PoolParams, RegisterBalancerStablePoolParams,
    RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams, RegisterV2PoolParams,
    RegisterV3PoolParams, RegisterV4PoolParams, V4PoolKey,
};
use degenbot_pools::state_history::JournalError;
use degenbot_uniswap::dex_identity::DexVariant;
use pyo3::types::{PyDict, PyList};
use pyo3::Bound;

/// Build an `alloy::rpc::types::Log` from the WS-log shape Python tests pass —
/// `(address, topics, data, block_number)` reconstructed into the same
/// `alloy::rpc::types::Log` the `BlockPump` feeds `Bot::dispatch_log`. Hex
/// strings accept an optional `0x` prefix. This is the marshalling seam for
/// the Python-facing `dispatch_log` (ADR-006, deferred §17 closure): it lets
/// an offline test drive the full pump→dispatch→notify→solve loop without a
/// live WS node, reusing the existing pure-logic dispatcher untouched.
fn build_rpc_log(
    address: &str,
    topics: Vec<String>,
    data: &str,
    block_number: u64,
) -> PyResult<alloy::rpc::types::Log> {
    let addr: Address = address.parse().map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid address '{address}': {e}"))
    })?;
    let mut topic_hashes = Vec::with_capacity(topics.len());
    for t in topics {
        let stripped = t.strip_prefix("0x").unwrap_or(&t);
        let b: alloy::primitives::B256 = stripped.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid topic '{t}': {e}"))
        })?;
        topic_hashes.push(b);
    }
    let data_stripped = data.strip_prefix("0x").unwrap_or(data);
    let data_bytes = alloy::hex::decode(data_stripped).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid data hex '{data}': {e}"))
    })?;
    let inner = alloy::primitives::Log::new_unchecked(
        addr,
        topic_hashes,
        alloy::primitives::Bytes::from(data_bytes),
    );
    Ok(alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    })
}

/// Encode a byte slice as a lowercase hex string (no "0x" prefix).
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize] as char);
        s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// PyBot — owns the Bot orchestrator; hands out shared BotState Arcs
// ---------------------------------------------------------------------------

/// Python handle to the per-chain `Bot` orchestrator (ADR-006 D4).
///
/// Python constructs a `PyBot` (or receives a shared handle), registers
/// pools/tokens, then reads results. `PyBot` owns a [`Bot`] via `Arc` and hands
/// out clones of its shared `Arc<RwLock<BotState>>` (`state_arc`) so
/// `PyLiquidityPool` / `PyErc20Token` / `UniswapEngine` all reach ONE
/// Rust-owned `BotState`; `BlockPump` clones the same `Arc<Bot>` so its
/// `dispatch_log` writes flow through to the engine's reads (N handles → one
/// state — the Polars three-layer invariant, preserved + generalized by D4).
#[pyclass(skip_from_py_object)]
pub struct PyBot {
    bot: Arc<Bot>,
    /// ADR-006 D4 (T3): the pump lifecycle state, shared with the
    /// `PyUniswapArbEngine` this bot owns. `None` until an engine is
    /// constructed against this bot (the engine attaches its `Arc<PumpState>`
    /// back here during `new()`). Once attached, the three pump methods —
    /// `subscribe`, `backfill_from_snapshot`, `resume` — are drivable from
    /// `PyBot` (the D4 owner) and read/write the SAME `PumpState` the
    /// engine's snapshot/solve slices read.
    pump: parking_lot::Mutex<Option<Arc<crate::bot::pump::PumpState>>>,
}

/// Crate-internal Rust surface on `PyBot` (not Python-visible).
impl PyBot {
    /// Hand out a clone of the shared `Arc<Bot>` so `BlockPump` (and other
    /// sibling consumers) drive the SAME `Bot`'s `dispatch_log` + state that
    /// `PyBot` owns (ADR-006 D4: pump lifecycle relocation onto `Bot`).
    #[must_use]
    pub(crate) fn bot_arc(&self) -> Arc<Bot> {
        Arc::clone(&self.bot)
    }

    /// ADR-006 D4 (T3): attach the pump lifecycle state owned by a
    /// `PyUniswapArbEngine` constructed against this bot. Called from
    /// `PyUniswapArbEngine::new` when `py_bot` is supplied. After this, the
    /// pump methods on `PyBot` drive the same `PumpState` the engine reads.
    pub(crate) fn attach_pump_state(&self, pump: Arc<crate::bot::pump::PumpState>) {
        *self.pump.lock() = Some(pump);
    }

    /// Borrow the attached `PumpState`, or error if no engine was constructed
    /// against this bot.
    #[allow(dead_code)] // wired by the pump lifecycle methods (subscribe/
                        // backfill_from_snapshot/resume) in the #[pymethods] impl
    fn pump_state(&self) -> PyResult<Arc<crate::bot::pump::PumpState>> {
        self.pump.lock().clone().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "No engine attached to this Bot. Construct a UniswapArbEngine(py_bot=...) \
                 before calling pump lifecycle methods on Bot.",
            )
        })
    }
}

#[pymethods]
impl PyBot {
    #[new]
    #[pyo3(signature = (chain_id = 0))]
    fn new(chain_id: u64) -> Self {
        // ADR-006 slice 8b: the Python ``Bot`` facade is now single-chain,
        // so it passes its ``config.default_chain_id`` here. The ``chain_id = 0``
        // default keeps the bare ``PyBot()`` lower-level test fixtures (which
        // only exercise the Rust core) working without a chain invariant.
        // `Arc<Bot>` so `BlockPump` clones the same orchestrator (ADR-006 D4).
        Self {
            bot: Arc::new(Bot::new(chain_id)),
            pump: parking_lot::Mutex::new(None),
        }
    }

    /// Load the V3 + V4 DB snapshot into the core `BotState` (B3OROH, JUCFCB).
    ///
    /// Called at Python `Bot.__init__` time when a DB path is configured
    /// (Shape 2: eager construction-time load). Opens a read-only
    /// `DegenbotDb` handle from `db_path`, then calls the core
    /// `Bot::load_snapshot_from_db` — the single Rust entry point that
    /// `stream_liquidity_maps`-loads V3+V4 pools into the core `SnapshotStore`s
    /// + records `S = min(fetch_newest_update_block(V3), V4)`. After this,
    /// pool registration consumes the store via `take()` (RUQ637) — the
    /// Python builder passes `tick_data=None, coverage=Tracked` and lets the
    /// store decide Tracked-vs-Sparse per pool.
    ///
    /// `None`/cold-start (no pools) is NOT an error — the pump anchors on
    /// `first_observed_block` at resume. Idempotent across the two families,
    /// but a second call after a successful load will `begin_load` a store
    /// that's already loaded (panics) — call exactly once at construction.
    ///
    /// # Errors
    /// `PyRuntimeError` on a DB open/read failure or a liquidity value
    /// out of range.
    #[pyo3(signature = (db_path, chain_id))]
    fn load_snapshot_from_db(&self, db_path: &str, chain_id: u64) -> PyResult<()> {
        let db = degenbot_db::connection::DegenbotDb::open(&std::path::PathBuf::from(db_path))
            .map_err(|e| crate::db::db_err_to_py(&e))?
            .0;
        self.bot.load_snapshot_from_db(&db, chain_id).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("load_snapshot_from_db failed: {e}"))
        })
    }

    /// The snapshot seed block `S` (or `None` when no DB snapshot was loaded —
    /// the cold-start path). Python reads this in `engine_registry.start()`
    /// to stash `_verify_snapshot_block` for the per-pool two-step verify.
    #[getter]
    fn snapshot_seed_block(&self) -> Option<u64> {
        self.bot.state_arc().read().snapshot_seed_block()
    }

    /// Subscribe to the WS `newHeads` + logs streams (ADR-006 D4 T3).
    ///
    /// The Bot-owned pump entry point — delegates to the shared `PumpState`
    /// attached when a `UniswapArbEngine` was constructed against this bot.
    /// Blocks (sync, via the shared tokio runtime) until the first block is
    /// observed, then returns the first WS block number (the backfill target).
    ///
    /// # Errors
    /// `PyRuntimeError` if no engine is attached, the pump is already
    /// started/subscribed, or the WS subscribe fails.
    #[pyo3(signature = (rpc_url))]
    fn subscribe(&self, rpc_url: &str) -> PyResult<u64> {
        self.pump_state()?.subscribe(rpc_url)
    }

    /// Resume the pump — begin normal WS processing (ADR-006 D4 T3).
    ///
    /// The snapshot→WS gap is closed automatically inside the core
    /// `BlockPump::resume_from_subscribe` (J3FMDO); the pyo3
    /// `backfill_from_snapshot` method is retired (2SM4Y7). Delegates to the
    /// shared `PumpState`.
    fn resume(&self, _py: Python<'_>) -> PyResult<()> {
        self.pump_state()?.resume()
    }

    /// Stop the pump and signal the Rust core to clean up (ADR-006 D4).
    ///
    /// The symmetric teardown half of the `subscribe` → `backfill_from_snapshot`
    /// → `resume` lifecycle. Sets the shutdown flag and aborts the spawned
    /// pump task so a Ctrl-C exits promptly (the pump loop otherwise blocks
    /// up to 60s on a silent WS stream). Idempotent — safe to call from both
    /// the `__aexit__` path and a signal handler. Delegates to the shared
    /// `PumpState`.
    fn stop(&self, _py: Python<'_>) -> PyResult<()> {
        self.pump_state()?.stop()
    }

    /// Set the HTTP RPC URL used for verification (ADR-006 D4 T4).
    /// Delegates to the shared `PumpState`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: &str) {
        if let Ok(pump) = self.pump_state() {
            pump.set_verify_rpc_url(rpc_url);
        }
    }

    /// Set the `StateView` contract address for V4 verification (ADR-006 D4 T4).
    #[pyo3(signature = (state_view_address))]
    fn set_verify_state_view(&self, state_view_address: &str) {
        if let Ok(pump) = self.pump_state() {
            pump.set_verify_state_view(state_view_address);
        }
    }

    /// Verify all V3 + V4 pool liquidity maps against on-chain state
    /// (ADR-006 D4 T4). Delegates to the shared `PumpState`.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, tick_lens_address, state_view_address, block_number))]
    fn verify_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        tick_lens_address: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?.verify_liquidity_maps(
            py,
            rpc_url,
            tick_lens_address,
            state_view_address,
            block_number,
        )
    }

    /// Verify V3 liquidity maps only (ADR-006 D4 T4).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, block_number))]
    fn verify_v3_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?
            .verify_v3_liquidity_maps(py, rpc_url, block_number)
    }

    /// Verify V4 liquidity maps only (ADR-006 D4 T4).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, state_view_address, block_number))]
    fn verify_v4_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?
            .verify_v4_liquidity_maps(py, rpc_url, state_view_address, block_number)
    }

    /// Verify a single V3 pool's pinned snapshot seed against on-chain@snapshot
    /// block (CBCH6H — the rolling-start race fix). Step-1 of the two-step
    /// verify at the registry drain seam routes here.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (address, rpc_url, block_number))]
    fn verify_v3_snapshot_seed<'py>(
        &self,
        py: Python<'py>,
        address: String,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?
            .verify_v3_snapshot_seed(py, address, rpc_url, block_number)
    }

    /// Verify a single V4 pool's pinned snapshot seed against on-chain@snapshot
    /// block (CBCH6H — V4 twin of `verify_v3_snapshot_seed`). Step-1 of the
    /// two-step verify routes here.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (pool_manager_address, pool_id_hex, rpc_url, state_view_address, block_number))]
    fn verify_v4_snapshot_seed<'py>(
        &self,
        py: Python<'py>,
        pool_manager_address: String,
        pool_id_hex: String,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?.verify_v4_snapshot_seed(
            py,
            pool_manager_address,
            pool_id_hex,
            rpc_url,
            state_view_address,
            block_number,
        )
    }

    /// Verify a single V3 pool's pinned post-drain `tick_data` against
    /// on-chain@**pinned block** (step-2 race fix, twin of
    /// `verify_v3_snapshot_seed`). Step-2 of the two-step verify routes here.
    /// The block is the one captured atomically with the drain — the pin
    /// owns its block (the caller passes no `block_number`).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (address, rpc_url))]
    fn verify_v3_post_drain_snapshot<'py>(
        &self,
        py: Python<'py>,
        address: String,
        rpc_url: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?
            .verify_v3_post_drain_snapshot(py, address, rpc_url)
    }

    /// Verify a single V4 pool's pinned post-drain `tick_data` against
    /// on-chain@**pinned block** (step-2 race fix, V4 twin of
    /// `verify_v3_post_drain_snapshot`).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (pool_manager_address, pool_id_hex, rpc_url, state_view_address))]
    fn verify_v4_post_drain_snapshot<'py>(
        &self,
        py: Python<'py>,
        pool_manager_address: String,
        pool_id_hex: String,
        rpc_url: String,
        state_view_address: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?.verify_v4_post_drain_snapshot(
            py,
            pool_manager_address,
            pool_id_hex,
            rpc_url,
            state_view_address,
        )
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, reserve0, reserve1, gamma_numer0, fee_denom0, gamma_numer1, fee_denom1, factory, update_block=0, variant="uniswap-v2", stable_swap=false, fee_denominator=None))]
    fn register_v2_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        gamma_numer0: u64,
        fee_denom0: u64,
        gamma_numer1: u64,
        fee_denom1: u64,
        factory: &str,
        update_block: u64,
        variant: &str,
        stable_swap: bool,
        fee_denominator: Option<u64>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;
        let variant_enum = DexVariant::from_kebab(variant).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown variant: {variant}"))
        })?;

        // Verify the pool address against the JSON-sourced CREATE2 deployer +
        // init hash (Fork A, JC6OFG). Skipped if (chain, factory) is not in the
        // shipped JSON — preserves the manual/ad-hoc registration path.
        crate::bot::deployments::verify_v2(self.bot.chain_id(), fac, addr, t0, t1)?;

        // Resolve the JSON-sourced CREATE2 deployer + init hash (Fork A,
        // NSAZ4X). Stored on the V2 identity so the `dex` getter merges the
        // per-(chain,factory) deployer/init_hash into the protocol preset
        // (replacing the canonical-mainnet preset values). Non-JSON pools
        // default to factory-as-deployer + the V2 mainnet fallback init hash.
        let chain_id = self.bot.chain_id();
        let deployer = degenbot_uniswap::deployments::resolve_deployer(chain_id, fac);
        let init_hash_b256 = degenbot_uniswap::deployments::resolve_v2_init_hash(chain_id, fac);

        self.bot
            .state_arc()
            .write()
            .register_v2_pool(&RegisterV2PoolParams {
                address: addr,
                token0: t0,
                token1: t1,
                reserve0: r0,
                reserve1: r1,
                fee_token0: (gamma_numer0, fee_denom0),
                fee_token1: (gamma_numer1, fee_denom1),
                factory: fac,
                deployer,
                init_hash: init_hash_b256,
                update_block,
                variant: variant_enum,
                stable_swap,
                fee_denominator,
            })
            .map_err(map_register_v2_err)
    }

    /// Update a V2 pool's reserves from a Sync event.
    #[pyo3(signature = (address, reserve0, reserve1, block_number))]
    fn update_v2_pool(
        &self,
        address: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = parse_address(address)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;

        self.bot
            .state_arc()
            .write()
            .update_v2_pool(addr, r0, r1, block_number);
        Ok(())
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_in`: Input token amount (Python int).
    ///
    /// Returns:
    ///     The output token amount as a Python int.
    #[pyo3(signature = (pool_id, zero_for_one, amount_in))]
    fn calculate_tokens_out(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let result = {
            let state = self.bot.state_arc();
            let core = state.read();
            core.calculate_tokens_out(pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Calculate the required input token amount for a given output amount.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_out`: Desired output token amount (Python int).
    ///
    /// Returns:
    ///     The required input token amount as a Python int.
    #[pyo3(signature = (pool_id, zero_for_one, amount_out))]
    fn calculate_tokens_in(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let result = {
            let state = self.bot.state_arc();
            let core = state.read();
            core.calculate_tokens_in(pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Number of registered pools.
    fn pool_count(&self) -> usize {
        self.bot.state_arc().read().pool_count()
    }

    /// The chain this `PyBot` orchestrates (ADR-006 D4). Wired from the
    /// single-chain Python `Bot` facade's ``config.default_chain_id``; `0` on
    /// the bare-test-fixture path (see `PyBot::new`).
    #[getter]
    fn chain_id(&self) -> u64 {
        self.bot.chain_id()
    }

    /// Drive a raw WS log through the event bus (ADR-006 D4).
    ///
    /// This is the Python-facing mirror of the `BlockPump`'s per-log call to
    /// `Bot::dispatch_log`: decode via the registered `LogDecoder`s, apply the
    /// decoded event to the shared `BotState` under a write guard, release it,
    /// then notify every attached `PoolStateSubscriber` (the engine adapter) so
    /// the affected `pool_id` is dirtied for the next `solve_all_paths`.
    ///
    /// Reconstructs an `alloy::rpc::types::Log` from the WS-log shape Python
    /// passes — `(address, topics, data, block_number)` — so an offline test
    /// can drive the full pump→dispatch→notify→solve loop without a live node.
    /// No-op if no decoder recognizes the log or the pool isn't registered.
    ///
    /// Args:
    ///     `address`: Emitter contract address (hex string, `0x` optional).
    ///     `topics`: Log topics (hex strings, `0x` optional). `topics[0]` is
    ///         the event signature hash the decoders match against.
    ///     `data`: ABI-encoded log data (hex string, `0x` optional).
    ///     `block_number`: Block number the synthetic log belongs to.
    #[pyo3(signature = (address, topics, data, block_number=0))]
    fn dispatch_log(
        &self,
        address: &str,
        topics: Vec<String>,
        data: &str,
        block_number: u64,
    ) -> PyResult<()> {
        let log = build_rpc_log(address, topics, data, block_number)?;
        self.bot.dispatch_log(&log);
        Ok(())
    }

    /// Get a thin `PyLiquidityPool` handle for the given pool ID.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///
    /// Returns:
    ///     A `PyLiquidityPool` handle, or `None` if the pool ID is not registered.
    fn get_pool(&self, pool_id: u64) -> Option<PyLiquidityPool> {
        if self.bot.state_arc().read().has_pool(pool_id) {
            Some(PyLiquidityPool::new(self.bot.state_arc(), pool_id))
        } else {
            None
        }
    }

    /// Unregister a V2/V3 pool by its contract address.
    ///
    /// ADR-007 U3. Drops the `PoolEntry`, its reorg journal, its index entry,
    /// and any buffered V3 liquidity events for the address so a re-register
    /// does not replay stale Mint/Burn onto the fresh pool. `next_pool_id` is
    /// not reused — removed ids are retired to prevent stale `PyLiquidityPool`
    /// handles aliasing to a different pool on recreate.
    ///
    /// V2/V3 path only — `PyBot` exposes `register_v2/v3_pool` (no V4;
    /// V4 registration lives on `UniswapArbEngine`, and its symmetric
    /// unregister belongs there too — see ADR-007 Consequences).
    ///
    /// Returns `True` if a pool was found and removed; `False` if the address
    /// was never registered (silent no-op, mirroring Python `PoolRegistry.remove`).
    /// Register stays refusal-on-panic for duplicates (ADR-007 U2) — the
    /// asymmetry reflects the asymmetry in the operations' invariants.
    ///
    /// Args:
    ///     `address`: The V2/V3 pool contract address (checksum or 0x-hex).
    ///     `pool_id`: Reserved — must be `None` on `PyBot`. (V4 tuple-key
    ///         unregister is engine-side; kept in the signature for parity
    ///         with `BotState::unregister_pool`.)
    ///
    /// Raises:
    ///     `ValueError`: If `address` cannot be parsed.
    #[pyo3(signature = (address, pool_id=None))]
    #[allow(clippy::needless_pass_by_value)] // PyO3 binding idiom for optional bytes
    fn unregister_pool(&self, address: &str, pool_id: Option<Vec<u8>>) -> PyResult<bool> {
        let addr = parse_address(address)?;
        // V4 on PyBot is intentionally not exposed: registration for V4 lives
        // on UniswapArbEngine (bot::engine), so the symmetric V4 unregister
        // belongs there too (ADR-007 Consequences / Deferred). A `Some` here
        // would be a caller bug — surface it rather than silently no-op.
        if pool_id.is_some() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "PyBot.unregister_pool does not handle V4 (pool_id set); \
                 V4 unregister is engine-side — use the engine’s unregister path.",
            ));
        }
        Ok(self.bot.state_arc().write().unregister_pool(addr, None))
    }

    /// Register a V3 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[allow(clippy::too_many_arguments)]
    /// Register a V3 pool, optionally seeding `tick_data` inline.
    ///
    /// ADR-006 rolling-start race closure: the builder previously called
    /// ``register_v3_pool`` (empty `tick_data`) THEN ``update_tick_data`` to seed
    /// the snapshot. Because ``resume()`` runs before ``build_paths``, a pump
    /// Mint/Burn landing between register and seed was applied to the empty
    /// `tick_data` then CLOBBERED by the seed overwrite — a lost update the
    /// on-chain ``verify_liquidity_maps`` reproduces. Folding the seed into
    /// ``register_v3_pool`` (one ``BotState`` write lock) closes the window:
    /// the pool is never visible to the pump in an unseeded state, so pump
    /// events always land on top of the seed and are never overwritten.
    ///
    /// `tick_data` is ``{tick: (liquidity_gross, liquidity_net, block)}``
    /// (symmetric with ``PyLiquidityPool.tick_data_snapshot``).
    /// `coverage` is ``"tracked"`` (complete DB snapshot) or ``"sparse"``
    /// (RPC-fetched active word only / no snapshot).
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, tick_data=None, update_block=0, coverage="sparse", tick_data_fetcher=None))]
    fn register_v3_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        tick_data: Option<Bound<'_, PyDict>>,
        update_block: u64,
        coverage: &str,
        tick_data_fetcher: Option<Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        // liquidity is uint128 — extracted as U256 then narrowed.
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();

        // Convert `{tick: (gross, net, block)}` → `HashMap<i32, TickInfo>`
        // (mirrors `PyLiquidityPool.update_tick_data`'s boundary conversion).
        let rust_tick_data: std::collections::HashMap<i32, degenbot_bot::bot_core::TickInfo> =
            match tick_data {
                Some(dict) => {
                    let parsed: std::collections::HashMap<i32, (u128, i128, u64)> =
                        dict.extract().map_err(|_| {
                            pyo3::exceptions::PyTypeError::new_err(
                                "register_v3_pool: tick_data must be {tick: (gross, net, block)}",
                            )
                        })?;
                    parsed
                        .into_iter()
                        .map(|(tick, (gross, net, blk))| {
                            (
                                tick,
                                degenbot_bot::bot_core::TickInfo {
                                    liquidity_gross: alloy::primitives::U128::from(gross),
                                    liquidity_net: alloy::primitives::I256::try_from(net)
                                        .unwrap_or(alloy::primitives::I256::ZERO),
                                    block: blk,
                                },
                            )
                        })
                        .collect()
                }
                None => std::collections::HashMap::new(),
            };
        let cov = match coverage {
            "tracked" => PoolTickCoverage::Tracked,
            "sparse" => PoolTickCoverage::Sparse,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "register_v3_pool: coverage must be 'tracked' or 'sparse', got {other:?}",
                )));
            }
        };

        // Verify the pool address against the JSON-sourced CREATE2 deployer +
        // init hash (Fork A, JC6OFG). Skipped if (chain, factory) is not in the
        // shipped JSON — preserves the manual/ad-hoc registration path.
        crate::bot::deployments::verify_v3(self.bot.chain_id(), fac, addr, t0, t1, fee)?;

        // Resolve the JSON-sourced CREATE2 deployer + init hash for this
        // (chain, factory) (Fork A, P62DKO). Stored on the pool identity so the
        // Python companion reads it off the handle (retired ClassVar / no
        // per-class `_verified_address`). Non-JSON pools default to
        // factory-as-deployer + the Uniswap V3 mainnet fallback init hash.
        let chain_id = self.bot.chain_id();
        let deployer = degenbot_uniswap::deployments::resolve_deployer(chain_id, fac);
        let init_hash_b256 = degenbot_uniswap::deployments::resolve_v3_init_hash(chain_id, fac);
        // seed_from_store: when coverage is Tracked but no inline tick_data was
        // passed, the core SnapshotStore must be the source (the DB snapshot
        // loaded at Bot.__init__ feeds the store; register_v3_pool consumes it
        // via `take()`). Inline tick_data (test fixtures / file snapshots) wins.
        let seed_from_store = cov == PoolTickCoverage::Tracked && rust_tick_data.is_empty();

        self.bot
            .state_arc()
            .write()
            .register_v3_pool(&RegisterV3PoolParams {
                address: addr,
                token0: t0,
                token1: t1,
                fee,
                tick_spacing,
                factory: fac,
                deployer,
                init_hash: init_hash_b256,
                sqrt_price_x96: spx,
                liquidity: liq,
                tick,
                tick_data: rust_tick_data,
                update_block,
                coverage: cov,
                seed_from_store,
                fetcher: tick_data_fetcher
                    .filter(|f| !f.is_none())
                    .map(|f| crate::bot::pool::make_tick_fetcher(f.clone().unbind())),
            })
            .map_err(map_register_v3_err)
    }

    /// Register a V4 pool by `(pool_manager, pool_id)`.
    ///
    /// Returns the auto-assigned pool ID. The hook + dynamic-fee admission
    /// floor lives in `BotState::register_v4_pool` (ADR-005 slice 9a): pools
    /// with amount-modifying hooks (`hook_flags & 0xCC != 0`) or dynamic fees
    /// (`fee == 0x100000`) are rejected here, surfacing as typed Python
    /// exceptions (`HookedPoolRejectedError` / `DynamicFeePoolRejectedError`)
    /// so Python classifies by type, not string matching.
    ///
    /// ADR-006 rolling-start race closure: the snapshot `tick_data` is seeded
    /// INLINE in `register_v4_pool` (one `BotState` write lock) so the pool is
    /// never visible to the live pump (resumed before `build_paths`) in an
    /// unseeded state. Previously the builder registered with empty `tick_data`
    /// then called `update_tick_data` to seed — a pump `ModifyLiquidity` landing
    /// between register and seed was applied then CLOBBERED by the seed
    /// overwrite (lost update → V4 tick-map desync → `verify_liquidity_maps`
    /// mismatch). Mirrors the V3 closure.
    ///
    /// Raises:
    ///     `HookedPoolRejectedError`: If `hook_flags & 0xCC != 0`.
    ///     `DynamicFeePoolRejectedError`: If `fee == 0x100000`.
    ///     `ValueError`: If `addresses/pool_id` are malformed or already
    ///         registered.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, sqrt_price_x96, liquidity, tick, block, tick_data=None, coverage="sparse", tick_data_fetcher=None))]
    fn register_v4_pool(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: &str,
        currency1: &str,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
        tick_data: Option<Bound<'_, pyo3::types::PyDict>>,
        coverage: &str,
        tick_data_fetcher: Option<Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;
        let c0 = currency0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency0 address: {e}"))
        })?;
        let c1 = currency1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency1 address: {e}"))
        })?;
        let sp = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        // Convert `{tick: (gross, net, block)}` → `HashMap<i32, TickInfo>` —
        // mirrors `register_v3_pool` (ADR-006 rolling-start race closure for
        // V4: seed inline so the pool is never visible to the live pump
        // unseeded).
        let rust_tick_data: std::collections::HashMap<i32, degenbot_bot::bot_core::TickInfo> =
            match tick_data {
                Some(dict) => {
                    let parsed: std::collections::HashMap<i32, (u128, i128, u64)> =
                        dict.extract().map_err(|_| {
                            pyo3::exceptions::PyTypeError::new_err(
                                "register_v4_pool: tick_data must be {tick: (gross, net, block)}",
                            )
                        })?;
                    parsed
                        .into_iter()
                        .map(|(tick, (gross, net, blk))| {
                            (
                                tick,
                                degenbot_bot::bot_core::TickInfo {
                                    liquidity_gross: alloy::primitives::U128::from(gross),
                                    liquidity_net: alloy::primitives::I256::try_from(net)
                                        .unwrap_or(alloy::primitives::I256::ZERO),
                                    block: blk,
                                },
                            )
                        })
                        .collect()
                }
                None => std::collections::HashMap::new(),
            };
        let cov = match coverage {
            "tracked" => PoolTickCoverage::Tracked,
            "sparse" => PoolTickCoverage::Sparse,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "register_v4_pool: coverage must be 'tracked' or 'sparse', got {other:?}",
                )));
            }
        };
        // seed_from_store: Tracked coverage + no inline tick_data → the core
        // SnapshotStore is the source (DB snapshot loaded at Bot.__init__).
        let seed_from_store = cov == PoolTickCoverage::Tracked && rust_tick_data.is_empty();
        self.bot
            .state_arc()
            .write()
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: pm,
                pool_id,
                pool_key: V4PoolKey {
                    currency0: c0,
                    currency1: c1,
                    fee,
                    tick_spacing,
                    hooks: Address::ZERO, // Hook filtering already done via hook_flags.
                },
                hook_flags,
                sqrt_price_x96: sp,
                liquidity,
                tick,
                tick_data: rust_tick_data,
                update_block: block,
                coverage: cov,
                seed_from_store,
                fetcher: tick_data_fetcher
                    .filter(|f| !f.is_none())
                    .map(|f| crate::bot::pool::make_tick_fetcher(f.clone().unbind())),
            })
            .map_err(map_register_v4_err)
    }

    /// Register a Curve `StableSwap` pool by contract address.
    ///
    /// Returns the auto-assigned pool ID. The `tokens` / `rate_multipliers` /
    /// `balances` lists must all have the same length `N` (the coin count).
    ///
    /// Raises:
    ///     `ValueError`: If the pool address is malformed or already
    ///         registered, or the list lengths mismatch.
    #[allow(clippy::too_many_arguments)]
    // `y_variant` / `yd_variant` mirror the public Python kwargs and the
    // `resolve_y_variant` / `resolve_yd_variant` vocabulary in
    // `degenbot.curve._variant_groups`; they are intentional domain terms,
    // not a naming slip, so the `similar_names` nudge does not apply here.
    #[allow(clippy::similar_names)]
    #[pyo3(signature = (
        address,
        tokens,
        a_coefficient,
        fee,
        admin_fee,
        rate_multipliers,
        balances,
        update_block,
        swap_style=0,
        lending_rate_style=0,
        d_variant=0,
        y_variant=0,
        yd_variant=0,
        base_pool=None,
        initial_a_coefficient=None,
        future_a_coefficient=None,
        initial_a_coefficient_time=None,
        future_a_coefficient_time=None,
        create_timestamp=None,
        fee_gamma=None,
        mid_fee=None,
        offpeg_fee_multiplier=None,
        out_fee=None,
        gamma=None,
        lp_token=None,
        use_lending=None,
        precision_multipliers=None,
        tokens_underlying=None,
        metapool_rate_style=1,
        metapool_underlying_style=1,
        data_provider=None
    ))]
    fn register_curve_pool(
        &self,
        address: &str,
        tokens: &Bound<'_, PyList>,
        a_coefficient: u128,
        fee: u64,
        admin_fee: u64,
        rate_multipliers: &Bound<'_, PyList>,
        balances: &Bound<'_, PyList>,
        update_block: u64,
        swap_style: u8,
        lending_rate_style: u8,
        d_variant: u8,
        y_variant: u8,
        yd_variant: u8,
        base_pool: Option<&str>,
        initial_a_coefficient: Option<u128>,
        future_a_coefficient: Option<u128>,
        initial_a_coefficient_time: Option<u64>,
        future_a_coefficient_time: Option<u64>,
        create_timestamp: Option<u64>,
        fee_gamma: Option<u64>,
        mid_fee: Option<u64>,
        offpeg_fee_multiplier: Option<u64>,
        out_fee: Option<u64>,
        gamma: Option<u64>,
        lp_token: Option<&str>,
        use_lending: Option<&Bound<'_, PyList>>,
        precision_multipliers: Option<&Bound<'_, PyList>>,
        tokens_underlying: Option<&Bound<'_, PyList>>,
        metapool_rate_style: u8,
        metapool_underlying_style: u8,
        data_provider: Option<Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let token_addrs = parse_address_list(tokens)?;
        let rate_mults = extract_u256_list(rate_multipliers)?;
        let bal_vals = extract_u256_list(balances)?;
        let base = match base_pool {
            Some(s) => Some(parse_address(s)?),
            None => None,
        };
        let lp = match lp_token {
            Some(s) => Some(parse_address(s)?),
            None => None,
        };
        let use_lend: Vec<bool> = match use_lending {
            Some(l) => l.extract()?,
            None => Vec::new(),
        };
        let prec_mults: Vec<alloy::primitives::U256> = match precision_multipliers {
            Some(l) => extract_u256_list(l)?,
            None => Vec::new(),
        };
        let tokens_under = match tokens_underlying {
            Some(l) => Some(parse_address_list(l)?),
            None => None,
        };
        Ok(self
            .bot
            .state_arc()
            .write()
            .register_curve_pool(&RegisterCurvePoolParams {
                address: addr,
                tokens: token_addrs,
                a_coefficient,
                fee,
                admin_fee,
                rate_multipliers: rate_mults,
                balances: bal_vals,
                update_block,
                swap_style,
                lending_rate_style,
                d_variant,
                y_variant,
                yd_variant,
                base_pool: base,
                initial_a_coefficient,
                future_a_coefficient,
                initial_a_coefficient_time,
                future_a_coefficient_time,
                create_timestamp,
                fee_gamma,
                mid_fee,
                offpeg_fee_multiplier,
                out_fee,
                gamma,
                lp_token: lp,
                use_lending: use_lend,
                precision_multipliers: prec_mults,
                tokens_underlying: tokens_under,
                metapool_rate_style,
                metapool_underlying_style,
                data_provider: data_provider
                    .map(|b| crate::bot::pool::make_curve_data_provider(b.unbind())),
            }))
    }

    /// Register a Balancer V2 weighted pool (ADR-005 slice 12a state port).
    ///
    /// Stores immutable pool config (`pool_id`, vault, tokens, weights,
    /// `scaling_factors`, `swap_fee`, `pow_version`) + registration balances + a
    /// genesis reorg journal delta. The slice 12b Python `BalancerV2Pool`
    /// companion is constructed over the returned `PyLiquidityPool` handle
    /// (call `get_pool(id)` after this).
    ///
    /// Returns the auto-assigned numeric pool ID. `tokens` / `weights` /
    /// `scaling_factors` / `balances` lists MUST all have length `N`.
    ///
    /// Raises:
    ///     `ValueError`: If an address is malformed, the pool is already
    ///         registered, the list lengths mismatch, or `pool_id_hex` is not
    ///         a 32-byte hex string.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        address,
        vault,
        pool_id_hex,
        tokens,
        weights,
        scaling_factors,
        swap_fee,
        pow_version,
        balances,
        update_block
    ))]
    fn register_balancer_weighted_pool(
        &self,
        address: &str,
        vault: &str,
        pool_id_hex: &str,
        tokens: &Bound<'_, PyList>,
        weights: &Bound<'_, PyList>,
        scaling_factors: &Bound<'_, PyList>,
        swap_fee: u128,
        pow_version: u8,
        balances: &Bound<'_, PyList>,
        update_block: u64,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let vault_addr = parse_address(vault)?;
        let pool_id_bytes = hex_string_to_pool_id(pool_id_hex)?;
        let token_addrs = parse_address_list(tokens)?;
        let weight_vals = extract_u256_list(weights)?;
        let scaling_vals = extract_u256_list(scaling_factors)?;
        let bal_vals = extract_u256_list(balances)?;
        Ok(self
            .bot
            .state_arc()
            .write()
            .register_balancer_weighted_pool(&RegisterBalancerWeightedPoolParams {
                address: addr,
                vault: vault_addr,
                pool_id: pool_id_bytes,
                tokens: token_addrs,
                weights: weight_vals,
                scaling_factors: scaling_vals,
                swap_fee,
                pow_version,
                balances: bal_vals,
                update_block,
            }))
    }

    /// Register a Balancer V2 stable pool (ADR-005 slice 12c state port).
    ///
    /// Stores immutable pool config (`pool_id`, vault, tokens, amp,
    /// `scaling_factors`, `swap_fee`, `bpt_idx`, `invariant_version`) +
    /// registration balances + a genesis reorg journal delta. The slice 12d
    /// Python `BalancerV2StablePool` companion will be constructed over the
    /// returned `PyLiquidityPool` handle (call `get_pool(id)` after this).
    ///
    /// Returns the auto-assigned numeric pool ID. `tokens` / `scaling_factors`
    /// / `balances` lists MUST all have length `N`. `bpt_idx` is `None` for
    /// `MetaStablePools` and `Some(i)` (`i < N`) for `ComposableStablePools`.
    ///
    /// Raises:
    ///     `ValueError`: If an address is malformed, the pool is already
    ///         registered, the list lengths mismatch, `bpt_idx` is
    ///         out-of-range, or `pool_id_hex` is not a 32-byte hex string.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        address,
        vault,
        pool_id_hex,
        tokens,
        amp,
        scaling_factors,
        swap_fee,
        bpt_idx,
        invariant_version,
        balances,
        update_block,
        rate_provider=None
    ))]
    fn register_balancer_stable_pool(
        &self,
        address: &str,
        vault: &str,
        pool_id_hex: &str,
        tokens: &Bound<'_, PyList>,
        amp: u128,
        scaling_factors: &Bound<'_, PyList>,
        swap_fee: u128,
        bpt_idx: Option<usize>,
        invariant_version: u8,
        balances: &Bound<'_, PyList>,
        update_block: u64,
        rate_provider: Option<Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let vault_addr = parse_address(vault)?;
        let pool_id_bytes = hex_string_to_pool_id(pool_id_hex)?;
        let token_addrs = parse_address_list(tokens)?;
        let scaling_vals = extract_u256_list(scaling_factors)?;
        let bal_vals = extract_u256_list(balances)?;
        let provider =
            rate_provider.map(|b| crate::bot::pool::make_balancer_rate_provider(b.unbind()));
        Ok(self.bot.state_arc().write().register_balancer_stable_pool(
            &RegisterBalancerStablePoolParams {
                address: addr,
                vault: vault_addr,
                pool_id: pool_id_bytes,
                tokens: token_addrs,
                amp,
                scaling_factors: scaling_vals,
                swap_fee,
                bpt_idx,
                invariant_version,
                balances: bal_vals,
                update_block,
                rate_provider: provider,
            },
        ))
    }

    /// Register an Aerodrome V2 pool by contract address (ADR-005 Aerodrome
    /// state port).
    ///
    /// Stores immutable identity (address, tokens, factory, variant, stable
    /// flag, unidirectional fee) + the registration reserves + a genesis
    /// reorg-journal anchor (mirror of V2's discipline). Returns the
    /// auto-assigned pool ID. Call `get_pool(id)` after this to obtain the
    /// `PyLiquidityPool` handle.
    ///
    /// Raises:
    ///     `ValueError`: If an address is malformed, the pool is already
    ///         registered, or `variant` is not a recognized Aerodrome variant.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, factory, variant, stable, fee_numer, fee_denom, reserve0, reserve1, update_block=0))]
    fn register_aerodrome_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        factory: &str,
        variant: &str,
        stable: bool,
        fee_numer: u64,
        fee_denom: u64,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        update_block: u64,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;
        let variant_enum = DexVariant::from_kebab(variant).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown variant: {variant}"))
        })?;
        Ok(self
            .bot
            .state_arc()
            .write()
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                address: addr,
                token0: t0,
                token1: t1,
                factory: fac,
                variant: variant_enum,
                stable,
                fee: (fee_numer, fee_denom),
                reserve0: r0,
                reserve1: r1,
                update_block,
            }))
    }

    /// Update a V3 pool's state from a Swap event.
    ///
    /// No-op if the pool is not registered.
    #[pyo3(signature = (address, sqrt_price_x96, liquidity, tick, block_number))]
    fn update_v3_pool(
        &self,
        address: &str,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = parse_address(address)?;
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();

        self.bot
            .state_arc()
            .write()
            .update_v3_pool(addr, spx, liq, tick, block_number, vec![]);
        Ok(())
    }

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    fn v3_journal_len(&self, pool_id: u64) -> usize {
        self.bot.state_arc().read().v3_journal_len(pool_id)
    }

    /// Discard V3 reorg journal deltas earlier than the given block.
    /// Discard V3 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the target
    /// is past the newest delta. Raises `ValueError` on error (ADR-005 slice 4).
    #[pyo3(signature = (pool_id, block))]
    fn v3_discard_before_block(&self, pool_id: u64, block: u64) -> PyResult<()> {
        self.bot
            .state_arc()
            .write()
            .v3_discard_before_block(pool_id, block)
            .map_err(journal_err_to_py)
    }

    /// Restore V3 pool state prior to a target block.
    ///
    /// Returns `(sqrt_price_x96, liquidity, tick, block)` as Python ints,
    /// or `None` if the pool ID is not registered or not a V3 pool.
    #[pyo3(signature = (pool_id, block))]
    fn v3_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let state = self.bot.state_arc();
            let mut core = state.write();
            core.v3_restore_before_block(pool_id, block)
        };
        match result {
            Some(restore) => {
                // `restore.scalar_priors` is always `Some` post-restore: the
                // core `v3_restore_before_block` populates it with the current
                // state scalars when the rolled-back range was tick-only
                // (None internally — the scalars were never changed by the
                // rolled-back events, so the current scalars ARE the restored
                // scalars). See ADR-004.
                let p = restore
                    .scalar_priors
                    .as_ref()
                    .expect("post-restore scalar_priors must be Some");
                let liq_u128 = p.liquidity_before;
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::conversion::alloy::u256_to_py(py, &p.sqrt_price_x96_before)?
                            .unbind(),
                        liq_u128.into_pyobject(py)?.into_any().unbind(),
                        p.tick_before.into_pyobject(py)?.into_any().unbind(),
                        restore.block.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
            None => Ok(None),
        }
    }

    /// Register a token.
    ///
    /// Args:
    ///     `address`: Token contract address (hex string).
    ///     `name`: Token name.
    ///     `symbol`: Token symbol.
    ///     `decimals`: Token decimals.
    ///     `chain_id`: Chain ID.
    #[pyo3(signature = (address, name, symbol, decimals, chain_id))]
    fn register_token(
        &self,
        address: &str,
        name: &str,
        symbol: &str,
        decimals: u8,
        chain_id: u64,
    ) -> PyResult<PyErc20Token> {
        let addr = parse_address(address)?;
        self.bot.state_arc().write().register_token(
            addr,
            name.to_string(),
            symbol.to_string(),
            decimals,
            chain_id,
        );
        Ok(PyErc20Token::new(self.bot.state_arc(), addr))
    }

    /// Get a thin `PyErc20Token` handle for the given address.
    ///
    /// Args:
    ///     `address`: Token contract address (hex string).
    ///
    /// Returns:
    ///     A `PyErc20Token` handle, or `None` if the address is not registered.
    fn get_token(&self, address: &str) -> PyResult<Option<PyErc20Token>> {
        let addr = parse_address(address)?;
        if self.bot.state_arc().read().has_token(&addr) {
            Ok(Some(PyErc20Token::new(self.bot.state_arc(), addr)))
        } else {
            Ok(None)
        }
    }

    /// Encode a V2 swap call, returning `(to_address_hex, calldata_hex, value)`.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_out`: Output token amount (Python int).
    ///     `recipient`: Address to receive output tokens (hex string).
    ///
    /// Returns:
    ///     A tuple `(to_hex, calldata_hex, value)` or `None` if pool not found.
    #[pyo3(signature = (pool_id, zero_for_one, amount_out, recipient))]
    fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        recipient: &str,
    ) -> PyResult<Option<(String, String, u64)>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let recip = parse_address(recipient)?;

        let result = {
            let state = self.bot.state_arc();
            let core = state.read();
            core.encode_swap(pool_id, zero_for_one, amount, recip)
        };

        Ok(result.map(|call| {
            let to_hex = format!("{:#x}", call.to);
            let data_hex = format!("0x{}", bytes_to_hex(&call.data));
            (to_hex, data_hex, call.value.to::<u64>())
        }))
    }

    /// Get the number of deltas in the reorg journal for a V2 pool.
    ///
    /// Returns 0 if the pool ID is not registered.
    fn v2_journal_len(&self, pool_id: u64) -> usize {
        self.bot.state_arc().read().v2_journal_len(pool_id)
    }

    /// Discard V2 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the
    /// target is past the newest delta (would remove every known state).
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta.
    #[pyo3(signature = (pool_id, block))]
    fn v2_discard_before_block(&self, pool_id: u64, block: u64) -> PyResult<()> {
        self.bot
            .state_arc()
            .write()
            .v2_discard_before_block(pool_id, block)
            .map_err(journal_err_to_py)
    }

    /// Restore V2 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block and restores the
    /// landed-at state into the current fluid fields.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None`
    /// if the pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If the target is at or before the registration block
    ///         (no state exists before it) — rolling back past registration is
    ///         a hard error, not a silent no-op (ADR-005 slice 4 decision 3).
    #[pyo3(signature = (pool_id, block))]
    fn v2_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let state = self.bot.state_arc();
            let mut core = state.write();
            core.v2_restore_before_block(pool_id, block)
        };
        match result {
            None => Ok(None),
            Some(Err(e)) => Err(journal_err_to_py(e)),
            Some(Ok((r0, r1, blk))) => {
                let r0 = r0.to::<alloy::primitives::U256>();
                let r1 = r1.to::<alloy::primitives::U256>();
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
                        crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
                        blk.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_address(s: &str) -> PyResult<Address> {
    s.parse()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid address '{s}': {e}")))
}

/// Parse a Python list of address strings into `Vec<Address>`.
fn parse_address_list(list: &Bound<'_, PyList>) -> PyResult<Vec<Address>> {
    list.iter()
        .map(|item| {
            let s: String = item.extract().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("token address must be a str: {e}"))
            })?;
            parse_address(&s)
        })
        .collect()
}

/// Extract a Python list of ints (or int-like) into `Vec<U256>`.
fn extract_u256_list(list: &Bound<'_, PyList>) -> PyResult<Vec<alloy::primitives::U256>> {
    list.iter()
        .map(|item| crate::conversion::alloy::extract_python_u256(&item))
        .collect()
}

/// Map a [`JournalError`] to a Python `ValueError` with the `NoPoolStateAvailable`
///-shaped message the Python pool companion expects (and re-raises as
/// `NoPoolStateAvailable`). ADR-005 slice 4 decision 2: reorg errors that used
/// to panic must surface as `ValueError`. Shared by `PyBot` and `PyLiquidityPool`.
pub(crate) fn journal_err_to_py(e: JournalError) -> PyErr {
    match e {
        JournalError::NoStatePriorToBlock { block } => pyo3::exceptions::PyValueError::new_err(
            format!("No pool state known prior to block {block}"),
        ),
        JournalError::NoStateAtOrAfterBlock { block } => pyo3::exceptions::PyValueError::new_err(
            format!("No pool state known at or after block {block}"),
        ),
    }
}
