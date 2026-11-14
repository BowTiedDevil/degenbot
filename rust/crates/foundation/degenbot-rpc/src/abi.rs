//! The typed read verb, the static ABI declarations it decodes against, and
//! the named narrowings a few reads need.
//!
//! Alloy's `sol!` owns calldata and return shape here: a caller names the
//! getter once as a typed call (`IUniswapV2Pair::getReservesCall {}`) and
//! [`ContractRead::read`] turns that into one `eth_call` plus a typed decode.
//! A getter therefore enters this module as one line of an interface
//! declaration, never as an `encode_*`/`decode_*` pair plus a `fetch_*` wrapper
//! that re-assembles the pipeline at every call site.
//!
//! ## Read transport
//!
//! The call is the only step that varies with the transport, so it is the only
//! one abstracted: [`ContractRead::call`] is the one call primitive (GLOSSARY)
//! and [`ContractRead::read`] is its typed form. `&AlloyProvider` remains the
//! straightforward standalone consumer's transport, a shared
//! `&Arc<AlloyProvider>` reads the same, and a construction-side transport (the
//! bot core's handle, its tick-paced view) reaches the identical ABI shapes
//! through the identical code. A transport supplies call policy — pacing,
//! retry, a double's canned bytes — never a second read family.
//!
//! ## Narrowings
//!
//! A few reads answer more (or wider) than the pool snapshot keeps. Those keep
//! a named adapter, so the discarded or widened fields are visible in the type
//! rather than hidden in a bespoke decoder: [`V2Reserves`] (two of
//! `getReserves()`'s three), [`Slot0Prefix`] (the v3 family's shared leading
//! pair), [`BalancerPoolId`] (the raw 32-byte id), [`V4Slot0`] (the state-view
//! answer widened to the widths the pool math carries) and [`TickLiquidity`]
//! (the pair `ticks(int24)` leads with).
//!
//! ## Declared-shape reads
//!
//! Reads whose return shape the *caller* declares — a fee tier decoded at a
//! declared width, the stable/volatile flags the resolver probes — have no
//! getter to declare: the signature is data. They hash it with [`selector`],
//! cross the same [`ContractRead::call`] primitive, and decode to the shape
//! the caller named. That shape is a STATIC Solidity type (`U24`, `I24`,
//! `bool`, `U256`), whose width the decode validates exactly as
//! [`ContractRead::read`] validates a declared return — a live `DynSolType`
//! would carry the same width and lose the check. The ERC-20 metadata family
//! (name/symbol/decimals with its selector fallback and field decodes) has its
//! own home in [`crate::erc20`].
//!
//! # Errors
//!
//! Every read returns [`ProviderError`]: `eth_call` failures surface verbatim
//! (reverts as [`ProviderError::ExecutionReverted`], transport errors
//! classified by the provider); return data that does not decode surfaces as
//! [`ProviderError::DecodingError`]. A word that is not canonical for its
//! declared type is part of "does not decode" on every read THIS module
//! declares: [`ContractRead::read`] validates the `sol!` return type, the
//! declared-shape helpers ([`fetch_no_arg_value`],
//! [`fetch_address_returning`], [`fetch_uint256_field`]) validate the static
//! type their caller named in the signature, and the narrowings that take raw
//! bytes ([`Slot0Prefix`], [`TickLiquidity`]) validate the words they keep.
//! The claim stops there: an answer whose shape no declaration here names —
//! the ERC-20 metadata family in [`crate::erc20`], which classifies between a
//! `string`, a `bytes32` and a whole-word integer under its own fall-through
//! contract, and the storage-word paths outside this module — keeps its own
//! decode policy and inherits nothing from this section. No swallowing —
//! callers decide retry/policy.

use std::future::Future;
use std::sync::Arc;

use alloy::primitives::{address, Address, Bytes, I256, U128, U160, U256};
use alloy::sol_types::{SolCall, SolType, SolValue};
use degenbot_core::errors::{ProviderError, ProviderResult};

use crate::provider::AlloyProvider;

/// The one contract-read primitive: one `eth_call(to, data, block)`, returning
/// the raw return bytes — plus [`read`](ContractRead::read), the typed form.
///
/// `call` is the only step that touches a transport, so naming it lets the
/// whole read family be generic over the transport while the ABI knowledge
/// (selectors, calldata, return shapes) stays in the `sol!` declarations below,
/// written once: the same read runs through an [`AlloyProvider`], through a
/// provider shared behind an `Arc`, and through a construction-side transport
/// that wraps the call in its own policy.
///
/// # Errors
///
/// Each implementation returns its transport's own [`ProviderError`];
/// [`read`](ContractRead::read) adds [`ProviderError::DecodingError`] for return
/// data that does not decode.
pub trait ContractRead: Send + Sync {
    /// Issue one `eth_call` at `block` (`None` = the transport's own latest) and
    /// return the raw return bytes.
    fn call(
        &self,
        to: Address,
        data: Bytes,
        block: Option<u64>,
    ) -> impl Future<Output = ProviderResult<Bytes>> + Send;

    /// Call the getter `call` names and decode its return into the typed
    /// answer: encode the calldata, one `eth_call`, decode `C::Return` with
    /// Alloy's VALIDATING decoder.
    ///
    /// Validation is the policy for every read that crosses THIS method, and it
    /// is what makes the declared width the answer's bound: a `uint112` word
    /// with bits above position 112 set, a `bool` other than 0/1, or an
    /// `address` word with a dirty high twelve bytes is a decode FAILURE, not a
    /// value whose excess bits are dropped. Decoding without validation
    /// discards those bits, so an over-wide answer arrives as a plausible small
    /// number indistinguishable from real state.
    ///
    /// Reads that assemble their own answer do not cross this method, and none
    /// of them decodes unvalidated: a declared-shape helper
    /// ([`fetch_no_arg_value`], [`fetch_address_returning`]) reads through
    /// [`ContractRead::call`] and validates the static type its caller named,
    /// and a narrowing adapter that must keep a tail or a prefix
    /// ([`Slot0Prefix`], [`TickLiquidity`]) validates the words it reads and
    /// leaves the rest unread.
    ///
    /// Validation is not STRICT decoding: trailing bytes past the declared
    /// tuple stay accepted, so a getter whose answer leads with the fields a
    /// snapshot keeps — and a transport answering more words than the tuple
    /// declares — reads unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return data does not decode as
    /// the getter's declared return type.
    fn read<C: SolCall>(
        &self,
        to: Address,
        call: C,
        block: Option<u64>,
    ) -> impl Future<Output = ProviderResult<C::Return>> + Send {
        let calldata = Bytes::from(call.abi_encode());
        async move {
            let data = self.call(to, calldata, block).await?;
            C::abi_decode_returns_validate(&data).map_err(|e| ProviderError::DecodingError {
                message: format!("{} decode: {e}", C::SIGNATURE),
            })
        }
    }
}

/// The provider is the primitive's native transport: one hardened `eth_call`
/// (retry/backoff, revert classification) per read.
impl ContractRead for AlloyProvider {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        self.eth_call(&to, data, block).await
    }
}

/// A shared transport reads exactly like the transport it points at, so a
/// caller holding `&Arc<AlloyProvider>` — or any other shared handle that itself
/// implements this trait — reaches the read family without cloning or
/// unwrapping.
impl<T: ContractRead + ?Sized> ContractRead for Arc<T> {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        (**self).call(to, data, block).await
    }
}

alloy::sol! {
    /// ERC-20 token interface (read methods used by the fetchers).
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256 balance);
        function allowance(address owner, address spender) external view returns (uint256 remaining);
        function totalSupply() external view returns (uint256 totalSupply);
    }

    /// Uniswap-V2-style pair interface.
    interface IUniswapV2Pair {
        function getReserves()
            external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    /// Aerodrome V2 factory read: the per-pool unidirectional fee, answered per
    /// (pool, stable) pair because the volatile and stable pools share one
    /// factory.
    interface IAerodromeFactory {
        function getFee(address pool, bool stable) external view returns (uint256);
    }

    /// Uniswap-V3-style pool interface.
    interface IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function liquidity() external view returns (uint128 liquidity);
        function tickBitmap(int16 wordPosition) external view returns (uint256 word);
        function ticks(int24 tick) external view returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128,
            int56 tickCumulativeOutside,
            uint160 secondsPerLiquidityOutsideX128,
            uint32 secondsOutside,
            bool initialized
        );
    }

    /// Uniswap-V4 state-view interface.
    interface IUniswapV4StateView {
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
        function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
        function getTickBitmap(bytes32 poolId, int16 wordPosition) external view returns (uint256 word);
        function getTickLiquidity(bytes32 poolId, int24 tick) external view returns (uint128 gross, int128 net);
    }

    /// Balancer V2 pool interface (the read methods the builder issues).
    interface IBalancerPool {
        function getPoolId() external view returns (bytes32);
        function getSwapFeePercentage() external view returns (uint256);
        function getAmplificationParameter() external view returns (uint256 value, bool isUpdating, uint256 precision);
        function getNormalizedWeights() external view returns (uint256[] memory);
        function getRateProviders() external view returns (address[] memory);
        function getRate() external view returns (uint256);
    }

    /// Balancer V2 singleton Vault interface (pool-token reads).
    interface IBalancerVault {
        function getPoolTokens(bytes32 poolId) external view returns (address[] memory tokens, uint256[] memory balances, uint256 lastChangeBlock);
    }

    /// Curve `StableSwap` pool read interface (all pool-state detection reads).
    /// The `uint256`-prototype coin/balance getters; the `int128` prototype is
    /// declared separately in `ICurvePoolInt128` so alloy's `sol!` does not need
    /// to disambiguate overloads.
    interface ICurvePool {
        function coins(uint256 i) external view returns (address);
        function balances(uint256 i) external view returns (uint256);
        function admin_balances(uint256 i) external view returns (uint256);
        function price_scale(uint256 k) external view returns (uint256);
        function A() external view returns (uint256);
        function fee() external view returns (uint256);
        function admin_fee() external view returns (uint256);
        function D() external view returns (uint256);
        function get_virtual_price() external view returns (uint256);
        function base_virtual_price() external view returns (uint256);
        function base_cache_updated() external view returns (uint256);
        function oracle_method() external view returns (uint256);
        function redemption_price_snap() external view returns (address);
        function snappedRedemptionPrice() external view returns (uint256);
        function initial_A() external view returns (uint256);
        function initial_A_time() external view returns (uint256);
        function future_A() external view returns (uint256);
        function future_A_time() external view returns (uint256);
        function fee_gamma() external view returns (uint256);
        function mid_fee() external view returns (uint256);
        function out_fee() external view returns (uint256);
        function gamma() external view returns (uint256);
        function offpeg_fee_multiplier() external view returns (uint256);
        function base_pool() external view returns (address);
    }

    /// Curve `StableSwap` coin/balance reads under the `int128` prototype
    /// (older pools). Kept as a separate interface so the overloaded
    /// `coins`/`balances` names land in distinct `SolCall` types.
    interface ICurvePoolInt128 {
        function coins(int128 i) external view returns (address);
        function balances(int128 i) external view returns (uint256);
    }

    /// Curve registry / factory read interface (LP token, metapool, coins).
    interface ICurveRegistry {
        function get_lp_token(address pool) external view returns (address);
        function is_meta(address pool) external view returns (bool);
        function get_underlying_coins(address pool) external view returns (address[8] memory);
        function get_base_pool(address pool) external view returns (address);
    }

    /// Balancer V2 pool-factory membership read (every factory revision
    /// answers it; `getPoolCount` / `getPools` / `getPoolAt` revert).
    interface IBalancerPoolFactory {
        function isPoolFromFactory(address pool) external view returns (bool);
    }

    /// Lending-token detection reads (cToken / yToken probes).
    interface ILendingToken {
        function isCToken() external view returns (bool);
        function underlying() external view returns (address);
        function token() external view returns (address);
    }

    /// Curve lending-style rate reads: the per-coin rate getters behind the
    /// `CTOKEN` / `CYTOKEN` accrual, `YTOKEN` share price, and `AETH` / `RETH`
    /// exchange-rate styles.
    interface ILendingRateReads {
        function exchangeRateStored() external view returns (uint256);
        function supplyRatePerBlock() external view returns (uint256);
        function accrualBlockNumber() external view returns (uint256);
        function getPricePerFullShare() external view returns (uint256);
        function ratio() external view returns (uint256);
        function getExchangeRate() external view returns (uint256);
    }

    /// Pool identity getters. Declared apart from `IUniswapV2Pair` /
    /// `IUniswapV3Pool` because these reads run before a pool's family is
    /// known: the caller holds an address and nothing else.
    interface IPoolIdentity {
        function factory() external view returns (address);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
    /// Uniswap-V3-periphery `Quoter`: the canonical exact-input single-pool
    /// read for a v3-family pool. The quoter performs the swap itself (inside
    /// its own `try`/`catch`) and returns the pool's own answer, so the amount
    /// it reports is the chain's arithmetic, never this crate's.
    interface IUniswapV3Quoter {
        function quoteExactInputSingle(
            address tokenIn,
            address tokenOut,
            uint24 fee,
            uint256 amountIn,
            uint160 sqrtPriceLimitX96
        ) external returns (uint256 amountOut);
    }

    /// Uniswap-V4-periphery `Quoter`: the execute-then-answer single-pool read
    /// over the canonical `PoolManager`.
    ///
    /// Its `PoolKey` carries the HOOK address, so a hooked pool is priced with
    /// its hook attached — the reason a hooked leg must never be priced by
    /// in-process math (ADR-090 §1/§2).
    interface IUniswapV4Quoter {
        struct PoolKey {
            address currency0;
            address currency1;
            uint24 fee;
            int24 tickSpacing;
            address hooks;
        }

        struct QuoteExactSingleParams {
            PoolKey poolKey;
            bool zeroForOne;
            uint128 exactAmount;
            bytes hookData;
        }

        function quoteExactInputSingle(QuoteExactSingleParams params)
            external
            returns (uint256 amountOut, uint256 gasEstimate);
    }
}

