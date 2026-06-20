//! Per-state-subject publisher/subscriber event bus (ADR-006 D4).
//!
//! `LogDispatcher` owns a decoder registry + a `Weak<dyn PoolStateSubscriber>`
//! registry keyed by `pool_id`. `Bot` drives `dispatch(log)`: decode the log,
//! apply the decoded event to `BotState` under a **write** guard, **release the
//! guard**, then notify each subscriber of the affected `pool_id`
//! (`on_pool_state_updated`). The subscriber (an engine adapter) dirties the
//! `pool_id` in its own per-engine set taking only the engine `Mutex` — the
//! core write is already released, so D2's engine-then-core order is preserved
//! by not nesting.
//!
//! This module ships the bus + subscriber seam in isolation (ADR-006 slice 4).
//! The pump driving the dispatcher lands in slice 5; the live `apply_log` hot
//! loop is untouched until then (rewiring it through `engine.apply_log` would
//! self-notify under the engine's non-reentrant `Mutex` — a deadlock; the
//! pump-side relocation in slice 5 avoids that).

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use alloy::rpc::types::Log;

use crate::bot_core::v3_mint_burn_decoder::{decode_v3_burn_log, decode_v3_mint_log};
use crate::bot_core::v3_swap_decoder::decode_v3_swap_log;
use crate::bot_core::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;
use crate::bot_core::v4_swap_decoder::decode_v4_swap_log;
use crate::bot_core::BotState;
use crate::optimizers::v2_sync_decoder::decode_sync_log;

/// A subscriber to pool-state updates (ADR-006 D4).
///
/// `on_pool_state_updated` fires after `BotState` has been mutated and the
/// write guard released. The implementer dirties `pool_id` in its own state
/// taking only its own lock — never the `BotState` lock synchronously (that
/// would re-nest against D2's engine-then-core order).
///
/// Ships as `PoolStateSubscriber` until a second state-subject type proves
/// generality (then rename to `StateSubscriber`).
#[allow(dead_code)]
pub trait PoolStateSubscriber: Send + Sync {
    /// `pool_id`'s state in the associated `BotState` just changed; re-solve
    /// dirtied paths at the drain tick (slice 6).
    fn on_pool_state_updated(&self, pool_id: u64);
}

/// A decoded pool-state event ready to apply to `BotState`.
///
/// One variant per pool family — the decoder selects the family; the apply
/// method matches on it. Future families (Curve/Aave) add variants + decoders
/// without `Bot` knowledge.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum DecodedPoolEvent {
    /// V2 `Sync` — `(address, reserve0, reserve1)`.
    V2Sync {
        pool_address: alloy::primitives::Address,
        reserve0: alloy::primitives::U256,
        reserve1: alloy::primitives::U256,
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
        pool_id: crate::bot_core::v4_swap_decoder::PoolId,
        sqrt_price_x96: alloy::primitives::U256,
        liquidity: alloy::primitives::U256,
        tick: i32,
        block_number: u64,
    },
    /// V4 `ModifyLiquidity` — `(pool_manager, pool_id, tick_lower, tick_upper, delta)`.
    V4Liquidity {
        pool_manager: alloy::primitives::Address,
        pool_id: crate::bot_core::v4_swap_decoder::PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    },
}

impl DecodedPoolEvent {
    /// The block number carried by this event's source log.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

    /// Apply this event to `bot_state`, returning the affected `pool_id` (or
    /// `None` if the pool isn't registered / the event is a no-op).
    #[allow(dead_code)]
    fn apply(self, bot_state: &mut BotState) -> Option<u64> {
        match self {
            Self::V2Sync {
                pool_address,
                reserve0,
                reserve1,
                block_number,
            } => bot_state.apply_v2_sync(pool_address, reserve0, reserve1, block_number),
            Self::V3Swap {
                pool_address,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
            } => bot_state.apply_v3_swap(
                pool_address,
                sqrt_price_x96,
                liquidity.to::<u128>(),
                tick,
                block_number,
                &[],
            ),
            Self::V3Liquidity {
                pool_address,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            } => bot_state.apply_v3_liquidity_update(
                pool_address,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            ),
            Self::V4Swap {
                pool_manager,
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
            } => bot_state.apply_v4_swap(
                &crate::bot_core::V4SwapUpdate {
                    pool_manager,
                    pool_id,
                    sqrt_price_x96,
                    liquidity: liquidity.to::<u128>(),
                    tick,
                    tick_priors: vec![],
                },
                block_number,
            ),
            Self::V4Liquidity {
                pool_manager,
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            } => bot_state.apply_v4_liquidity_update(
                pool_manager,
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            ),
        }
    }
}

