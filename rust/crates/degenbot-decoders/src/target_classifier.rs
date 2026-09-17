//! Pending-tx calldata classification for the `MEVBlocker` backrun feed
//! (task 34LVLH).
//!
//! The feed event carries the call calldata of an unsigned pending tx. This
//! module turns `(to, data)` into a `TargetClass`:
//!
//! - `Swap` — decodable swap legs (protocol, pool hint where the wire names
//!   the pool directly, tokens, exact/min amounts),
//! - `Inert` — provably uninteresting for a backrun,
//! - `Opaque` — interesting but undecodable (unknown selector on a known hub,
//!   truncated calldata, or argument decoding failure).
//!
//! The classification is TABLE-DRIVEN and conservative: anything we cannot
//! decode with certainty is `Opaque`, never a guess. Unknown targets with
//! unknown selectors are `Inert` (a random contract call cannot be backrun
//! against pool state we track); known hubs with unknown selectors are
//! `Opaque` (interesting, worth surfacing, dangerous to act on).
//!
//! IMPORTANT: `SwapLeg::value` is NOT sourced from calldata — the native
//! `msg.value` rides on the feed event itself; the driver overlays it at
//! wiring time (NYVL2F). Router ETH-in paths put WETH as `token_in` via the
//! path array.

use alloy::hex::FromHex;
use alloy::primitives::{Address, U256};

const SEL_LEN: usize = 4;
const WORD: usize = 32;

/// Per-target intent of the pending call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetClass {
    /// One or more decodable swap legs.
    Swap(Vec<SwapLeg>),
    /// Provably uninteresting for backrun purposes.
    Inert,
    /// Interesting but not decodable with certainty. Never guess.
    Opaque(OpaqueReason),
}

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

/// Address table for known hubs; the owner wires protocol routers here as
/// they are identified (the v4 router is a registry concern, not a constant).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RouterRegistry {
    v2_router: Option<Address>,
    v3_router: Option<Address>,
    universal_router: Option<Address>,
    v3_npm: Option<Address>,
    v4_pool_manager: Option<Address>,
    v4_router: Option<Address>,
}

impl RouterRegistry {
    /// Canonical mainnet hub table.
    #[must_use]
    pub fn mainnet() -> Self {
        Self {
            v2_router: Some(addr("0x7a250d5630b4cf539739df2c5dacb4c659f2488d")),
            v3_router: Some(addr("0xE592427A0AEce92De3Edee1F18E0157C05861564")),
            universal_router: Some(addr("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD")),
            v3_npm: Some(addr("0xC36442b4a4522E871399CD717aBDD847Ab11FE88")),
            v4_pool_manager: Some(addr("0x000000000004444c5dc75cb358380d2e3de08a90")),
            v4_router: None,
        }
    }

    /// Extend the table with a newly identified hub (returns the previous
    /// binding, if any, so callers can journal rotation).
    pub fn register_v2_router(&mut self, a: Address) -> Option<Address> {
        self.v2_router.replace(a)
    }

    /// Bind a v4 router so its unlocks classify as actionable v4 targets
    /// (returns the previous binding, if any).
    pub fn register_v4_router(&mut self, a: Address) -> Option<Address> {
        self.v4_router.replace(a)
    }
}

// Embedded mainnet constants; every variant is exercised in unit tests.
#[expect(clippy::expect_used)]
fn addr(s: &str) -> Address {
    Address::from_hex(s.trim_start_matches("0x")).expect("embedded mainnet constants are valid hex")
}