// ---------------------------------------------------------------------------
// Narrowings: the reads whose answer is wider than the snapshot keeps
// ---------------------------------------------------------------------------

/// A V2-style pair's reserves: `getReserves()`'s first two words.
///
/// The third field — `blockTimestampLast` — is unused by the snapshot, and a
/// pool that answered a stale or zero timestamp must still read the same. It is
/// validated all the same: a word above `uint32` fails the decode whether or not
/// a consumer keeps it.
///
/// Both words are `uint112` on the pair, so the declaration is the bound: the
/// validating decode behind [`ContractRead::read`] refuses a word wider than the
/// field instead of truncating it into a plausible reserve.
///
/// The fields are `U256` because that is the width the consumers' swap math
/// carries, not because a wider word can arrive: [`Self::read`] is the only
/// production constructor, and it cannot yield a word above `uint112`. A
/// narrowing projection downstream (`reserve0.to::<U112>()`) relies on exactly
/// that constructor invariant instead of re-checking the answer — a struct
/// literal is what would make such a projection panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Reserves {
    /// `getReserves()`'s first word, widened from the pair's `uint112` field.
    pub reserve0: U256,
    /// `getReserves()`'s second word, widened from the pair's `uint112` field.
    pub reserve1: U256,
}

impl V2Reserves {
    /// Read `getReserves()` from `pool` and keep the two reserve words.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return is not the
    /// `(uint112, uint112, uint32)` tuple — which includes either reserve word
    /// being wider than its `uint112` field, and a timestamp word wider than
    /// `uint32` even though no caller keeps it.
    pub async fn read(
        transport: &impl ContractRead,
        pool: Address,
        block: Option<u64>,
    ) -> ProviderResult<Self> {
        let r = transport
            .read(pool, IUniswapV2Pair::getReservesCall {}, block)
            .await?;
        Ok(Self {
            reserve0: U256::from(r.reserve0),
            reserve1: U256::from(r.reserve1),
        })
    }
}

/// A Balancer pool's `getPoolId()` identifier as the raw 32 bytes the Vault's
/// `getPoolTokens(bytes32)` takes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalancerPoolId(pub [u8; 32]);

impl BalancerPoolId {
    /// Read `getPoolId()` from `pool`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return is not one `bytes32`
    /// word.
    pub async fn read(
        transport: &impl ContractRead,
        pool: Address,
        block: Option<u64>,
    ) -> ProviderResult<Self> {
        let id = transport
            .read(pool, IBalancerPool::getPoolIdCall {}, block)
            .await?;
        Ok(Self(id.0))
    }
}

/// A v3-family pool's `slot0()` leading pair: `sqrtPriceX96` and `tick`.
///
/// The v3 family's returns share that two-word static prefix and differ in the
/// tail (Uniswap/Pancake answer seven words, the Aerodrome Slipstream pools
/// six — `ICLPoolState` has no `feeProtocol` field), so the snapshot reads the
/// shared prefix and leaves the tail unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot0Prefix {
    pub sqrt_price_x96: U256,
    pub tick: I256,
}

impl Slot0Prefix {
    /// Read `slot0()` from `pool` and keep the leading pair.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return is shorter than the
    /// two-word prefix or either word is not canonical for its declared width
    /// (a `uint160` with its high bits set, an `int24` that is not
    /// sign-extended). A longer tail is unread and unvalidated.
    pub async fn read(
        transport: &impl ContractRead,
        pool: Address,
        block: Option<u64>,
    ) -> ProviderResult<Self> {
        let calldata = Bytes::from(IUniswapV3Pool::slot0Call {}.abi_encode());
        let data = transport.call(pool, calldata, block).await?;
        Self::decode(&data)
    }

    /// The decode half of the narrowing, for a caller that obtains the bytes
    /// through a batch (`multicall3`) rather than through
    /// [`read`](Self::read).
    ///
    /// The declaration is the bound: the leading `(uint160, int24)` pair is
    /// decoded by Alloy's VALIDATING decoder, so an over-wide price word or a
    /// tick word that is not sign-extended is a decode failure rather than a
    /// truncated price or a flipped tick. The tail the v3 family differs in is
    /// left unread, so every answer at two words or longer reads.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::DecodingError`] when the bytes are shorter than
    /// the pair or either word is not canonical for its declared width.
    fn decode(bytes: &[u8]) -> ProviderResult<Self> {
        type Prefix = (U160, alloy::primitives::aliases::I24);
        let (sqrt_price_x96, tick) =
            <Prefix as SolValue>::abi_decode_validate(bytes).map_err(|e| {
                ProviderError::DecodingError {
                    message: format!("slot0 prefix decode: {e}"),
                }
            })?;
        Ok(Self {
            sqrt_price_x96: U256::from(sqrt_price_x96),
            tick: widen_int24(tick.as_i32()),
        })
    }
}

/// A Uniswap-V4 pool's `getSlot0(bytes32)` answer at the widths the pool math
/// carries.
///
/// The state-view contract answers `uint160`/`int24`/`uint24`/`uint24`; the
/// pool snapshot keeps the price and tick at `U256`/`I256` and both fee words
/// at `U256`, so the adapter widens once here rather than at every reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4Slot0 {
    pub sqrt_price_x96: U256,
    pub tick: I256,
    pub protocol_fee: U256,
    pub lp_fee: U256,
}

impl V4Slot0 {
    /// Read `getSlot0(bytes32)` from the `state_view` contract.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return is not the four-word
    /// state-view tuple.
    pub async fn read(
        transport: &impl ContractRead,
        state_view: Address,
        pool_id: [u8; 32],
        block: Option<u64>,
    ) -> ProviderResult<Self> {
        let r = transport
            .read(
                state_view,
                IUniswapV4StateView::getSlot0Call {
                    poolId: pool_id.into(),
                },
                block,
            )
            .await?;
        Ok(Self {
            sqrt_price_x96: U256::from(r.sqrtPriceX96),
            tick: widen_int24(r.tick.as_i32()),
            protocol_fee: U256::from(r.protocolFee),
            lp_fee: U256::from(r.lpFee),
        })
    }
}

/// A tick's liquidity: `ticks(int24)`'s leading `(liquidityGross,
/// liquidityNet)` pair, or the state view's whole `getTickLiquidity` answer.
///
/// The v3 `ticks(int24)` tuple carries six more words (fee growth, cumulative
/// and seconds-outside accumulators, `initialized`) that the tick snapshot does
/// not keep; only the first two are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickLiquidity {
    pub gross: U128,
    pub net: i128,
}

impl TickLiquidity {
    /// Read `ticks(int24)` from a v3-family `pool`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return is shorter than the
    /// leading pair or either word is not canonical for its declared width
    /// (see [`decode`](Self::decode)).
    pub async fn read(
        transport: &impl ContractRead,
        pool: Address,
        tick: i32,
        block: Option<u64>,
    ) -> ProviderResult<Self> {
        let call = IUniswapV3Pool::ticksCall { tick: int24(tick)? };
        let calldata = Bytes::from(call.abi_encode());
        let data = transport.call(pool, calldata, block).await?;
        Self::decode(&data)
    }

    /// Read `getTickLiquidity(bytes32,int24)` from the V4 `state_view`
    /// contract.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` or
    /// [`ProviderError::DecodingError`] when the return is not the
    /// `(uint128, int128)` pair.
    pub async fn read_v4(
        transport: &impl ContractRead,
        state_view: Address,
        pool_id: [u8; 32],
        tick: i32,
        block: Option<u64>,
    ) -> ProviderResult<Self> {
        let call = IUniswapV4StateView::getTickLiquidityCall {
            poolId: pool_id.into(),
            tick: int24(tick)?,
        };
        let r = transport.read(state_view, call, block).await?;
        Ok(Self {
            gross: U128::from(r.gross),
            net: r.net,
        })
    }

    /// Decode `ticks(int24)` return data into the leading pair.
    ///
    /// The decode half of the narrowing, for a caller that obtains the bytes
    /// through a batch (`multicall3`) rather than through
    /// [`read`](Self::read).
    ///
    /// The declaration is the bound here too: the leading pair is decoded as a
    /// `(uint128, int128)` tuple by Alloy's VALIDATING decoder, so a gross word
    /// with bits above 128 set and a net word that is not a sign-extended
    /// `int128` are decode failures rather than a truncated gross or a flipped
    /// net. The six words past the pair that the v3 `ticks(int24)` tuple
    /// carries are left unread, so a two-word answer and the full eight-word
    /// answer both read — which is what lets the same decode serve a pool that
    /// answers only the pair and the real contract.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::DecodingError`] when the bytes are shorter than
    /// the leading pair or either word is not canonical for its declared width.
    pub fn decode(bytes: &[u8]) -> ProviderResult<Self> {
        let (gross, net) = <(u128, i128) as SolValue>::abi_decode_validate(bytes).map_err(|e| {
            ProviderError::DecodingError {
                message: format!("ticks(int24) decode: {e}"),
            }
        })?;
        Ok(Self {
            gross: U128::from(gross),
            net,
        })
    }
}

/// The canonical mainnet Uniswap-V3-periphery `Quoter` — the deployment the
/// V3 exact-input quote reads (`quoteExactInputSingle`).
///
/// Its `quoteExactInputSingle` performs the pool's own `swap` inside a
/// `try`/`catch` and returns the amount the pool produced, so a quote is the
/// chain's answer at that block rather than a model of it.
pub const V3_QUOTER: Address = address!("b27308F9F90D607463bb33eA1BeBb41C27CE5AB6");

