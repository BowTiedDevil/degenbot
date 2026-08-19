//! Pure-Rust `TickBootstrapRpc` impl backed by an [`AlloyProvider`].
//!
//! This is the standalone-consumer path: a `cargo add degenbot` Rust consumer
//! passes this impl to `assemble_{v3,v4}_tick_map`'s `chain: Option<&dyn
//! TickBootstrapRpc>` parameter; no Python, no `PyBotIo`, no GIL. The
//! `degenbot-python` wrapper has its own pyo3 adapter (`PyTickBootstrapRpc`,
//! lives in `degenbot-python/src/bot/py_bot_io.rs`'s sibling `tick_bootstrap`
//! module) that wraps a `PyBotIo` and delegates per-call via GIL re-entry.
//!
//! ## Choreography
//!
//! Both methods port the Python Branch 3 sparse-RPC loop verbatim:
//! 1. Compute the tick-bitmap word via
//!    [`degenbot_concentrated_liquidity_math::liquidity_mapping::get_tick_word_and_bit_position`].
//! 2. `eth_call` the bitmap (`tickBitmap(int16)` for V3; `getTickBitmap(bytes32,int16)` for V4).
//! 3. If the bitmap is zero → return `Ok(None)` (no initialized ticks in this
//!    word — matches the Python "don't seed empty" rule).
//! 4. Enumerate the 256 bits; for each set bit, `eth_call` the tick's liquidity
//!    (`ticks(int24)` for V3; `getTickLiquidity(bytes32,int24)` for V4) and
//!    decode into `TickInfo { liquidity_gross, liquidity_net, block }`.
//! 5. Return `Ok(Some(BootstrapTickWord { word, ticks }))`.
//!
//! The choreography is identical to the Python Branch 3 path in
//! `src/degenbot/builders/v3_pool_builder.py::V3PoolBuilder.build` and its V4
//! twin — same word computation, same bit enumeration order, same decode
//! shapes. Behavior preservation is exact for live pools.
//!
//! ## Sync bridging
//!
//! The trait method is `fn ... -> Result<...>` (synchronous). The underlying
//! `AlloyProvider::eth_call` is `async`. The alloy impl bridges this via
//! [`degenbot_core::runtime::get_runtime`] + `Runtime::block_on(async { ... })`
//! — the same pattern `PyBotIo::forward_call_to_provider` uses for its native
//! fast path. This is safe inside the synchronous callsite (`assemble_*` runs
//! in a synchronous callsite already wrapped in `py.detach` when invoked from
//! the pyo3 adapter), and for the standalone consumer that has no GIL at all.
//!
//! A future pure-async standalone `Bot` API can introduce a sibling `async`
//! trait sharing the same value types — out of scope for the
//! `5NT2OC` epic (see `docs/migration-guides/chain-bootstrap-tick-map.md` §1).

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_core::runtime::get_runtime;
use degenbot_pools::tick_fetch::{BootstrapTickError, BootstrapTickWord, TickBootstrapRpc};
use degenbot_pools::TickInfo;

use crate::abi::{fetch_tick_bitmap, fetch_tick_data, fetch_v4_tick_bitmap, fetch_v4_tick_data};
use crate::provider::AlloyProvider;

/// `TickBootstrapRpc` impl backed by an [`AlloyProvider`].
///
/// Holds the provider by value (cloning is cheap — inner is `Arc`-shared) so
/// the struct is `Send + Sync` (the trait bounds). Construct via
/// [`AlloyTickBootstrapRpc::new`]. Manual `Debug` impl because `AlloyProvider`
/// itself doesn't impl `Debug` (its inner `dyn Provider<Ethereum>` doesn't).
#[derive(Clone)]
pub struct AlloyTickBootstrapRpc {
    provider: Arc<AlloyProvider>,
}

impl std::fmt::Debug for AlloyTickBootstrapRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlloyTickBootstrapRpc")
            .field("provider", &"Arc<AlloyProvider>")
            .finish()
    }
}

impl AlloyTickBootstrapRpc {
    /// Construct a new bootstrap RPC impl wrapping the given provider.
    #[must_use]
    pub fn new(provider: Arc<AlloyProvider>) -> Self {
        Self { provider }
    }
}

