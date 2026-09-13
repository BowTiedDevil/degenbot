//! Per-state-subject log dispatch (ADR-006 D4).
//!
//! `LogDispatcher` owns a decoder registry. `Bot` drives `dispatch(log)`:
//! decode the log, apply the decoded event to `BotState` under a **write**
//! guard, **release the guard**, then record the touched key into the epoch's
//! `EpochDelta` ledger (the sole dirty-tracking mechanism since LXDY4C).
//!
//! This module ships the dispatcher in isolation (ADR-006 slice 4). The pump
//! driving the dispatcher lands in slice 5.

#![expect(clippy::doc_markdown)]

use degenbot_core::diag;
use std::sync::Arc;

use alloy::rpc::types::Log;

use crate::bot_core::cl_route::ApplyOutcome;
use crate::bot_core::state_lock::StateLock;
use crate::bot_core::BotState;
use degenbot_decoders::v2_sync_decoder::decode_sync_log;
use degenbot_decoders::v3_mint_burn_decoder::{decode_v3_burn_log, decode_v3_mint_log};
use degenbot_decoders::v3_pancakeswap_swap_decoder::decode_v3_pancakeswap_swap_log;
use degenbot_decoders::v3_swap_decoder::decode_v3_swap_log;
use degenbot_decoders::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;
use degenbot_decoders::v4_swap_decoder::decode_v4_swap_log;

/// Whether `topic0` is one of the six degenbot pool-event signatures we
/// dispatch on (`crate::bot_core::RELEVANT_TOPICS`). A log carrying
/// a KNOWN signature that nonetheless fails every decoder is malformed event
/// data — a silent-drop class `dispatch` asserts on loudly rather than silently
/// skipping (the WS-decoder-drop failure mode).
fn is_known_pool_topic(topic0: Option<&alloy::primitives::B256>) -> bool {
    matches!(topic0, Some(t) if crate::bot_core::RELEVANT_TOPICS.contains(t))
}

/// A decoded pool-state event ready to apply to `BotState`.
///
/// One variant per pool family — the decoder selects the family; the apply
/// method matches on it. Future families (Curve/Aave) add variants + decoders
/// without `Bot` knowledge.
#[derive(Clone, Debug)]
pub enum DecodedPoolEvent {
    /// V2 `Sync` — `(address, reserve0, reserve1)`.
    V2Sync {
        pool_address: alloy::primitives::Address,
        reserve0: alloy::primitives::aliases::U112,
        reserve1: alloy::primitives::aliases::U112,
        block_number: u64,
    },
    /// V3 `Swap` — `(address, sqrt_price_x96, liquidity, tick)`.
    V3Swap {
        pool_address: alloy::primitives::Address,
        sqrt_price_x96: alloy::primitives::U256,
        liquidity: alloy::primitives::U256,
        tick: i32,
        block_number: u64,
    },
    /// V3 `Mint`/`Burn` — `(address, tick_lower, tick_upper, liquidity_delta)`.
    V3Liquidity {
        pool_address: alloy::primitives::Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    },
    /// V4 `Swap` — `(pool_manager, pool_id, sqrt_price_x96, liquidity, tick)`.
    V4Swap {
        pool_manager: alloy::primitives::Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        sqrt_price_x96: alloy::primitives::U256,
        liquidity: alloy::primitives::U256,
        tick: i32,
        block_number: u64,
    },
    /// V4 `ModifyLiquidity` — `(pool_manager, pool_id, tick_lower, tick_upper, delta)`.
    V4Liquidity {
        pool_manager: alloy::primitives::Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    },
}

impl DecodedPoolEvent {
    /// The hop family this event belongs to — the `pool_to_paths` reverse
    /// index's hop half. The decoder selects the family, so LOG APPLICATION
    /// knows it directly: the retired `EngineSubscriber` classification
    /// (BotState bucket lookups per notify) is subsumed by this method
    /// (epic MROOY7, task LXDY4C).
    #[must_use]
    pub fn hop_type(&self) -> degenbot_solvers::mixed::HopType {
        match self {
            Self::V2Sync { .. } => degenbot_solvers::mixed::HopType::V2,
            Self::V3Swap { .. } | Self::V3Liquidity { .. } => degenbot_solvers::mixed::HopType::V3,
            Self::V4Swap { .. } | Self::V4Liquidity { .. } => degenbot_solvers::mixed::HopType::V4,
        }
    }

    /// The block number carried by this event's source log.
    #[must_use]
    pub fn block_number(&self) -> u64 {
        match self {
            Self::V2Sync { block_number, .. }
            | Self::V3Swap { block_number, .. }
            | Self::V3Liquidity { block_number, .. }
            | Self::V4Swap { block_number, .. }
            | Self::V4Liquidity { block_number, .. } => *block_number,
        }
    }

    /// Resolve the `pool_id` this event targets WITHOUT applying it forward.
    /// `ReorgCoordinator` uses this to identify which pool a `removed: true`
    /// log is about (the removed event's content is unused — only its block
    /// + pool identity matter; the journal's stored "before" values are truth).
    #[must_use]
    pub fn resolve_pool_id(&self, bot_state: &BotState) -> Option<u64> {
        match self {
            Self::V2Sync { pool_address, .. }
            | Self::V3Swap { pool_address, .. }
            | Self::V3Liquidity { pool_address, .. } => bot_state.pool_id_by_address(pool_address),
            Self::V4Swap {
                pool_manager,
                pool_id,
                ..
            }
            | Self::V4Liquidity {
                pool_manager,
                pool_id,
                ..
            } => bot_state.v4_pool_id_by_key(*pool_manager, pool_id),
        }
    }