/// The canonical mainnet Uniswap-V4-periphery `Quoter` — the deployment the V4
/// exact-input quote reads, bound immutably to the canonical `PoolManager`.
///
/// The same deployment `degenbot_simulation::sim::evm2::v4_black_box` drives
/// inside an anchor-pinned scratch EVM; there the frame is local, here the
/// answer is the node's at a pinned block.
pub const V4_QUOTER: Address = address!("52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203");

/// `TickMath.MIN_SQRT_RATIO + 1` — the lowest price a `zeroForOne` swap may
/// walk to.
pub const MIN_SQRT_RATIO_PLUS_ONE: U256 = U256::from_limbs([4_295_128_740, 0, 0, 0]);

/// `TickMath.MAX_SQRT_RATIO - 1` — the highest price a `oneForZero` swap may
/// walk to.
pub const MAX_SQRT_RATIO_MINUS_ONE: U256 = U256::from_limbs([
    6_743_328_256_752_651_557,
    17_280_870_778_742_802_505,
    4_294_805_859,
    0,
]);

/// The `sqrtPriceLimitX96` a V3 quote hands the quoter for one direction.
///
/// `Quoter.quoteExactInputSingle` substitutes `MIN_SQRT_RATIO + 1` /
/// `MAX_SQRT_RATIO - 1` when its caller passes `0` ("no limit"). Encoding the
/// substituted value directly is the SAME quote — the substitution is the
/// quoter's own arithmetic — and it keeps the calldata byte-stable for every
/// caller of this seam (the recorded `py_oracle` quoter corpora carry exactly
/// these words), so a replay transport answers it.
#[must_use]
pub fn v3_quote_sqrt_price_limit(zero_for_one: bool) -> U160 {
    if zero_for_one {
        U160::from_limbs([4_295_128_740, 0, 0])
    } else {
        U160::from_limbs([
            6_743_328_256_752_651_557,
            17_280_870_778_742_802_505,
            4_294_805_859,
        ])
    }
}

/// One V3 exact-input single-pool quote: the pool's fee tier, the direction's
/// token pair, and the canonical quoter's answer.
///
/// The amount is the quoter's, never the concentrated-liquidity walk's, so a
/// ramp of ticks, a fee-tier override, or a pool that the in-process math
/// models imperfectly all price the way the chain does at that block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3Quote {
    /// The pool's fee tier (hundredths of a bip).
    pub fee: u32,
    /// The output amount the quoter reported.
    pub amount_out: U256,
}

/// One V3 exact-input single-pool quote request: the pool's fee tier, the
/// direction's token pair, the exact input, and the block it prices at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3QuoteRequest {
    /// The token the hop spends (the pool's `token0` or `token1`).
    pub token_in: Address,
    /// The token the hop receives.
    pub token_out: Address,
    /// The pool's fee tier (hundredths of a bip).
    pub fee: u32,
    /// The exact input, in `token_in`'s units.
    pub amount_in: U256,
    /// `true` when `token_in` is the pool's `token0`.
    pub zero_for_one: bool,
    /// The block the quote prices at.
    pub block: u64,
}

impl V3Quote {
    /// Quote one request through `quoter` at the request's block.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` (a quoter
    /// revert — a partial fill, a stalled price limit — surfaces as
    /// [`ProviderError::ExecutionReverted`]), [`ProviderError::InvalidParams`]
    /// when the fee tier does not fit the quoter's `uint24` field, or
    /// [`ProviderError::DecodingError`] when the return is not one word.
    pub async fn quote_exact_in(
        transport: &impl ContractRead,
        quoter: Address,
        request: &V3QuoteRequest,
    ) -> ProviderResult<Self> {
        let call = IUniswapV3Quoter::quoteExactInputSingleCall {
            tokenIn: request.token_in,
            tokenOut: request.token_out,
            fee: request
                .fee
                .try_into()
                .map_err(|_| ProviderError::InvalidParams {
                    message: format!("fee {} outside the uint24 range", request.fee),
                })?,
            amountIn: request.amount_in,
            sqrtPriceLimitX96: v3_quote_sqrt_price_limit(request.zero_for_one),
        };
        let amount_out = transport.read(quoter, call, Some(request.block)).await?;
        Ok(Self {
            fee: request.fee,
            amount_out,
        })
    }
}

/// One V4 exact-input single-pool quote over the canonical quoter: the pool's
/// key (HOOK INCLUDED), the direction, and the quoter's answer.
///
/// The key is carried whole rather than rebuilt from parts: a V4 pool's
/// identity IS its key, and its hook address is part of it, so a quote can never
/// silently price a hooked pool as its unhooked twin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V4Quote {
    /// The pool's immutable key (currencies, fee, tick spacing, hook address).
    pub pool_key: degenbot_pools::V4PoolKey,
    /// The output amount the quoter reported.
    pub amount_out: U256,
    /// The gas the quoter's own swap frame measured.
    pub gas_estimate: U256,
}

/// One V4 exact-input single-pool quote request: the pool's key (HOOK
/// INCLUDED), the direction, the exact input, and the block it prices at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V4QuoteRequest {
    /// The pool's immutable key — currencies, fee, tick spacing, hook address.
    pub pool_key: degenbot_pools::V4PoolKey,
    /// `true` when currency0 is the input.
    pub zero_for_one: bool,
    /// The exact input, in the input currency's units.
    pub amount_in: U256,
    /// The block the quote prices at.
    pub block: u64,
}

impl V4Quote {
    /// Quote one request through `quoter` at the request's block.
    ///
    /// `hook_data` is empty: a quoter call cannot supply the hook data a real
    /// swap would carry, and the canonical quoter prices the pool with its hook
    /// attached regardless (that is the point of quoting it rather than
    /// modelling it).
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] from the underlying `eth_call` (a quoter
    /// revert — a hook that demands settlement, a pool with no liquidity —
    /// surfaces as [`ProviderError::ExecutionReverted`]), or
    /// [`ProviderError::InvalidParams`] when `amount_in` does not fit the
    /// quoter's `uint128` field or the key's fee does not fit `uint24`.
    pub async fn quote_exact_in(
        transport: &impl ContractRead,
        quoter: Address,
        request: &V4QuoteRequest,
    ) -> ProviderResult<Self> {
        let pool_key = &request.pool_key;
        let exact_amount =
            u128::try_from(request.amount_in).map_err(|_| ProviderError::InvalidParams {
                message: format!(
                    "V4 quote amount {} outside the uint128 range",
                    request.amount_in
                ),
            })?;
        let call = IUniswapV4Quoter::quoteExactInputSingleCall {
            params: IUniswapV4Quoter::QuoteExactSingleParams {
                poolKey: IUniswapV4Quoter::PoolKey {
                    currency0: pool_key.currency0,
                    currency1: pool_key.currency1,
                    fee: pool_key
                        .fee
                        .try_into()
                        .map_err(|_| ProviderError::InvalidParams {
                            message: format!("fee {} outside the uint24 range", pool_key.fee),
                        })?,
                    tickSpacing: int24(pool_key.tick_spacing)?,
                    hooks: pool_key.hooks,
                },
                zeroForOne: request.zero_for_one,
                exactAmount: exact_amount,
                hookData: Bytes::new(),
            },
        };
        let answer = transport.read(quoter, call, Some(request.block)).await?;
        Ok(Self {
            pool_key: request.pool_key.clone(),
            amount_out: answer.amountOut,
            gas_estimate: answer.gasEstimate,
        })
    }
}

/// Widen a signed 24-bit `tick` value (already extracted as `i32`) into
/// `I256`, preserving sign. The value always fits in `I256`; `expect` is safe.
fn widen_int24(tick: i32) -> I256 {
    #[expect(clippy::expect_used)] // int24 -> i128 -> I256 always fits (documented)
    let widened = I256::try_from(i128::from(tick)).expect("int24 value always fits in I256");
    widened
}

/// Narrow a tick to the ABI's `int24` argument width.
///
/// # Errors
///
/// Returns [`ProviderError::InvalidParams`] when `tick` is outside the signed
/// 24-bit range.
fn int24(tick: i32) -> ProviderResult<alloy::primitives::aliases::I24> {
    tick.try_into().map_err(|_| ProviderError::InvalidParams {
        message: format!("tick {tick} outside the int24 range"),
    })
}

// ---------------------------------------------------------------------------
// Signature-driven reads
//
// Reads whose shape the caller declares rather than the ABI. Every
// contract-fixed, single-shape getter has a typed `sol!` home above; what is
// left here is the reads where the *decode* is the caller's decision:
//
// - ERC-20 metadata read through prototype lists (`name()`/`NAME()`,
//   `symbol()`/`SYMBOL()`, `decimals()`/`DECIMALS()`), where a token may answer
//   with `string`, `bytes32`, or a word whose width only a fall-through shape
//   can classify.
// - The concentrated-liquidity fee tier and tick spacing, decoded at a declared
//   width (`U24` / `I24`) so an out-of-width answer is a decode failure rather
//   than a truncated value.
// - The solidly-family stable-vs-volatile flags (`stable()`, `stableSwap()`) and
//   Camelot's fee probes (`FEE_DENOMINATOR()`, `token0FeePercent()`,
//   `token1FeePercent()`), read by the resolver that classifies the pool and by
//   the construction path that prices it (a stable pool's fee is per-pool).
//
// The caller passes the Solidity signature (`b"decimals()"`, …), which is hashed
// to the 4-byte selector here, called once through the read primitive, and
// decoded to the named STATIC type — the width rides that type, so the decode
// validates it ([`fetch_no_arg_value`]). Test transports key their canned
// answers on [`selector`] for the same reason: the double asserts the calldata
// the chain would receive, whether the caller encoded it from a typed home or
// from signature text.
// ---------------------------------------------------------------------------

/// Compute the 4-byte Solidity function selector, `keccak256(signature)[..4]`.
///
/// The signature is the canonical Solidity text (`b"decimals()"`,
/// `b"allowance(address,address)"`), never a pre-computed hex selector.
#[must_use]
pub fn selector(signature: &[u8]) -> [u8; 4] {
    let hash = alloy::primitives::keccak256(signature);
    let mut s = [0u8; 4];
    s.copy_from_slice(&hash[..4]);
    s
}

/// The decode failure a declared-shape read reports: the signature the caller
/// asked for, then the decoder's own reason.
fn declared_decode_failure(signature: &[u8], error: &alloy::sol_types::Error) -> ProviderError {
    ProviderError::DecodingError {
        message: format!("{} decode: {error}", String::from_utf8_lossy(signature)),
    }
}

/// Call the no-argument `signature` on `to` and decode the `address` it returns
/// — the pipeline for an address getter whose shape the caller declares (the
/// [`IPoolIdentity`] family is the typed alternative).
///
/// The word crosses Alloy's VALIDATING `address` decoder, so the padding is
/// part of the type: a word whose high twelve bytes are dirty is a decode
/// failure rather than an address assembled from bits the getter never
/// answered, and a return shorter than one word fails the same way.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] when the return is not one canonical
/// `address` word.
pub async fn fetch_address_returning(
    provider: &impl ContractRead,
    signature: &[u8],
    to: &Address,
    block: Option<u64>,
) -> ProviderResult<Address> {
    let calldata = Bytes::copy_from_slice(&selector(signature));
    let bytes = provider.call(*to, calldata, block).await?;
    Address::abi_decode_validate(&bytes).map_err(|e| declared_decode_failure(signature, &e))
}

