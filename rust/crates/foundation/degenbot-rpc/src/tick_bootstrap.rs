//! The tick-word I/O layer: the [`AlloyTickWordSource`] plus the ONE
//! transport-generic choreography loop (`fetch_tick_word_via`) every Rust
//! tick-word read shares (GLOSSARY **Tick-word source**).
//!
//! This is the standalone-consumer path: a `cargo add degenbot` Rust consumer
//! passes [`AlloyTickWordSource`] to `assemble_{v3,v4}_tick_map`'s
//! `chain: Option<&dyn TickWordSource>` parameter; no Python, no `PyBotIo`,
//! no GIL. The `degenbot-python` shell constructs the same impl over a
//! `PyBotIo`'s native provider (`make_tick_bootstrap_rpc`); the bot core's
//! live-miss source is the same adapter over its `ConstructionIo`
//! [`TickWordTransport`] impl in `degenbot_bot`'s pool-builder tick source.
//!
//! ## One choreography, two transports
//!
//! The read ports the Python Branch 3 sparse-RPC loop verbatim:
//! 1. read the word's bitmap (`tickBitmap(int16)` for V3;
//!    `getTickBitmap(bytes32,int16)` for V4);
//! 2. if the bitmap is zero → return `Ok(None)` (no initialized ticks in this
//!    word — matches the Python "don't seed empty" rule);
//! 3. enumerate the 256 bits; for each set bit, read the tick's liquidity
//!    (`ticks(int24)` for V3; `getTickLiquidity(bytes32,int24)` for V4) and
//!    decode into `TickInfo { liquidity_gross, liquidity_net, block }`;
//! 4. return `Ok(Some(FetchedTickWord { word, ticks, bitmap }))`.
//!
//! [`fetch_tick_word_via`] is that loop, written once, generic over
//! [`TickWordTransport`] — the two raw contract reads (bitmap, tick
//! liquidity). The transports: [`AlloyProvider`] here, and the bot core's
//! `ConstructionIo` (paced, `PyBotIo`-mirroring) over the same trait. One
//! adapter ([`TransportTickWordSource`]) drives either transport, so the word
//! range check, the error bucketing and the bridge exist once. The loop is
//! also where the bitmap-failure contract lives: a bitmap read that fails to
//! decode propagates the transport's error UNCHANGED, so no caller can
//! reinterpret malformed return data as empty tick state — only a DECODED zero
//! bitmap is a known-empty word (`Ok(None)`). Before this loop the two arms
//! carried divergent hand-rolled copies of the enumeration.
//!
//! ## Sync bridging
//!
//! [`TickWordSource`] is synchronous; the transports are `async`. Every source
//! bridges through the ONE seam, [`degenbot_core::runtime::bridged_block_on`]
//! (an `block_in_place` + ambient-handle drive inside a multi-threaded runtime,
//! the shared runtime outside one) — so a read is callable from the
//! registration path, the pump's threads, and a plain synchronous consumer
//! alike. A future pure-async standalone `Bot` API can introduce a sibling
//! `async` trait sharing the same value types — out of scope here.

use hashbrown::HashMap;
use std::future::Future;
use std::sync::Arc;