    /// 42FL35: displayable pool identity WITHOUT resolving against BotState -
    /// used by the APPLY MISS trace so a failed lookup still names WHICH pool
    /// was missed (the whole point of that trace). V4 renders the
    /// `(pool_manager, pool_id)` key; V2/V3 render the pool address.
    #[must_use]
    pub fn display_identity(&self) -> String {
        match self {
            Self::V2Sync { pool_address, .. }
            | Self::V3Swap { pool_address, .. }
            | Self::V3Liquidity { pool_address, .. } => format!("{pool_address:x}"),
            Self::V4Swap {
                pool_manager,
                pool_id,
                ..
            }
            | Self::V4Liquidity {
                pool_manager,
                pool_id,
                ..
            } => {
                let id_hex = degenbot_core::hex_utils::encode_hex(pool_id);
                format!("{pool_manager:x}/{id_hex}")
            }
        }
    }

    /// Apply this event to `bot_state`, returning the affected `pool_id` (or
    /// `None` if the pool isn't registered / the event is a no-op).
    #[expect(
        clippy::too_many_lines,
        reason = "five family arms plus the atomic-telemetry prelude"
    )]
    fn apply(self, bot_state: &mut BotState) -> ApplyOutcome {
        // Family-cost telemetry (2SDIQW): self tropical atomic split of the
        // apply wall per family, surfaced on the block-end event.
        let family = match self {
            Self::V2Sync { .. } => crate::bot_core::apply_telemetry::ApplyFamily::V2Sync,
            Self::V3Swap { .. } => crate::bot_core::apply_telemetry::ApplyFamily::V3Swap,
            Self::V3Liquidity { .. } => crate::bot_core::apply_telemetry::ApplyFamily::V3Liquidity,
            Self::V4Swap { .. } => crate::bot_core::apply_telemetry::ApplyFamily::V4Swap,
            Self::V4Liquidity { .. } => crate::bot_core::apply_telemetry::ApplyFamily::V4Liquidity,
        };
        let at0 = std::time::Instant::now();
        let out = match self {
            Self::V2Sync {
                pool_address,
                reserve0,
                reserve1,
                block_number,
            } => match bot_state.apply_v2_sync(pool_address, reserve0, reserve1, block_number) {
                Some(pool_id) => ApplyOutcome::Applied(pool_id),
                None => ApplyOutcome::NoOp(
                    crate::bot_core::cl_route::NoOpReason::ScalarReseedAtRegistration,
                ),
            },
            Self::V3Swap {
                pool_address,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
            } => bot_state.route_v3_event(
                crate::bot_core::cl_route::Phase::Live,
                pool_address,
                crate::bot_core::BufferedV3PoolEvent::Swap(
                    degenbot_pools::v3_state::BufferedV3SwapEvent {
                        sqrt_price_x96,
                        liquidity: liquidity.to::<u128>(),
                        tick,
                        block_number,
                    },
                ),
                &[],
            ),
            Self::V3Liquidity {
                pool_address,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            } => bot_state.route_v3_event(
                crate::bot_core::cl_route::Phase::Live,
                pool_address,
                crate::bot_core::BufferedV3PoolEvent::Liquidity(
                    degenbot_pools::v3_state::BufferedV3LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    },
                ),
                &[],
            ),
            Self::V4Swap {
                pool_manager,
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
            } => bot_state.route_v4_event(
                crate::bot_core::cl_route::Phase::Live,
                pool_manager,
                pool_id,
                crate::bot_core::BufferedV4PoolEvent::Swap(
                    degenbot_pools::v4_state::BufferedV4SwapEvent {
                        sqrt_price_x96,
                        liquidity: liquidity.to::<u128>(),
                        tick,
                        block_number,
                    },
                ),
                &[],
            ),
            Self::V4Liquidity {
                pool_manager,
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            } => bot_state.route_v4_event(
                crate::bot_core::cl_route::Phase::Live,
                pool_manager,
                pool_id,
                crate::bot_core::BufferedV4PoolEvent::Liquidity(
                    degenbot_pools::v4_state::BufferedV4LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    },
                ),
                &[],
            ),
        };
        crate::bot_core::apply_telemetry::record(
            family,
            u64::try_from(at0.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        out
    }
}

/// A decoder that recognizes one family of pool-state logs (ADR-006 D4).
///
/// The strategy-extension seam: a future Curve/Aave decoder registers here
/// without `Bot` knowing its event shapes.
pub trait LogDecoder: Send + Sync {
    /// Decode `log` into a [`DecodedPoolEvent`], or `None` if unrecognized.
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent>;
}

/// Decode V2 `Sync` events. (No state — the topic check lives in the free fn.)
struct V2SyncDecoder;
impl LogDecoder for V2SyncDecoder {
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
        let ev = decode_sync_log(log)?;
        Some(DecodedPoolEvent::V2Sync {
            pool_address: ev.pool_address,
            reserve0: ev.reserve0,
            reserve1: ev.reserve1,
            block_number: log.block_number.unwrap_or_default(),
        })
    }
}

/// Decode V3 `Swap` events.
struct V3SwapDecoder;
impl LogDecoder for V3SwapDecoder {
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
        let ev = decode_v3_swap_log(log)?;
        Some(DecodedPoolEvent::V3Swap {
            pool_address: ev.pool_address,
            sqrt_price_x96: ev.sqrt_price_x96,
            liquidity: alloy::primitives::U256::from(ev.liquidity.to::<u128>()),
            tick: ev.tick,
            block_number: log.block_number.unwrap_or_default(),
        })
    }
}

