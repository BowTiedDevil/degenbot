//! Neutral opportunity descriptors — the hop vocabulary every Encode part
//! shares (ADR-098 D1).
//!
//! A solved arbitrage path is a hop list, not a command stream: the pool
//! family, the participating currencies, the swap direction, and (for V4) the
//! PoolManager pool identity are facts about the *opportunity*. They live
//! beside the [`ExecutionAdapter`](crate::adapter::ExecutionAdapter) seam so a
//! foreign contract's Encode part can name its intake without reaching the
//! built-in `cmd_executor` implementation. The built-in command executor
//! depends on these descriptors; the reverse edge does not exist.
//!
//! The command stream's own vocabulary — the command grammar, opcode
//! encoders, packed command config, warmup-slot math, and Vyper artifact
//! selection — stays in `degenbot-executor`, together with the `CurrencyBridge`
//! classification that turns a native-ETH/WETH boundary into `WETH_DEPOSIT` /
//! `WETH_WITHDRAW` commands.

use alloy::primitives::Address;

/// Engine-facing hop descriptor — one variant per pool family, carrying the
/// typed hop the solver emits and an Encode part consumes.
#[derive(Clone, Debug)]
pub enum HopInfo {
    /// A Uniswap-V2 (or V2-compatible) pool hop.
    V2(V2HopInfo),
    /// A Uniswap-V3 pool hop.
    V3(V3HopInfo),
    /// A Uniswap-V4 pool hop.
    V4(V4HopInfo),
}

/// V2 hop descriptor — `pool_address`, `token0/1_address`, `fee` (bips of
/// 10000, e.g. 30 = 0.3%), `zfo` (zero-for-one direction).
#[derive(Clone, Debug)]
pub struct V2HopInfo {
    pub pool_address: Address,
    pub token0_address: Address,
    pub token1_address: Address,
    pub fee: u16,
    pub zfo: bool,
}

/// V3 hop descriptor — `pool_address`, `token0/1_address`, `fee` (bips of
/// 1e6, e.g. 3000 = 0.3%), `zfo`.
///
/// `fee` is the pool's declared fee tier. Whether an adapter encodes it, and
/// in what form, is the adapter's decision.
#[derive(Clone, Debug)]
pub struct V3HopInfo {
    pub pool_address: Address,
    pub token0_address: Address,
    pub token1_address: Address,
    pub fee: u32,
    pub zfo: bool,
}

/// V4 hop descriptor — `pool_manager_address`, `pool_id_hex` (0x-prefixed),
/// `currency0/1_address`, `fee` (uint24), `tick_spacing` (int24), `hook_address`,
/// `zfo`.
#[derive(Clone, Debug)]
pub struct V4HopInfo {
    pub pool_manager_address: Address,
    pub pool_id_hex: String,
    pub currency0_address: Address,
    pub currency1_address: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hook_address: Address,
    pub zfo: bool,
}

/// An arbitrage path's ordered hops.
#[derive(Clone, Debug)]
pub struct PathInfo {
    /// Ordered hops; the V2/V3/V4 mix defines the path's pool-family sequence.
    pub hops: Vec<HopInfo>,
}

impl PathInfo {
    /// Construct from an ordered hop slice.
    #[must_use]
    pub fn new(hops: Vec<HopInfo>) -> Self {
        Self { hops }
    }
}