impl TickBootstrapRpc for AlloyTickBootstrapRpc {
    fn bootstrap_v3_tick_word(
        &self,
        pool_address: &str,
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
        // Parse the address outside the runtime (no async needed).
        let pool = parse_address(pool_address)?;

        let provider = Arc::clone(&self.provider);
        get_runtime().block_on(async move {
            // 1. Compute the tick-bitmap word containing `tick`.
            let (word, _) =
                degenbot_concentrated_liquidity_math::liquidity_mapping::get_tick_word_and_bit_position(
                    tick,
                    tick_spacing,
                );
            // The Solidity `tickBitmap(int16)` selector takes an int16; the
            // helper's `word: i32` is guaranteed to fit in int16 for any valid
            // V3 tick (tick range is int24, word = tick.div_euclid(spacing) >> 8).
            #[expect(clippy::expect_used)] // V3 tick word always fits in int16
            let word_i16 = i16::try_from(word).expect("tick word fits in int16");

            // 2. Fetch the bitmap for this word.
            let bitmap = fetch_tick_bitmap(&provider, &pool, word_i16, Some(block))
                .await
                .map_err(map_provider_err)?;

            // 3. Zero bitmap → no initialized ticks in this word.
            if bitmap.is_zero() {
                return Ok(None);
            }

            // 4. Enumerate set bits + fetch each tick's liquidity.
            let mut ticks: HashMap<i32, TickInfo> = HashMap::new();
            for i in 0..=255u8 {
                if bitmap.bit(i.into()) {
                    let active_tick = ((word << 8) + i32::from(i)) * tick_spacing;
                    let (liquidity_gross, liquidity_net) =
                        fetch_tick_data(&provider, &pool, active_tick, Some(block))
                            .await
                            .map_err(map_provider_err)?;
                    ticks.insert(
                        active_tick,
                        TickInfo {
                            liquidity_gross,
                            liquidity_net,
                            block,
                        },
                    );
                }
            }

            Ok(Some(BootstrapTickWord { word, ticks }))
        })
    }

    fn bootstrap_v4_tick_word(
        &self,
        state_view: &str,
        pool_id: &[u8; 32],
        tick: i32,
        tick_spacing: i32,
        block: u64,
    ) -> Result<Option<BootstrapTickWord>, BootstrapTickError> {
        let state_view_addr = parse_address(state_view)?;

        let provider = Arc::clone(&self.provider);
        get_runtime().block_on(async move {
            let (word, _) =
                degenbot_concentrated_liquidity_math::liquidity_mapping::get_tick_word_and_bit_position(
                    tick,
                    tick_spacing,
                );
            #[expect(clippy::expect_used)] // V3 tick word always fits in int16
            let word_i16 = i16::try_from(word).expect("tick word fits in int16");

            let bitmap =
                fetch_v4_tick_bitmap(&provider, &state_view_addr, pool_id, word_i16, Some(block))
                    .await
                    .map_err(map_provider_err)?;

            if bitmap.is_zero() {
                return Ok(None);
            }

            let mut ticks: HashMap<i32, TickInfo> = HashMap::new();
            for i in 0..=255u8 {
                if bitmap.bit(i.into()) {
                    let active_tick = ((word << 8) + i32::from(i)) * tick_spacing;
                    let (liquidity_gross, liquidity_net) = fetch_v4_tick_data(
                        &provider,
                        &state_view_addr,
                        pool_id,
                        active_tick,
                        Some(block),
                    )
                    .await
                    .map_err(map_provider_err)?;
                    ticks.insert(
                        active_tick,
                        TickInfo {
                            liquidity_gross,
                            liquidity_net,
                            block,
                        },
                    );
                }
            }

            Ok(Some(BootstrapTickWord { word, ticks }))
        })
    }
}

/// Map a hex-string pool address into `alloy::primitives::Address`. The pool's
/// contract address comes from Python's `ChecksummedAddress` / `str` form in the
/// pyo3 adapter path; the standalone Rust consumer passes the same hex string.
fn parse_address(s: &str) -> Result<Address, BootstrapTickError> {
    s.parse::<Address>()
        .map_err(|_| BootstrapTickError::InvalidReturn)
}

