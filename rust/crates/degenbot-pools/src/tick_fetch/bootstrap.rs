//! Sparse tick-map bootstrap RPC seam — the **registration-time** Chain arm
//! of the `assemble_{v3,v4}_tick_map` precedence (`Store → Db → Chain`).
//!
//! The callable answers the whole-word sparse read performed once per pool at
//! registration: the tick-bitmap word containing the pool's current `tick`, plus
//! the `(liquidity_gross, liquidity_net, block)` for every initialized tick in
//! that word. Returns `None` for a word whose bitmap is all-zero (no
//! initialized ticks — matches the Python builder's "don't seed empty" rule).
//!
//! ## Distinct from `super::miss::TickWordFetcher`
//!
//! This trait is keyed by *address* (the pool's contract address — known BEFORE
//! `register_*_pool` assigns the internal `pool_id`), whereas
//! `TickWordFetcher` is keyed by *`pool_id`* (the live-pump miss path operates
//! on already-registered pools). The orchestration choreography is identical
//! between the two (compute word, fetch bitmap, enumerate bits, fetch ticks)
//! but the key type differs — see
//! `docs/migration-guides/chain-bootstrap-tick-map.md` §3 for why they are
//! kept separate for now (consolidation is a post-`XEANMB` follow-up).
//!
//! ## Why this trait lives in `degenbot-pools` (not the bot)
//!
//! The assemble helper (`degenbot-bot::bot_core::tick_assembly`) consumes this
//! trait, and `degenbot-bot` depends on `degenbot-pools`; the trait type must
//! be visible to both crates. Following the `std::io::Read` precedent (defining
//! an interface pulls no I/O), the trait *definition* + its value-only
//! error/return types live here, while the RPC/pyo3 *implementations* live in
//! `degenbot-rpc` / `degenbot-python`.
//!
//! ## Sync, not async
//!
//! The trait method signature is `fn ... -> Result<...>` (synchronous), not
//! `async fn`. The only consumer (`assemble_*_tick_map`) runs in a synchronous
//! callsite already wrapped in `py.detach` (A4YUYJ's GIL release); the alloy
//! impl handles the async `eth_call` internally via
//! `get_runtime().block_on(async { ... })`, matching the pattern in
//! `rust/crates/degenbot-python/src/bot/py_bot_io.rs::forward_call_to_provider`.
//! A future pure-async standalone `Bot` API can introduce a sibling `async`
//! trait that shares the same value types.

use std::collections::HashMap;

use crate::TickInfo;

/// One word's bootstrap result. Mirrors `super::miss::FetchedTickWord` in
/// structure but lives separately for the call-site distinction
/// (registration-time Chain arm vs live-pump miss path). Both can be unified
/// post-`XEANMB` (see `docs/migration-guides/chain-bootstrap-tick-map.md` §3).
#[derive(Debug, Clone)]
pub struct BootstrapTickWord {
    /// The tick-bitmap word position that was fetched
    /// (`degenbot_cl_math::cl_lib::get_tick_word_and_bit_position(tick, spacing).0`).
    pub word: i32,
    /// Initialized ticks in this word. `liquidity_gross` / `liquidity_net` /
    /// `block` — the same shape `register_*_pool` accepts as `tick_data`.
    /// May be empty (the helper returns `None` instead — see
    /// [`TickBootstrapRpc::bootstrap_v3_tick_word`]'s contract).
    pub ticks: HashMap<i32, TickInfo>,
}

/// Why a [`TickBootstrapRpc`] call could not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapTickError {
    /// The fetch itself failed (RPC error, timeout, transport, revert).
    #[error("rpc failure (transport, timeout, or revert)")]
    Rpc,
    /// Malformed return data (short bytes, bad ABI decode, type-tag mismatch).
    #[error("malformed return data (short bytes or bad abi decode)")]
    InvalidReturn,
}