/// Call the no-argument `signature` on `to` and decode its answer as the STATIC
/// Solidity type `S` — `U24` for a fee tier, `I24` for a tick spacing, `U256`
/// for a full-word getter, `bool` for a flag.
///
/// `S` is both the shape the caller declares and the bound the decode enforces:
/// the word is read through [`ContractRead::call`] and decoded by Alloy's
/// validating decoder for `S`'s static `SolType` (the same policy
/// [`ContractRead::read`] applies to a declared return). A `U24` getter
/// answering bit 24 set, an `I24` answer whose word is not sign-extended, and a
/// `bool` word outside `{0, 1}` are therefore decode failures, never values
/// whose excess bits were dropped. Words past `S` stay accepted, so a getter
/// answering more than the caller reads still reads.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] when the return does not decode as `S` at
/// its declared width.
pub async fn fetch_no_arg_value<S>(
    provider: &impl ContractRead,
    signature: &[u8],
    to: &Address,
    block: Option<u64>,
) -> ProviderResult<S>
where
    S: SolValue,
    S: From<<S::SolType as SolType>::RustType>,
{
    let calldata = Bytes::copy_from_slice(&selector(signature));
    let bytes = provider.call(*to, calldata, block).await?;
    S::abi_decode_validate(&bytes).map_err(|e| declared_decode_failure(signature, &e))
}