/// Map a `ProviderError` to a `BootstrapTickError`. RPC failures (transport,
/// revert, timeout) → [`BootstrapTickError::Rpc`]; decode failures →
/// [`BootstrapTickError::InvalidReturn`]. Both are explicitly bucketed — a
/// caller that wants to retry only on RPC errors can match on the variant.
///
/// Takes the error by value to match the `Result::map_err` callsite contract
/// (which moves the error in).
#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err passes the error by value"
)]
fn map_provider_err(err: degenbot_core::errors::ProviderError) -> BootstrapTickError {
    use degenbot_core::errors::ProviderError;
    match err {
        ProviderError::DecodingError { .. } => BootstrapTickError::InvalidReturn,
        _ => BootstrapTickError::Rpc,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::offline::OfflineProvider;
    use alloy::primitives::{address, I256, U128, U256};
    use serde_json::json;

    /// The V3 pool address used in the recorded-call fixtures.
    const V3_POOL: Address = address!("0000000000000000000000000000000000000001");

    /// An arbitrary state-view contract address for V4.
    const V4_STATE_VIEW: Address = address!("0000000000000000000000000000000000000002");

    /// V3 lookup key — must match `OfflineTransport::call_key`'s format
    /// exactly: `"<lowercase_addr_with_0x>:0x<data_hex>"`.
    fn call_key(to: Address, calldata: &[u8]) -> String {
        format!(
            "{}:0x{}",
            to.to_checksum(None).to_lowercase(),
            to_hex(calldata)
        )
    }

    /// Encode a byte slice as lowercase hex (no `0x` prefix). Inline to
    /// avoid a `hex` crate dependency for tests only.
    fn to_hex(bytes: &[u8]) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(HEX_CHARS[(b >> 4) as usize] as char);
            s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
        }
        s
    }

    /// 32-byte ABI-encoded `uint256` value, big-endian.
    fn word_u256(v: U256) -> String {
        let mut buf = [0u8; 32];
        v.to_be_bytes::<32>().iter().enumerate().for_each(|(i, b)| {
            buf[i] = *b;
        });
        to_hex(&buf)
    }
    /// 32-byte ABI-encoded `uint128` value (right-aligned in 32-byte word).
    fn word_u128(v: u128) -> String {
        let mut buf = [0u8; 32];
        buf[16..].copy_from_slice(&v.to_be_bytes());
        to_hex(&buf)
    }

    /// 32-byte ABI-encoded `int128` value, sign-extended.
    fn word_i128(v: i128) -> String {
        let mut buf = [0u8; 32];
        let be = v.to_be_bytes();
        if v < 0 {
            buf[..16].fill(0xff);
        }
        buf[16..].copy_from_slice(&be);
        to_hex(&buf)
    }

    /// Build an `OfflineProvider` recording the V3 tick-bitmap + tick-data
    /// responses.
    fn v3_offline_provider(
        bitmap_word: i16,
        bitmap_value: U256,
        tick_responses: &[(i32, u128, i128)],
        block: u64,
    ) -> AlloyProvider {
        let mut calls: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();

        // tickBitmap(int16) → uint256
        let bitmap_calldata = crate::abi::encode_tick_bitmap(bitmap_word);
        calls.insert(
            call_key(V3_POOL, &bitmap_calldata),
            Some(word_u256(bitmap_value)),
        );

        // ticks(int24) → (uint128 gross, int128 net)
        for &(tick, gross, net) in tick_responses {
            let td_calldata = crate::abi::encode_tick_data(tick);
            let mut response = String::new();
            response.push_str(&word_u128(gross));
            response.push_str(&word_i128(net));
            calls.insert(call_key(V3_POOL, &td_calldata), Some(response));
        }

        let json = json!({
            "chain_id": 1u64,
            "block_number": block,
            "timestamp": 0u64,
            "calls": calls,
            "code": {},
        });
        let json_str = json.to_string();
        OfflineProvider::from_json_str(&json_str)
            .expect("valid JSON")
            .as_alloy_provider()
    }

    #[test]
    fn v3_known_word_with_two_active_ticks_returns_some_with_both() {
        // tick=10, tick_spacing=1 → word=0, bit_pos=10.
        // Set bits 10 + 20 → bitmap = (1<<10) | (1<<20) = 0x10040001000 + ...
        let bitmap = U256::from(1u128 << 10) | U256::from(1u128 << 20);

        let provider = v3_offline_provider(0, bitmap, &[(10, 1_000, -500), (20, 2_000, 750)], 100);

        let rpc = AlloyTickBootstrapRpc::new(Arc::new(provider));

        let result = rpc
            .bootstrap_v3_tick_word(&V3_POOL.to_string(), 10, 1, 100)
            .expect("ok");

        let fetched = result.expect("non-zero bitmap → Some");
        assert_eq!(fetched.word, 0);
        assert_eq!(fetched.ticks.len(), 2);

        let tick_10 = &fetched.ticks[&10];
        assert_eq!(tick_10.liquidity_gross, U128::from(1_000u64));
        assert_eq!(tick_10.liquidity_net, I256::try_from(-500i64).unwrap());
        assert_eq!(tick_10.block, 100);

        let tick_20 = &fetched.ticks[&20];
        assert_eq!(tick_20.liquidity_gross, U128::from(2_000u64));
        assert_eq!(tick_20.liquidity_net, I256::try_from(750i64).unwrap());
        assert_eq!(tick_20.block, 100);
    }

    #[test]
    fn v3_all_zero_bitmap_returns_none() {
        let provider = v3_offline_provider(0, U256::ZERO, &[], 100);
        let rpc = AlloyTickBootstrapRpc::new(Arc::new(provider));

        let result = rpc
            .bootstrap_v3_tick_word(&V3_POOL.to_string(), 10, 1, 100)
            .expect("ok");

        assert!(result.is_none(), "all-zero bitmap → None");
    }

    #[test]
    fn v3_unrecorded_tick_bitmap_returns_rpc_err() {
        // Empty recorded data → every eth_call returns "unrecorded".
        let provider = v3_offline_provider(0, U256::ZERO, &[], 100);
        let rpc = AlloyTickBootstrapRpc::new(Arc::new(provider));

        // Use a different word position so the bitmap lookup misses the (empty)
        // recorded data.
        let err = rpc
            .bootstrap_v3_tick_word(&V3_POOL.to_string(), 27_000, 100, 100)
            .expect_err("unrecorded eth_call → Err");
        assert_eq!(err, BootstrapTickError::Rpc);
    }

    #[test]
    fn v3_negative_tick_computes_correct_word() {
        // tick=-10, tick_spacing=1 → word=-1, bit_pos=246 (Solidity
        // int24 tick >> 8 = -1 for negative ticks < -256).
        // Set bit 246 → bitmap = 1 << 246.
        let bitmap_val = U256::from(1u64) << 246;

        let expected_active_tick = ((-1i32) << 8) + 246;
        let provider =
            v3_offline_provider(-1, bitmap_val, &[(expected_active_tick, 500, 250)], 100);
        let rpc = AlloyTickBootstrapRpc::new(Arc::new(provider));

        let result = rpc
            .bootstrap_v3_tick_word(&V3_POOL.to_string(), -10, 1, 100)
            .expect("ok");
        let fetched = result.expect("non-zero bitmap → Some");
        assert_eq!(fetched.word, -1);
        assert_eq!(fetched.ticks.len(), 1);
        assert!(fetched.ticks.contains_key(&expected_active_tick));
    }

    #[test]
    fn v4_known_word_returns_some_with_ticks() {
        // V4 — uses pool_manager + pool_id.
        let pool_id = [0xaa; 32];

        // tick=10, spacing=1 → word=0, bit_pos=10. Set bit 10.
        let bitmap = U256::from(1u128 << 10);

        // Build V4 fixture.
        let mut calls: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        let bm_calldata = crate::abi::encode_v4_tick_bitmap(&pool_id, 0);
        calls.insert(
            call_key(V4_STATE_VIEW, &bm_calldata),
            Some(word_u256(bitmap)),
        );

        // Encode ticks(int24) — V4 tick at (word<<8 + bit)*spacing = 10.
        let td_calldata = crate::abi::encode_v4_tick_data(&pool_id, 10);
        let mut response = String::new();
        response.push_str(&word_u128(42));
        response.push_str(&word_i128(-7));
        calls.insert(call_key(V4_STATE_VIEW, &td_calldata), Some(response));

        let json = json!({
            "chain_id": 1u64,
            "block_number": 9_999u64,
            "timestamp": 0u64,
            "calls": calls,
            "code": {},
        });
        let provider = OfflineProvider::from_json_str(&json.to_string())
            .expect("valid JSON")
            .as_alloy_provider();
        let rpc = AlloyTickBootstrapRpc::new(Arc::new(provider));

        let result = rpc
            .bootstrap_v4_tick_word(&V4_STATE_VIEW.to_string(), &pool_id, 10, 1, 9_999)
            .expect("ok");

        let fetched = result.expect("non-zero bitmap → Some");
        assert_eq!(fetched.word, 0);
        assert_eq!(fetched.ticks.len(), 1);
        let tick_info = &fetched.ticks[&10];
        assert_eq!(tick_info.liquidity_gross, U128::from(42u64));
        assert_eq!(tick_info.liquidity_net, I256::try_from(-7i64).unwrap());
        assert_eq!(tick_info.block, 9_999);
    }

    #[test]
    fn v3_bad_address_returns_invalid_return_err() {
        // Pair a valid provider with an unparseable address — the parse
        // failure surfaces as InvalidReturn (it's a "malformed client input"
        // bucketed with malformed-return for now; no separate variant).
        let provider = v3_offline_provider(0, U256::ZERO, &[], 100);
        let rpc = AlloyTickBootstrapRpc::new(Arc::new(provider));

        let err = rpc
            .bootstrap_v3_tick_word("0xnot-an-address", 10, 1, 100)
            .expect_err("bad address → Err");
        assert_eq!(err, BootstrapTickError::InvalidReturn);
    }
}
