//! `PyO3` wrapper for the `UniswapEngine` — register `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/optimizers/uniswap_engine/`'s per-concern
//! layout. PyO3 allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::*;
use crate::prelude::*;

use super::verify::{map_verify_err, EngineVerifyRpc};

#[pymethods]
impl PyUniswapArbEngine {
    #[new]
    #[pyo3(signature = (bot=None))]
    fn new(py: Python<'_>, bot: Option<Py<PyBot>>) -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        // ADR-006 D1+D4: if a `PyBot` is supplied, adopt its shared
        // `Arc<RwLock<BotState>>` so the engine reads/writes the SAME core that
        // `PyBot`/`PyLiquidityPool`/`PyErc20Token` share — and clone its `Arc<Bot>`
        // so `BlockPump`'s `dispatch_log` writes flow through to the engine's
        // reads (dissolving the dual-`BotState` split —
        // `rust-owned-bot.md` §17 stale-state root cause). Without one, allocate
        // a standalone core + wrap it in a fresh `Bot` (no-pyo3 / legacy path).
        let (engine, bot) = if let Some(bot) = bot {
            let bot = bot.borrow(py).bot_arc();
            (UniswapEngine::with_core(bot.state_arc()), bot)
        } else {
            let core = Arc::new(parking_lot::RwLock::new(
                degenbot_bot::bot_core::BotState::new(),
            ));
            let bot = Arc::new(Bot::with_core(Arc::clone(&core)));
            (UniswapEngine::with_core(core), bot)
        };
        let mut engine = engine;
        engine.set_result_channel(result_tx);
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        // ADR-006 slice 6: wrap the shared engine in `EngineHandle` (an
        // `Arc<dyn Engine>` view) and build the coordinator. The coordinator
        // replaces slice 5a's `EngineDrainSink` pass-through; it fans drain-tick
        // calls to the engine under a `drain_lock` and exposes a
        // drain-consistent `last_processed_block` (Python polls block until
        // any in-flight drain completes — no Rust/Python race).
        let coordinator = Arc::new(SolveCoordinator::new(vec![EngineHandle::arc_dyn(
            Arc::clone(&engine),
        )]));
        // ADR-006 slice 7: the per-event reorg coordinator. Holds `Arc<Bot>`;
        // `dispatch_reorg_log` decodes a `removed: true` log, restores the
        // targeted pool via `BotState::restore_before_block`, then notifies
        // subscribers (the same path as forward `dispatch_log`).
        let reorg_coordinator = Arc::new(ReorgCoordinator::new(Arc::clone(&bot)));
        Self {
            engine,
            coordinator,
            reorg_coordinator,
            bot,
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: parking_lot::Mutex::new(None),
            subscribe_state: parking_lot::Mutex::new(None),
            result_rx: Arc::new(parking_lot::Mutex::new(Some(result_rx))),
            verify_on_register: std::sync::atomic::AtomicBool::new(false),
            verify_rpc_url: parking_lot::Mutex::new(None),
            verify_provider: parking_lot::Mutex::new(None),
            verify_state_view: parking_lot::Mutex::new(None),
            verify_snapshot_block: parking_lot::Mutex::new(None),
            verify_backfill_block: parking_lot::Mutex::new(None),
            phase: std::sync::atomic::AtomicU8::new(EnginePhase::Created as u8),
            v3_snapshot: SnapshotStore::new(),
            v4_snapshot: SnapshotStore::new(),
        }
    }

    /// Register a V2 pool by contract address and initial reserves.
    /// Returns the assigned `pool_id` (orientation is selected at solve time
    /// via `zero_for_one`; there is no separate reverse id — ADR-006 D3).
    #[pyo3(signature = (address, reserve0, reserve1, gamma_numer, fee_denom))]
    fn register_v2_pool(
        &self,
        address: &str,
        reserve0: &Bound<'_, pyo3::PyAny>,
        reserve1: &Bound<'_, pyo3::PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<u64> {
        let addr: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;
        let r0 = crate::conversion::alloy::extract_python_u256(reserve0)?;
        let r1 = crate::conversion::alloy::extract_python_u256(reserve1)?;

        // ADR-006 D3: pool construction is a `BotState` concern only. The engine
        // registers into its associated `BotState` (`core`) — for a standalone
        // engine that's its own `BotState`; for a `bot=`-constructed engine
        // it's the shared `PyBot` core (so the caller must NOT also register
        // the same pool via `PyBot`, or `BotState::register_v2_pool` panics).
        let params = degenbot_bot::bot_core::RegisterV2PoolParams {
            address: addr,
            token0: Address::ZERO,
            token1: Address::ZERO,
            reserve0: r0,
            reserve1: r1,
            fee_token0: (gamma_numer, fee_denom),
            fee_token1: (gamma_numer, fee_denom),
            factory: Address::ZERO,
            update_block: 0,
        };
        Ok(self.engine.lock().core.write().register_v2_pool(&params))
    }