/// A decoder that recognizes one family of pool-state logs (ADR-006 D4).
///
/// The strategy-extension seam: a future Curve/Aave decoder registers here
/// without `Bot` knowing its event shapes.
#[allow(dead_code)]
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

/// The per-`Bot` event bus: decoder registry + `pool_id` → subscribers.
///
/// `Bot` owns one and mediates the registry (cleaner than per-`PoolEntry`
/// callback vecs in Rust). `dispatch` is the single entry point the pump
/// (slice 5) calls per WS log.
///
/// Decoders are frozen after construction (read-only `&self` access). The
/// subscriber registry uses interior mutability so [`subscribe`](Self::subscribe)
/// (registration-time) and [`dispatch`](Self::dispatch) (hot loop) both take
/// `&self` — `Bot` is shared across threads.
#[allow(dead_code)]
pub struct LogDispatcher {
    decoders: Vec<Box<dyn LogDecoder>>,
    subscribers: parking_lot::Mutex<HashMap<u64, Vec<Weak<dyn PoolStateSubscriber>>>>,
}

impl LogDispatcher {
    /// Construct an empty dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoders: Vec::new(),
            subscribers: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Construct a dispatcher pre-loaded with the 6 Uniswap (V2/V3/V4)
    /// decoders. This is the constructor `Bot` uses; strategy decoders add
    /// themselves afterwards via [`register_decoder`](Self::register_decoder).
    #[must_use]
    pub fn with_uniswap_decoders() -> Self {
        let mut d = Self::new();
        d.register_decoder(Box::new(V2SyncDecoder));
        d.register_decoder(Box::new(V3SwapDecoder));
        d.register_decoder(Box::new(V3MintBurnDecoder));
        d.register_decoder(Box::new(V4SwapDecoder));
        d.register_decoder(Box::new(V4ModifyLiquidityDecoder));
        d
    }

    /// Register a decoder. Decoders are tried in registration order.
    pub fn register_decoder(&mut self, decoder: Box<dyn LogDecoder>) {
        self.decoders.push(decoder);
    }

    /// Subscribe `subscriber` to updates for `pool_id`. `Bot` calls this when
    /// an engine registers a path touching `pool_id` (ADR-006 D4).
    #[allow(dead_code)]
    pub fn subscribe(&self, pool_id: u64, subscriber: Weak<dyn PoolStateSubscriber>) {
        self.subscribers
            .lock()
            .entry(pool_id)
            .or_default()
            .push(subscriber);
    }

    /// Decode `log`, apply it to `state`, release the write guard, then notify
    /// every live subscriber of the affected `pool_id`. Dead `Weak`s are
    /// skipped (no panic). No-op if no decoder recognizes the log or the pool
    /// isn't registered.
    ///
    /// **Lock order:** the `state` write guard is acquired and released BEFORE
    /// any subscriber notify — subscribers take only their own lock (D2's
    /// engine-then-core order preserved by not nesting).
    #[allow(dead_code)]
    pub fn dispatch(&self, log: &Log, state: &Arc<parking_lot::RwLock<BotState>>) {
        let Some(decoded) = self.decoders.iter().find_map(|d| d.try_decode(log)) else {
            return;
        };
        // Apply under the write guard, then RELEASE before notifying.
        let Some(pool_id) = decoded.apply(&mut state.write()) else {
            return;
        };
        self.notify(pool_id);
    }

    /// Decode `log` into a [`DecodedPoolEvent`] via the decoder registry, or
    /// `None` if no decoder recognizes it. Used by `dispatch` (forward) and
    /// `ReorgCoordinator::dispatch_reorg_log` (removed) — both decode the pool
    /// identity; only the forward path `apply`s.
    #[allow(dead_code)]
    pub fn try_decode_log(&self, log: &Log) -> Option<DecodedPoolEvent> {
        self.decoders.iter().find_map(|d| d.try_decode(log))
    }

    /// Notify every live subscriber of `pool_id` (skipping dropped `Weak`s).
    /// `ReorgCoordinator` calls this after a per-pool `restore_before_block` —
    /// the SAME notify path forward `dispatch` uses, so the engine dirties +
    /// re-solves at the next drain tick with no distinct reorg path.
    #[allow(dead_code)]
    pub fn notify(&self, pool_id: u64) {
        let Some(subs) = self.subscribers.lock().get(&pool_id).cloned() else {
            return;
        };
        for weak in subs {
            if let Some(sub) = weak.upgrade() {
                sub.on_pool_state_updated(pool_id);
            }
        }
    }
}

