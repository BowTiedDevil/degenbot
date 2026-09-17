//! Target-intent types shared by the backrun lane's decision surface.
//!
//! `TargetClass` routes the sidecar's bid decision: a frame the pipeline
//! judged actionable is presented as `Swap` without consulting call bytes —
//! replay extraction (touched set + journalled words), never calldata
//! decoding, shapes frames.

use alloy::primitives::{Address, U256};

/// Per-target intent of a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetClass {
    /// One or more decodable swap legs.
    Swap(Vec<SwapLeg>),
    /// Provably uninteresting for backrun purposes.
    Inert,
    /// Interesting but not decodable with certainty. Never guess.
    Opaque(OpaqueReason),
}

/// Why a frame could not be decoded with certainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueReason {
    /// Calldata shorter than a selector.
    CalldataTooShort,
    /// Known hub, selector not in the decode table.
    HubSelectorUnknown,
    /// Hub detected whose inner command encoding we deliberately do not
    /// decode yet (universal router, v4 unlock path).
    HubInnerUndecodable,
    /// ABI argument decoding failed against the known layout.
    MalformedArgs,
}

/// A pool protocol family a swap leg moves through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolProtocol {
    V2,
    V3,
}

/// A single decodable swap intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapLeg {
    pub protocol: PoolProtocol,
    /// Some when the wire names the pool directly (v2 pair `swap()`); None
    /// for router calldata (the on-demand resolver owes pool discovery).
    pub pool: Option<Address>,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    /// Native value carried by the call itself (0 unless the driver overlays
    /// the feed event's `value` for ETH-in paths).
    pub value: U256,
    /// Exact amount in, when the method carries one.
    pub amount_in: Option<U256>,
    /// Minimum out (slippage bound), when the method carries one.
    pub amount_out_min: Option<U256>,
    /// Exact amount out (exact-output methods).
    pub amount_out: Option<U256>,
    /// Maximum amount in (exact-output methods).
    pub amount_in_max: Option<U256>,
    /// Intermediate hops (v2: path length - 1; v3: encoded fee-steps).
    pub hops: u16,
}