    /// Register a V3 pool by contract address and initial state.
    /// Returns the pool key for use in path registration.
    ///
    /// Tick data is resolved automatically from the stored V3 snapshot:
    /// - Pool found in snapshot → `Tracked` coverage (`tick_data` consumed via `remove()`)
    /// - Pool not in snapshot → `Sparse` coverage (empty `tick_data`)
    ///
    /// The buffer is always applied (Plan 098: snapshot data is always stale
    /// from the DB, so the buffer must bring it forward).
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, block=0))]
    fn register_v3_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
    ) -> PyResult<u64> {
        // No phase check on registration — the engine lock serializes access.
        // Registration is allowed in any phase.

        let addr = address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let t0 = token0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token0 address: {e}"))
        })?;
        let t1 = token1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token1 address: {e}"))
        })?;
        let fac = factory.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid factory address: {e}"))
        })?;
        let sp = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;

        // Look up tick_data from stored V3 snapshot (one-way transfer via remove)
        let (rust_tick_data, coverage) = self.v3_snapshot.take(&addr);
        let is_tracked = coverage == PoolTickCoverage::Tracked;

        // Clone tick_data for snapshot verification before it's moved into register_pool.
        let tick_data_for_snapshot_verify = if is_tracked {
            Some(rust_tick_data.clone())
        } else {
            None
        };

        let (key, backfill_verify_snapshot) = register_with_cl_buffers(
            &self.engine,
            |engine| {
                engine.core.write().register_v3_pool(&RegisterV3PoolParams {
                    address: addr,
                    token0: t0,
                    token1: t1,
                    fee,
                    tick_spacing,
                    factory: fac,
                    sqrt_price_x96: sp,
                    liquidity,
                    tick,
                    tick_data: rust_tick_data,
                    update_block: block,
                    coverage,
                })
            },
            |engine| engine.core.write().apply_backfill_buffer_v3(&addr),
            |engine, key| {
                is_tracked
                    .then(|| engine.core.read().get_v3_pool(*key).cloned())
                    .flatten()
            },
            |engine| engine.core.write().apply_pump_buffer_v3(&addr),
        );

        if is_tracked
            && self
                .verify_on_register
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            let snapshot_block = *self.verify_snapshot_block.lock();
            let backfill_block = *self.verify_backfill_block.lock();

            // ADR-006 slice-5 candidate-2: the verify closures shrink to
            // delegate to the `VerifyRpc` trait (the `AlloyProvider` I/O +
            // `block_on` + error-formatting live inside the `EngineVerifyRpc`
            // impl). The pure orchestrator `run_cl_verification` drives the
            // two phases in order without touching pyo3/tokio at this layer.
            let rpc = EngineVerifyRpc {
                rpc_url: &self.verify_rpc_url,
                provider: &self.verify_provider,
            };
            let verify_snapshot =
                |rpc: &EngineVerifyRpc<'_>, block: u64| -> Result<(), VerifyError> {
                    let Some(ref td) = tick_data_for_snapshot_verify else {
                        return Ok(());
                    };
                    rpc.verify_v3_snapshot(addr, td, block)
                };
            let verify_backfill =
                |rpc: &EngineVerifyRpc<'_>, block: u64| -> Result<(), VerifyError> {
                    let Some(ref pool_snapshot) = backfill_verify_snapshot else {
                        return Ok(());
                    };
                    let mut pool_map = HashMap::new();
                    pool_map.insert(key, pool_snapshot.clone());
                    rpc.verify_v3_backfill(&pool_map, block)
                };

            map_verify_err(run_cl_verification(
                &rpc,
                snapshot_block,
                backfill_block,
                verify_snapshot,
                verify_backfill,
            ))?;
        }

        Ok(key)
    }

    /// Register a V4 pool with the engine.
    ///
    /// Hook filtering: pools with amount-modifying hook flags (`BEFORE_SWAP`,
    /// `AFTER_SWAP`, `BEFORE_SWAP_RETURNS_DELTA`, `AFTER_SWAP_RETURNS_DELTA`)
    /// are rejected. Dynamic-fee pools (fee=0x100000) are also rejected.
    ///
    /// Tick data is resolved automatically from the stored V4 snapshot:
    /// - Pool found in snapshot → `Tracked` coverage (`tick_data` consumed via `remove()`)
    /// - Pool not in snapshot → `Sparse` coverage (empty `tick_data`)
    ///
    /// The buffer is always applied (Plan 098: snapshot data is always stale
    /// from the DB, so the buffer must bring it forward).
    ///
    /// Returns the forward pool key for use in path registration,
    /// or raises `ValueError` if the pool is excluded.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, sqrt_price_x96, liquidity, tick, block=0))]
    fn register_v4_pool(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: &str,
        currency1: &str,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
    ) -> PyResult<u64> {
        // No phase check on registration — the engine lock serializes access.
        // Registration is allowed in any phase.

        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;

        // Decode pool_id from hex string (e.g. "0x1234...") to [u8; 32]
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;

        let c0 = currency0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency0 address: {e}"))
        })?;
        let c1 = currency1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency1 address: {e}"))
        })?;
        let sp = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;

        // Look up tick_data from stored V4 snapshot (one-way transfer via remove)
        let (rust_tick_data, coverage) = self.v4_snapshot.take(&(pm, pool_id));
        let is_tracked = coverage == PoolTickCoverage::Tracked;

        // Clone tick_data for snapshot verification before it's moved into register_pool.
        let tick_data_for_snapshot_verify = if is_tracked {
            Some(rust_tick_data.clone())
        } else {
            None
        };

        let (key, backfill_verify_snapshot) = register_with_cl_buffers(
            &self.engine,
            |engine| -> Result<u64, pyo3::PyErr> {
                engine
                    .core
                    .write()
                    .register_v4_pool(&RegisterV4PoolParams {
                        pool_manager: pm,
                        pool_id,
                        pool_key: degenbot_bot::bot_core::V4PoolKey {
                            currency0: c0,
                            currency1: c1,
                            fee,
                            tick_spacing,
                            hooks: Address::ZERO, // Not needed for solving; hook filtering already done
                        },
                        hook_flags,
                        sqrt_price_x96: sp,
                        liquidity,
                        tick,
                        tick_data: rust_tick_data,
                        update_block: block,
                        coverage,
                    })
                    .map_err(map_register_v4_err)
            },
            |engine| engine.core.write().apply_backfill_buffer_v4(pm, pool_id),
            |engine, key| {
                let Ok(key) = key else {
                    return None;
                };
                is_tracked
                    .then(|| engine.core.read().get_v4_pool(*key).cloned())
                    .flatten()
            },
            |engine| engine.core.write().apply_pump_buffer_v4(pm, pool_id),
        );

        let key = key?;

        if is_tracked
            && self
                .verify_on_register
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            let state_view = *self.verify_state_view.lock();
            let snapshot_block = *self.verify_snapshot_block.lock();
            let backfill_block = *self.verify_backfill_block.lock();

            // ADR-006 slice-5 candidate-2: verify closures shrink to delegate
            // to the `VerifyRpc` trait (same shape as the V3 register path).
            let rpc = EngineVerifyRpc {
                rpc_url: &self.verify_rpc_url,
                provider: &self.verify_provider,
            };
            let verify_snapshot = |rpc: &EngineVerifyRpc<'_>,
                                   block: u64|
             -> Result<(), VerifyError> {
                let (Some(sv), Some(ref td)) = (state_view, tick_data_for_snapshot_verify) else {
                    return Ok(());
                };
                rpc.verify_v4_snapshot(sv, pool_id, td, block)
            };
            let verify_backfill = |rpc: &EngineVerifyRpc<'_>,
                                   block: u64|
             -> Result<(), VerifyError> {
                let (Some(sv), Some(ref pool_snapshot)) = (state_view, backfill_verify_snapshot)
                else {
                    return Ok(());
                };
                let mut pool_map = HashMap::new();
                pool_map.insert(key, pool_snapshot.clone());
                rpc.verify_v4_backfill(sv, &pool_map, block)
            };

            map_verify_err(run_cl_verification(
                &rpc,
                snapshot_block,
                backfill_block,
                verify_snapshot,
                verify_backfill,
            ))?;
        }

        Ok(key)
    }

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut hops = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (pool_id, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_id: u64 = tuple.get_item(0)?.extract()?;
            let zero_for_one: bool = tuple.get_item(1)?.extract()?;
            hops.push(PoolHop {
                pool_id,
                zero_for_one,
            });
        }

        if hops.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", hops.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let pool_ids: Vec<u64> = hops.iter().map(|h| h.pool_id).collect();
        let path_id = self
            .engine
            .lock()
            .register_path(hops)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        // ADR-006 D4: subscribe the engine to each pool_id's state updates so
        // `Bot::dispatch_log` (driven by BlockPump) dirties the engine via the
        // `EngineSubscriber` adapter. Without this, dispatched logs apply to
        // `BotState` but never mark paths dirty → no solves (the live chain was
        // severed when `apply_log` was replaced by `dispatch_log` in slice 5).
        // Duplicate pool_ids across paths are harmless (`insert_dirty` is
        // idempotent via `HashSet`).
        let subscriber = EngineSubscriber::weak_handle(&self.engine);
        for pool_id in pool_ids {
            self.bot.attach_engine(pool_id, subscriber.clone());
        }
        Ok(path_id)
    }

    /// Register a mixed arbitrage path and eagerly solve it.
    ///
    /// Unlike `register_path`, this method also resolves and solves the path
    /// immediately, appending any profitable result to the engine's results.
    /// Used when the engine is already running (after the pump has started)
    /// so that new paths are immediately available to `latest_results()`.
    #[pyo3(signature = (pool_refs))]
    fn register_and_solve_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut hops = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (pool_id, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_id: u64 = tuple.get_item(0)?.extract()?;
            let zero_for_one: bool = tuple.get_item(1)?.extract()?;
            hops.push(PoolHop {
                pool_id,
                zero_for_one,
            });
        }

        if hops.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", hops.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let pool_ids: Vec<u64> = hops.iter().map(|h| h.pool_id).collect();
        let path_id = self
            .engine
            .lock()
            .register_and_solve_path(hops)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        // ADR-006 D4: subscribe the engine to each pool_id (see `register_path`).
        let subscriber = EngineSubscriber::weak_handle(&self.engine);
        for pool_id in pool_ids {
            self.bot.attach_engine(pool_id, subscriber.clone());
        }
        Ok(path_id)
    }

    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns the first observed block number. Python should:
    /// 1. Run backfill up to the returned block number
    /// 2. Call `resume()` to begin normal processing
    ///
    /// A "complete" block is one where both a `newHeads` notification and at
    /// least one log for the same block have been received. This guarantees
    /// the logs subscription did not miss the start of the block.
    /// No events are buffered during subscribe — the backfill is the sole
    /// authority for the gap between snapshot and WS start.
    ///
    /// Raises `RuntimeError` if the pump is already started or subscribed.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url))]
    fn subscribe(&self, rpc_url: String) -> PyResult<u64> {
        // Phase check: must be Created
        let phase = self.current_phase();
        phase
            .require(EnginePhase::Created, "subscribe")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure we're not already running
        if self.pump_handle.lock().is_some() {
            let msg = "Cannot subscribe: pump is already started. Call stop() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if self.subscribe_state.lock().is_some() {
            let msg = "Cannot subscribe: already subscribed. Call resume() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let bot = Arc::clone(&self.bot);
        let sink: Arc<dyn DrainSink> = self.coordinator.clone();
        let reorg_coordinator = Arc::clone(&self.reorg_coordinator);
        let shutdown = Arc::clone(&self.shutdown);

        // Run the subscribe phase synchronously (blocks Python until first block observed)
        let runtime = degenbot_core::runtime::get_runtime();
        let subscribe_result = runtime
            .block_on(async {
                BlockPump::subscribe(&rpc_url, bot, sink, reorg_coordinator, shutdown).await
            })
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        let (pump, state) = subscribe_result;

        // Store the subscribe state for resume()
        *self.subscribe_state.lock() = Some(PySubscribeState {
            pump,
            first_block: state.first_block,
            combined_stream: state
                .combined_stream
                .expect("subscribe() always returns a stream"),
        });

        // Advance phase
        self.set_phase(EnginePhase::Subscribed);

        Ok(state.first_block)
    }

    /// Backfill Mint/Burn/ModifyLiquidity events from the last DB snapshot
    /// block to the first WS block observed during `subscribe()`.
    ///
    /// Must be called after `subscribe()`, before `resume()`. Uses
    /// `eth_getLogs` to fetch events for the gap between the DB snapshot
    /// and the live WS connection, then applies them to the V3/V4 engines
    /// via `backfill_logs()`.
    ///
    /// This ensures that when pools are registered (with `tick_data` from the
    /// DB snapshot), any liquidity changes between the snapshot block and
    /// the current chain head are reflected in the Rust engine's state.
    ///
    /// Args:
    ///     `rpc_url`: HTTP RPC endpoint for `eth_getLogs` requests
    ///     `chunk_size`: Number of blocks per `eth_getLogs` request (default 2000)
    ///
    /// Returns the number of blocks backfilled (0 if snapshot is current).
    #[pyo3(signature = (rpc_url, snapshot_block, chunk_size=2000))]
    fn backfill_from_snapshot(
        &self,
        rpc_url: &str,
        snapshot_block: u64,
        chunk_size: u64,
    ) -> PyResult<u64> {
        // Phase check: must be at least SnapshotLoaded
        let phase = self.current_phase();
        phase
            .require(EnginePhase::SnapshotLoaded, "backfill_from_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure no double-backfill
        phase
            .require_before(EnginePhase::Backfilled, "backfill_from_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure subscribe() was called — we need the first WS block
        let first_ws_block = {
            let state_lock = self.subscribe_state.lock();
            if let Some(s) = state_lock.as_ref() {
                s.first_block
            } else {
                let msg =
                    "Cannot backfill: subscribe() has not been called. Call subscribe() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        };

        if snapshot_block == 0 {
            log::warn!("backfill_from_snapshot: snapshot_block is 0, skipping");
            return Ok(0);
        }

        if snapshot_block >= first_ws_block {
            log::info!(
                "backfill_from_snapshot: snapshot at {snapshot_block} >= WS block {first_ws_block}, nothing to backfill"
            );
            return Ok(0);
        }

        let from_block = snapshot_block + 1;
        // Backfill up to (first_ws_block - 1) to avoid overlap with
        // WS events that the pump already captured during subscribe().
        let to_block = first_ws_block - 1;
        let total_blocks = to_block - from_block + 1;

        log::info!(
            "backfill_from_snapshot: fetching events from block {from_block} to {to_block} ({total_blocks} blocks, chunk_size={chunk_size})"
        );

        // Create an HTTP provider for eth_getLogs
        let runtime = degenbot_core::runtime::get_runtime();
        let provider = runtime.block_on(async {
            degenbot_rpc::provider::AlloyProvider::new(rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "Failed to create provider: {e}"
                    ))
                })
        })?;

        let provider_arc = provider.provider_arc();

        // Fetch and apply logs in paginated chunks
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);

            let filter =
                degenbot_bot::bot_core::block_pump::build_backfill_filter(chunk_start, chunk_end);

            let logs = runtime.block_on(async {
                provider_arc.get_logs(&filter).await.map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}"
                    ))
                })
            })?;

            let chunk_log_count = logs.len();
            total_logs += chunk_log_count;

            // Apply to the engines — process_backfill_logs splits V3/V4
            {
                let mut engine = self.engine.lock();
                engine.process_backfill_logs(&logs, chunk_end);
            }

            log::info!(
                "backfill_from_snapshot: blocks {chunk_start}-{chunk_end}: {chunk_log_count} logs applied"
            );

            chunk_start = chunk_end + 1;
        }

        log::info!(
            "backfill_from_snapshot: complete — {total_logs} total logs applied across {total_blocks} blocks"
        );

        // Capture verification blocks. These are used by verify_on_register
        // to check tick data at two boundaries:
        // 1. snapshot_block: raw DB tick_data (before buffer) vs on-chain
        // 2. backfill block: engine state (after buffer) vs on-chain
        *self.verify_snapshot_block.lock() = Some(snapshot_block);
        let backfill_block = self
            .engine
            .lock()
            .last_processed_block()
            .unwrap_or(to_block);
        *self.verify_backfill_block.lock() = Some(backfill_block);

        // Advance phase
        self.set_phase(EnginePhase::Backfilled);

        Ok(total_blocks)
    }

    /// Resume phase: begin normal pump processing.
    ///
    /// Must be called after `subscribe()`. Takes the WS stream from the
    /// subscribe phase and begins processing events on block boundaries.
    ///
    /// After calling `resume()`, the engine processes events autonomously.
    /// Python reads results via `latest_results()` and awaits new blocks
    /// via `wait_for_block()`.
    ///
    /// Raises `RuntimeError` if `subscribe()` has not been called first.
    fn resume(&self, _py: Python<'_>) -> PyResult<()> {
        // Phase check: must be at least SnapshotLoaded (can skip backfill)
        let phase = self.current_phase();
        phase
            .require(EnginePhase::SnapshotLoaded, "resume")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // No double-resume
        if phase == EnginePhase::Resumed {
            let msg = "Cannot resume: engine is already in Resumed phase.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let subscribe_state = self.subscribe_state.lock().take();
        let state = subscribe_state.ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot resume: subscribe() has not been called. Call subscribe() first.",
            )
        })?;

        let mut pump = state.pump;
        let first_block = state.first_block;
        let combined_stream = state.combined_stream;

        // ADR-006 slice 6 precondition: all engines are registered before the
        // pump's WS phase begins (precondition 1) and all engines' snapshot
        // backfill has completed to a consistent cursor (precondition 2).
        // `resume()` is the last gate before WS, so assert + seed the
        // coordinator's `last_drained_block` here. `start()` panics on cursor
        // divergence — a wiring bug. For the single-engine case today this is
        // trivially satisfied; for the multi-engine case the precondition is
        // documented and enforced.
        self.coordinator.start();

        // Spawn the resume task on the Tokio runtime
        let handle = degenbot_core::runtime::get_runtime().spawn(async move {
            let inner_state = degenbot_bot::bot_core::block_pump::SubscribeState {
                first_block,
                first_timestamp: 0,
                combined_stream: Some(combined_stream),
            };
            pump.resume_from_subscribe(inner_state).await;
        });

        *self.pump_handle.lock() = Some(handle);

        // Advance phase
        self.set_phase(EnginePhase::Resumed);

        Ok(())
    }
}