/// Decode PancakeSwap V3 `Swap` events (a forked V3 Swap with a non-canonical
/// topic0 — see `v3_pancakeswap_swap_decoder`). Same `DecodedPoolEvent::V3Swap`
/// shape as the canonical V3 decoder; only `topic0` and the trailing words
/// differ, so the decoded state feeds `apply_v3_swap` unchanged.
struct V3PancakeSwapDecoder;
impl LogDecoder for V3PancakeSwapDecoder {
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
        let ev = decode_v3_pancakeswap_swap_log(log)?;
        Some(DecodedPoolEvent::V3Swap {
            pool_address: ev.pool_address,
            sqrt_price_x96: ev.sqrt_price_x96,
            liquidity: alloy::primitives::U256::from(ev.liquidity.to::<u128>()),
            tick: ev.tick,
            block_number: log.block_number.unwrap_or_default(),
        })
    }
}

/// Decode V3 `Mint`/`Burn` events (both produce a liquidity delta on a tick range).
struct V3MintBurnDecoder;
impl LogDecoder for V3MintBurnDecoder {
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
        if let Some(mint) = decode_v3_mint_log(log) {
            return Some(DecodedPoolEvent::V3Liquidity {
                pool_address: mint.pool_address,
                tick_lower: mint.tick_lower,
                tick_upper: mint.tick_upper,
                liquidity_delta: i128::try_from(mint.amount).ok()?,
                block_number: log.block_number.unwrap_or_default(),
            });
        }
        if let Some(burn) = decode_v3_burn_log(log) {
            return Some(DecodedPoolEvent::V3Liquidity {
                pool_address: burn.pool_address,
                tick_lower: burn.tick_lower,
                tick_upper: burn.tick_upper,
                // Burn removes liquidity → negative delta.
                liquidity_delta: -i128::try_from(burn.amount).ok()?,
                block_number: log.block_number.unwrap_or_default(),
            });
        }
        None
    }
}

/// Decode V4 `Swap` events.
struct V4SwapDecoder;
impl LogDecoder for V4SwapDecoder {
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
        let ev = decode_v4_swap_log(log)?;
        Some(DecodedPoolEvent::V4Swap {
            pool_manager: log.address(),
            pool_id: ev.pool_id,
            sqrt_price_x96: ev.sqrt_price_x96,
            liquidity: alloy::primitives::U256::from(ev.liquidity.to::<u128>()),
            tick: ev.tick,
            block_number: log.block_number.unwrap_or_default(),
        })
    }
}

/// Decode V4 `ModifyLiquidity` events.
struct V4ModifyLiquidityDecoder;
impl LogDecoder for V4ModifyLiquidityDecoder {
    fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
        let ev = decode_v4_modify_liquidity_log(log)?;
        Some(DecodedPoolEvent::V4Liquidity {
            pool_manager: log.address(),
            pool_id: ev.pool_id,
            tick_lower: ev.tick_lower,
            tick_upper: ev.tick_upper,
            liquidity_delta: ev.liquidity_delta,
            block_number: log.block_number.unwrap_or_default(),
        })
    }
}

/// The per-`Bot` event bus: the decoder registry.
///
/// `Bot` owns one and mediates the registry (cleaner than per-`PoolEntry`
/// callback vecs in Rust). `dispatch` is the single entry point the pump
/// (slice 5) calls per WS log.
///
/// Decoders are frozen after construction (read-only `&self` access); `Bot`
/// is shared across threads.
pub struct LogDispatcher {
    decoders: Vec<Box<dyn LogDecoder>>,
    /// KAHU5W: strict decode-miss hard-fault gate. Historically the
    /// presence-gated `DEGENBOT_WS_COMPLETENESS` env var; now the typed
    /// `pump.ws_completeness` schema default AND'ed with the owning pump's
    /// per-pump opt-out (tests set the field OFF deterministically, keeping
    /// the synthetic-fixture streams decode-miss-neutral).
    strict_decode_fault: std::sync::atomic::AtomicBool,
    /// NO4DIW: per-epoch log tally — the dispatcher outcomes sampled and
    /// reset by the pump at each header epilogue and projected to the
    /// `degenbot.epoch.logs_*` funnel gauges. Single-writer: the pump task.
    tally: EpochLogTally,
}

/// NO4DIW: the per-epoch leg of the log funnel. Order of magnitude of each
/// leg (seen \u2265 received \u2265 [applied + ignored]); the pump samples and
/// resets this at each accepted header, so a snapshot covers exactly the
/// epoch that just closed.
#[derive(Debug, Default)]
pub struct EpochLogTally {
    seen: std::sync::atomic::AtomicU64,
    received: std::sync::atomic::AtomicU64,
    decoded: std::sync::atomic::AtomicU64,
    undecoded: std::sync::atomic::AtomicU64,
    apply_missed: std::sync::atomic::AtomicU64,
    applied: std::sync::atomic::AtomicU64,
}

/// One epoch's ledger snapshot (see [`EpochLogTally`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EpochLogCounts {
    pub seen: u64,
    pub received: u64,
    pub decoded: u64,
    pub undecoded: u64,
    pub apply_missed: u64,
    pub applied: u64,
}