/// Sparse tick-map bootstrap RPC seam — Chain arm of the
/// `assemble_{v3,v4}_tick_map` precedence (`Store → Db → Chain`).
///
/// Implementations may do I/O (RPC). The assemble helper runs the Chain arm
/// AFTER the Store arm's closure has returned and dropped the `BotState` read
/// guard (A4YUYJ's two-phase lock protocol — no `BotState` guard held across
/// the RPC read). A pyo3-adapter impl that re-enters the GIL must do so via
/// the standard `Python::attach` per-call hop pattern (mirrors
/// `PyTickWordFetcher`); an alloy-backed impl releases the GIL across the RPC
/// via `py.detach(|| get_runtime().block_on(async { ... }))`.
pub trait TickBootstrapRpc: Send + Sync + std::fmt::Debug {
    /// Fetch the tick-bitmap word containing `tick` for the V3 pool at
    /// `pool_address`, plus the liquidity for every initialized tick in that
    /// word, at snapshot block `block`.
    ///
    /// Returns `None` for an all-zero bitmap (no initialized ticks in this
    /// word — do not seed; the pool registers with `tick_data=None,
    /// coverage="sparse"`).
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapTickError::Rpc`] on RPC failure (timeout, transport,
    /// revert), or [`BootstrapTickError::InvalidReturn`] on a malformed return
    /// (short bytes, bad ABI decode).
    fn bootstrap_v3_tick_word(
        &self,
        pool_address: &str,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError>;

    /// Fetch the tick-bitmap word containing `tick` for the V4 pool identified
    /// by (`pool_manager`, `pool_id`), plus the liquidity for every tick in
    /// that word, at snapshot block `block`. V4 twin of
    /// [`Self::bootstrap_v3_tick_word`].
    ///
    /// `state_view` is the V4 **`StateView`** contract address (the contract that
    /// exposes `getTickBitmap(bytes32,int16)` + `getTickLiquidity(bytes32,int24)`
    /// — NOT the `PoolManager`; V4 state reads route through a separate
    /// `StateView` deployment).
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapTickError::Rpc`] on RPC failure, or
    /// [`BootstrapTickError::InvalidReturn`] on a malformed return.
    fn bootstrap_v4_tick_word(
        &self,
        state_view: &str,
        pool_id: &[u8; 32],
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError>;
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use alloy::primitives::{I256, U128};

    /// Fake `TickBootstrapRpc` impl mirroring the pattern of
    /// `degenbot-bot::bot_core::mod::tests::FakeFetcher` — exercises the trait
    /// shape with no real RPC.
    ///
    /// Configured per-instance with a fixed bitmap (u256 → u128 truncation
    /// suffices for the test envelope) + a hardcoded `(liquidity_gross,
    /// liquidity_net)` pair returned for every active tick in the configured
    /// word.
    #[derive(Debug)]
    struct FakeBootstrapRpc {
        /// The bitmap value to return. Zero → `None`.
        bitmap: u128,
        /// The liquidity pair returned for every active tick.
        gross: u128,
        net: i128,
        /// If `Some(tick)`, the call returns `Err(BootstrapTickError::Rpc)`.
        fail_on_tick: Option<i32>,
    }

    impl TickBootstrapRpc for FakeBootstrapRpc {
        #[expect(clippy::cast_possible_wrap, reason = "bit index 0..128 fits in i32")]
        fn bootstrap_v3_tick_word(
            &self,
            _pool_address: &str,
            tick: i32,
            tick_spacing: i32,
            block: u64,
        ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
            if self.fail_on_tick == Some(tick) {
                return Err(BootstrapTickError::Rpc);
            }
            if self.bitmap == 0 {
                return Ok(None);
            }
            let (word, _) =
                degenbot_cl_math::cl_lib::liquidity_mapping::get_tick_word_and_bit_position(
                    tick,
                    tick_spacing,
                );
            let mut ticks = HashMap::new();
            for i in 0..u128::BITS {
                if (self.bitmap & (1u128 << i)) > 0 {
                    let active_tick = ((word << 8) + i as i32) * tick_spacing;
                    ticks.insert(
                        active_tick,
                        TickInfo {
                            liquidity_gross: U128::from(self.gross),
                            liquidity_net: I256::try_from(self.net).unwrap_or(I256::ZERO),
                            block,
                        },
                    );
                }
            }
            Ok(Some(BootstrapTickWord { word, ticks }))
        }

        fn bootstrap_v4_tick_word(
            &self,
            _state_view: &str,
            _pool_id: &[u8; 32],
            tick: i32,
            tick_spacing: i32,
            block: u64,
        ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
            // V4 path uses the same choreography as V3 — delegate.
            self.bootstrap_v3_tick_word("", tick, tick_spacing, block)
        }
    }

    #[test]
    fn fake_bootstrap_rpc_known_tick_returns_some_with_ticks() {
        // bitmap = 0b1010 → bits 1 + 3 set → 2 active ticks.
        let rpc = FakeBootstrapRpc {
            bitmap: 0b1010,
            gross: 1_000,
            net: -500,
            fail_on_tick: None,
        };
        // tick=10, tick_spacing=1 → word = 10.div_euclid(1) >> 8 = 0; bits
        // 1, 3 → active ticks at (0<<8 + 1)*1 = 1 and (0<<8 + 3)*1 = 3.
        let result = rpc
            .bootstrap_v3_tick_word("0xdeadbeef", 10, 1, 100)
            .unwrap();
        let fetched = result.expect("non-zero bitmap → Some");
        assert_eq!(fetched.word, 0);
        assert_eq!(fetched.ticks.len(), 2);
        assert!(fetched.ticks.contains_key(&1));
        assert!(fetched.ticks.contains_key(&3));
        let tick_info = &fetched.ticks[&1];
        assert_eq!(tick_info.liquidity_gross, U128::from(1_000u64));
        assert_eq!(tick_info.liquidity_net, I256::try_from(-500i64).unwrap());
        assert_eq!(tick_info.block, 100);
    }

    #[test]
    fn fake_bootstrap_rpc_all_zero_bitmap_returns_none() {
        let rpc = FakeBootstrapRpc {
            bitmap: 0,
            gross: 0,
            net: 0,
            fail_on_tick: None,
        };
        let result = rpc
            .bootstrap_v3_tick_word("0xdeadbeef", 10, 1, 100)
            .unwrap();
        assert!(result.is_none(), "all-zero bitmap → None");
    }

    #[test]
    fn fake_bootstrap_rpc_configured_fail_tick_returns_rpc_err() {
        let rpc = FakeBootstrapRpc {
            bitmap: 0b1,
            gross: 0,
            net: 0,
            fail_on_tick: Some(42),
        };
        let err = rpc
            .bootstrap_v3_tick_word("0xdeadbeef", 42, 1, 100)
            .expect_err("configured-fail tick → Err");
        assert_eq!(err, BootstrapTickError::Rpc);
    }

    #[test]
    fn v4_twin_uses_same_choreography_as_v3() {
        // Same fake → V4 should return the same `BootstrapTickWord` as V3
        // (the test impl delegates).
        let rpc = FakeBootstrapRpc {
            bitmap: 0b1,
            gross: 7,
            net: 3,
            fail_on_tick: None,
        };
        let pool_id = [0xff; 32];
        let v4_result = rpc
            .bootstrap_v4_tick_word("0xpoolmgr", &pool_id, 10, 1, 200)
            .unwrap();
        let v3_result = rpc.bootstrap_v3_tick_word("", 10, 1, 200).unwrap();
        let v4 = v4_result.expect("Some");
        let v3 = v3_result.expect("Some");
        assert_eq!(v4.word, v3.word);
        assert_eq!(v4.ticks.len(), v3.ticks.len());
        assert_eq!(
            v4.ticks.keys().collect::<Vec<_>>(),
            v3.ticks.keys().collect::<Vec<_>>()
        );
    }
}