// --- V4 registration error mapping (free helper) ---
/// Map a [`RegisterV4PoolError`] to a typed Python exception (Plan 102).
///
/// - `HookedPool` → [`HookedPoolRejectedError`]
/// - `DynamicFee` → [`DynamicFeePoolRejectedError`]
/// - `AlreadyRegistered` → `PyValueError` (a wiring/programming error, not an
///   admission category)
///
/// The message text is byte-for-byte unchanged from the legacy `Err(String)`
/// formatting so `build_paths`'s classification (now `isinstance`, was
/// substring) matches the same diagnostics.
#[allow(clippy::needless_pass_by_value)]
fn map_register_v4_err(err: degenbot_bot::bot_core::RegisterV4PoolError) -> pyo3::PyErr {
    match err {
        degenbot_bot::bot_core::RegisterV4PoolError::HookedPool { hook_flags } => {
            HookedPoolRejectedError::new_err(format!(
                "V4 pool has amount-modifying hooks (flags=0x{hook_flags:04X}, mask=0x{:04X}) — excluded from arbitrage",
                degenbot_bot::bot_core::AMOUNT_MODIFYING_HOOK_MASK
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::DynamicFee { fee } => {
            DynamicFeePoolRejectedError::new_err(format!(
                "V4 pool has dynamic fee (fee=0x{fee:06X}) — excluded from arbitrage"
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::AlreadyRegistered {
            pool_manager,
            pool_id,
        } => pyo3::exceptions::PyValueError::new_err(format!(
            "V4 pool already registered: pool_manager={pool_manager}, pool_id=0x{}",
            alloy::hex::encode(pool_id),
        )),
    }
}