impl EpochLogTally {
    fn inc(field: &std::sync::atomic::AtomicU64) {
        field.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Swap the ledger out (one-shot reset) — the pump's header epilogue.
    #[must_use]
    fn snapshot_and_reset(&self) -> EpochLogCounts {
        use std::sync::atomic::Ordering::Relaxed;
        EpochLogCounts {
            seen: self.seen.swap(0, Relaxed),
            received: self.received.swap(0, Relaxed),
            decoded: self.decoded.swap(0, Relaxed),
            undecoded: self.undecoded.swap(0, Relaxed),
            apply_missed: self.apply_missed.swap(0, Relaxed),
            applied: self.applied.swap(0, Relaxed),
        }
    }
}

impl LogDispatcher {
    /// Construct an empty dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoders: Vec::new(),
            strict_decode_fault: std::sync::atomic::AtomicBool::new(
                crate::bot_core::stance::config().pump.ws_completeness,
            ),
            tally: EpochLogTally::default(),
        }
    }

    /// NO4DIW: the pump's header epilogue — sample and reset the per-epoch
    /// log ledger for projection onto the `degenbot.epoch.logs_*` gauges.
    #[must_use]
    pub fn snapshot_epoch_logs_and_reset(&self) -> EpochLogCounts {
        self.tally.snapshot_and_reset()
    }

    /// NO4DIW: one WS-delivered log event (pre topic-filter). The pump calls
    /// this in the `WsEvent::Pool` arm next to `count_ws_log_seen` — the
    /// dispatcher never sees the logs the pre-filter drops, so `seen` must
    /// be tallied here for the funnel composition to close.
    pub fn inc_seen(&self) {
        EpochLogTally::inc(&self.tally.seen);
    }

    /// Per-pump opt-out for the strict decode-miss fault (the test pumps
    /// disable the completeness machinery deterministically; production
    /// keeps the schema default ON).
    pub fn set_strict_decode_fault(&self, on: bool) {
        self.strict_decode_fault
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Construct a dispatcher pre-loaded with the 6 Uniswap (V2/V3/V4)
    /// decoders. This is the constructor `Bot` uses; strategy decoders add
    /// themselves afterwards via [`register_decoder`](Self::register_decoder).
    #[must_use]
    pub fn with_uniswap_decoders() -> Self {
        let mut d = Self::new();
        d.register_decoder(Box::new(V2SyncDecoder));
        d.register_decoder(Box::new(V3SwapDecoder));
        d.register_decoder(Box::new(V3PancakeSwapDecoder));
        d.register_decoder(Box::new(V3MintBurnDecoder));
        d.register_decoder(Box::new(V4SwapDecoder));
        d.register_decoder(Box::new(V4ModifyLiquidityDecoder));
        d
    }

    /// Register a decoder. Decoders are tried in registration order.
    pub fn register_decoder(&mut self, decoder: Box<dyn LogDecoder>) {
        self.decoders.push(decoder);
    }

    /// Decode `log`, apply it to `state`, release the write guard, then record
    /// the touched pool into the epoch `EpochDelta`. No-op if no decoder
    /// recognizes the log or the pool isn't registered.
    ///
    /// # Panics
    ///
    /// Panics (in strict `DEGENBOT_WS_COMPLETENESS` mode) if a log carrying a
    /// KNOWN pool-event topic0 fails every decoder — malformed event data that
    /// must fail loudly rather than be silently dropped.
    #[tracing::instrument(name = "degenbot.log.dispatch", skip(self, log, state, delta), fields(block = %log.block_number.unwrap_or_default()))]
    #[expect(clippy::too_many_lines)]
    pub fn dispatch(
        &self,
        log: &Log,
        state: &Arc<StateLock<BotState>>,
        delta: Option<&crate::bot_core::EpochDelta>,
    ) {
        // Phase-labeled `measure_block!` for the rolling-start dirty-path
        // diagnostic: distinguishes "decode miss" (no decoder recognized the
        // log) from "apply miss" (pool not registered in BotState → no-op).
        // Each appears as its own row in the hotpath functions-timing table
        // with a per-phase call count — zero-cost no-ops when the `hotpath`
        // feature is off. See `src/profiling.rs`.
        // T2: relevant-topic log entering the dispatcher.
        EpochLogTally::inc(&self.tally.received);
        if let Some(p) = crate::instruments::pipeline() {
            p.count_log_received();
        }
        // Telemetry: raw-event arrival (field exprs evaluate lazily — zero
        // cost unless RUST_LOG enables debug for this target). Attaches to the
        // per-log dispatch span, which parents under `degenbot.epoch` (the
        // per-epoch root, BF43PM).
        diag!(domain = ingest, block = log.block_number,
            address = format!("{:#x}", log.address()),
            topic0 = format!("{:#x}", log.topics().first().copied().unwrap_or_default()),
            tx = ?log.transaction_hash,
            "log received"
        );
        let decode_start = std::time::Instant::now();
        let decoded = hotpath::measure_block!("dispatch.decode", {
            self.decoders.iter().find_map(|d| d.try_decode(log))
        });
        if let Some(p) = crate::instruments::pipeline() {
            p.observe_log_decode(decode_start.elapsed().as_secs_f64());
        }
        if decoded.is_some() {
            EpochLogTally::inc(&self.tally.decoded);
            if let Some(p) = crate::instruments::pipeline() {
                p.count_log_decoded();
            }
        } else if let Some(p) = crate::instruments::pipeline() {
            EpochLogTally::inc(&self.tally.undecoded);
            p.count_log_undecoded();
        }
        let Some(decoded) = decoded else {
            // [trace-dispatch] (the dispatch trace): a relevant-topic log that
            // NO decoder recognized. Distinct from "apply miss". Zero-cost unless
            // the env is set. (The WS_COMPLETENESS assert below only fires in strict
            // loud mode; this surfaces the same miss in dry runs.)
            // Telemetry: always-on DEBUG (was always-on
            // WARN — a decode miss on a pre-filtered relevant-topic log is
            // abnormal enough to keep visible whenever debug is enabled, and
            // the strict-mode assert below remains the loud gate).
            diag!(
                domain = ingest,
                block = log.block_number,
                topic0 = format!("{:#x}", log.topics().first().copied().unwrap_or_default()),
                address = format!("{:#x}", log.address()),
                "DECODE MISS — relevant-topic log matched no decoder"
            );
            // LOUD failure: a log carrying a KNOWN degenbot event signature
            // (topic0 in RELEVANT_TOPICS) failed every decoder. The forward
            // path only dispatches logs that already passed the relevant-topic
            // pre-filter, so a decode miss means malformed event data — a
            // silently-dropped event is exactly the class that stalls the
            // engine. Panic loudly rather than silently skip. Gated on
            // `DEGENBOT_WS_COMPLETENESS` (the strict loud mode) so synthetic
            // test fixtures that use a relevant-topic log purely as a block
            // tombstone (not for state) don't trip it; production enables it
            // explicitly by setting the env var. Deliberately opt-in (presence-
            // gated), distinct from the block-pump completeness cross-check
            // which is default-ON: surfacing a decode miss is a malformed-data
            // hard-fault that should be switched on deliberately, not silently
            // defaulted, and it has no per-pump opt-out here to keep the
            // synthetic-fixture tests deterministic.
            assert!(
                !self
                    .strict_decode_fault
                    .load(std::sync::atomic::Ordering::Relaxed)
                    || !is_known_pool_topic(log.topics().first()),
                "dispatch: relevant-topic log failed to decode (malformed event?): \
                 block={block:?} tx={tx:?} log_index={idx:?} topic0={t0:x?} address={addr}",
                block = log.block_number,
                tx = log.transaction_hash,
                idx = log.log_index,
                t0 = log.topics().first(),
                addr = log.address(),
            );
            return;
        };
        // 42FL35: capture displayable identity BEFORE apply consumes the event
        // - the APPLY MISS trace below needs to name WHICH pool was missed.
        let identity = decoded.display_identity();
        // Verdict-only fast path (cl_route contract): a decoded event whose
        // routing-table row is a confirmed Drop may skip the exclusive lock —
        // absence of work IS the semantics (the router would return NoOp
        // without mutating anything). Everything else falls through to the
        // write-lock router: registered pools (any lifecycle), and
        // unregistered TICK-MUTATION events which must stage into a buffer
        // (FUWYUR: these were silently lost when this pre-check used to
        // decide their fate itself).
        let confirmed_drop = decoded
            .resolve_pool_id(&state.read_at(crate::bot_core::state_lock::LockSite::Core))
            .is_none()
            && matches!(
                decoded,
                DecodedPoolEvent::V2Sync { .. }
                    | DecodedPoolEvent::V3Swap { .. }
                    | DecodedPoolEvent::V4Swap { .. }
            );
        if confirmed_drop {
            diag!(domain = ingest, block = log.block_number,
                pool = %identity,
                "APPLY MISS - unregistered scalar refresh (row re-seed trust); skipped write lock"
            );
            EpochLogTally::inc(&self.tally.apply_missed);
            if let Some(p) = crate::instruments::pipeline() {
                p.count_log_apply_missed();
            }
            return;
        }
        // Route + execute under the write guard (cl_route table owns policy),
        // then RELEASE before notifying.
        // LXDY4C: the event's hop family must be read BEFORE `apply`
        // consumes the decoded event.
        let event_hop = decoded.hop_type();
        // 7S4QAG: the ledger buckets by block, so the apply site carries the
        // decoded event's block into the record (read before `apply` consumes it).
        let event_block = decoded.block_number();
        let apply_start = std::time::Instant::now();
        let outcome = hotpath::measure_block!(
            "dispatch.apply",
            decoded.apply(&mut state.write_at(crate::bot_core::state_lock::LockSite::Core))
        );
        if let Some(p) = crate::instruments::pipeline() {
            p.observe_state_apply(apply_start.elapsed().as_secs_f64());
        }
        match outcome {
            ApplyOutcome::Applied(pool_id) => {
                // Structured outcome event: the "pool event processed +
                // state updated" node in every Jaeger trace, carrying concrete
                // pool identity AND engine pool_id. DEMOTED to the state DEBUG
                // stream (ADR-043 §2: per-entity cardinality).
                diag!(domain = state, block = log.block_number,
                    log.index = ?log.log_index,
                    tx.index = ?log.transaction_index,
                    pool.id = pool_id,
                    pool = %identity,
                    "pool event applied"
                );
                // T2: successful apply to a registered pool.
                EpochLogTally::inc(&self.tally.applied);
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_log_applied();
                }
                // EpochDelta dirty tracking (epic MROOY7, task LXDY4C): log
                // application records the touched pool into the block's
                // ledger as a BYPRODUCT of the apply outcome — the
                // subscriber-side dirty write is retired. Event family comes
                // from the decode (no BotState classification).
                if let Some(delta) = delta {
                    delta.record_affected(event_hop, pool_id, event_block);
                }
            }
            ApplyOutcome::Buffered(kind) => {
                diag!(domain = ingest, block = log.block_number,
                    pool = %identity,
                    ?kind,
                    "APPLY MISS - staged into buffer for registration drain/set_live"
                );
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_log_apply_missed();
                }
            }
            ApplyOutcome::NoOp(_) => {
                diag!(domain = ingest, block = log.block_number,
                    pool = %identity,
                    "APPLY MISS - no-op"
                );
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_log_apply_missed();
                }
            }
        }
    }

    /// Decode `log` into a [`DecodedPoolEvent`] via the decoder registry, or
    /// `None` if no decoder recognizes it. Used by `dispatch` (forward) and
    /// `ReorgCoordinator::dispatch_reorg_log` (removed) — both decode the pool
    /// identity; only the forward path `apply`s.
    pub fn try_decode_log(&self, log: &Log) -> Option<DecodedPoolEvent> {
        self.decoders.iter().find_map(|d| d.try_decode(log))
    }
}