mod sel {
    pub const PAIR_SWAP: [u8; 4] = [0x02, 0x2c, 0x0d, 0x9f];
    pub const V2_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
    pub const V2_TOKENS_FOR_EXACT_TOKENS: [u8; 4] = [0x4a, 0x25, 0xd9, 0x4a];
    pub const V2_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
    pub const V2_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
    pub const V2_FOT_TOKENS_FOR_TOKENS: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];
    pub const V2_FOT_TOKENS_FOR_ETH: [u8; 4] = [0x79, 0x1a, 0xc9, 0x47];
    pub const V3_EXACT_INPUT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
    pub const V3_EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
    pub const V3_EXACT_OUTPUT_SINGLE: [u8; 4] = [0xdb, 0x3e, 0x21, 0x98];
    pub const V3_EXACT_OUTPUT: [u8; 4] = [0xf2, 0x8c, 0x04, 0x91];
    pub const NPM_COLLECT: [u8; 4] = [0xac, 0x96, 0x50, 0xd8];
    pub const NPM_DECREASE_LIQUIDITY: [u8; 4] = [0x0e, 0xc9, 0x3d, 0x7c];
    pub const UR_EXECUTE_ARR: [u8; 4] = [0x24, 0x85, 0x6b, 0xc3];
    pub const UR_EXECUTE_BYTES: [u8; 4] = [0x35, 0x93, 0x56, 0x4c];
}

/// Classify a pending call. `to`/`data` come straight off a feed event.
#[must_use]
pub fn classify(to: Address, data: &[u8], reg: &RouterRegistry) -> TargetClass {
    if data.len() < SEL_LEN {
        return TargetClass::Opaque(OpaqueReason::CalldataTooShort);
    }
    let Ok(sel) = <[u8; 4]>::try_from(&data[..4]) else {
        return TargetClass::Opaque(OpaqueReason::CalldataTooShort);
    };
    let args = &data[SEL_LEN..];

    // Direct v2 pair swap — pool is the callee itself.
    if sel == sel::PAIR_SWAP {
        // swap(uint256 amount0Out, uint256 amount1Out, address to, bytes data)
        if args.len() < 4 * WORD {
            return TargetClass::Opaque(OpaqueReason::MalformedArgs);
        }
        let amount0 = read_u256(args, 0);
        let amount1 = read_u256(args, 1);
        let leg = SwapLeg {
            protocol: PoolProtocol::V2,
            pool: Some(to),
            token_in: None,
            token_out: None,
            value: U256::ZERO,
            amount_in: None,
            amount_out_min: None,
            amount_out: Some(amount0.max(amount1)),
            amount_in_max: None,
            hops: 0,
        };
        return TargetClass::Swap(vec![leg]);
    }

    if Some(to) == reg.v3_npm {
        if sel == sel::NPM_COLLECT || sel == sel::NPM_DECREASE_LIQUIDITY {
            return TargetClass::Inert;
        }
        return TargetClass::Opaque(OpaqueReason::HubSelectorUnknown);
    }
    if Some(to) == reg.universal_router {
        if sel == sel::UR_EXECUTE_ARR || sel == sel::UR_EXECUTE_BYTES {
            return TargetClass::Opaque(OpaqueReason::HubInnerUndecodable);
        }
        return TargetClass::Opaque(OpaqueReason::HubSelectorUnknown);
    }
    if reg.v4_router.is_some_and(|r| r == to) {
        // Registered v4 router: actionable target, inner unlock-bytes decode
        // is a documented follow-up.
        return TargetClass::Opaque(OpaqueReason::HubInnerUndecodable);
    }
    if Some(to) == reg.v4_pool_manager {
        // v4 swaps enter via a router holding `unlock(bytes)`; direct
        // pool-manager calls are liquidity/initialize ops.
        return TargetClass::Opaque(OpaqueReason::HubInnerUndecodable);
    }
    if Some(to) == reg.v2_router {
        return classify_v2_router(sel, args);
    }
    if Some(to) == reg.v3_router {
        return classify_v3_router(sel, args);
    }
    // Unknown target + selector: provably uninteresting for our pool set.
    TargetClass::Inert
}

fn is_v2_shape(sel: [u8; 4]) -> bool {
    sel == sel::V2_EXACT_TOKENS_FOR_TOKENS
        || sel == sel::V2_TOKENS_FOR_EXACT_TOKENS
        || sel == sel::V2_EXACT_ETH_FOR_TOKENS
        || sel == sel::V2_EXACT_TOKENS_FOR_ETH
        || sel == sel::V2_FOT_TOKENS_FOR_TOKENS
        || sel == sel::V2_FOT_TOKENS_FOR_ETH
}