/// Call the no-argument `signature` on `to` and decode the first return word as
/// a `uint256` (`decimals()` / `DECIMALS()`, and any other single-word getter
/// whose declared width is the whole word).
///
/// This is [`fetch_no_arg_value`] at `S = U256`: the full word needs no padding
/// check beyond "one whole word" — every 32-byte word is a canonical `uint256`
/// — and words past it stay unread.
///
/// # Errors
///
/// Returns [`ProviderError`] from the underlying `eth_call` or
/// [`ProviderError::DecodingError`] when the return is shorter than one word.
pub async fn fetch_uint256_field(
    provider: &impl ContractRead,
    signature: &[u8],
    to: &Address,
    block: Option<u64>,
) -> ProviderResult<U256> {
    fetch_no_arg_value(provider, signature, to, block).await
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use alloy::dyn_abi::{DynSolType, DynSolValue};
    use alloy::primitives::aliases::{I24, U112, U24};

    const TO: Address = alloy::primitives::address!("00000000000000000000000000000000000000a1");
    const OTHER: Address = alloy::primitives::address!("00000000000000000000000000000000000000b2");
    const BLOCK: Option<u64> = Some(21_000_000);

    /// A calldata-keyed transport double: one canned answer (or canned error)
    /// per encoded calldata, plus the calls it received. The interface is the
    /// test surface — every read below crosses this one `call`.
    #[derive(Default)]
    struct TableTransport {
        answers: HashMap<Vec<u8>, Result<Bytes, ProviderError>>,
        calls: Mutex<Vec<(Address, Bytes, Option<u64>)>>,
    }

    impl TableTransport {
        fn new() -> Self {
            Self::default()
        }

        fn answer(mut self, calldata: Vec<u8>, response: Vec<u8>) -> Self {
            self.answers.insert(calldata, Ok(Bytes::from(response)));
            self
        }

        fn fail(mut self, calldata: Vec<u8>, error: ProviderError) -> Self {
            self.answers.insert(calldata, Err(error));
            self
        }

        fn calls(&self) -> Vec<(Address, Bytes, Option<u64>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ContractRead for TableTransport {
        fn call(
            &self,
            to: Address,
            data: Bytes,
            block: Option<u64>,
        ) -> impl Future<Output = ProviderResult<Bytes>> + Send {
            self.calls.lock().unwrap().push((to, data.clone(), block));
            let answer = self.answers.get(data.as_ref()).cloned().unwrap_or_else(|| {
                Err(ProviderError::ExecutionReverted {
                    code: -32000,
                    message: "no canned answer".into(),
                })
            });
            std::future::ready(answer)
        }
    }

    /// A `uint256` return word.
    fn uint_return(v: U256) -> Vec<u8> {
        DynSolValue::Uint(v, 256).abi_encode()
    }

    /// One raw ABI word holding `v` in its low four bytes — the shape a narrow
    /// field arrives in when its padding is the thing under test.
    fn word_of(v: u32) -> Vec<u8> {
        let mut word = [0u8; 32];
        word[28..32].copy_from_slice(&v.to_be_bytes());
        word.to_vec()
    }

    /// A `uint256` return of `bits` width (right-aligned in the word).
    fn narrow_uint_return(v: U256, bits: usize) -> Vec<u8> {
        DynSolValue::Uint(v, bits).abi_encode()
    }

    /// A `getReserves()` return at FULL word width — the bytes a pair answers,
    /// and the shape an over-wide reserve arrives in (a `uint112`-width encode
    /// could not express one).
    fn get_reserves_return(reserve0: U256, reserve1: U256) -> Vec<u8> {
        get_reserves_return_at(reserve0, reserve1, U256::from(1_700_000_000u64))
    }

    /// The same answer with the timestamp word as given — also at full width, so
    /// a timestamp past `uint32` can be expressed at all.
    fn get_reserves_return_at(reserve0: U256, reserve1: U256, timestamp: U256) -> Vec<u8> {
        DynSolValue::Tuple(vec![
            DynSolValue::Uint(reserve0, 256),
            DynSolValue::Uint(reserve1, 256),
            DynSolValue::Uint(timestamp, 256),
        ])
        .abi_encode()
    }

    /// `2^112`: one past the pair's reserve field.
    fn two_pow_112() -> U256 {
        U256::from(1u8) << 112
    }

    /// The canonical seven-word `slot0()` return.
    fn slot0_return(sqrt: U256, tick: i32) -> Vec<u8> {
        DynSolValue::Tuple(vec![
            DynSolValue::Uint(sqrt, 160),
            DynSolValue::Int(I256::try_from(i128::from(tick)).unwrap(), 24),
            DynSolValue::Uint(U256::ZERO, 16),
            DynSolValue::Uint(U256::from(1u8), 16),
            DynSolValue::Uint(U256::from(1u8), 16),
            DynSolValue::Uint(U256::ZERO, 8),
            DynSolValue::Bool(true),
        ])
        .abi_encode()
    }

    /// The six-word `slot0()` return the Aerodrome Slipstream pools answer
    /// (`ICLPoolState` has no `feeProtocol` field).
    fn slipstream_slot0_return(sqrt: U256, tick: i32) -> Vec<u8> {
        DynSolValue::Tuple(vec![
            DynSolValue::Uint(sqrt, 160),
            DynSolValue::Int(I256::try_from(i128::from(tick)).unwrap(), 24),
            DynSolValue::Uint(U256::ZERO, 16),
            DynSolValue::Uint(U256::from(1u8), 16),
            DynSolValue::Uint(U256::from(1u8), 16),
            DynSolValue::Bool(true),
        ])
        .abi_encode()
    }

    /// A `ticks(int24)` leading pair from two raw words.
    fn tick_pair_return(gross: [u8; 32], net: [u8; 32]) -> Vec<u8> {
        let mut out = gross.to_vec();
        out.extend_from_slice(&net);
        out
    }

    /// The eight-word `ticks(int24)` return.
    fn ticks_return(gross: u128, net: i128) -> Vec<u8> {
        DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(gross), 128),
            DynSolValue::Int(I256::try_from(net).unwrap(), 128),
            DynSolValue::Uint(U256::ZERO, 256),
            DynSolValue::Uint(U256::ZERO, 256),
            DynSolValue::Int(I256::ZERO, 56),
            DynSolValue::Uint(U256::ZERO, 160),
            DynSolValue::Uint(U256::ZERO, 32),
            DynSolValue::Bool(false),
        ])
        .abi_encode()
    }

    // ── the typed read verb ──

    #[tokio::test]
    async fn read_crosses_the_transport_once_with_the_typed_calldata() {
        let owner = alloy::primitives::address!("1111111111111111111111111111111111111111");
        let balance = U256::from(5u64);
        let transport = TableTransport::new().answer(
            IERC20::balanceOfCall { account: owner }.abi_encode(),
            uint_return(balance),
        );

        let got = transport
            .read(TO, IERC20::balanceOfCall { account: owner }, BLOCK)
            .await
            .unwrap();

        assert_eq!(got, balance);
        assert_eq!(
            transport.calls(),
            vec![(
                TO,
                Bytes::from(IERC20::balanceOfCall { account: owner }.abi_encode()),
                BLOCK
            )],
            "one eth_call, at the caller's block, with the getter's calldata"
        );
    }

    #[tokio::test]
    async fn read_reports_undecodable_return_data_as_a_decode_failure() {
        let transport =
            TableTransport::new().answer(IERC20::totalSupplyCall {}.abi_encode(), vec![0u8; 31]);

        let err = transport
            .read(TO, IERC20::totalSupplyCall {}, BLOCK)
            .await
            .unwrap_err();

        let ProviderError::DecodingError { message } = err else {
            panic!("a short return is a decode failure, got {err:?}");
        };
        assert!(
            message.contains("totalSupply()"),
            "the failure names the getter it decoded: {message}"
        );
    }

    /// Validation is not STRICT decoding: a transport answering more words than
    /// the declared tuple still reads, which is what keeps a getter whose
    /// leading fields are all a snapshot keeps working.
    #[tokio::test]
    async fn read_keeps_trailing_words_past_the_declared_tuple() {
        let mut answer = get_reserves_return(U256::from(11u8), U256::from(22u8));
        answer.extend_from_slice(&uint_return(U256::from(999u64)));

        let transport =
            TableTransport::new().answer(IUniswapV2Pair::getReservesCall {}.abi_encode(), answer);

        let reserves = V2Reserves::read(&transport, TO, BLOCK).await.unwrap();

        assert_eq!(
            (reserves.reserve0, reserves.reserve1),
            (U256::from(11u8), U256::from(22u8)),
            "the declared tuple reads; the words past it are ignored"
        );
    }

    /// The validation policy belongs to the read verb, not to V2: a `uint128`
    /// getter refuses a word carrying bits above position 128.
    #[tokio::test]
    async fn read_refuses_a_narrow_uint_word_with_bits_above_its_width() {
        let over_wide = U256::from(1u8) << 128;
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::liquidityCall {}.abi_encode(),
            uint_return(over_wide),
        );

        let err = transport
            .read(TO, IUniswapV3Pool::liquidityCall {}, BLOCK)
            .await
            .unwrap_err();

        let ProviderError::DecodingError { message } = err else {
            panic!("an over-wide uint128 word is a decode failure, got {err:?}");
        };
        assert!(
            message.contains("liquidity()") && message.contains("uint128"),
            "the failure names the getter and the type it could not produce: {message}"
        );
    }

    /// An `address` return with dirty high twelve bytes is refused by the same
    /// policy — the padding is checked, not just the length.
    #[tokio::test]
    async fn read_refuses_an_address_word_with_dirty_high_bytes() {
        let mut word = [0u8; 32];
        word[0] = 0x01;
        word[31] = 0x42;
        let transport =
            TableTransport::new().answer(IPoolIdentity::token0Call {}.abi_encode(), word.to_vec());

        let err = transport
            .read(TO, IPoolIdentity::token0Call {}, BLOCK)
            .await
            .unwrap_err();

        let ProviderError::DecodingError { message } = err else {
            panic!("a dirty address word is a decode failure, got {err:?}");
        };
        assert!(
            message.contains("token0()") && message.contains("address"),
            "the failure names the getter and the type it could not produce: {message}"
        );
    }

    /// The same two getters answer their canonical words exactly: validation
    /// refuses non-canonical padding, it does not narrow what a getter may say.
    #[tokio::test]
    async fn read_accepts_canonical_narrow_and_address_words_at_their_bounds() {
        let token = alloy::primitives::address!("00000000000000000000000000000000000000c3");
        let widest_liquidity = u128::MAX;

        let transport = TableTransport::new()
            .answer(
                IPoolIdentity::token0Call {}.abi_encode(),
                token.into_word().as_slice().to_vec(),
            )
            .answer(
                IUniswapV3Pool::liquidityCall {}.abi_encode(),
                narrow_uint_return(U256::from(widest_liquidity), 128),
            );

        assert_eq!(
            transport
                .read(TO, IPoolIdentity::token0Call {}, BLOCK)
                .await
                .unwrap(),
            token
        );
        assert_eq!(
            transport
                .read(TO, IUniswapV3Pool::liquidityCall {}, BLOCK)
                .await
                .unwrap(),
            widest_liquidity
        );
    }

    /// A `bool` return is canonical only as 0 or 1: the validating decode
    /// refuses `2` rather than reading it as `true`.
    #[tokio::test]
    async fn read_refuses_a_bool_word_other_than_zero_or_one() {
        let transport = TableTransport::new().answer(
            ILendingToken::isCTokenCall {}.abi_encode(),
            uint_return(U256::from(2u8)),
        );

        let err = transport
            .read(TO, ILendingToken::isCTokenCall {}, BLOCK)
            .await
            .unwrap_err();

        let ProviderError::DecodingError { message } = err else {
            panic!("a bool word that is neither 0 nor 1 is a decode failure, got {err:?}");
        };
        assert!(
            message.contains("isCToken()") && message.contains("bool"),
            "the failure names the getter and the type it could not produce: {message}"
        );
    }

    /// The two canonical `bool` words are the ones that read — validation
    /// refuses the rest, it does not reshape what a getter may answer.
    #[tokio::test]
    async fn read_accepts_the_canonical_bool_words() {
        for (word, expected) in [(0u8, false), (1u8, true)] {
            let transport = TableTransport::new().answer(
                ILendingToken::isCTokenCall {}.abi_encode(),
                uint_return(U256::from(word)),
            );

            assert_eq!(
                transport
                    .read(TO, ILendingToken::isCTokenCall {}, BLOCK)
                    .await
                    .unwrap(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn read_surfaces_a_transport_error_verbatim() {
        let transport = TableTransport::new().fail(
            IERC20::totalSupplyCall {}.abi_encode(),
            ProviderError::ExecutionReverted {
                code: -32000,
                message: "execution reverted".into(),
            },
        );

        assert!(matches!(
            transport.read(TO, IERC20::totalSupplyCall {}, BLOCK).await,
            Err(ProviderError::ExecutionReverted { code: -32000, .. })
        ));
    }

    #[tokio::test]
    async fn a_shared_transport_reads_the_same() {
        let supply = U256::from(7u64);
        let transport = std::sync::Arc::new(
            TableTransport::new()
                .answer(IERC20::totalSupplyCall {}.abi_encode(), uint_return(supply)),
        );

        assert_eq!(
            transport
                .read(TO, IERC20::totalSupplyCall {}, BLOCK)
                .await
                .unwrap(),
            supply
        );
        assert_eq!(transport.calls().len(), 1);
    }

    // ── narrowings ──

    #[tokio::test]
    async fn v2_reserves_narrows_get_reserves_to_its_two_reserve_words() {
        let transport = TableTransport::new().answer(
            IUniswapV2Pair::getReservesCall {}.abi_encode(),
            DynSolValue::Tuple(vec![
                DynSolValue::Uint(U256::from(1_500u64), 112),
                DynSolValue::Uint(U256::from(2_500u64), 112),
                // The snapshot keeps neither the timestamp nor its staleness.
                DynSolValue::Uint(U256::from(999u64), 32),
            ])
            .abi_encode(),
        );

        let reserves = V2Reserves::read(&transport, TO, BLOCK).await.unwrap();

        assert_eq!(reserves.reserve0, U256::from(1_500u64));
        assert_eq!(reserves.reserve1, U256::from(2_500u64));
    }

    /// The pair's own `uint112` declaration is the shape this read crosses, and
    /// the wire call is the selector alone — unchanged by the width it decodes.
    #[tokio::test]
    async fn v2_reserves_reads_the_uint112_getter_on_the_unchanged_pair_calldata() {
        assert_eq!(
            IUniswapV2Pair::getReservesCall::SELECTOR,
            [0x09, 0x02, 0xf1, 0xac],
            "`getReserves()` as the pair declares it"
        );
        assert_eq!(
            IUniswapV2Pair::getReservesCall {}.abi_encode(),
            IUniswapV2Pair::getReservesCall::SELECTOR.to_vec(),
            "a getter with no arguments encodes to its selector alone"
        );

        let transport = TableTransport::new().answer(
            IUniswapV2Pair::getReservesCall {}.abi_encode(),
            get_reserves_return(U256::from(1u8), U256::from(2u8)),
        );

        V2Reserves::read(&transport, TO, BLOCK).await.unwrap();

        assert_eq!(
            transport.calls(),
            vec![(
                TO,
                Bytes::from(IUniswapV2Pair::getReservesCall {}.abi_encode()),
                BLOCK
            )],
            "the pair's own calldata, at the caller's block"
        );
    }

    #[tokio::test]
    async fn v2_reserves_reads_every_word_inside_the_uint112_field() {
        for (reserve0, reserve1) in [
            (U256::ZERO, U256::ZERO),
            (U256::from(1_500u64), U256::from(2_500u64)),
            (U112::MAX.to::<U256>(), U256::ZERO),
            (U256::ZERO, U112::MAX.to::<U256>()),
            (U112::MAX.to::<U256>(), U112::MAX.to::<U256>()),
        ] {
            let transport = TableTransport::new().answer(
                IUniswapV2Pair::getReservesCall {}.abi_encode(),
                get_reserves_return(reserve0, reserve1),
            );

            let reserves = V2Reserves::read(&transport, TO, BLOCK).await.unwrap();

            assert_eq!(
                (reserves.reserve0, reserves.reserve1),
                (reserve0, reserve1),
                "the field's own maximum and every value below it keep their exact value"
            );
            assert_eq!(
                transport.calls(),
                vec![(
                    TO,
                    Bytes::from(IUniswapV2Pair::getReservesCall {}.abi_encode()),
                    BLOCK
                )],
                "the pair's own getReserves() calldata, at the caller's block"
            );
        }
    }

    /// The declaration's width is the bound, in either position: a reserve word
    /// past `uint112` is refused at the decode, so no caller ever sees a
    /// truncated reserve.
    #[tokio::test]
    async fn v2_reserves_rejects_a_word_wider_than_the_uint112_field_in_either_position() {
        let two_pow_112 = two_pow_112();
        let just_over = two_pow_112 + U256::from(1u8);
        for (reserve0, reserve1) in [
            (two_pow_112, U256::from(1u8)),
            (just_over, U256::from(1u8)),
            (U256::from(1u8), two_pow_112),
            (U256::from(1u8), U256::MAX),
        ] {
            let transport = TableTransport::new().answer(
                IUniswapV2Pair::getReservesCall {}.abi_encode(),
                get_reserves_return(reserve0, reserve1),
            );

            let err = V2Reserves::read(&transport, TO, BLOCK).await.unwrap_err();

            let ProviderError::DecodingError { message } = err else {
                panic!("a reserve wider than the pair's field is a decode failure, got {err:?}");
            };
            assert!(
                message.contains("getReserves()") && message.contains("uint112"),
                "the failure names the getter and the type it could not produce: {message}"
            );
        }
    }

    /// The timestamp word is validated even though no caller keeps it: a word
    /// above `uint32` is not the `(uint112, uint112, uint32)` tuple the pair
    /// declares, so the read fails instead of discarding the excess.
    #[tokio::test]
    async fn v2_reserves_rejects_a_timestamp_word_above_uint32() {
        for timestamp in [U256::from(1u8) << 32, U256::MAX] {
            let transport = TableTransport::new().answer(
                IUniswapV2Pair::getReservesCall {}.abi_encode(),
                get_reserves_return_at(U256::from(1u8), U256::from(2u8), timestamp),
            );

            let err = V2Reserves::read(&transport, TO, BLOCK).await.unwrap_err();

            let ProviderError::DecodingError { message } = err else {
                panic!("a timestamp wider than uint32 is a decode failure, got {err:?}");
            };
            assert!(
                message.contains("getReserves()") && message.contains("uint32"),
                "the failure names the getter and the type it could not produce: {message}"
            );
        }
    }

    /// A zero or stale timestamp is still a canonical `uint32`, so the reserves
    /// read exactly as they would beside a fresh one.
    #[tokio::test]
    async fn v2_reserves_accepts_a_zero_or_stale_timestamp() {
        for timestamp in [U256::ZERO, U256::from(1u64), U256::from(u32::MAX)] {
            let transport = TableTransport::new().answer(
                IUniswapV2Pair::getReservesCall {}.abi_encode(),
                get_reserves_return_at(U256::from(1_500u64), U256::from(2_500u64), timestamp),
            );

            let reserves = V2Reserves::read(&transport, TO, BLOCK).await.unwrap();

            assert_eq!(
                (reserves.reserve0, reserves.reserve1),
                (U256::from(1_500u64), U256::from(2_500u64)),
                "a timestamp the snapshot drops cannot change the reserves it keeps"
            );
        }
    }

    /// The `(2^112, 1)` pair must be a decode failure, never the truncated
    /// `reserve0 = 0, reserve1 = 1` an unvalidated decode of the same bytes
    /// produces.
    #[tokio::test]
    async fn v2_reserves_regression_two_to_the_112_is_a_decode_failure_not_a_zero_reserve() {
        let bytes = get_reserves_return(two_pow_112(), U256::from(1u8));
        let transport = TableTransport::new().answer(
            IUniswapV2Pair::getReservesCall {}.abi_encode(),
            bytes.clone(),
        );

        assert!(matches!(
            V2Reserves::read(&transport, TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));

        let unvalidated = IUniswapV2Pair::getReservesCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(
            (unvalidated.reserve0, unvalidated.reserve1),
            (U112::ZERO, U112::from(1u8)),
            "the unvalidated decode drops bit 112 — the reason the read verb validates"
        );
    }

    #[tokio::test]
    async fn v2_reserves_rejects_a_return_shorter_than_its_tuple() {
        for length in [95usize, 64] {
            let short = get_reserves_return(U256::from(1u8), U256::from(2u8))[..length].to_vec();
            let transport = TableTransport::new()
                .answer(IUniswapV2Pair::getReservesCall {}.abi_encode(), short);

            assert!(
                matches!(
                    V2Reserves::read(&transport, TO, BLOCK).await,
                    Err(ProviderError::DecodingError { .. })
                ),
                "a {length}-byte return cannot hold the three-word tuple"
            );
        }
    }

    #[tokio::test]
    async fn balancer_pool_id_is_the_raw_bytes32() {
        let id: [u8; 32] = [0x5a; 32];
        let transport = TableTransport::new().answer(
            IBalancerPool::getPoolIdCall {}.abi_encode(),
            DynSolValue::FixedBytes(alloy::primitives::B256::from(id), 32).abi_encode(),
        );

        assert_eq!(
            BalancerPoolId::read(&transport, TO, BLOCK).await.unwrap().0,
            id
        );
    }

    #[tokio::test]
    async fn slot0_prefix_reads_the_leading_pair_and_keeps_the_tick_signed() {
        let sqrt = U256::from(1u128 << 96);
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::slot0Call {}.abi_encode(),
            slot0_return(sqrt, -123),
        );

        let slot0 = Slot0Prefix::read(&transport, TO, BLOCK).await.unwrap();

        assert_eq!(slot0.sqrt_price_x96, sqrt);
        assert_eq!(slot0.tick, I256::try_from(-123i128).unwrap());
    }

    #[tokio::test]
    async fn slot0_prefix_tolerates_the_six_word_slipstream_tail() {
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::slot0Call {}.abi_encode(),
            slipstream_slot0_return(U256::from(42u64), 7),
        );

        assert_eq!(
            Slot0Prefix::read(&transport, TO, BLOCK).await.unwrap(),
            Slot0Prefix {
                sqrt_price_x96: U256::from(42u64),
                tick: I256::try_from(7i128).unwrap(),
            }
        );
    }

    #[tokio::test]
    async fn slot0_prefix_rejects_a_return_shorter_than_its_pair() {
        let transport =
            TableTransport::new().answer(IUniswapV3Pool::slot0Call {}.abi_encode(), vec![0u8; 32]);

        assert!(matches!(
            Slot0Prefix::read(&transport, TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));
    }

    /// The leading pair's declaration is its bound: the price is `uint160` and
    /// the tick is `int24`, so a price word carrying bits above position 160 and
    /// a tick word that is not sign-extended are both decode failures — never a
    /// truncated price or a sign read off the low 24 bits.
    #[tokio::test]
    async fn slot0_prefix_rejects_words_outside_their_declared_widths() {
        let mut over_wide_price = uint_return(U256::from(1u8) << 160);
        over_wide_price.extend_from_slice(&DynSolValue::Int(I256::ZERO, 24).abi_encode());
        let price = TableTransport::new()
            .answer(IUniswapV3Pool::slot0Call {}.abi_encode(), over_wide_price);
        assert!(matches!(
            Slot0Prefix::read(&price, TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));

        // `-123` at int24 is `0xffff85`; answered with no sign padding above it
        // the word is not a canonical `int24`.
        let mut unpadded_tick = uint_return(U256::from(1u8) << 96);
        unpadded_tick.extend_from_slice(&word_of(0x00ff_ff85));
        let tick = TableTransport::new().answer(
            IUniswapV3Pool::slot0Call {}.abi_encode(),
            unpadded_tick.clone(),
        );
        assert!(matches!(
            Slot0Prefix::read(&tick, TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));
        assert_eq!(
            I24::abi_decode(&unpadded_tick[32..]).unwrap(),
            I24::try_from(-123i32).unwrap(),
            "the typed unvalidated decode reads -123 off the low three bytes"
        );
    }

    /// The declarations' own edges are canonical: `uint160::MAX` is a price a
    /// pool may report and `int24`'s MIN/MAX are ticks it may hold, so the bound
    /// is each field's width rather than a narrower guess that would refuse real
    /// state.
    #[tokio::test]
    async fn slot0_prefix_reads_the_uint160_and_int24_bounds() {
        const INT24_MIN: i32 = -8_388_608;
        const INT24_MAX: i32 = 8_388_607;
        for (sqrt, tick) in [
            (U256::from(U160::MAX), INT24_MIN),
            (U256::from(U160::MAX), INT24_MAX),
            (U256::ZERO, INT24_MIN),
        ] {
            let transport = TableTransport::new().answer(
                IUniswapV3Pool::slot0Call {}.abi_encode(),
                slot0_return(sqrt, tick),
            );

            assert_eq!(
                Slot0Prefix::read(&transport, TO, BLOCK).await.unwrap(),
                Slot0Prefix {
                    sqrt_price_x96: sqrt,
                    tick: widen_int24(tick),
                },
                "sqrt={sqrt} tick={tick} sit inside uint160/int24, not outside them"
            );
        }
    }

    #[tokio::test]
    async fn tick_liquidity_reads_the_leading_pair_of_ticks() {
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::ticksCall {
                tick: (-4).try_into().unwrap(),
            }
            .abi_encode(),
            ticks_return(500, -100),
        );

        let liquidity = TickLiquidity::read(&transport, TO, -4, BLOCK)
            .await
            .unwrap();

        assert_eq!(liquidity.gross, U128::from(500u64));
        assert_eq!(liquidity.net, -100i128);
    }

    #[tokio::test]
    async fn tick_liquidity_reads_a_pool_that_answers_only_the_pair() {
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::ticksCall {
                tick: 0.try_into().unwrap(),
            }
            .abi_encode(),
            ticks_return(9, 4)[..64].to_vec(),
        );

        let liquidity = TickLiquidity::read(&transport, TO, 0, BLOCK).await.unwrap();

        assert_eq!((liquidity.gross, liquidity.net), (U128::from(9u64), 4i128));
    }

    #[tokio::test]
    async fn tick_liquidity_rejects_a_short_ticks_return() {
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::ticksCall {
                tick: 0.try_into().unwrap(),
            }
            .abi_encode(),
            vec![0u8; 63],
        );

        assert!(matches!(
            TickLiquidity::read(&transport, TO, 0, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));
    }

    /// The pair's declaration is its bound: `uint128` gross and `int128` net, so
    /// an over-wide gross word and a net word that is not sign-extended are
    /// decode failures rather than a truncated gross and a sign read off the low
    /// half-word.
    #[test]
    fn tick_liquidity_decode_rejects_words_outside_their_declared_widths() {
        let mut over_wide_gross = [0u8; 32];
        over_wide_gross.copy_from_slice(&uint_return(U256::from(1u8) << 128));
        let net_word = ticks_return(0, -100)[32..64]
            .try_into()
            .expect("word 1 of the eight-word answer");
        assert!(matches!(
            TickLiquidity::decode(&tick_pair_return(over_wide_gross, net_word)),
            Err(ProviderError::DecodingError { .. })
        ));

        // `-100` at int128 with its high 16 bytes left zero: the low half-word is
        // still `-100`, which is what a bare low-half slice reported.
        let mut unpadded_net = [0u8; 32];
        unpadded_net[16..32].copy_from_slice(&(-100i128).to_be_bytes());
        let unpadded = tick_pair_return([0u8; 32], unpadded_net);
        assert!(matches!(
            TickLiquidity::decode(&unpadded),
            Err(ProviderError::DecodingError { .. })
        ));
        assert_eq!(
            <(u128, i128) as SolValue>::abi_decode(&unpadded).unwrap(),
            (0u128, -100i128),
            "the unvalidated decode reads -100 off the low half-word"
        );
    }

    /// Both shapes a `ticks(int24)` answer arrives in read the same pair: the
    /// two words a pool answering only the leading pair returns, and the full
    /// eight-word tuple the real contract does. A byte short of the pair does
    /// not.
    #[test]
    fn tick_liquidity_decode_reads_two_word_and_eight_word_answers() {
        let expected = TickLiquidity {
            gross: U128::from(500u64),
            net: -100i128,
        };
        let full = ticks_return(500, -100);
        assert_eq!(TickLiquidity::decode(&full).unwrap(), expected);
        assert_eq!(TickLiquidity::decode(&full[..64]).unwrap(), expected);
        assert!(matches!(
            TickLiquidity::decode(&full[..63]),
            Err(ProviderError::DecodingError { .. })
        ));
    }

    /// The pair's edges are canonical for the same reason: a `uint128` gross of
    /// MAX and an `int128` net of MIN/MAX are values a tick may hold, and both
    /// answer shapes read them back unchanged.
    #[test]
    fn tick_liquidity_decode_reads_the_uint128_and_int128_bounds() {
        for (gross, net) in [
            (u128::MAX, i128::MIN),
            (u128::MAX, i128::MAX),
            (0u128, i128::MIN),
        ] {
            let expected = TickLiquidity {
                gross: U128::from(gross),
                net,
            };
            let full = ticks_return(gross, net);

            assert_eq!(
                TickLiquidity::decode(&full).unwrap(),
                expected,
                "gross={gross} net={net} sit inside uint128/int128, not outside them"
            );
            assert_eq!(TickLiquidity::decode(&full[..64]).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn tick_liquidity_rejects_a_tick_outside_the_int24_argument_width() {
        let transport = TableTransport::new();

        assert!(matches!(
            TickLiquidity::read(&transport, TO, 1 << 24, BLOCK).await,
            Err(ProviderError::InvalidParams { .. })
        ));
        assert!(
            transport.calls().is_empty(),
            "an unrepresentable tick never reaches the chain"
        );
    }

    #[tokio::test]
    async fn tick_liquidity_reads_the_state_view_pair_by_pool_id() {
        let pool_id: [u8; 32] = [0x77; 32];
        let transport = TableTransport::new().answer(
            IUniswapV4StateView::getTickLiquidityCall {
                poolId: pool_id.into(),
                tick: (-9).try_into().unwrap(),
            }
            .abi_encode(),
            DynSolValue::Tuple(vec![
                DynSolValue::Uint(U256::from(11u64), 128),
                DynSolValue::Int(I256::try_from(-22i128).unwrap(), 128),
            ])
            .abi_encode(),
        );

        let liquidity = TickLiquidity::read_v4(&transport, OTHER, pool_id, -9, BLOCK)
            .await
            .unwrap();

        assert_eq!(
            (liquidity.gross, liquidity.net),
            (U128::from(11u64), -22i128)
        );
        assert!(transport.calls().iter().all(|(to, _, _)| *to == OTHER));
    }

    #[tokio::test]
    async fn v4_slot0_widens_the_state_view_answer() {
        let pool_id: [u8; 32] = [0x11; 32];
        let transport = TableTransport::new().answer(
            IUniswapV4StateView::getSlot0Call {
                poolId: pool_id.into(),
            }
            .abi_encode(),
            DynSolValue::Tuple(vec![
                DynSolValue::Uint(U256::from(7u64), 160),
                DynSolValue::Int(I256::try_from(-3i128).unwrap(), 24),
                DynSolValue::Uint(U256::from(0x11u64), 24),
                DynSolValue::Uint(U256::from(0x22u64), 24),
            ])
            .abi_encode(),
        );

        let slot0 = V4Slot0::read(&transport, OTHER, pool_id, BLOCK)
            .await
            .unwrap();

        assert_eq!(
            slot0,
            V4Slot0 {
                sqrt_price_x96: U256::from(7u64),
                tick: I256::try_from(-3i128).unwrap(),
                protocol_fee: U256::from(0x11u64),
                lp_fee: U256::from(0x22u64),
            }
        );
    }

    // ── reads whose answer the plain typed verb already carries ──

    /// `getPoolTokens(bytes32)`'s `(address[], uint256[], uint256)` return,
    /// assembled word by word so the fixture never round-trips the decoder it
    /// exercises.
    fn vault_tokens_return(tokens: &[Address], balances: &[U256], last_change: U256) -> Vec<u8> {
        let n = u64::try_from(tokens.len()).expect("fixture fits u64");
        let mut out = Vec::new();
        out.extend_from_slice(&U256::from(96u64).to_be_bytes::<32>());
        out.extend_from_slice(&U256::from(96 + 32 + n * 32).to_be_bytes::<32>());
        out.extend_from_slice(&last_change.to_be_bytes::<32>());
        out.extend_from_slice(&U256::from(n).to_be_bytes::<32>());
        for token in tokens {
            out.extend_from_slice(&DynSolValue::Address(*token).abi_encode());
        }
        out.extend_from_slice(&U256::from(n).to_be_bytes::<32>());
        for balance in balances {
            out.extend_from_slice(&balance.to_be_bytes::<32>());
        }
        out
    }

    #[tokio::test]
    async fn vault_tokens_decode_both_arrays_and_the_change_block() {
        let pool_id: [u8; 32] = [0x22; 32];
        let transport = TableTransport::new().answer(
            IBalancerVault::getPoolTokensCall {
                poolId: pool_id.into(),
            }
            .abi_encode(),
            vault_tokens_return(
                &[TO, OTHER],
                &[U256::from(1_000u64), U256::from(2_000u64)],
                U256::from(12_345u64),
            ),
        );

        let vault = transport
            .read(
                OTHER,
                IBalancerVault::getPoolTokensCall {
                    poolId: pool_id.into(),
                },
                BLOCK,
            )
            .await
            .unwrap();

        assert_eq!(vault.tokens, vec![TO, OTHER]);
        assert_eq!(
            vault.balances,
            vec![U256::from(1_000u64), U256::from(2_000u64)]
        );
        assert_eq!(vault.lastChangeBlock, U256::from(12_345u64));
    }

    #[tokio::test]
    async fn amplification_parameter_carries_its_whole_tuple() {
        let transport = TableTransport::new().answer(
            IBalancerPool::getAmplificationParameterCall {}.abi_encode(),
            DynSolValue::Tuple(vec![
                DynSolValue::Uint(U256::from(400_000u64), 256),
                DynSolValue::Bool(true),
                DynSolValue::Uint(U256::from(1_000u64), 256),
            ])
            .abi_encode(),
        );

        let amp = transport
            .read(TO, IBalancerPool::getAmplificationParameterCall {}, BLOCK)
            .await
            .unwrap();

        assert_eq!(amp.value, U256::from(400_000u64));
        assert!(amp.isUpdating);
        assert_eq!(amp.precision, U256::from(1_000u64));
    }

    #[tokio::test]
    async fn liquidity_reads_the_uint128_as_the_pool_math_width() {
        let transport = TableTransport::new().answer(
            IUniswapV3Pool::liquidityCall {}.abi_encode(),
            narrow_uint_return(U256::from(1_000_000u64), 128),
        );

        let liquidity = transport
            .read(TO, IUniswapV3Pool::liquidityCall {}, BLOCK)
            .await
            .unwrap();

        assert_eq!(U256::from(liquidity), U256::from(1_000_000u64));
    }

    // ── declared-shape reads ──

    #[tokio::test]
    async fn declared_address_reads_hash_the_signature_to_the_selector() {
        let returning = alloy::primitives::address!("66f9664f97f2b50f62d13ea064982f936de76657");
        let calldata = selector(b"factory()");
        let transport = TableTransport::new().answer(
            calldata.to_vec(),
            DynSolValue::Address(returning).abi_encode(),
        );
        let short = TableTransport::new().answer(calldata.to_vec(), vec![0u8; 31]);

        assert_eq!(
            fetch_address_returning(&transport, b"factory()", &TO, BLOCK)
                .await
                .unwrap(),
            returning
        );
        assert_eq!(
            transport.calls(),
            vec![(TO, Bytes::copy_from_slice(&calldata), BLOCK)],
            "the signature is hashed to the 4-byte selector the chain receives"
        );
        assert!(matches!(
            fetch_address_returning(&short, b"factory()", &TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));
    }

    #[tokio::test]
    async fn declared_scalars_decode_at_the_width_their_type_declares() {
        let transport = TableTransport::new()
            .answer(
                selector(b"fee()").to_vec(),
                narrow_uint_return(U256::from(3_000u64), 24),
            )
            .answer(
                selector(b"tickSpacing()").to_vec(),
                DynSolValue::Int(I256::try_from(-60i128).unwrap(), 24).abi_encode(),
            )
            .answer(
                selector(b"stable()").to_vec(),
                DynSolValue::Bool(true).abi_encode(),
            );

        assert_eq!(
            fetch_no_arg_value::<U24>(&transport, b"fee()", &TO, BLOCK)
                .await
                .unwrap(),
            U24::from(3_000u32)
        );
        assert_eq!(
            fetch_no_arg_value::<I24>(&transport, b"tickSpacing()", &TO, BLOCK)
                .await
                .unwrap(),
            I24::try_from(-60i32).unwrap(),
            "the declared int24 keeps its sign"
        );
        assert!(
            fetch_no_arg_value::<bool>(&transport, b"stable()", &TO, BLOCK)
                .await
                .unwrap()
        );
        assert_eq!(transport.calls().len(), 3, "each scalar read is one call");
    }

    /// The declared type is the bound: a `uint24` getter answering bit 24 set is
    /// a decode failure, not the truncated fee an unvalidated decode yields.
    #[tokio::test]
    async fn declared_uint_read_refuses_a_word_wider_than_its_declared_width() {
        let dirty = narrow_uint_return(U256::from(1u8) << 24, 256);
        let transport = TableTransport::new().answer(selector(b"fee()").to_vec(), dirty.clone());

        let err = fetch_no_arg_value::<U24>(&transport, b"fee()", &TO, BLOCK)
            .await
            .unwrap_err();

        let ProviderError::DecodingError { message } = err else {
            panic!("an over-wide uint24 word is a decode failure, got {err:?}");
        };
        assert!(
            message.contains("fee()"),
            "the failure names the signature it decoded: {message}"
        );
        assert_eq!(
            DynSolType::Uint(24).abi_decode(&dirty).unwrap(),
            DynSolValue::Uint(U256::from(1u8) << 24, 24),
            "the dynamic decode enforced no width at all — the fee would have carried bit 24"
        );
    }

    /// An `int24` word that is not sign-extended is refused: the low 24 bits
    /// still read as a negative tick without validation, which is exactly the
    /// value a bare low-half slice would have produced.
    #[tokio::test]
    async fn declared_int_read_refuses_a_word_that_is_not_sign_extended() {
        // `-60` at int24 is `0xffffc4`; a getter that answered those three bytes
        // with no sign padding is not a canonical `int24` word.
        let dirty = word_of(0x00ff_ffc4);
        let transport =
            TableTransport::new().answer(selector(b"tickSpacing()").to_vec(), dirty.clone());

        let err = fetch_no_arg_value::<I24>(&transport, b"tickSpacing()", &TO, BLOCK)
            .await
            .unwrap_err();

        assert!(matches!(err, ProviderError::DecodingError { .. }));
        assert_eq!(
            DynSolType::Int(24).abi_decode(&dirty).unwrap(),
            DynSolValue::Int(I256::try_from(0x00ff_ffc4i32).unwrap(), 24),
            "the dynamic decode kept the unpadded word as a large positive number"
        );
        assert_eq!(
            I24::abi_decode(&dirty).unwrap(),
            I24::try_from(-60i32).unwrap(),
            "the typed unvalidated decode sign-extends bit 23 into -60 regardless of the padding"
        );
    }

    /// A `bool` is canonical only as 0 or 1: a getter answering 2 is refused
    /// rather than read as `true`.
    #[tokio::test]
    async fn declared_bool_read_refuses_a_word_other_than_zero_or_one() {
        let transport = TableTransport::new()
            .answer(selector(b"stable()").to_vec(), uint_return(U256::from(2u8)));

        let err = fetch_no_arg_value::<bool>(&transport, b"stable()", &TO, BLOCK)
            .await
            .unwrap_err();

        assert!(matches!(err, ProviderError::DecodingError { .. }));
    }

    /// A getter answering more than the caller reads keeps working: the words
    /// past the declared type are unread.
    #[tokio::test]
    async fn declared_reads_keep_trailing_words_past_the_declared_type() {
        let mut answer = uint_return(U256::from(3_000u64));
        answer.extend_from_slice(&uint_return(U256::from(999u64)));
        let transport = TableTransport::new().answer(selector(b"fee()").to_vec(), answer);

        assert_eq!(
            fetch_no_arg_value::<U24>(&transport, b"fee()", &TO, BLOCK)
                .await
                .unwrap(),
            U24::from(3_000u32)
        );
    }

    /// A return shorter than one word decodes as nothing — never as a zero.
    #[tokio::test]
    async fn declared_reads_refuse_a_short_answer() {
        let fee = TableTransport::new().answer(selector(b"fee()").to_vec(), vec![0u8; 31]);
        assert!(
            matches!(
                fetch_no_arg_value::<U24>(&fee, b"fee()", &TO, BLOCK).await,
                Err(ProviderError::DecodingError { .. })
            ),
            "a 31-byte fee answer is not a word"
        );

        let identity = TableTransport::new().answer(selector(b"factory()").to_vec(), vec![0u8; 31]);
        assert!(matches!(
            fetch_address_returning(&identity, b"factory()", &TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));

        let decimals =
            TableTransport::new().answer(selector(b"decimals()").to_vec(), vec![0u8; 31]);
        assert!(matches!(
            fetch_uint256_field(&decimals, b"decimals()", &TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));
    }

    /// An `address` word with dirty high bytes is refused by the declared-shape
    /// read for the same reason the typed verb refuses it: the padding is part
    /// of the type.
    #[tokio::test]
    async fn declared_address_read_refuses_a_word_with_dirty_high_bytes() {
        let mut word = [0u8; 32];
        word[0] = 0x01;
        word[31] = 0x42;
        let transport =
            TableTransport::new().answer(selector(b"factory()").to_vec(), word.to_vec());

        assert!(matches!(
            fetch_address_returning(&transport, b"factory()", &TO, BLOCK).await,
            Err(ProviderError::DecodingError { .. })
        ));
    }

    /// The padding is what the decode checks, not the value: a canonical
    /// `address` word — twelve zero bytes above the twenty — reads back whole.
    #[tokio::test]
    async fn declared_address_read_accepts_a_canonically_padded_word() {
        let expected = alloy::primitives::address!("00000000000000000000000000000000000000c3");
        let transport =
            TableTransport::new().answer(selector(b"factory()").to_vec(), expected.abi_encode());

        assert_eq!(
            fetch_address_returning(&transport, b"factory()", &TO, BLOCK)
                .await
                .unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn declared_word_reads_decode() {
        let transport = TableTransport::new().answer(
            selector(b"decimals()").to_vec(),
            narrow_uint_return(U256::from(18u64), 8),
        );

        assert_eq!(
            fetch_uint256_field(&transport, b"decimals()", &TO, BLOCK)
                .await
                .unwrap(),
            U256::from(18u64)
        );
    }

    // ── the wire spellings ──

    /// The leading four bytes of `calldata` — a getter's selector.
    fn first_four(calldata: &[u8]) -> [u8; 4] {
        let mut out = [0u8; 4];
        out.copy_from_slice(&calldata[..4]);
        out
    }

    /// Selectors computed outside alloy (Foundry `cast sig`): a `sol!`
    /// declaration verifies the spelling it was given, so the spelling itself
    /// is pinned here. Every no-argument getter also encodes to its selector
    /// alone.
    #[test]
    #[expect(clippy::too_many_lines)]
    fn getter_selectors_match_reference_vectors() {
        let cases: &[(Vec<u8>, [u8; 4])] = &[
            (ICurvePool::ACall {}.abi_encode(), [0xf4, 0x46, 0xc1, 0xd0]),
            (
                ICurvePool::admin_feeCall {}.abi_encode(),
                [0xfe, 0xe3, 0xf7, 0xf9],
            ),
            (
                ICurvePool::base_cache_updatedCall {}.abi_encode(),
                [0x82, 0x96, 0xf8, 0x4f],
            ),
            (
                ICurvePool::base_poolCall {}.abi_encode(),
                [0x5d, 0x63, 0x62, 0xbb],
            ),
            (
                ICurvePool::base_virtual_priceCall {}.abi_encode(),
                [0x95, 0xcc, 0xc0, 0x2f],
            ),
            (
                ICurvePool::feeCall {}.abi_encode(),
                [0xdd, 0xca, 0x3f, 0x43],
            ),
            (
                ICurvePool::fee_gammaCall {}.abi_encode(),
                [0x72, 0xd4, 0xf0, 0xe2],
            ),
            (
                ICurvePool::future_ACall {}.abi_encode(),
                [0xb4, 0xb5, 0x77, 0xad],
            ),
            (
                ICurvePool::future_A_timeCall {}.abi_encode(),
                [0x14, 0x05, 0x22, 0x88],
            ),
            (
                ICurvePool::gammaCall {}.abi_encode(),
                [0xb1, 0x37, 0x39, 0x29],
            ),
            (
                ICurvePool::get_virtual_priceCall {}.abi_encode(),
                [0xbb, 0x7b, 0x8b, 0x80],
            ),
            (
                ICurvePool::initial_ACall {}.abi_encode(),
                [0x54, 0x09, 0x49, 0x1a],
            ),
            (
                ICurvePool::initial_A_timeCall {}.abi_encode(),
                [0x20, 0x81, 0x06, 0x6c],
            ),
            (ICurvePool::DCall {}.abi_encode(), [0x0f, 0x52, 0x9b, 0xa2]),
            (
                ICurvePool::mid_feeCall {}.abi_encode(),
                [0x92, 0x52, 0x6c, 0x0c],
            ),
            (
                ICurvePool::offpeg_fee_multiplierCall {}.abi_encode(),
                [0x8e, 0xdf, 0xdd, 0x5f],
            ),
            (
                ICurvePool::oracle_methodCall {}.abi_encode(),
                [0x34, 0x95, 0x01, 0x8d],
            ),
            (
                ICurvePool::out_feeCall {}.abi_encode(),
                [0xee, 0x8d, 0xe6, 0x75],
            ),
            (
                ICurvePool::redemption_price_snapCall {}.abi_encode(),
                [0x85, 0xca, 0xb7, 0x55],
            ),
            (
                ICurvePool::snappedRedemptionPriceCall {}.abi_encode(),
                [0x69, 0x98, 0xc4, 0x8d],
            ),
            (
                ILendingRateReads::exchangeRateStoredCall {}.abi_encode(),
                [0x18, 0x2d, 0xf0, 0xf5],
            ),
            (
                ILendingRateReads::supplyRatePerBlockCall {}.abi_encode(),
                [0xae, 0x9d, 0x70, 0xb0],
            ),
            (
                ILendingRateReads::accrualBlockNumberCall {}.abi_encode(),
                [0x6c, 0x54, 0x0b, 0xaf],
            ),
            (
                ILendingRateReads::getPricePerFullShareCall {}.abi_encode(),
                [0x77, 0xc7, 0xb8, 0xfc],
            ),
            (
                ILendingRateReads::ratioCall {}.abi_encode(),
                [0x71, 0xca, 0x33, 0x7d],
            ),
            (
                ILendingRateReads::getExchangeRateCall {}.abi_encode(),
                [0xe6, 0xaa, 0x21, 0x6c],
            ),
            (
                IPoolIdentity::factoryCall {}.abi_encode(),
                [0xc4, 0x5a, 0x01, 0x55],
            ),
            (
                IPoolIdentity::token0Call {}.abi_encode(),
                [0x0d, 0xfe, 0x16, 0x81],
            ),
            (
                IPoolIdentity::token1Call {}.abi_encode(),
                [0xd2, 0x12, 0x20, 0xa7],
            ),
            (
                IUniswapV2Pair::getReservesCall {}.abi_encode(),
                [0x09, 0x02, 0xf1, 0xac],
            ),
            (
                IUniswapV3Pool::slot0Call {}.abi_encode(),
                selector(b"slot0()"),
            ),
            (
                IUniswapV3Pool::liquidityCall {}.abi_encode(),
                selector(b"liquidity()"),
            ),
            (
                first_four(
                    &IERC20::balanceOfCall {
                        account: Address::ZERO,
                    }
                    .abi_encode(),
                )
                .to_vec(),
                [0x70, 0xa0, 0x82, 0x31],
            ),
            (
                first_four(
                    &IERC20::allowanceCall {
                        owner: Address::ZERO,
                        spender: Address::ZERO,
                    }
                    .abi_encode(),
                )
                .to_vec(),
                [0xdd, 0x62, 0xed, 0x3e],
            ),
            (
                IERC20::totalSupplyCall {}.abi_encode(),
                [0x18, 0x16, 0x0d, 0xdd],
            ),
            (
                first_four(
                    &IAerodromeFactory::getFeeCall {
                        pool: Address::ZERO,
                        stable: false,
                    }
                    .abi_encode(),
                )
                .to_vec(),
                selector(b"getFee(address,bool)"),
            ),
        ];

        for (calldata, expected) in cases {
            assert_eq!(
                &calldata[..4],
                expected,
                "selector mismatch for calldata {calldata:02x?}"
            );
        }

        for calldata in [
            ICurvePool::ACall {}.abi_encode(),
            IPoolIdentity::token0Call {}.abi_encode(),
            ILendingRateReads::ratioCall {}.abi_encode(),
        ] {
            assert_eq!(
                calldata.len(),
                4,
                "a no-argument getter encodes to its selector alone"
            );
        }
    }

    #[test]
    fn aerodrome_fee_calldata_carries_the_pool_and_the_stable_flag() {
        let pool = alloy::primitives::address!("00000000000000000000000000000000000000ff");
        let calldata = IAerodromeFactory::getFeeCall { pool, stable: true }.abi_encode();

        assert_eq!(calldata.len(), 4 + 64, "selector plus two static words");
        assert_eq!(&calldata[4..36][12..], pool.as_slice());
        assert_eq!(calldata[4 + 32..].len(), 32);
        assert_eq!(u8::from(calldata[4 + 63] == 1), 1, "the bool is word two");
    }

    // ── the concentrated-liquidity quoter reads ──

    /// The two quoter interfaces declare the canonical signatures — the
    /// selectors are re-derived from the Solidity text, never hard-coded.
    #[test]
    fn quoter_interfaces_declare_the_canonical_selectors() {
        assert_eq!(
            IUniswapV3Quoter::quoteExactInputSingleCall::SELECTOR,
            selector(b"quoteExactInputSingle(address,address,uint24,uint256,uint160)"),
        );
        assert_eq!(
            IUniswapV4Quoter::quoteExactInputSingleCall::SELECTOR,
            selector(b"quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))"),
        );
    }

    /// A V3 quote hands the quoter the pool's own fee tier, its direction's
    /// token pair, the exact input, and the quoter's documented limit
    /// substitution — for BOTH directions, at the pinned block.
    #[tokio::test]
    async fn a_v3_quote_encodes_the_limit_for_each_direction() {
        let token_in = alloy::primitives::address!("0x2260fac5e5542a773aa44fbcfedf7c193bc2c599");
        let token_out = alloy::primitives::address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        let answer = uint_return(U256::from(1_234u64));

        for (zero_for_one, limit) in [
            (true, MIN_SQRT_RATIO_PLUS_ONE),
            (false, MAX_SQRT_RATIO_MINUS_ONE),
        ] {
            // The canned answer is keyed on the calldata the seam produces, so a
            // green read proves the encoding this case asserts below.
            let calldata = IUniswapV3Quoter::quoteExactInputSingleCall {
                tokenIn: token_in,
                tokenOut: token_out,
                fee: 3_000u32.try_into().unwrap(),
                amountIn: U256::from(1_000_000u64),
                sqrtPriceLimitX96: v3_quote_sqrt_price_limit(zero_for_one),
            }
            .abi_encode();
            assert_eq!(
                U256::from_be_slice(&calldata[4 + 4 * 32..]),
                limit,
                "the substituted limit is in the fifth argument"
            );
            assert_eq!(
                U256::from_be_slice(&calldata[4 + 2 * 32..4 + 3 * 32]),
                U256::from(3_000u64),
                "the fee tier is the third argument"
            );
            let transport = TableTransport::new().answer(calldata.clone(), answer.clone());
            let quote = V3Quote::quote_exact_in(
                &transport,
                V3_QUOTER,
                &V3QuoteRequest {
                    token_in,
                    token_out,
                    fee: 3_000,
                    amount_in: U256::from(1_000_000u64),
                    zero_for_one,
                    block: 24_407_242,
                },
            )
            .await
            .unwrap();
            assert_eq!(quote.amount_out, U256::from(1_234u64));
            assert_eq!(quote.fee, 3_000);
            let (to, sent, block) = transport.calls().remove(0);
            assert_eq!(to, V3_QUOTER, "the canonical v3-periphery quoter");
            assert_eq!(sent.as_ref(), calldata.as_slice());
            assert_eq!(block, Some(24_407_242), "quoted at the caller's block");
        }
    }

    /// A V4 quote carries the WHOLE pool key — hook address included — so a
    /// hooked pool can never be priced as its unhooked twin, and the direction
    /// and exact amount ride the word layout the recorded corpora carry.
    #[tokio::test]
    async fn a_v4_quote_carries_the_pool_key_and_its_hook() {
        let hook = alloy::primitives::address!("0x00000000000000000000000000000000000000c4");
        let pool_key = degenbot_pools::V4PoolKey {
            currency0: alloy::primitives::address!("0x0000000000000000000000000000000000000000"),
            currency1: alloy::primitives::address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            fee: 500,
            tick_spacing: 10,
            hooks: hook,
        };
        let answer = DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(777u64), 256),
            DynSolValue::Uint(U256::from(31_000u64), 256),
        ])
        .abi_encode();

        for zero_for_one in [true, false] {
            let calldata = IUniswapV4Quoter::quoteExactInputSingleCall {
                params: IUniswapV4Quoter::QuoteExactSingleParams {
                    poolKey: IUniswapV4Quoter::PoolKey {
                        currency0: pool_key.currency0,
                        currency1: pool_key.currency1,
                        fee: pool_key.fee.try_into().unwrap(),
                        tickSpacing: int24(pool_key.tick_spacing).unwrap(),
                        hooks: hook,
                    },
                    zeroForOne: zero_for_one,
                    exactAmount: 1_000_000u128,
                    hookData: Bytes::new(),
                },
            }
            .abi_encode();
            assert_eq!(calldata.len(), 4 + 10 * 32, "the dynamic params tuple");
            assert_eq!(
                U256::from_be_slice(&calldata[4..4 + 32]),
                U256::from(0x20u64),
                "one dynamic tuple argument"
            );
            assert_eq!(
                &calldata[4 + 5 * 32 + 12..4 + 6 * 32],
                hook.as_slice(),
                "the hook address is the key's fifth word"
            );
            let transport = TableTransport::new().answer(calldata, answer.clone());
            let quote = V4Quote::quote_exact_in(
                &transport,
                V4_QUOTER,
                &V4QuoteRequest {
                    pool_key: pool_key.clone(),
                    zero_for_one,
                    amount_in: U256::from(1_000_000u64),
                    block: 24_407_242,
                },
            )
            .await
            .unwrap();
            assert_eq!(quote.amount_out, U256::from(777u64));
            assert_eq!(quote.gas_estimate, U256::from(31_000u64));
            assert_eq!(quote.pool_key, pool_key, "the answer names its own key");
        }
    }

    /// A V4 amount the quoter's `uint128` field cannot hold is refused before
    /// the call, never truncated into a wrong quote.
    #[tokio::test]
    async fn a_v4_quote_refuses_an_amount_wider_than_uint128() {
        let transport = TableTransport::new();
        let error = V4Quote::quote_exact_in(
            &transport,
            V4_QUOTER,
            &V4QuoteRequest {
                pool_key: degenbot_pools::V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: OTHER,
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                zero_for_one: true,
                amount_in: U256::from(u128::MAX) + U256::from(1u8),
                block: 24_407_242,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ProviderError::InvalidParams { .. }));
        assert!(
            transport.calls().is_empty(),
            "no call crossed the transport"
        );
    }
}