use alloy::primitives::{U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_core::runtime::bridged_block_on;
use degenbot_pools::TickInfo;
use degenbot_pools::{FetchedTickWord, TickWordFetchError, TickWordKey, TickWordSource};

use crate::abi::{ContractRead, IUniswapV3Pool, IUniswapV4StateView, TickLiquidity};
use crate::provider::AlloyProvider;

/// The two raw contract reads one tick-word choreography needs, addressed
/// by the pool descriptor the key carries.
///
/// The ABI shapes (`tickBitmap`/`ticks` for V3,
/// `getTickBitmap`/`getTickLiquidity` for V4) and their decode are the
/// impl's business; [`fetch_tick_word_via`] only orchestrates. Errors are
/// the transport's native [`ProviderError`] so the registration arm keeps
/// full error fidelity — the [`TickWordSource`] boundary buckets them into
/// [`TickWordFetchError`] (`map_provider_err`).
pub trait TickWordTransport: Send + Sync {
    /// Read the pool's tick-bitmap word `word` as a `U256`.
    fn read_bitmap(
        &self,
        key: &TickWordKey,
        word: i16,
        block: u64,
    ) -> impl Future<Output = Result<U256, ProviderError>> + Send;

    /// Read one initialized tick's `(liquidity_gross, liquidity_net)`.
    fn read_tick(
        &self,
        key: &TickWordKey,
        tick: i32,
        block: u64,
    ) -> impl Future<Output = Result<(U128, i128), ProviderError>> + Send;
}

/// The ONE tick-word choreography: read the bitmap, skip an empty word,
/// enumerate the 256 bits, read each initialized tick's liquidity, and stamp
/// every `TickInfo` with `block`.
///
/// Written once so the registration arm and the miss path cannot diverge;
/// the transports (`AlloyProvider`, the bot core's `ConstructionIo`)
/// supply the contract reads. `word` is the raw `int16` the bitmap
/// selectors take — the caller converts and maps its own out-of-range
/// verdict ([`TickWordFetchError::OutOfRange`] at the
/// [`TickWordSource`] boundary).
///
/// A decoded zero bitmap is a known-empty word (`Ok(None)`). A bitmap read
/// that fails refuses the whole word, decode failures included: malformed
/// return data is never reinterpreted as empty tick state, because an empty
/// word reads as "no initialized ticks here" to every downstream walker.
///
/// # Errors
///
/// Returns the transport's [`ProviderError`] unchanged — the bitmap read's
/// failure and every tick read's failure propagate as-is.
pub async fn fetch_tick_word_via<T: TickWordTransport>(
    transport: &T,
    key: &TickWordKey,
    word: i16,
    block: u64,
) -> Result<Option<FetchedTickWord>, ProviderError> {
    let bitmap = transport.read_bitmap(key, word, block).await?;
    if bitmap.is_zero() {
        return Ok(None);
    }
    let tick_spacing = key.tick_spacing();
    let mut ticks: HashMap<i32, TickInfo> = HashMap::new();
    for i in 0..=255u8 {
        if bitmap.bit(i.into()) {
            let active_tick = ((i32::from(word) << 8) + i32::from(i)) * tick_spacing;
            let (liquidity_gross, liquidity_net) =
                transport.read_tick(key, active_tick, block).await?;
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
    Ok(Some(FetchedTickWord {
        word: i32::from(word),
        ticks,
        bitmap: Some(bitmap),
    }))
}

/// The ONE [`TickWordSource`] adapter: a [`TickWordTransport`] driven by the
/// shared choreography loop and sync-bridged for the trait's synchronous call
/// sites.
///
/// The word-range check, the bitmap-failure refusal, the error bucketing,
/// and the bridge are therefore identical for every transport; a source
/// differs only in the transport it wraps —
/// [`AlloyTickWordSource`] over an [`AlloyProvider`], the bot core's live-miss
/// source over its construction I/O seam.
pub struct TransportTickWordSource<T> {
    transport: T,
}

impl<T: TickWordTransport> std::fmt::Debug for TransportTickWordSource<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportTickWordSource")
            .field("transport", &std::any::type_name::<T>())
            .finish()
    }
}

impl<T: TickWordTransport> Clone for TransportTickWordSource<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
        }
    }
}

impl<T: TickWordTransport> TransportTickWordSource<T> {
    /// Wrap a transport in the shared source adapter.
    #[must_use]
    pub fn from_transport(transport: T) -> Self {
        Self { transport }
    }
}

impl TransportTickWordSource<Arc<AlloyProvider>> {
    /// Construct a tick-word source over the given provider (the standalone
    /// `cargo add degenbot` consumer's source).
    #[must_use]
    pub fn new(provider: Arc<AlloyProvider>) -> Self {
        Self::from_transport(provider)
    }
}

/// The alloy-backed tick-word source — the standalone consumer's entry point.
pub type AlloyTickWordSource = TransportTickWordSource<Arc<AlloyProvider>>;