fn classify_v2_router(sel: [u8; 4], args: &[u8]) -> TargetClass {
    if !is_v2_shape(sel) {
        return TargetClass::Opaque(OpaqueReason::HubSelectorUnknown);
    }
    let eth_in = sel == sel::V2_EXACT_ETH_FOR_TOKENS;
    let exact_out = sel == sel::V2_TOKENS_FOR_EXACT_TOKENS;
    // Static head layouts:
    //   token-side variants: (a, b, pathOff, to, deadline) — 5 words
    //   eth-in variant:      (minOut, pathOff, to, deadline) — 4 words
    let head_statics = usize::from(!eth_in) + 4;
    if args.len() < head_statics * WORD {
        return TargetClass::Opaque(OpaqueReason::MalformedArgs);
    }
    let (amount_in, amount_out_min, amount_out, amount_in_max, path_off_idx) = if eth_in {
        (None, Some(read_u256(args, 0)), None, None, 1)
    } else if exact_out {
        (
            None,
            None,
            Some(read_u256(args, 0)),
            Some(read_u256(args, 1)),
            2,
        )
    } else {
        (
            Some(read_u256(args, 0)),
            Some(read_u256(args, 1)),
            None,
            None,
            2,
        )
    };
    let Some(path_off) =
        read_usize(args, path_off_idx).filter(|&i| i + WORD <= args.len() && i % WORD == 0)
    else {
        return TargetClass::Opaque(OpaqueReason::MalformedArgs);
    };
    let Some(path) = read_address_array(args, path_off) else {
        return TargetClass::Opaque(OpaqueReason::MalformedArgs);
    };
    if path.len() < 2 {
        return TargetClass::Opaque(OpaqueReason::MalformedArgs);
    }
    let leg = SwapLeg {
        protocol: PoolProtocol::V2,
        pool: None,
        token_in: path.first().copied(),
        token_out: path.last().copied(),
        value: U256::ZERO,
        amount_in,
        amount_out_min,
        amount_out,
        amount_in_max,
        hops: u16::try_from(path.len().saturating_sub(1)).unwrap_or_default(),
    };
    TargetClass::Swap(vec![leg])
}

fn classify_v3_router(sel: [u8; 4], args: &[u8]) -> TargetClass {
    match sel {
        s if s == sel::V3_EXACT_INPUT_SINGLE => {
            // (ExactInputSingleParams): (tokenIn, tokenOut, fee, recipient,
            //  amountIn, amountOutMinimum, sqrtPriceLimitX96)
            let Some(t) = read_tuple(args) else {
                return TargetClass::Opaque(OpaqueReason::MalformedArgs);
            };
            if t.len() < 7 * WORD {
                return TargetClass::Opaque(OpaqueReason::MalformedArgs);
            }
            TargetClass::Swap(vec![SwapLeg {
                protocol: PoolProtocol::V3,
                pool: None,
                token_in: read_address(t, 0),
                token_out: read_address(t, 1),
                value: U256::ZERO,
                amount_in: Some(read_u256(t, 4)),
                amount_out_min: Some(read_u256(t, 5)),
                amount_out: None,
                amount_in_max: None,
                hops: 0,
            }])
        }
        s if s == sel::V3_EXACT_OUTPUT_SINGLE => {
            // (tokenIn, tokenOut, fee, recipient, amountOut, amountInMaximum, sqrtLimit)
            let Some(t) = read_tuple(args) else {
                return TargetClass::Opaque(OpaqueReason::MalformedArgs);
            };
            if t.len() < 7 * WORD {
                return TargetClass::Opaque(OpaqueReason::MalformedArgs);
            }
            TargetClass::Swap(vec![SwapLeg {
                protocol: PoolProtocol::V3,
                pool: None,
                token_in: read_address(t, 0),
                token_out: read_address(t, 1),
                value: U256::ZERO,
                amount_in: None,
                amount_out_min: None,
                amount_out: Some(read_u256(t, 4)),
                amount_in_max: Some(read_u256(t, 5)),
                hops: 0,
            }])
        }
        s if s == sel::V3_EXACT_INPUT => {
            // (ExactInputParams): (bytes path, address recipient,
            //  uint256 amountIn, uint256 amountOutMinimum)
            let Some(t) = read_tuple(args) else {
                return TargetClass::Opaque(OpaqueReason::MalformedArgs);
            };
            TargetClass::Swap(vec![SwapLeg {
                protocol: PoolProtocol::V3,
                pool: None,
                token_in: read_path_edge(t, &PathEdge::First),
                token_out: read_path_edge(t, &PathEdge::Last),
                value: U256::ZERO,
                amount_in: Some(read_u256(t, 2)),
                amount_out_min: Some(read_u256(t, 3)),
                amount_out: None,
                amount_in_max: None,
                hops: v3_path_hops(t).unwrap_or_default(),
            }])
        }
        s if s == sel::V3_EXACT_OUTPUT => {
            // (bytes path, address recipient, uint256 amountOut,
            //  uint256 amountInMaximum). For exact-output the encoded path
            // direction REVERSES relative to the swap (first = tokenOut).
            let Some(t) = read_tuple(args) else {
                return TargetClass::Opaque(OpaqueReason::MalformedArgs);
            };
            TargetClass::Swap(vec![SwapLeg {
                protocol: PoolProtocol::V3,
                pool: None,
                token_in: read_path_edge(t, &PathEdge::Last),
                token_out: read_path_edge(t, &PathEdge::First),
                value: U256::ZERO,
                amount_in: None,
                amount_out_min: None,
                amount_out: Some(read_u256(t, 2)),
                amount_in_max: Some(read_u256(t, 3)),
                hops: v3_path_hops(t).unwrap_or_default(),
            }])
        }
        _ => TargetClass::Opaque(OpaqueReason::HubSelectorUnknown),
    }
}