impl Default for LogDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// A fake decoder that recognizes a sentinel topic and emits a
    /// `DecodedPoolEvent::V2Sync` for a fixed address — enough to drive the
    /// dispatch ordering assertions without the full V2 ABI.
    struct FakeDecoder {
        pool_address: alloy::primitives::Address,
    }

    impl LogDecoder for FakeDecoder {
        fn try_decode(&self, log: &Log) -> Option<DecodedPoolEvent> {
            // Match a synthetic single-topic log whose topic0 is the sentinel.
            if log.topics().first().map(|t| t.0) == Some(SENTRY_TOPIC) {
                Some(DecodedPoolEvent::V2Sync {
                    pool_address: self.pool_address,
                    reserve0: alloy::primitives::aliases::U112::from(1),
                    reserve1: alloy::primitives::aliases::U112::from(2),
                    block_number: 0,
                })
            } else {
                None
            }
        }
    }

    /// Sentinel topic (arbitrary nonzero value).
    const SENTRY_TOPIC: [u8; 32] = [0xff; 32];

    fn sentinel_log() -> Log {
        use alloy::primitives::B256;
        let inner = alloy::primitives::Log::new_unchecked(
            alloy::primitives::Address::ZERO,
            vec![B256::from(SENTRY_TOPIC)],
            alloy::primitives::Bytes::default(),
        );
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    /// RED: the apply-miss funnel pre-check (a read-side `resolve_pool_id`)
    /// must agree with what `apply` would do under the write lock. If they
    /// diverge, the funnel could skip a real apply or pessimistically take the
    /// write lock for a guaranteed no-op. Guard the invariant: `resolve == Some`
    /// exactly when `apply != None`.
    #[test]
    fn apply_miss_resolve_matches_apply_registration_gate() {
        let registered = alloy::primitives::Address::from([0x33u8; 20]);
        let unregistered = alloy::primitives::Address::from([0x44u8; 20]);
        let state = Arc::new(StateLock::new(BotState::new()));
        state
            .write_at(crate::bot_core::state_lock::LockSite::Core)
            .register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
                address: registered,
                token0: alloy::primitives::Address::ZERO,
                token1: alloy::primitives::Address::ZERO,
                reserve0: alloy::primitives::aliases::U112::from(1000),
                reserve1: alloy::primitives::aliases::U112::from(2000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: alloy::primitives::Address::ZERO,
                update_block: 0,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");

        let mk = |addr, block| DecodedPoolEvent::V2Sync {
            pool_address: addr,
            reserve0: alloy::primitives::aliases::U112::from(1),
            reserve1: alloy::primitives::aliases::U112::from(2),
            block_number: block,
        };

        // Registered pool: read-side resolve finds it AND apply mutates.
        let ev = mk(registered, 1);
        assert_eq!(
            ev.resolve_pool_id(&state.read_at(crate::bot_core::state_lock::LockSite::Core)),
            Some(1),
            "resolve must find the registered pool"
        );
        assert_eq!(
            ev.apply(&mut state.write_at(crate::bot_core::state_lock::LockSite::Core)),
            ApplyOutcome::Applied(1),
            "apply must apply the registered pool"
        );

        // Unregistered pool: resolve says miss AND apply is a named NoOp - so
        // the verdict-only fast path may skip the write lock without losing a
        // real apply.
        let miss = mk(unregistered, 1);
        assert_eq!(
            miss.resolve_pool_id(&state.read_at(crate::bot_core::state_lock::LockSite::Core)),
            None,
            "resolve must miss the unregistered pool"
        );
        // The funnel fast path may skip the write lock ONLY on this exact
        // confirmed-Drop row (unregistered + scalar refresh): absence of work
        // is the semantics. Tick mutations must never land here.
        assert_eq!(
            miss.apply(&mut state.write_at(crate::bot_core::state_lock::LockSite::Core)),
            ApplyOutcome::NoOp(crate::bot_core::cl_route::NoOpReason::ScalarReseedAtRegistration),
            "apply must no-op (named Drop) for the unregistered scalar refresh"
        );
    }

    /// RED: dispatching a log that decodes but targets an unregistered pool is
    /// a silent apply-miss through the funnel early-return - no subscriber
    /// notify, no state mutation.
    /// NO4DIW: the per-epoch log tally sums the dispatcher's outcomes and
    /// snapshot_and_reset is one-shot (a second snapshot returns zeros).
    #[test]
    fn epoch_tally_sums_and_resets() {
        let applied_addr = alloy::primitives::Address::from([0x55u8; 20]);
        let state = Arc::new(StateLock::new(BotState::new()));

        let mut dispatcher = LogDispatcher::new();
        dispatcher.register_decoder(Box::new(FakeDecoder {
            pool_address: applied_addr,
        }));
        dispatcher.dispatch(&sentinel_log(), &state, None); // apply miss (pool unregistered)

        state
            .write_at(crate::bot_core::state_lock::LockSite::Core)
            .register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
                address: applied_addr,
                token0: alloy::primitives::Address::ZERO,
                token1: alloy::primitives::Address::ZERO,
                reserve0: alloy::primitives::aliases::U112::from(1000),
                reserve1: alloy::primitives::aliases::U112::from(2000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: alloy::primitives::Address::ZERO,
                update_block: 0,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        dispatcher.dispatch(&sentinel_log(), &state, None); // applied

        let c = dispatcher.snapshot_epoch_logs_and_reset();
        assert_eq!(
            (
                c.received,
                c.decoded,
                c.applied,
                c.apply_missed,
                c.undecoded
            ),
            (2, 2, 1, 1, 0),
            "two dispatched logs: 1 applied + 1 apply-missed, all relevant decoded"
        );
        let c2 = dispatcher.snapshot_epoch_logs_and_reset();
        assert_eq!(c2.received, 0, "one-shot reset");
    }

    /// NO4DIW: the `seen` leg is tallied at the WS event source by the pump
    /// (`inc_seen`), NOT by `dispatch` — the pre-filter drops most events
    /// before the dispatcher, so `seen` is independent of `received`.
    #[test]
    fn seen_leg_tallies_at_the_source() {
        let dispatcher = LogDispatcher::new();
        dispatcher.inc_seen();
        dispatcher.inc_seen();
        dispatcher.inc_seen();
        let c = dispatcher.snapshot_epoch_logs_and_reset();
        assert_eq!(
            (c.seen, c.received, c.applied),
            (3, 0, 0),
            "seen counts WS events; dispatch outcomes are orthogonal"
        );
    }

    #[test]
    fn dispatch_unregistered_pool_is_apply_miss_no_delta() {
        let pool_address = alloy::primitives::Address::from([0x55u8; 20]);
        let state = Arc::new(StateLock::new(BotState::new()));
        // NOTE: the pool is intentionally NOT registered.
        let mut dispatcher = LogDispatcher::new();
        dispatcher.register_decoder(Box::new(FakeDecoder { pool_address }));

        let delta = crate::bot_core::EpochDelta::new(0u64);
        dispatcher.dispatch(&sentinel_log(), &state, Some(&delta));

        assert!(
            delta.is_empty(),
            "apply-miss must not record any touched pool into the EpochDelta"
        );
    }

    /// RED-CAPABLE LOOP (V3 tick-map desync, perm logs): a real Mint event for
    /// pool 0x88e6A0c2 (USDC/ETH 0.05%) at block 25390812 touches tick
    /// tickLower=201020 (tickUpper=201814) with amount `24_703_323_223_522`.
    /// On-chain, tick 201020's liquidityGross goes `689_141_000_492_849` →
    /// `713_844_323_716_371` (exact: seed + amount). In production the engine
    /// stays FROZEN at the seed across verify at blocks 25390812/38/85 while
    /// the journal grows (3→4→12) — i.e. events land on the pool but not on
    /// tick 201020. This test applies the exact Mint log through the real
    /// `dispatch` path (`with_uniswap_decoders` → `V3MintBurnDecoder`) and
    /// asserts tick 201020 == seed + amount. Red => the Mint is mis-routed/
    /// dropped at the decode+apply seam.
    /// FUWYUR (dispatcher level): dispatching a Mint whose pool is NOT yet
    /// registered must route the event into the V3 pump buffer (staged
    /// application at the later registration's drain+pin seam), NOT silently
    /// drop it at the APPLY-MISS funnel. Liquidity events mutate `tick_data`,
    /// which a DB-row snapshot cannot retro-supply when registration lands
    /// AFTER these logs landed on the wire.
    #[test]
    fn fuwyur_unregistered_mint_is_buffered_not_dropped() {
        use alloy::primitives::{Address, B256};
        use std::str::FromStr;

        const MINT_AMOUNT: u128 = 118_748_558_607_688;

        let pool_addr = Address::from([0x66u8; 20]);
        let state = Arc::new(StateLock::new(BotState::new()));
        // Intentionally NO registration — crawl mid-flight.
        let dispatcher = LogDispatcher::with_uniswap_decoders();

        let mint_topic =
            B256::from_str("0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde")
                .unwrap();
        let owner_topic =
            B256::from_str("0x000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88")
                .unwrap();
        let tick_lower_topic =
            B256::from_str("0x000000000000000000000000000000000000000000000000000000000003113c")
                .unwrap();
        let tick_upper_topic =
            B256::from_str("0x0000000000000000000000000000000000000000000000000000000000031a56")
                .unwrap();

        // data = abi.encode(sender, uint128 amount, uint256 amount0, uint256 amount1).
        let mut data: Vec<u8> = Vec::with_capacity(128);
        data.extend_from_slice(&[0u8; 12]); // sender left-pad
        data.extend_from_slice(&[
            0xc3u8, 0x64, 0x42, 0xb4, 0xa4, 0x52, 0x2e, 0x87, 0x13, 0x99, 0xcd, 0x71, 0x7a, 0xbd,
            0xd8, 0x47, 0xab, 0x11, 0xfe, 0x88,
        ]);
        data.extend_from_slice(&[0u8; 16]); // amount word high half
        data.extend_from_slice(&MINT_AMOUNT.to_be_bytes());
        data.extend_from_slice(&[0u8; 32]); // amount0
        data.extend_from_slice(&[0u8; 32]); // amount1
        let inner = alloy::primitives::Log::new_unchecked(
            pool_addr,
            vec![mint_topic, owner_topic, tick_lower_topic, tick_upper_topic],
            alloy::primitives::Bytes::from(data),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: Some(10),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        dispatcher.dispatch(&log, &state, None);

        assert!(
            state
                .read_at(crate::bot_core::state_lock::LockSite::Core)
                .buffered_v3_event_count(&pool_addr)
                > 0,
            "FUWYUR: an unregistered pool's Mint must be buffered for staged \
             application at registration — the APPLY-MISS funnel must not drop it"
        );
    }

    #[test]
    #[expect(clippy::too_many_lines)]
    fn v3_mint_log_lands_on_decoded_tick_lower() {
        use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, TickInfo};
        use alloy::primitives::{Address, B256, U256};
        use std::str::FromStr;

        const SEED_201020: u128 = 689_141_000_492_849;
        const SEED_203350: u128 = 1_000_000_000_000;
        const MINT_AMOUNT: u128 = 24_703_323_223_522;

        let pool_addr: Address = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"
            .parse()
            .unwrap();
        let block = 25_390_812_u64;

        let mut tick_data = hashbrown::HashMap::new();
        tick_data.insert(
            201_020,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(SEED_201020),
                liquidity_net: 0,
                block: 0,
            },
        );
        tick_data.insert(
            203_350,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(SEED_203350),
                liquidity_net: 0,
                block: 0,
            },
        );

        let state = Arc::new(StateLock::new(BotState::new()));
        let pool_id = state
            .write_at(crate::bot_core::state_lock::LockSite::Core)
            .register_v3_pool(&RegisterV3PoolParams {
                address: pool_addr,
                token0: Address::ZERO,
                token1: Address::ZERO,
                fee: 500,
                tick_spacing: 10,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000_000,
                tick: 201_020,
                tick_data,
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration");
        // DFQYM5: Tracked pools register `Quarantined`; transition to `Live`
        // (the driver's post-verify `set_live`) so the dispatched Mint
        // direct-applies as this test models.
        state
            .write_at(crate::bot_core::state_lock::LockSite::Core)
            .set_v3_pool_live(pool_addr);

        // The exact Mint log emitted at block 25390812 (decoded from cast).
        // topics[1]=owner, topics[2]=tickLower=0x03113c=201020,
        // topics[3]=tickUpper=0x031a56=201814.
        let mint_topic =
            B256::from_str("0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde")
                .unwrap();
        let owner_topic =
            B256::from_str("0x000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88")
                .unwrap();
        let tick_lower_topic =
            B256::from_str("0x000000000000000000000000000000000000000000000000000000000003113c")
                .unwrap();
        let tick_upper_topic =
            B256::from_str("0x0000000000000000000000000000000000000000000000000000000000031a56")
                .unwrap();

        // data = abi.encode(sender address [20B, left-pad], uint128 amount,
        // uint256 amount0, uint256 amount1) — 128 bytes. Built from the decoded
        // field values (amount = on-chain delta = 713844323716371 - seed).
        let mut data: Vec<u8> = Vec::with_capacity(128);
        // word 0: sender (left-padded 20-byte address)
        let sender = [
            0xc3, 0x64, 0x42, 0xb4, 0xa4, 0x52, 0x2e, 0x87, 0x13, 0x99, 0xcd, 0x71, 0x7a, 0xbd,
            0xd8, 0x47, 0xab, 0x11, 0xfe, 0x88,
        ];
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(&sender);
        // word 1: amount (uint128 → 32-byte ABI word = 16 zero bytes + 16 be)
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&MINT_AMOUNT.to_be_bytes());
        // word 2: amount0 (uint256, fits in 128 bits)
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&0x2ab_6b07_u128.to_be_bytes());
        // word 3: amount1 (uint256, fits in 128 bits)
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&0x0094_4200_9f44_c61du128.to_be_bytes());
        let data = alloy::primitives::Bytes::from(data);
        assert_eq!(data.len(), 128, "Mint data must be 4×32 bytes");

        let inner = alloy::primitives::Log::new_unchecked(
            pool_addr,
            vec![mint_topic, owner_topic, tick_lower_topic, tick_upper_topic],
            data,
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: Some(block),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        let dispatcher = LogDispatcher::with_uniswap_decoders();
        dispatcher.dispatch(&log, &state, None);

        let s = state.read_at(crate::bot_core::state_lock::LockSite::Core);
        let pool = s.get_v3_pool(pool_id).expect("pool registered");
        let t201020 = pool.tick_data.get(&201_020).cloned().expect("tick 201020");
        let t203350 = pool.tick_data.get(&203_350).cloned().expect("tick 203350");

        // On-chain ground truth: tick_lower gross += amount.
        assert_eq!(
            t201020.liquidity_gross.to::<u128>(),
            SEED_201020 + MINT_AMOUNT,
            "tick 201020 (Mint tickLower) must equal on-chain 713_844_323_716_371 — \
             red means the Mint was mis-routed or dropped at decode+apply"
        );
        // tick_upper gross also += amount (liquidity_gross is bipartisan).
        assert_eq!(
            t203350.liquidity_gross.to::<u128>(),
            SEED_203350 + MINT_AMOUNT,
            "tick 203350 (Mint tickUpper 0x031a56) must also reflect the Mint amount"
        );
    }
}