/// The shared-ownership alloy transport reads exactly like the provider it
/// points at, so the source clones per call site without cloning the provider.
impl TickWordTransport for Arc<AlloyProvider> {
    async fn read_bitmap(
        &self,
        key: &TickWordKey,
        word: i16,
        block: u64,
    ) -> Result<U256, ProviderError> {
        (**self).read_bitmap(key, word, block).await
    }

    async fn read_tick(
        &self,
        key: &TickWordKey,
        tick: i32,
        block: u64,
    ) -> Result<(U128, i128), ProviderError> {
        (**self).read_tick(key, tick, block).await
    }
}

/// The alloy transport: the same `eth_call` reads the source used to carry
/// inline, now the [`TickWordTransport`] impl the shared loop drives.
impl TickWordTransport for AlloyProvider {
    async fn read_bitmap(
        &self,
        key: &TickWordKey,
        word: i16,
        block: u64,
    ) -> Result<U256, ProviderError> {
        match key {
            TickWordKey::V3 { address, .. } => {
                self.read(
                    *address,
                    IUniswapV3Pool::tickBitmapCall { wordPosition: word },
                    Some(block),
                )
                .await
            }
            TickWordKey::V4 {
                state_view,
                pool_id,
                ..
            } => {
                self.read(
                    *state_view,
                    IUniswapV4StateView::getTickBitmapCall {
                        poolId: (*pool_id).into(),
                        wordPosition: word,
                    },
                    Some(block),
                )
                .await
            }
        }
    }

    async fn read_tick(
        &self,
        key: &TickWordKey,
        tick: i32,
        block: u64,
    ) -> Result<(U128, i128), ProviderError> {
        let liquidity = match key {
            TickWordKey::V3 { address, .. } => {
                TickLiquidity::read(self, *address, tick, Some(block)).await?
            }
            TickWordKey::V4 {
                state_view,
                pool_id,
                ..
            } => TickLiquidity::read_v4(self, *state_view, *pool_id, tick, Some(block)).await?,
        };
        Ok((liquidity.gross, liquidity.net))
    }
}

impl<T: TickWordTransport> TickWordSource for TransportTickWordSource<T> {
    fn fetch_tick_word(
        &self,
        key: &TickWordKey,
        word: i32,
        block: u64,
    ) -> Result<Option<FetchedTickWord>, TickWordFetchError> {
        // The Solidity bitmap selector takes an int16; a word outside that
        // range cannot name a real bitmap slot.
        let word_i16 = i16::try_from(word).map_err(|_| TickWordFetchError::OutOfRange)?;

        // An unfetchable word must surface as a fetch error, not degrade to a
        // silently empty region the walker would cache as authoritative.
        let read = fetch_tick_word_via(&self.transport, key, word_i16, block);

        bridged_block_on(read).map_err(map_provider_err)
    }
}