enum PathEdge {
    First,
    Last,
}

/// v3 multihop path: token(20) || fee(3) repeated; n tokens => n-1 hops.
fn v3_path_hops(tuple: &[u8]) -> Option<u16> {
    let path = read_bytes_at(tuple, 0)?;
    let tokens = if path.is_empty() {
        0
    } else {
        1 + (path.len().saturating_sub(20)) / 23
    };
    Some(u16::try_from(tokens.saturating_sub(1)).unwrap_or_default())
}

fn read_path_edge(tuple: &[u8], edge: &PathEdge) -> Option<Address> {
    let path = read_bytes_at(tuple, 0)?;
    match edge {
        PathEdge::First if path.len() >= 20 => Some(Address::from_slice(&path[..20])),
        PathEdge::Last if path.len() >= 20 => Some(Address::from_slice(&path[path.len() - 20..])),
        _ => None,
    }
}

#[inline]
fn read_u256(buf: &[u8], word: usize) -> U256 {
    let o = word * WORD;
    if o + WORD > buf.len() {
        return U256::ZERO;
    }
    U256::from_be_slice(&buf[o..o + WORD])
}

#[inline]
fn read_usize(buf: &[u8], word: usize) -> Option<usize> {
    usize::try_from(read_u256(buf, word)).ok()
}

#[inline]
fn read_address(buf: &[u8], word: usize) -> Option<Address> {
    let o = word * WORD + WORD - 20;
    Some(Address::from_slice(buf.get(o..o + 20)?))
}

fn read_address_array(buf: &[u8], offset: usize) -> Option<Vec<Address>> {
    if !offset.is_multiple_of(WORD) || offset / WORD >= buf.len() / WORD {
        return None;
    }
    let len = read_usize(buf, offset / WORD)?;
    let base = offset / WORD + 1;
    if len > 16 || base.checked_add(len)? * WORD > buf.len() {
        return None;
    }
    (0..len).map(|i| read_address(buf, base + i)).collect()
}

fn read_bytes_at(buf: &[u8], word: usize) -> Option<&[u8]> {
    let off = read_usize(buf, word)?;
    if off % WORD != 0 || off >= buf.len() {
        return None;
    }
    let len = read_usize(buf, off / WORD)?;
    let start = off + WORD;
    buf.get(start..start + len)
}

/// For `(Struct)` single-arg methods: follow the dynamic offset at arg0.
fn read_tuple(args: &[u8]) -> Option<&[u8]> {
    let off = read_usize(args, 0)?;
    if off % WORD != 0 {
        return None;
    }
    args.get(off..)
}