impl Default for LogDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

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
                    reserve0: alloy::primitives::U256::from(1),
                    reserve1: alloy::primitives::U256::from(2),
                    block_number: 0,
                })
            } else {
                None
            }
        }
    }

    /// Sentinel topic (arbitrary nonzero value).
    const SENTRY_TOPIC: [u8; 32] = [0xff; 32];

    /// A recording subscriber that notes the order of events: did the
    /// `BotState` write guard appear released by the time notify fired?
    #[derive(Clone)]
    struct RecordingSubscriber {
        calls: Arc<Mutex<Vec<SubObservation>>>,
        /// Probes the state lock at notify time to assert it's free.
        state: Arc<parking_lot::RwLock<BotState>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum SubObservation {
        Notified {
            pool_id: u64,
            write_guard_free: bool,
        },
    }

    impl PoolStateSubscriber for RecordingSubscriber {
        fn on_pool_state_updated(&self, pool_id: u64) {
            // If the dispatcher held the write guard across notify, this
            // `try_write` would fail (guard not free). D4 requires the guard
            // released first.
            let write_guard_free = self.state.try_write().is_some();
            self.calls.lock().unwrap().push(SubObservation::Notified {
                pool_id,
                write_guard_free,
            });
        }
    }

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

    /// RED→GREEN tracer (ADR-006 slice 4): dispatch decodes via a registered
    /// decoder, applies to `BotState` under a write guard, RELEASES the guard,
    /// then notifies subscribers. The subscriber observes the guard is free.
    #[test]
    fn dispatch_decodes_applies_then_notifies_after_write_release() {
        let pool_address = alloy::primitives::Address::from([0x11u8; 20]);
        let state = Arc::new(parking_lot::RwLock::new(BotState::new()));
        // Register the pool so apply produces a pool_id (apply_v2_sync returns
        // None for unregistered addresses).
        state
            .write()
            .register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
                address: pool_address,
                token0: alloy::primitives::Address::ZERO,
                token1: alloy::primitives::Address::ZERO,
                reserve0: alloy::primitives::U256::from(1000),
                reserve1: alloy::primitives::U256::from(2000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: alloy::primitives::Address::ZERO,
                update_block: 0,
            });

        let mut dispatcher = LogDispatcher::new();
        dispatcher.register_decoder(Box::new(FakeDecoder { pool_address }));

        let calls = Arc::new(Mutex::new(Vec::new()));
        let subscriber = RecordingSubscriber {
            calls: Arc::clone(&calls),
            state: Arc::clone(&state),
        };
        let subscriber: Arc<dyn PoolStateSubscriber> = Arc::new(subscriber);
        dispatcher.subscribe(1, Arc::downgrade(&subscriber));

        dispatcher.dispatch(&sentinel_log(), &state);

        let observed = calls.lock().unwrap().clone();
        assert_eq!(
            observed,
            vec![SubObservation::Notified {
                pool_id: 1,
                write_guard_free: true,
            }],
            "subscriber must be notified exactly once for pool_id=1, after the write guard released"
        );
    }

    /// Dead subscribers (dropped `Weak`) are silently skipped — no panic.
    #[test]
    fn dispatch_skips_dropped_subscribers() {
        let pool_address = alloy::primitives::Address::from([0x22u8; 20]);
        let state = Arc::new(parking_lot::RwLock::new(BotState::new()));
        state
            .write()
            .register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
                address: pool_address,
                token0: alloy::primitives::Address::ZERO,
                token1: alloy::primitives::Address::ZERO,
                reserve0: alloy::primitives::U256::from(1000),
                reserve1: alloy::primitives::U256::from(2000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: alloy::primitives::Address::ZERO,
                update_block: 0,
            });

        let mut dispatcher = LogDispatcher::new();
        dispatcher.register_decoder(Box::new(FakeDecoder { pool_address }));

        // Subscribe a live + a (soon-dropped) subscriber.
        let live_calls = Arc::new(Mutex::new(Vec::new()));
        let live = RecordingSubscriber {
            calls: Arc::clone(&live_calls),
            state: Arc::clone(&state),
        };
        let live_strong: Arc<dyn PoolStateSubscriber> = Arc::new(live);
        dispatcher.subscribe(1, Arc::downgrade(&live_strong));
        {
            let tmp = RecordingSubscriber {
                calls: Arc::new(Mutex::new(Vec::new())),
                state: Arc::clone(&state),
            };
            let tmp_strong: Arc<dyn PoolStateSubscriber> = Arc::new(tmp);
            dispatcher.subscribe(1, Arc::downgrade(&tmp_strong));
            // tmp_strong drops here → Weak goes dead.
        }

        dispatcher.dispatch(&sentinel_log(), &state);

        // The live subscriber still fires exactly once; the dead one skipped.
        assert_eq!(live_calls.lock().unwrap().len(), 1);
    }
}