/// Bucket a [`ProviderError`] into the [`TickWordFetchError`] vocabulary the
/// [`TickWordSource`] boundary speaks: RPC failures (transport, revert,
/// timeout) → [`TickWordFetchError::FetchFailed`]; decode failures →
/// [`TickWordFetchError::InvalidReturn`]. Both are explicitly bucketed — a
/// caller that wants to retry only on RPC errors can match on the variant.
///
/// Takes the error by value to match the `Result::map_err` callsite contract
/// (which moves the error in).
#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err passes the error by value"
)]
#[must_use]
pub fn map_provider_err(err: ProviderError) -> TickWordFetchError {
    match err {
        ProviderError::DecodingError { .. } => TickWordFetchError::InvalidReturn,
        _ => TickWordFetchError::FetchFailed,
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::offline::OfflineProvider;
    use alloy::primitives::{address, Address, U128, U256};
    use alloy::sol_types::SolCall;
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
        let bitmap_calldata = IUniswapV3Pool::tickBitmapCall {
            wordPosition: bitmap_word,
        }
        .abi_encode();
        calls.insert(
            call_key(V3_POOL, &bitmap_calldata),
            Some(word_u256(bitmap_value)),
        );

        // ticks(int24) → (uint128 gross, int128 net)
        for &(tick, gross, net) in tick_responses {
            let td_calldata = IUniswapV3Pool::ticksCall {
                tick: tick.try_into().expect("fixture tick within int24"),
            }
            .abi_encode();
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

        let rpc = AlloyTickWordSource::new(Arc::new(provider));

        let result = rpc
            .fetch_tick_word(
                &TickWordKey::V3 {
                    address: V3_POOL,
                    tick_spacing: 1,
                },
                0,
                100,
            )
            .expect("ok");

        let fetched = result.expect("non-zero bitmap → Some");
        assert_eq!(fetched.word, 0);
        assert_eq!(fetched.ticks.len(), 2);

        let tick_10 = &fetched.ticks[&10];
        assert_eq!(tick_10.liquidity_gross, U128::from(1_000u64));
        assert_eq!(tick_10.liquidity_net, -500i128);
        assert_eq!(tick_10.block, 100);

        let tick_20 = &fetched.ticks[&20];
        assert_eq!(tick_20.liquidity_gross, U128::from(2_000u64));
        assert_eq!(tick_20.liquidity_net, 750i128);
        assert_eq!(tick_20.block, 100);
    }

    #[test]
    fn v3_all_zero_bitmap_returns_none() {
        let provider = v3_offline_provider(0, U256::ZERO, &[], 100);
        let rpc = AlloyTickWordSource::new(Arc::new(provider));

        let result = rpc
            .fetch_tick_word(
                &TickWordKey::V3 {
                    address: V3_POOL,
                    tick_spacing: 1,
                },
                0,
                100,
            )
            .expect("ok");

        assert!(result.is_none(), "all-zero bitmap → None");
    }

    #[test]
    fn v3_unrecorded_tick_bitmap_returns_rpc_err() {
        // Empty recorded data → every eth_call returns "unrecorded".
        let provider = v3_offline_provider(0, U256::ZERO, &[], 100);
        let rpc = AlloyTickWordSource::new(Arc::new(provider));

        // Use a different word position so the bitmap lookup misses the (empty)
        // recorded data.
        let err = rpc
            .fetch_tick_word(
                &TickWordKey::V3 {
                    address: V3_POOL,
                    tick_spacing: 100,
                },
                1,
                100,
            )
            .expect_err("unrecorded eth_call → Err");
        assert_eq!(err, TickWordFetchError::FetchFailed);
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
        let rpc = AlloyTickWordSource::new(Arc::new(provider));

        let result = rpc
            .fetch_tick_word(
                &TickWordKey::V3 {
                    address: V3_POOL,
                    tick_spacing: 1,
                },
                -1,
                100,
            )
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
        let bm_calldata = IUniswapV4StateView::getTickBitmapCall {
            poolId: pool_id.into(),
            wordPosition: 0,
        }
        .abi_encode();
        calls.insert(
            call_key(V4_STATE_VIEW, &bm_calldata),
            Some(word_u256(bitmap)),
        );

        // Encode ticks(int24) — V4 tick at (word<<8 + bit)*spacing = 10.
        let td_calldata = IUniswapV4StateView::getTickLiquidityCall {
            poolId: pool_id.into(),
            tick: 10i32.try_into().expect("fixture tick within int24"),
        }
        .abi_encode();
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
        let rpc = AlloyTickWordSource::new(Arc::new(provider));

        let result = rpc
            .fetch_tick_word(
                &TickWordKey::V4 {
                    state_view: V4_STATE_VIEW,
                    pool_id,
                    tick_spacing: 1,
                },
                0,
                9_999,
            )
            .expect("ok");

        let fetched = result.expect("non-zero bitmap → Some");
        assert_eq!(fetched.word, 0);
        assert_eq!(fetched.ticks.len(), 1);
        let tick_info = &fetched.ticks[&10];
        assert_eq!(tick_info.liquidity_gross, U128::from(42u64));
        assert_eq!(tick_info.liquidity_net, -7i128);
        assert_eq!(tick_info.block, 9_999);
    }

    /// A tick-word read must be callable from a synchronous call site that is
    /// ALREADY inside the multi-threaded runtime (the live-miss backfill runs
    /// on the pump's threads). The bridge there is `block_in_place`; a bare
    /// `block_on` on a runtime from one of its own worker threads panics.
    #[test]
    fn fetch_tick_word_works_from_inside_a_runtime_thread() {
        let provider = v3_offline_provider(0, U256::ZERO, &[], 100);
        let source = AlloyTickWordSource::new(Arc::new(provider));
        let key = TickWordKey::V3 {
            address: V3_POOL,
            tick_spacing: 1,
        };

        let fetched = degenbot_core::runtime::get_runtime().block_on(async move {
            tokio::spawn(async move { source.fetch_tick_word(&key, 0, 100) })
                .await
                .expect("the worker task completes")
        });

        assert!(
            fetched.expect("ok").is_none(),
            "the all-zero bitmap is a known-empty word"
        );
    }

    /// A transport with a scripted bitmap answer. The tick read is
    /// deliberately an error: a bitmap-only verdict that reached it would
    /// fail the assertion instead of silently passing.
    #[derive(Clone)]
    struct ScriptedBitmapTransport {
        bitmap: Result<U256, ProviderError>,
    }

    impl TickWordTransport for ScriptedBitmapTransport {
        fn read_bitmap(
            &self,
            _key: &TickWordKey,
            _word: i16,
            _block: u64,
        ) -> impl Future<Output = Result<U256, ProviderError>> + Send {
            std::future::ready(self.bitmap.clone())
        }

        fn read_tick(
            &self,
            _key: &TickWordKey,
            _tick: i32,
            _block: u64,
        ) -> impl Future<Output = Result<(U128, i128), ProviderError>> + Send {
            std::future::ready(Err(ProviderError::ExecutionReverted {
                code: -32000,
                message: "a bitmap verdict has no tick to read".into(),
            }))
        }
    }

    /// Malformed bitmap return data REFUSES the word: the decode failure
    /// crosses the shared choreography unchanged and buckets to
    /// `InvalidReturn` at the source boundary. Reinterpreting it as empty
    /// tick state would hand every caller a word the provider never served.
    #[test]
    fn malformed_tick_bitmap_refuses_with_the_transport_error_unchanged() {
        let transport = ScriptedBitmapTransport {
            bitmap: Err(ProviderError::DecodingError {
                message: "tickBitmap(int16) decode: malformed return data".into(),
            }),
        };
        let key = TickWordKey::V3 {
            address: V3_POOL,
            tick_spacing: 1,
        };

        let error = degenbot_core::runtime::get_runtime()
            .block_on(fetch_tick_word_via(&transport, &key, 0, 100))
            .expect_err("a malformed bitmap must refuse the word");
        assert!(
            matches!(
                &error,
                ProviderError::DecodingError { message }
                    if message == "tickBitmap(int16) decode: malformed return data"
            ),
            "the decode error must cross the choreography unchanged, got {error:?}"
        );

        let source = TransportTickWordSource::from_transport(transport);
        let refused = source
            .fetch_tick_word(&key, 0, 100)
            .expect_err("the source boundary refuses a decode failure");
        assert_eq!(
            refused,
            TickWordFetchError::InvalidReturn,
            "the decode bucket reaches the boundary as a refusal, never an empty word"
        );
    }

    /// The genuinely empty word stays distinct from the malformed one: a
    /// decoded zero bitmap is a successful read answering `Ok(None)`.
    #[test]
    fn zero_tick_bitmap_is_a_known_empty_word_at_the_shared_choreography() {
        let transport = ScriptedBitmapTransport {
            bitmap: Ok(U256::ZERO),
        };
        let key = TickWordKey::V3 {
            address: V3_POOL,
            tick_spacing: 1,
        };

        let word = degenbot_core::runtime::get_runtime()
            .block_on(fetch_tick_word_via(&transport, &key, 0, 100))
            .expect("a decoded zero bitmap is a successful read");
        assert!(
            word.is_none(),
            "a decoded zero bitmap is a known-empty word"
        );
    }
}
