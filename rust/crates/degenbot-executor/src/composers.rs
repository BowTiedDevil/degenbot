//! 2-hop + N-hop command-stream path composers.
//!
//! The composer layer combines the primitive `enc_*` opcode builders from
//! [`crate::encoders`] into a complete `cmd_executor` command-stream `bytes`
//! payload per path type, returning [`None`] on unsupported or failing paths.
//!
//! ## Sign conventions (§10.2)
//!
//! * V3 `amountSpecified` is a positive `uint96` exact-input (the contract
//!   negates it internally).
//! * V4 compact amounts are positive `uint96`; the contract negates for
//!   exact-input direction.
//!
//! ## Native ETH / WETH (§10.3)
//!
//! `NATIVE_CURRENCY_ADDRESS` (`address(0)`) is the V4 native-ETH currency.
//! WETH and native ETH are distinct PM delta currencies — when a path crosses
//! the WETH↔native boundary, an explicit `WETH_DEPOSIT` (wrap) or
//! `WETH_WITHDRAW` (unwrap) bridges the representation gap inside `V4_UNLOCK`.

// composers.rs — arbitrage path encoders for the `cmd_executor` contract.
//
// 2-hop and 3-hop composers all take a `&ComposerInputs` bundle beyond the
// hops, so none trips `too_many_arguments`.

use crate::encoders::{
    self, AddressTable, V4BatchEntry, NATIVE_ADDRESS, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH,
};
use alloy::primitives::{Address, U256};
use degenbot_abi::abi_encoder::encode_rust;
use degenbot_abi::abi_types::AbiValue;

/// `NATIVE_CURRENCY_ADDRESS` — V4's native-ETH currency is `address(0)`.
///
/// Mirrors `degenbot.uniswap.v4_liquidity_pool.NATIVE_CURRENCY_ADDRESS` and
/// the encoders' [`NATIVE_ADDRESS`] (the same `Address::ZERO`).
pub const NATIVE_CURRENCY_ADDRESS: Address = Address::ZERO;

/// The `execute(bytes,uint256)` 4-byte function selector
/// (`keccak256("execute(bytes,uint256)")[:4]` = `0xab5898e8`).
///
/// The 4-byte selector for `execute(bytes,uint256)`, `0xab5898e8`.
pub const EXECUTE_SELECTOR: [u8; 4] = [0xab, 0x58, 0x98, 0xe8];

/// `INT128_MAX` = 2¹²⁷ − 1 — the upper bound the Python `fits_int128` guard
/// checks for every explicitly-amounted swap.
const INT128_MAX_U128: u128 = (1u128 << 127) - 1;

/// True if `value` fits in a signed 128-bit integer (the Python
/// `fits_int128` guard, restricted to the non-negative amounts the composers
/// accept).
fn fits_int128(value: u128) -> bool {
    value <= INT128_MAX_U128
}

// ═══════════════════════════════════════════════════════════════════════════
// Hop descriptors + PathInfo
// ═══════════════════════════════════════════════════════════════════════════

/// Engine-facing hop descriptor — the Rust mirror of the Python
/// `V2HopInfo`/`V3HopInfo`/`V4HopInfo` dataclasses in
/// `src/degenbot/arbitrage/hop_info.py`.
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
/// `fee` is informational only — V3 fees are encoded in the pool address,
/// not the command stream.
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
    /// Ordered hops; the V2/V3/V4 mix in `hops` selects the composer.
    pub hops: Vec<HopInfo>,
}

impl PathInfo {
    /// Construct from an ordered hop slice.
    #[must_use]
    pub fn new(hops: Vec<HopInfo>) -> Self {
        Self { hops }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// V4 native helpers
// ═══════════════════════════════════════════════════════════════════════════

/// True if the V4 swap's **input** currency is native ETH (`address(0)`).
///
/// `zfo=True` ⇒ input is `currency0`; `zfo=False` ⇒ input is `currency1`.
#[must_use]
pub fn v4_input_is_native(hop: &V4HopInfo) -> bool {
    let input_currency = if hop.zfo {
        hop.currency0_address
    } else {
        hop.currency1_address
    };
    input_currency == NATIVE_CURRENCY_ADDRESS
}

/// True if the V4 swap's **output** currency is native ETH (`address(0)`).
///
/// `zfo=True` ⇒ output is `currency1`; `zfo=False` ⇒ output is `currency0`.
#[must_use]
pub fn v4_output_is_native(hop: &V4HopInfo) -> bool {
    let output_currency = if hop.zfo {
        hop.currency1_address
    } else {
        hop.currency0_address
    };
    output_currency == NATIVE_CURRENCY_ADDRESS
}

// ═══════════════════════════════════════════════════════════════════════════
// Currency-bridge helpers (native-ETH ↔ WETH representation gap)
// ═══════════════════════════════════════════════════════════════════════════

/// The representation-bridge action needed at a V4↔X currency boundary.
///
/// V4 tracks native ETH and WETH as distinct delta currencies. When a
/// path's hop A outputs one and hop B's input expects the other, an explicit
/// `WETH_DEPOSIT` (wrap native→WETH) or `WETH_WITHDRAW` (unwrap WETH→native)
/// must bridge the gap inside `V4_UNLOCK` before hop B runs. See §10.3 of the
/// crate docs + `/workspaces/executor/tests/test_cmd_executor_v4v4_wrap_unwrap.py`
/// for the canonical on-chain pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrencyBridge {
    /// No bridge — both sides agree (both native or both WETH/ERC20).
    None,
    /// V4 output is native ETH, hop B needs WETH → `V4_TAKE_COMPACT(native)` + `WETH_DEPOSIT`.
    Wrap,
    /// V4 output is WETH, hop B needs native ETH → `V4_TAKE_COMPACT(weth)` + `WETH_WITHDRAW`.
    Unwrap,
}

impl CurrencyBridge {
    /// `true` when a wrap or unwrap is required at this boundary.
    #[must_use]
    pub const fn needs_bridge(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Classify the boundary from the mid-currencies of two adjacent hops.
    ///
    /// `output_currency_a` is the currency hop A *delivers* (its output
    /// currency); `input_currency_b` is the currency hop B *consumes* (its
    /// input currency). Only native-ETH (`address(0)`) vs anything-else is
    /// the distinguishing axis — WETH addresses and other ERC-20s are all
    /// "non-native" from the bridge's perspective.
    #[must_use]
    pub fn at_boundary(output_currency_a: Address, input_currency_b: Address) -> Self {
        let a_native = output_currency_a == NATIVE_CURRENCY_ADDRESS;
        let b_native = input_currency_b == NATIVE_CURRENCY_ADDRESS;
        match (a_native, b_native) {
            (true, false) => Self::Wrap,
            (false, true) => Self::Unwrap,
            _ => Self::None,
        }
    }

    /// The address-table indices a bridge boundary needs: `(take_idx,
    /// settle_idx)`.
    ///
    /// `take_idx` is the currency to `V4_TAKE_COMPACT` *from* the PoolManager
    /// (the source side of the representation gap: native for `Wrap`, WETH
    /// for `Unwrap`). `settle_idx` is the currency to `V4_SETTLE_DELTA` *into*
    /// the PoolManager after the downstream swap runs (the consumed side:
    /// WETH for `Wrap`, native for `Unwrap`) — the swap debited the opposite
    /// representation, so the executor settles the one it now holds.
    ///
    /// Call only when [`needs_bridge`] is true; for [`Self::None`] both
    /// indices are `0` (unused). `weth_idx` / `native_idx` are the
    /// address-table sentinels (typically `SENTINEL_WETH` / `SENTINEL_NATIVE`).
    ///
    /// [`needs_bridge`]: Self::needs_bridge
    #[must_use]
    pub const fn bridge_indices(self, weth_idx: u8, native_idx: u8) -> (u8, u8) {
        match self {
            Self::None => (0, 0), // caller guards `needs_bridge()`
            // Wrap: take native out, deposit as WETH, swap consumes WETH → settle WETH.
            Self::Wrap => (native_idx, weth_idx),
            // Unwrap: take WETH out, withdraw to native, swap consumes native → settle native.
            Self::Unwrap => (weth_idx, native_idx),
        }
    }
}

/// Emit the `V4_TAKE_COMPACT` + `WETH_DEPOSIT`/`WETH_WITHDRAW` bridge bytes
/// for a [`CurrencyBridge`] into `inner`.
///
/// `currency_idx` is the address-table index of the currency to take from
/// the PoolManager: the native-ETH index (`SENTINEL_NATIVE` or a registered
/// table entry) for [`CurrencyBridge::Wrap`], or the WETH index
/// (`SENTINEL_WETH`) for [`CurrencyBridge::Unwrap`]. `amount` is the
/// forward output hop A produced (the quantity to wrap or unwrap).
///
/// Returns `None` (for `?` propagation) only if `V4_TAKE_COMPACT` fails to
/// encode (uint96 overflow — caller should have guarded with `fits_int128`).
/// [`CurrencyBridge::None`] emits nothing.
pub(crate) fn emit_currency_bridge(
    inner: &mut Vec<u8>,
    bridge: CurrencyBridge,
    currency_idx: u8,
    amount: u128,
) -> Option<()> {
    match bridge {
        CurrencyBridge::None => {}
        CurrencyBridge::Wrap => {
            inner.extend_from_slice(
                &encoders::enc_v4_take_compact(currency_idx, SENTINEL_SELF, amount).ok()?,
            );
            inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(amount)));
        }
        CurrencyBridge::Unwrap => {
            inner.extend_from_slice(
                &encoders::enc_v4_take_compact(currency_idx, SENTINEL_SELF, amount).ok()?,
            );
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(amount)));
        }
    }
    Some(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Top-level dispatcher
// ═══════════════════════════════════════════════════════════════════════════

/// Tuning knobs for [`encode_cmd_stream`]. All default to `false`/`0`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeOptions {
    /// If `true`, use `V4_MINT_COMPACT` instead of `V4_TAKE_DELTA` for profit
    /// capture on pure-V4 paths (saves ~20K gas; needs `check_mode=2`).
    pub erc6909_profit: bool,
    /// If `true`, use `V4_BATCH` instead of individual `V4_SWAP_COMPACT`/`_DYNAMIC`
    /// for pure-V4 paths (single PM extcall).
    pub use_v4_batch: bool,
}

/// Bundled context every composer needs beyond the hops.
///
/// Built once per path (in [`encode_cmd_stream`] / [`encode_cmd_3_hop`]) and
/// passed by reference to each composer, collapsing every signature to
/// `(hops.., &ComposerInputs)` so no composer trips `too_many_arguments`.
#[derive(Clone, Copy)]
pub struct ComposerInputs<'a> {
    pub executor_address: Address,
    pub pool_manager_address: Address,
    pub weth_address: Address,
    pub optimal_input: u128,
    pub hop_outputs: &'a [u128],
    /// The per-hop executable input fed into each pool, as set by the solver's
    /// CL-hop clamp (`consumed_inputs[i]`). For a non-over-fed CL hop (and for
    /// V2/Curve/Balancer/Solidly hops) this equals `hop_outputs[i-1]`; for an
    /// over-fed CL hop the clamp reduces it to `input_consumed - 1` so the
    /// on-chain exact-in loop terminates on `amountRemaining == 0` instead of
    /// marching empty bitmap words (UO3JM4 / path-5000 EMPTY-HALT).
    pub consumed_inputs: &'a [u128],
    pub opts: EncodeOptions,
}

/// Encode an arbitrage path as a `cmd_executor` command stream.
///
/// Produces a `bytes` payload for `execute(commands)` on the `cmd_executor`
/// contract. Uses compact command encoding (`V2_SWAP_COMPACT`, `V2_SWAP_CALC`,
/// `V4_SWAP_COMPACT`, …) with an address table for minimal calldata size.
///
/// Returns `None` if encoding fails for this path type.
///
/// # Path-type routing
///
/// * all-V2 hops (≥2): [`encode_cmd_v2_n_hop`]
/// * 2-hop V4-V4 / V4-V3 / V3-V4 / V4-V2 / V2-V4 / V3-V3 / V2-V3 / V3-V2:
///   the corresponding `encode_cmd_*` two-hop composer
/// * 3-hop: [`encode_cmd_3_hop`] (all 27 V2/V3/V4 combinations)
#[must_use]
#[allow(clippy::too_many_arguments)] // encoder args (path + inputs + executor/pm/weth + opts)
pub fn encode_cmd_stream(
    path_info: &PathInfo,
    optimal_input: u128,
    hop_outputs: &[u128],
    consumed_inputs: &[u128],
    executor_address: Address,
    pool_manager_address: Address,
    weth_address: Address,
    opts: EncodeOptions,
) -> Option<Vec<u8>> {
    // Bribes are moved out of the stream into the `config` ABI parameter —
    // the caller passes bribes via `pack_config` at the call site, so there
    // is nothing to reject here.
    let num_hops = path_info.hops.len();
    let inputs = ComposerInputs {
        executor_address,
        pool_manager_address,
        weth_address,
        optimal_input,
        hop_outputs,
        consumed_inputs,
        opts,
    };

    // Generalized N-hop V2 (≥2 hops): flash borrow + chained V2_SWAP_CALC.
    if path_info.hops.iter().all(|h| matches!(h, HopInfo::V2(_))) {
        return encode_cmd_v2_n_hop(path_info, &inputs);
    }

    // 2-hop paths.
    if num_hops == 2 {
        let hops = &path_info.hops;
        match (&hops[0], &hops[1]) {
            (HopInfo::V4(a), HopInfo::V4(b)) => encode_cmd_v4_v4(a, b, &inputs),
            (HopInfo::V4(a), HopInfo::V3(b)) => encode_cmd_v4_v3(a, b, &inputs),
            (HopInfo::V3(a), HopInfo::V4(b)) => encode_cmd_v3_v4(a, b, &inputs),
            (HopInfo::V4(a), HopInfo::V2(b)) => encode_cmd_v4_v2(a, b, &inputs),
            (HopInfo::V2(a), HopInfo::V4(b)) => encode_cmd_v2_v4(a, b, &inputs),
            (HopInfo::V3(a), HopInfo::V3(b)) => encode_cmd_v3_v3(a, b, &inputs),
            (HopInfo::V2(a), HopInfo::V3(b)) => encode_cmd_v2_v3(a, b, &inputs),
            (HopInfo::V3(a), HopInfo::V2(b)) => encode_cmd_v3_v2(a, b, &inputs),
            // 2-hop Solidly / mixed-V2-V3-with-Solidly: not supported.
            _ => None,
        }
    } else if num_hops == 3 {
        encode_cmd_3_hop(
            path_info,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            executor_address,
            pool_manager_address,
            weth_address,
            opts,
        )
    } else {
        // 4+ hops and other unsupported arities → None.
        None
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// N-hop V2
// ═══════════════════════════════════════════════════════════════════════════

/// Encode N-hop V2 (N ≥ 2) arbitrage as a `cmd_executor` command stream.
///
/// Pattern: flash-borrow from pool A, `V2_SWAP_CALC` for pools B..N-1 with
/// direct custody (recipient = next pool, which accumulates the excess
/// balance that the next `V2_SWAP_CALC` reads as input), `V2_SWAP_CALC` pool
/// N → executor, then `ERC20_TRANSFER` repay pool A's flash borrow.
///
/// Returns `None` if any hop is non-V2, N < 2, any output ≤ 0, or any `enc_*`
/// step fails.
#[allow(clippy::too_many_lines)]
fn encode_cmd_v2_n_hop(path_info: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let v2_hops: Vec<&V2HopInfo> = path_info
        .hops
        .iter()
        .filter_map(|h| match h {
            HopInfo::V2(h) => Some(h),
            _ => None, // a non-V2 hop slipped in — unsupported
        })
        .collect();
    let num_hops = v2_hops.len();
    if num_hops < 2 {
        return None;
    }
    if hop_outputs.contains(&0) {
        // any(hop_outputs <= 0)
        return None;
    }

    let executor_idx = SENTINEL_SELF;

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);

    // Register all pool addresses (preserves insertion order).
    let pool_indices: Vec<u8> = v2_hops
        .iter()
        .map(|h| at.add(h.pool_address).ok())
        .collect::<Option<Vec<u8>>>()?;

    // Forward token from pool A (the intermediate sent to pool B).
    let hop_a = v2_hops[0];
    let zfo_a = hop_a.zfo;
    let forward_addr = if zfo_a {
        hop_a.token1_address
    } else {
        hop_a.token0_address
    };
    let forward_idx = at.add(forward_addr).ok()?;

    // WETH repayment token — the input token to pool A (output of last pool).
    let hop_last = v2_hops[num_hops - 1];
    let weth_addr = if hop_last.zfo {
        hop_last.token1_address
    } else {
        hop_last.token0_address
    };
    let weth_idx = at.add(weth_addr).ok()?;

    // 1. Transfer forward token to pool B (creates excess balance).
    let mut callback =
        encoders::enc_erc20_transfer(forward_idx, pool_indices[1], hop_outputs[0]).ok()?;

    // 2..N. V2_SWAP_CALC for each subsequent pool.
    for i in 1..num_hops {
        let hop = v2_hops[i];
        let recipient_idx = if i < num_hops - 1 {
            pool_indices[i + 1]
        } else {
            executor_idx
        };
        callback.extend_from_slice(&encoders::enc_v2_swap_calc(
            pool_indices[i],
            hop.zfo,
            recipient_idx,
            hop.fee,
        ));
    }

    // N+1. Flash repayment: WETH back to pool A.
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, pool_indices[0], optimal_input).ok()?,
    );

    // Top-level: V2_SWAP_COMPACT on pool A (flash borrow, recipient=executor).
    let commands = encoders::enc_v2_swap_compact(
        pool_indices[0],
        zfo_a,
        hop_outputs[0],
        executor_idx,
        hop_a.fee,
        &callback,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V4-V4
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V4-V4 2-hop arbitrage as a `cmd_executor` command stream.
///
/// Both swaps inside a single `V4_UNLOCK`. Three cases:
/// * **Same intermediate currency** (WETH↔WETH or ETH↔ETH): pool A
///   `V4_SWAP_COMPACT`, pool B `V4_SWAP_DYNAMIC` (reads delta ledger),
///   `V4_TAKE_DELTA` profit, `V4_SETTLE_ALL`.
/// * **Pool A outputs WETH, pool B needs native ETH**: pool A compact →
///   `V4_TAKE(WETH)` → `WETH_WITHDRAW` → pool B compact → `V4_SETTLE(native)`
///   → `V4_TAKE_DELTA(profit)` → `V4_SETTLE_ALL`.
/// * **Pool A outputs native ETH, pool B needs WETH**: pool A compact →
///   `V4_TAKE(ETH)` → `WETH_DEPOSIT` → pool B compact → `V4_SETTLE(WETH)` →
///   `V4_TAKE_DELTA(profit)` → `V4_SETTLE_ALL`.
#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn encode_cmd_v4_v4(
    hop_a: &V4HopInfo,
    hop_b: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;
    let opts = inputs.opts;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    // Int128 overflow guard for every explicitly-amounted swap.
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    // The hop-1 V4 pool's explicit swap-in is the solver's CL-clamp executable
    // forward (`consumed_inputs[1]`), not pool A's full output. (Only the
    // currency-gap branch mounts a fixed-amount B swap; the no-gap branch reads
    // the delta ledger and needs no amount.)
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mid_currency_a = if hop_a.zfo {
        hop_a.currency1_address
    } else {
        hop_a.currency0_address
    };
    let mid_currency_b = if hop_b.zfo {
        hop_b.currency0_address
    } else {
        hop_b.currency1_address
    };
    let input_currency_a = if hop_a.zfo {
        hop_a.currency0_address
    } else {
        hop_a.currency1_address
    };
    let output_currency_b = if hop_b.zfo {
        hop_b.currency1_address
    } else {
        hop_b.currency0_address
    };

    let a_outputs_native = mid_currency_a == NATIVE_CURRENCY_ADDRESS;
    let b_needs_native = mid_currency_b == NATIVE_CURRENCY_ADDRESS;
    let bridge = CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b);
    let currency_gap = bridge.needs_bridge();

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;

    let c0_a_idx = at.add(hop_a.currency0_address).ok()?;
    let c1_a_idx = at.add(hop_a.currency1_address).ok()?;
    let c0_b_idx = at.add(hop_b.currency0_address).ok()?;
    let c1_b_idx = at.add(hop_b.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let mut native_idx: u8 = SENTINEL_NATIVE;
    if a_outputs_native
        || b_needs_native
        || input_currency_a == NATIVE_CURRENCY_ADDRESS
        || output_currency_b == NATIVE_CURRENCY_ADDRESS
    {
        native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
    }

    let a_fee = u16::try_from(hop_a.fee).ok()?;
    let a_ts = i16::try_from(hop_a.tick_spacing).ok()?;
    let b_fee = u16::try_from(hop_b.fee).ok()?;
    let b_ts = i16::try_from(hop_b.tick_spacing).ok()?;

    // 1. V4 swap for pool A (always explicit amount).
    let mut inner: Vec<u8> = if !opts.use_v4_batch || currency_gap {
        encoders::enc_v4_swap_compact(
            c0_a_idx,
            c1_a_idx,
            a_fee,
            a_ts,
            zero_idx,
            hop_a.zfo,
            optimal_input,
        )
        .ok()?
    } else {
        Vec::new() // V4_BATCH will handle both swaps
    };

    if currency_gap {
        // Take the intermediate token out of PM, bridge the native↔WETH gap.
        let bridge_idx = match bridge {
            CurrencyBridge::Wrap => native_idx,
            CurrencyBridge::Unwrap => weth_idx,
            CurrencyBridge::None => unreachable!("currency_gap implies a bridge"),
        };
        emit_currency_bridge(&mut inner, bridge, bridge_idx, forward_out)?;
        // 4. V4_SWAP_COMPACT for pool B (explicit amount).
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b_idx, c1_b_idx, b_fee, b_ts, zero_idx, hop_b.zfo, b_swap_in,
            )
            .ok()?,
        );
        // 5. Settle pool B's input currency.
        if b_needs_native {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        }
        // 6. Take profit (output currency of pool B via delta ledger).
        if output_currency_b == NATIVE_CURRENCY_ADDRESS {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
        } else if output_currency_b == weth_address {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
        } else {
            let profit_idx = if hop_b.zfo { c1_b_idx } else { c0_b_idx };
            inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
        }
        // 7. Auto-settle remaining deltas (rounding residuals).
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    } else {
        // Same intermediate currency — delta netting.
        if opts.use_v4_batch {
            let batch = [
                V4BatchEntry {
                    c0_idx: c0_a_idx,
                    c1_idx: c1_a_idx,
                    fee: a_fee,
                    tick_spacing: a_ts,
                    hooks_idx: zero_idx,
                    zfo: hop_a.zfo,
                    amount_u96: optimal_input,
                },
                V4BatchEntry {
                    c0_idx: c0_b_idx,
                    c1_idx: c1_b_idx,
                    fee: b_fee,
                    tick_spacing: b_ts,
                    hooks_idx: zero_idx,
                    zfo: hop_b.zfo,
                    amount_u96: 0,
                },
            ];
            inner.extend_from_slice(&encoders::enc_v4_batch(&batch).ok()?);
            // V4_BATCH auto-settles WETH + native ETH deltas; ERC-20 still needs explicit take.
            if output_currency_b != NATIVE_CURRENCY_ADDRESS && output_currency_b != weth_address {
                let profit_idx = if hop_b.zfo { c1_b_idx } else { c0_b_idx };
                inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
            }
        } else {
            inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
                c0_b_idx, c1_b_idx, b_fee, b_ts, zero_idx, hop_b.zfo,
            ));
        }

        // Profit capture: V4_MINT_COMPACT (ERC6909) or V4_TAKE_DELTA (physical).
        if opts.erc6909_profit && output_currency_b == weth_address {
            let profit_amount = weth_out.saturating_sub(optimal_input);
            if profit_amount > 0 {
                inner.extend_from_slice(
                    &encoders::enc_v4_mint_compact(weth_idx, SENTINEL_SELF, profit_amount).ok()?,
                );
            }
        } else if !opts.use_v4_batch
            || (output_currency_b != NATIVE_CURRENCY_ADDRESS && output_currency_b != weth_address)
        {
            if output_currency_b == NATIVE_CURRENCY_ADDRESS {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
            } else if output_currency_b == weth_address {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
            } else {
                let profit_idx = if hop_b.zfo { c1_b_idx } else { c0_b_idx };
                inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
            }
        }

        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    }

    let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.append(&mut commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V4-V3
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V4-V3 2-hop arbitrage as a `cmd_executor` command stream.
///
/// All inside `V4_UNLOCK`. When V4 outputs native ETH, V3 needs WETH — insert
/// `WETH_DEPOSIT` between `V4_TAKE` and the V3 swap, then V3 auto-pays. When
/// V4 outputs an ERC-20, V3 auto-pays; if V4's input was native ETH, `WETH_WITHDRAW`
/// + `V4_SETTLE_DELTA(native)` before the final `V4_SETTLE_ALL`.
#[allow(clippy::too_many_lines)]
fn encode_cmd_v4_v3(
    hop_v4: &V4HopInfo,
    hop_v3: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // The V3 hop's swap-in is the solver's CL-clamp executable forward
    // (`consumed_inputs[1]`), not the V4 pool's full output. The V4 take stays
    // on `forward_out` (the V4 output is real). V4 is hop 0 (feeds
    // `optimal_input`), which the clamp never changes.
    let v3_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v3_swap_in) {
        return None;
    }

    let v4_out_native = v4_output_is_native(hop_v4);

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;

    let c0_v4_idx = at.add(hop_v4.currency0_address).ok()?;
    let c1_v4_idx = at.add(hop_v4.currency1_address).ok()?;
    let v3_idx = at.add(hop_v3.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let mut native_idx: u8 = SENTINEL_NATIVE;
    if v4_out_native {
        native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
    }

    let v4_fee = u16::try_from(hop_v4.fee).ok()?;
    let v4_ts = i16::try_from(hop_v4.tick_spacing).ok()?;

    // 1. V4 swap (input→forward).
    let mut inner = encoders::enc_v4_swap_compact(
        c0_v4_idx,
        c1_v4_idx,
        v4_fee,
        v4_ts,
        zero_idx,
        hop_v4.zfo,
        optimal_input,
    )
    .ok()?;

    if v4_out_native {
        // V4 output is native ETH — take to executor, then wrap.
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        // 3. V3 swap with auto-pay (executor has WETH after deposit).
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, hop_v3.zfo, v3_swap_in, SENTINEL_SELF, &[])
                .ok()?,
        );
        // 4. Settle V4's input currency debt.
        let input_idx = if hop_v4.zfo { c0_v4_idx } else { c1_v4_idx };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    } else {
        // V4 output is ERC-20 — take to executor, V3 auto-pay.
        let forward_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, hop_v3.zfo, v3_swap_in, SENTINEL_SELF, &[])
                .ok()?,
        );
        // 4. Settle V4's input currency debt. V3 sent WETH to executor.
        let v4_in_native = v4_input_is_native(hop_v4);
        if v4_in_native {
            let input_idx = if hop_v4.zfo { c0_v4_idx } else { c1_v4_idx };
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(optimal_input)));
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        }
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    }

    let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.append(&mut commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V3-V4
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V3-V4 2-hop arbitrage as a `cmd_executor` command stream.
///
/// `V3_SWAP_COMPACT` with a `forward_data` callback that runs `V4_UNLOCK`
/// (sourcing the WETH the executor needs to pay V3's debt). When V4 requires
/// native ETH input, the WETH output is unwrapped via `WETH_WITHDRAW` first.
#[allow(clippy::too_many_lines)]
fn encode_cmd_v3_v4(
    hop_v3: &V3HopInfo,
    hop_v4: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(weth_out) {
        return None;
    }
    // The V4 hop's swap-in is the solver's CL-clamp executable forward
    // (`consumed_inputs[1]`), not the V3 pool's full output.
    let v4_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v4_swap_in) {
        return None;
    }

    let v4_in_native = v4_input_is_native(hop_v4);

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;

    let v3_idx = at.add(hop_v3.pool_address).ok()?;
    let c0_v4_idx = at.add(hop_v4.currency0_address).ok()?;
    let c1_v4_idx = at.add(hop_v4.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let mut native_idx: u8 = SENTINEL_NATIVE;
    if v4_in_native {
        native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
    }

    let v4_fee = u16::try_from(hop_v4.fee).ok()?;
    let v4_ts = i16::try_from(hop_v4.tick_spacing).ok()?;

    let v3_callback = if v4_in_native {
        // V4 needs native ETH — unwrap WETH→ETH first, then V4 swap + settle + take.
        let mut v4_inner = encoders::enc_v4_swap_compact(
            c0_v4_idx, c1_v4_idx, v4_fee, v4_ts, zero_idx, hop_v4.zfo, v4_swap_in,
        )
        .ok()?;
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        let output_currency = if hop_v4.zfo {
            hop_v4.currency1_address
        } else {
            hop_v4.currency0_address
        };
        if output_currency == NATIVE_CURRENCY_ADDRESS {
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        } else {
            let output_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        }
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        // V3 callback: unwrap, V4 unlock, pay V3's forward-token debt.
        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        let input_currency_v3 = if hop_v3.zfo {
            hop_v3.token0_address
        } else {
            hop_v3.token1_address
        };
        if input_currency_v3 == weth_address || input_currency_v3 == NATIVE_CURRENCY_ADDRESS {
            // V3 owed WETH but V4 gave USDC — unsupported native-ETH-V4 mismatch.
            return None;
        }
        let forward_v3_idx = at.add(input_currency_v3).ok()?;
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_v3_idx, v3_idx, optimal_input).ok()?,
        );
        cb
    } else {
        // V4 needs WETH — standard sync+transfer+settle+swap+take.
        let forward_addr = if hop_v3.zfo {
            hop_v3.token1_address
        } else {
            hop_v3.token0_address
        };
        let forward_idx = at.add(forward_addr).ok()?;

        let mut v4_inner = encoders::enc_v4_sync(forward_idx);
        v4_inner.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, pm_idx, forward_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_v4_idx, c1_v4_idx, v4_fee, v4_ts, zero_idx, hop_v4.zfo, v4_swap_in,
            )
            .ok()?,
        );
        let output_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        // V3 callback: V4 unlock + pay V3's WETH debt.
        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3_idx, optimal_input).ok()?);
        cb
    };

    // Top-level: V3 swap with forward_data callback (V3 amount = optimal_input).
    let commands = encoders::enc_v3_swap_compact(
        v3_idx,
        hop_v3.zfo,
        optimal_input,
        SENTINEL_SELF,
        &v3_callback,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V4-V2
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V4-V2 2-hop arbitrage as a `cmd_executor` command stream.
///
/// V4 swap inside `V4_UNLOCK`; take the forward token to the V2 pair (direct
/// custody or wrap/unwrap), `V2_SWAP_CALC` or `V2_SWAP_COMPACT` reads the
/// excess and outputs to the executor, then settle V4's input debt.
#[allow(clippy::too_many_lines)]
fn encode_cmd_v4_v2(
    hop_v4: &V4HopInfo,
    hop_v2: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }

    let v4_out_native = v4_output_is_native(hop_v4);

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;

    let c0_v4_idx = at.add(hop_v4.currency0_address).ok()?;
    let c1_v4_idx = at.add(hop_v4.currency1_address).ok()?;
    let v2_idx = at.add(hop_v2.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let mut native_idx: u8 = SENTINEL_NATIVE;
    if v4_out_native {
        native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
    }

    let v4_fee = u16::try_from(hop_v4.fee).ok()?;
    let v4_ts = i16::try_from(hop_v4.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_v4_idx,
        c1_v4_idx,
        v4_fee,
        v4_ts,
        zero_idx,
        hop_v4.zfo,
        optimal_input,
    )
    .ok()?;

    if v4_out_native {
        // V4 output is native ETH, V2 needs WETH.
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        // 3. V2_SWAP_COMPACT with callback — V2 sends USDC to executor, then
        //    the callback pays WETH to V2 from the just-wrapped balance.
        //    (Cannot use V2_SWAP_CALC because on-chain output ≠ solver amount.)
        let v2_cb_cmds = encoders::enc_erc20_transfer(weth_idx, v2_idx, forward_out).ok()?;
        let v2_cmd = encoders::enc_v2_swap_compact(
            v2_idx,
            hop_v2.zfo,
            weth_out,
            SENTINEL_SELF,
            hop_v2.fee,
            &v2_cb_cmds,
        )
        .ok()?;
        inner.extend_from_slice(&v2_cmd);
        let input_idx = if hop_v4.zfo { c0_v4_idx } else { c1_v4_idx };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    } else {
        // V4 output is ERC-20 — take directly to V2 pool (direct custody).
        let forward_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, v2_idx, forward_out).ok()?,
        );
        // V2_SWAP_CALC reads excess balance in V2 pool, no callback needed.
        inner.extend_from_slice(&encoders::enc_v2_swap_calc(
            v2_idx,
            hop_v2.zfo,
            SENTINEL_SELF,
            hop_v2.fee,
        ));
        // Settle V4's input-currency debt.
        let v4_in_native = v4_input_is_native(hop_v4);
        if v4_in_native {
            let input_idx = if hop_v4.zfo { c0_v4_idx } else { c1_v4_idx };
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(optimal_input)));
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_sync(weth_idx));
            inner.extend_from_slice(
                &encoders::enc_erc20_transfer(weth_idx, pm_idx, optimal_input).ok()?,
            );
            inner.extend_from_slice(&encoders::enc_v4_settle());
        }
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    }

    let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.append(&mut commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V2-V4
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V2-V4 2-hop arbitrage as a `cmd_executor` command stream.
///
/// V2 flash borrow runs first (with a callback), then `V4_UNLOCK` wraps
/// sync+transfer+settle+swap+take. When V4 requires native ETH input, V2's
/// WETH output is unwrapped first; when V4 outputs native ETH, the callback
/// wraps it back to WETH before paying V2.
#[allow(clippy::too_many_lines)]
fn encode_cmd_v2_v4(
    hop_v2: &V2HopInfo,
    hop_v4: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(weth_out) {
        return None;
    }
    // The V4 hop's swap-in is the solver's CL-clamp executable forward
    // (`consumed_inputs[1]`), not the V2 pool's full output. Leave the V2-side
    // WETH-bridge / feed amounts on `forward_out` (the V2 output is real).
    let v4_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v4_swap_in) {
        return None;
    }

    let v4_in_native = v4_input_is_native(hop_v4);

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;

    let v2_idx = at.add(hop_v2.pool_address).ok()?;
    let c0_v4_idx = at.add(hop_v4.currency0_address).ok()?;
    let c1_v4_idx = at.add(hop_v4.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let mut native_idx_in: u8 = SENTINEL_NATIVE;
    if v4_in_native {
        native_idx_in = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
    }

    // Forward token = output of V2 swap.
    let forward_addr = if hop_v2.zfo {
        hop_v2.token1_address
    } else {
        hop_v2.token0_address
    };
    let forward_idx = at.add(forward_addr).ok()?;

    let v4_fee = u16::try_from(hop_v4.fee).ok()?;
    let v4_ts = i16::try_from(hop_v4.tick_spacing).ok()?;

    let callback_cmds: Vec<u8>;
    if v4_in_native {
        // V4 needs native ETH — unwrap WETH first, then V4 swap + settle + take.
        let mut v4_inner = encoders::enc_v4_swap_compact(
            c0_v4_idx, c1_v4_idx, v4_fee, v4_ts, zero_idx, hop_v4.zfo, v4_swap_in,
        )
        .ok()?;
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx_in));
        let output_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, v2_idx, optimal_input).ok()?,
        );
        callback_cmds = cb;
    } else {
        // V4 input is ERC-20 (not native ETH). Check V4 OUTPUT native.
        let v4_out_native = v4_output_is_native(hop_v4);

        let mut v4_inner = encoders::enc_v4_sync(forward_idx);
        v4_inner.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, pm_idx, forward_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_v4_idx, c1_v4_idx, v4_fee, v4_ts, zero_idx, hop_v4.zfo, v4_swap_in,
            )
            .ok()?,
        );
        let native_idx_out: u8;
        if v4_out_native {
            native_idx_out = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx_out, SENTINEL_SELF, weth_out).ok()?,
            );
        } else {
            let output_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        }
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        // V2 callback: V4 unlock first, then pay V2.
        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        if v4_out_native {
            cb.extend_from_slice(&encoders::enc_weth_deposit(U256::from(weth_out)));
        }
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, optimal_input).ok()?);
        callback_cmds = cb;
    }

    // Top-level: V2 flash swap.
    let outer = encoders::enc_v2_swap_compact(
        v2_idx,
        hop_v2.zfo,
        forward_out,
        SENTINEL_SELF,
        hop_v2.fee,
        &callback_cmds,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&outer);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V3-V3
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V3-V3 2-hop arbitrage as a `cmd_executor` command stream.
///
/// Forward-order with explicit WETH payment. V3a sends its USDC output to the
/// executor before the callback; the callback pays WETH to V3a, then V3b
/// swaps (auto-pay) and sends WETH back to the executor.
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn encode_cmd_v3_v3(
    hop_a: &V3HopInfo,
    hop_b: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let v3_a_idx = at.add(hop_a.pool_address).ok()?;
    let v3_b_idx = at.add(hop_b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    // V3_A callback: pay WETH to V3_A, then V3_B swap (auto-pay).
    let mut v3_a_callback = encoders::enc_erc20_transfer(weth_idx, v3_a_idx, optimal_input).ok()?;
    // V3_B is the CL hop at index 1; its swap-in is the solver's CL-clamp
    // executable forward (`consumed_inputs[1]`), not V3_A's full output.
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let v3_b_cmd =
        encoders::enc_v3_swap_compact(v3_b_idx, hop_b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?;
    v3_a_callback.extend_from_slice(&v3_b_cmd);

    // Top-level: V3_A swap (forward-order).
    let commands = encoders::enc_v3_swap_compact(
        v3_a_idx,
        hop_a.zfo,
        optimal_input,
        SENTINEL_SELF,
        &v3_a_callback,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V2-V3
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V2-V3 2-hop arbitrage as a `cmd_executor` command stream.
///
/// V2 flash borrow sends the forward token to the executor; in the callback,
/// V3 swap runs with a `forward_data` (an `ERC20_TRANSFER` satisfying V3's IIA
/// check during the callback), then WETH repays V2's flash.
///
/// **Critical:** the `ERC20_TRANSFER` must be inside V3's `forward_data`, NOT
/// before `V3.swap()` — otherwise V3 captures it in `balance_before` and IIA
/// fails.
fn encode_cmd_v2_v3(
    hop_a: &V2HopInfo,
    hop_b: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    // The V3 hop's swap-in (and the `forward_data` ERC20_TRANSFER prefunding
    // it) is the solver's CL-clamp executable forward (`consumed_inputs[1]`),
    // not the V2 pool's full output. V2 is hop 0 (feeds `optimal_input`).
    let v3_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v3_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let v2_idx = at.add(hop_a.pool_address).ok()?;
    let v3_idx = at.add(hop_b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    // Forward token from V2.
    let forward_addr = if hop_a.zfo {
        hop_a.token1_address
    } else {
        hop_a.token0_address
    };
    let forward_idx = at.add(forward_addr).ok()?;

    // V3 callback forward_data: pay USDC to V3 to satisfy IIA during callback.
    let v3_callback_cmds = encoders::enc_erc20_transfer(forward_idx, v3_idx, v3_swap_in).ok()?;
    let mut callback_cmds = encoders::enc_v3_swap_compact(
        v3_idx,
        hop_b.zfo,
        v3_swap_in,
        SENTINEL_SELF,
        &v3_callback_cmds,
    )
    .ok()?;
    callback_cmds
        .extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, optimal_input).ok()?);

    // Top-level: V2 flash swap.
    let commands = encoders::enc_v2_swap_compact(
        v2_idx,
        hop_a.zfo,
        forward_out,
        SENTINEL_SELF,
        hop_a.fee,
        &callback_cmds,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// V3-V2
// ═══════════════════════════════════════════════════════════════════════════

/// Encode V3-V2 2-hop arbitrage as a `cmd_executor` command stream.
///
/// V3 sends its USDC output to the executor before the callback. The callback
/// pays WETH to V3 from the executor's reserve, pre-funds V2 with USDC, then
/// runs a V2 direct swap (no flash) so V2 computes WETH output on-chain from
/// actual reserves (avoiding the WETH-amount mismatch that causes TF
/// failures). V3 `amountSpecified` is `optimal_input` (the WETH input to V3),
/// NOT `forward_out`.
fn encode_cmd_v3_v2(
    hop_a: &V3HopInfo,
    hop_b: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let v3_idx = at.add(hop_a.pool_address).ok()?;
    let v2_idx = at.add(hop_b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    // Forward token from V3.
    let forward_addr = if hop_a.zfo {
        hop_a.token1_address
    } else {
        hop_a.token0_address
    };
    let forward_idx = at.add(forward_addr).ok()?;

    // V3 callback forward_data.
    let mut v3_callback = encoders::enc_erc20_transfer(weth_idx, v3_idx, optimal_input).ok()?;
    v3_callback
        .extend_from_slice(&encoders::enc_erc20_transfer(forward_idx, v2_idx, forward_out).ok()?);
    let v2_cmd =
        encoders::enc_v2_swap_compact(v2_idx, hop_b.zfo, weth_out, SENTINEL_SELF, hop_b.fee, &[])
            .ok()?;
    v3_callback.extend_from_slice(&v2_cmd);

    // Top-level: V3 swap with forward_data (V3 amount = optimal_input = WETH in).
    let commands = encoders::enc_v3_swap_compact(
        v3_idx,
        hop_a.zfo,
        optimal_input,
        SENTINEL_SELF,
        &v3_callback,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// ABI wrap: execute(bytes, uint256)
// ═══════════════════════════════════════════════════════════════════════════

/// A single EVM call ready for on-chain submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedCall {
    /// Target contract address.
    pub to: Address,
    /// ABI-encoded calldata (selector + parameters).
    pub data: Vec<u8>,
    /// ETH value to send with the call.
    pub value: U256,
}

/// Wrap a command-stream `commands` payload in the `execute(bytes, uint256)`
/// ABI call to the `cmd_executor` contract.
///
/// `config` is the packed `execute()` config uint256 (see
/// [`encoders::pack_config`]); `0` = skip profit check, no bribe.
///
/// # Errors
///
/// Returns [`degenbot_abi::abi_types::AbiValue`] encoding errors (should not
/// happen with valid inputs).
pub fn encode_execute_call(
    executor_address: Address,
    commands: &[u8],
    config: U256,
) -> Result<EncodedCall, degenbot_core::errors::AbiDecodeError> {
    let values = [
        AbiValue::Bytes(commands.to_vec()),
        AbiValue::Uint(config, 256),
    ];
    let encoded = encode_rust(&["bytes", "uint256"], &values)?;
    let mut data = Vec::with_capacity(4 + encoded.len());
    data.extend_from_slice(&EXECUTE_SELECTOR);
    data.extend_from_slice(&encoded);
    Ok(EncodedCall {
        to: executor_address,
        data,
        value: U256::ZERO,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Payload builders (the 9th two-hop path + V4V3 payload)
// ═══════════════════════════════════════════════════════════════════════════

/// A configured V4 pool key for the payload builders.
#[derive(Clone, Debug)]
pub struct V4PoolKeyConfig {
    /// `currency0` (will be sorted into the key by `make_pool_key`).
    pub currency0: Address,
    /// `currency1`.
    pub currency1: Address,
    /// Fee tier (uint24).
    pub fee: u32,
    /// Tick spacing (int24).
    pub tick_spacing: i32,
    /// Hooks address (`Address::ZERO` = no hooks → `SENTINEL_NATIVE`).
    pub hooks: Address,
}

/// Builder for a 2-pool V4→V4 arbitrage command stream (the simplified
/// "9th" path dispatched via `SwapAmounts`).
///
/// The simplest arbitrage: two V4 swaps inside one unlock, delta netting
/// eliminates intermediate tokens, take profit and settle input.
#[derive(Clone, Debug)]
pub struct V4V4ArbitragePayload {
    pub pool_manager: Address,
    pub weth: Address,
    pub executor: Address,

    /// Pool A (first swap). Configured via [`Self::set_pool_a`].
    pub pool_a_key: Option<V4PoolKeyConfig>,
    pub pool_a_amount_in: u128,
    pub pool_a_amount_out: u128,
    pub pool_a_zfo: bool,

    /// Pool B (second swap). Configured via [`Self::set_pool_b`].
    pub pool_b_key: Option<V4PoolKeyConfig>,
    pub pool_b_amount_in: u128,
    pub pool_b_amount_out: u128,
    pub pool_b_zfo: bool,

    /// Profit token (`None` = derive from pool B's output currency).
    pub profit_currency: Option<Address>,
}

impl V4V4ArbitragePayload {
    /// Construct with the three protocol addresses; pools start unconfigured.
    #[must_use]
    pub fn new(pool_manager: Address, weth: Address, executor: Address) -> Self {
        Self {
            pool_manager,
            weth,
            executor,
            pool_a_key: None,
            pool_a_amount_in: 0,
            pool_a_amount_out: 0,
            pool_a_zfo: false,
            pool_b_key: None,
            pool_b_amount_in: 0,
            pool_b_amount_out: 0,
            pool_b_zfo: false,
            profit_currency: None,
        }
    }

    /// Configure pool A. `zero_for_one=None` derives `zfo` from the (unsorted)
    /// `currency0`/`currency1` ordering passed in.
    #[allow(clippy::too_many_arguments)] // reason: builder setters take the per-pool V4 kwargs directly;
                                         // collapsing into a params struct would harm call-site ergonomics.
    pub fn set_pool_a(
        &mut self,
        currency0: Address,
        currency1: Address,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
        amount_in: u128,
        amount_out: u128,
        zero_for_one: Option<bool>,
    ) {
        let key = encoders::make_pool_key(currency0, currency1, fee, tick_spacing, hooks);
        let stored = V4PoolKeyConfig {
            currency0: key.currency0,
            currency1: key.currency1,
            fee: key.fee,
            tick_spacing: key.tick_spacing,
            hooks: key.hooks,
        };
        self.pool_a_zfo = zero_for_one.unwrap_or(stored.currency0 == currency0);
        self.pool_a_key = Some(stored);
        self.pool_a_amount_in = amount_in;
        self.pool_a_amount_out = amount_out;
    }

    /// Configure pool B. See [`Self::set_pool_a`].
    #[allow(clippy::too_many_arguments)] // reason: builder setters take the per-pool V4 kwargs directly;
                                         // collapsing into a params struct would harm call-site ergonomics.
    pub fn set_pool_b(
        &mut self,
        currency0: Address,
        currency1: Address,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
        amount_in: u128,
        amount_out: u128,
        zero_for_one: Option<bool>,
    ) {
        let key = encoders::make_pool_key(currency0, currency1, fee, tick_spacing, hooks);
        let stored = V4PoolKeyConfig {
            currency0: key.currency0,
            currency1: key.currency1,
            fee: key.fee,
            tick_spacing: key.tick_spacing,
            hooks: key.hooks,
        };
        self.pool_b_zfo = zero_for_one.unwrap_or(stored.currency0 == currency0);
        self.pool_b_key = Some(stored);
        self.pool_b_amount_in = amount_in;
        self.pool_b_amount_out = amount_out;
    }

    /// Encode the full command stream for V4→V4 arbitrage.
    ///
    /// Returns `None` if a pool is unconfigured or any `enc_*` step fails.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn encode(&self) -> Option<Vec<u8>> {
        let pool_a = self.pool_a_key.as_ref()?;
        let pool_b = self.pool_b_key.as_ref()?;

        let mut at = AddressTable::with_sentinels(
            Some(self.weth),
            Some(self.executor),
            Some(self.pool_manager),
        );
        at.add(pool_a.currency0).ok()?;
        at.add(pool_a.currency1).ok()?;
        at.add(pool_b.currency0).ok()?;
        at.add(pool_b.currency1).ok()?;
        if let Some(profit) = self.profit_currency {
            if profit != NATIVE_ADDRESS && profit != Address::ZERO {
                at.add(profit).ok()?;
            }
            if profit == NATIVE_ADDRESS {
                at.add(NATIVE_ADDRESS).ok()?;
            }
        }

        let weth_idx = SENTINEL_WETH;
        let executor_idx = SENTINEL_SELF;
        let zero_idx = SENTINEL_NATIVE;

        let a_fee = u16::try_from(pool_a.fee).ok()?;
        let a_ts = i16::try_from(pool_a.tick_spacing).ok()?;
        let b_fee = u16::try_from(pool_b.fee).ok()?;
        let b_ts = i16::try_from(pool_b.tick_spacing).ok()?;

        // V4_SWAP_COMPACT for pool A.
        let mut inner = encoders::enc_v4_swap_compact(
            at.index_of(pool_a.currency0)?,
            at.index_of(pool_a.currency1)?,
            a_fee,
            a_ts,
            zero_idx,
            self.pool_a_zfo,
            self.pool_a_amount_in,
        )
        .ok()?;
        // V4_SWAP_COMPACT for pool B.
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                at.index_of(pool_b.currency0)?,
                at.index_of(pool_b.currency1)?,
                b_fee,
                b_ts,
                zero_idx,
                self.pool_b_zfo,
                self.pool_b_amount_in,
            )
            .ok()?,
        );

        // Settlement: the output currency of pool B (the profit token).
        let output_currency_b = if self.pool_b_zfo {
            pool_b.currency1
        } else {
            pool_b.currency0
        };

        if output_currency_b == NATIVE_ADDRESS
            || self.profit_currency.is_some_and(|p| p == NATIVE_ADDRESS)
        {
            // Cross-currency: native ETH output.
            let native_idx = at.index_of(NATIVE_ADDRESS)?;
            inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx, executor_idx, self.pool_b_amount_out)
                    .ok()?,
            );
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        } else {
            // Same-currency or ERC-20 output: take profit, settle input.
            let profit_amount = self.pool_b_amount_out.saturating_sub(self.pool_a_amount_in);
            if profit_amount > 0 {
                inner.extend_from_slice(
                    &encoders::enc_v4_take_compact(weth_idx, executor_idx, profit_amount).ok()?,
                );
            }
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        }

        let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
        let mut out = encoders::enc_preamble(&at);
        out.append(&mut commands);
        Some(out)
    }

    /// Encode using `V4_BATCH` for maximum compactness (auto-settles WETH +
    /// native ETH deltas; first swap explicit, second dynamic).
    #[must_use]
    pub fn encode_batch(&self) -> Option<Vec<u8>> {
        let pool_a = self.pool_a_key.as_ref()?;
        let pool_b = self.pool_b_key.as_ref()?;

        let mut at = AddressTable::with_sentinels(
            Some(self.weth),
            Some(self.executor),
            Some(self.pool_manager),
        );
        at.add(pool_a.currency0).ok()?;
        at.add(pool_a.currency1).ok()?;
        at.add(pool_b.currency0).ok()?;
        at.add(pool_b.currency1).ok()?;
        if let Some(profit) = self.profit_currency {
            if profit == NATIVE_ADDRESS {
                at.add(NATIVE_ADDRESS).ok()?;
            } else if profit != Address::ZERO {
                at.add(profit).ok()?;
            }
        }

        let zero_idx = SENTINEL_NATIVE;
        let a_fee = u16::try_from(pool_a.fee).ok()?;
        let a_ts = i16::try_from(pool_a.tick_spacing).ok()?;
        let b_fee = u16::try_from(pool_b.fee).ok()?;
        let b_ts = i16::try_from(pool_b.tick_spacing).ok()?;

        let batch = [
            V4BatchEntry {
                c0_idx: at.index_of(pool_a.currency0)?,
                c1_idx: at.index_of(pool_a.currency1)?,
                fee: a_fee,
                tick_spacing: a_ts,
                hooks_idx: zero_idx,
                zfo: self.pool_a_zfo,
                amount_u96: self.pool_a_amount_in,
            },
            V4BatchEntry {
                c0_idx: at.index_of(pool_b.currency0)?,
                c1_idx: at.index_of(pool_b.currency1)?,
                fee: b_fee,
                tick_spacing: b_ts,
                hooks_idx: zero_idx,
                zfo: self.pool_b_zfo,
                amount_u96: 0, // dynamic amount
            },
        ];

        let mut batch_bytes = encoders::enc_v4_batch(&batch).ok()?;
        let mut commands = encoders::enc_v4_unlock(&batch_bytes).ok()?;
        let mut out = encoders::enc_preamble(&at);
        out.append(&mut commands);
        let _ = &mut batch_bytes; // silence unused
        Some(out)
    }
}

/// Builder for a 2-pool V4→V3 arbitrage command stream.
///
/// Path: V4 swap (WETH→USDC) → V3 swap (USDC→WETH, auto-pay), with a
/// `V4_SETTLE_DELTA(WETH)` to settle the V4 input debt.
#[derive(Clone, Debug)]
pub struct V4V3ArbitragePayload {
    pub pool_manager: Address,
    pub weth: Address,
    pub executor: Address,
    pub v3_pool: Address,
    /// Intermediate token (e.g., USDC).
    pub intermediate_token: Address,

    /// V4 pool key (configured via [`Self::set_v4_pool`]).
    pub v4_key: Option<V4PoolKeyConfig>,
    pub v4_amount_in: u128,
    pub v4_amount_out: u128,
    pub v4_zfo: bool,

    pub v3_amount_in: u128,
    pub v3_amount_out: u128,
    pub v3_zfo: bool,
}

impl V4V3ArbitragePayload {
    /// Construct with the protocol addresses + V3 pool + intermediate token.
    #[must_use]
    pub fn new(
        pool_manager: Address,
        weth: Address,
        executor: Address,
        v3_pool: Address,
        intermediate_token: Address,
    ) -> Self {
        Self {
            pool_manager,
            weth,
            executor,
            v3_pool,
            intermediate_token,
            v4_key: None,
            v4_amount_in: 0,
            v4_amount_out: 0,
            v4_zfo: false,
            v3_amount_in: 0,
            v3_amount_out: 0,
            v3_zfo: false,
        }
    }

    /// Configure the V4 pool. `zero_for_one=None` derives `zfo` from the
    /// unsorted currency ordering.
    #[allow(clippy::too_many_arguments)] // reason: builder setters take the per-pool V4 kwargs directly;
                                         // collapsing into a params struct would harm call-site ergonomics.
    pub fn set_v4_pool(
        &mut self,
        currency0: Address,
        currency1: Address,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
        amount_in: u128,
        amount_out: u128,
        zero_for_one: Option<bool>,
    ) {
        let key = encoders::make_pool_key(currency0, currency1, fee, tick_spacing, hooks);
        let stored = V4PoolKeyConfig {
            currency0: key.currency0,
            currency1: key.currency1,
            fee: key.fee,
            tick_spacing: key.tick_spacing,
            hooks: key.hooks,
        };
        self.v4_zfo = zero_for_one.unwrap_or(stored.currency0 == currency0);
        self.v4_key = Some(stored);
        self.v4_amount_in = amount_in;
        self.v4_amount_out = amount_out;
    }

    /// Configure the V3 swap amounts + direction.
    pub fn set_v3_pool(&mut self, amount_in: u128, amount_out: u128, zero_for_one: bool) {
        self.v3_amount_in = amount_in;
        self.v3_amount_out = amount_out;
        self.v3_zfo = zero_for_one;
    }

    fn ensure_table(&self) -> Option<AddressTable> {
        let v4 = self.v4_key.as_ref()?;
        let mut at = AddressTable::with_sentinels(
            Some(self.weth),
            Some(self.executor),
            Some(self.pool_manager),
        );
        at.add(self.intermediate_token).ok()?;
        at.add(self.v3_pool).ok()?;
        at.add(v4.currency0).ok()?;
        at.add(v4.currency1).ok()?;
        Some(at)
    }

    /// Encode V4→V3 with auto-pay (the V3 callback auto-transfers owed tokens
    /// from the executor; no `forward_data`).
    #[must_use]
    pub fn encode(&self) -> Option<Vec<u8>> {
        let v4 = self.v4_key.as_ref()?;
        let at = self.ensure_table()?;
        let pm_idx = at.index_of(self.pool_manager)?;
        let weth_idx = SENTINEL_WETH;
        let usdc_idx = at.index_of(self.intermediate_token)?;
        let executor_idx = SENTINEL_SELF;
        let v3_idx = at.index_of(self.v3_pool)?;
        let zero_idx = SENTINEL_NATIVE;
        let _ = pm_idx;

        let v4_fee = u16::try_from(v4.fee).ok()?;
        let v4_ts = i16::try_from(v4.tick_spacing).ok()?;

        let mut inner = encoders::enc_v4_swap_compact(
            at.index_of(v4.currency0)?,
            at.index_of(v4.currency1)?,
            v4_fee,
            v4_ts,
            zero_idx,
            self.v4_zfo,
            self.v4_amount_in,
        )
        .ok()?;
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(usdc_idx, executor_idx, self.v4_amount_out).ok()?,
        );
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(
                v3_idx,
                self.v3_zfo,
                self.v3_amount_in,
                executor_idx,
                &[],
            )
            .ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));

        let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
        let mut out = encoders::enc_preamble(&at);
        out.append(&mut commands);
        Some(out)
    }

    /// Encode V4→V3 with explicit `forward_data` in the V3 callback (used
    /// when auto-pay isn't possible). The `forward_data` is:
    /// `ERC20_TRANSFER(USDC→V3)` + `V4_SYNC(WETH)` + `ERC20_TRANSFER(WETH→PM)`
    /// + `V4_SETTLE`.
    #[must_use]
    pub fn encode_with_forward_data(&self) -> Option<Vec<u8>> {
        let v4 = self.v4_key.as_ref()?;
        let at = self.ensure_table()?;
        let pm_idx = at.index_of(self.pool_manager)?;
        let weth_idx = SENTINEL_WETH;
        let usdc_idx = at.index_of(self.intermediate_token)?;
        let executor_idx = SENTINEL_SELF;
        let v3_idx = at.index_of(self.v3_pool)?;
        let zero_idx = SENTINEL_NATIVE;

        let v4_fee = u16::try_from(v4.fee).ok()?;
        let v4_ts = i16::try_from(v4.tick_spacing).ok()?;

        // Build V3 callback forward_data.
        let mut v3_callback =
            encoders::enc_erc20_transfer(usdc_idx, v3_idx, self.v4_amount_out).ok()?;
        v3_callback.extend_from_slice(&encoders::enc_v4_sync(weth_idx));
        v3_callback.extend_from_slice(
            &encoders::enc_erc20_transfer(weth_idx, pm_idx, self.v3_amount_out).ok()?,
        );
        v3_callback.extend_from_slice(&encoders::enc_v4_settle());

        let mut inner = encoders::enc_v4_swap_compact(
            at.index_of(v4.currency0)?,
            at.index_of(v4.currency1)?,
            v4_fee,
            v4_ts,
            zero_idx,
            self.v4_zfo,
            self.v4_amount_in,
        )
        .ok()?;
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(usdc_idx, executor_idx, self.v4_amount_out).ok()?,
        );
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(
                v3_idx,
                self.v3_zfo,
                self.v3_amount_in,
                executor_idx,
                &v3_callback,
            )
            .ok()?,
        );

        let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
        let mut out = encoders::enc_preamble(&at);
        out.append(&mut commands);
        Some(out)
    }
}

/// Compose swap amounts into a single `cmd_executor` `execute(commands, config)`
/// call.
///
/// Currently supports the simple 2-pool V4→V4 path. Additional path types
/// can be added by extending the routing.
#[derive(Clone, Debug)]
pub struct CmdExecutorComposer {
    pub pool_manager: Address,
    pub weth: Address,
    pub executor: Address,
}

/// A configured V4 swap (input + direction) for the composer.
#[derive(Clone, Debug)]
pub struct V4SwapAmounts {
    pub pool_key: V4PoolKeyConfig,
    pub zero_for_one: bool,
    pub amount_in: u128,
    pub amount_out: u128,
}

impl CmdExecutorComposer {
    /// Construct with the three protocol addresses.
    #[must_use]
    pub fn new(pool_manager: Address, weth: Address, executor: Address) -> Self {
        Self {
            pool_manager,
            weth,
            executor,
        }
    }

    /// Compose `swap_amounts` (a 2-pool V4→V4 path) into a single
    /// `execute(commands, config)` `EncodedCall`.
    ///
    /// `config` defaults to `0` (skip profit check, no bribe — operator
    /// verifies profitability off-chain).
    ///
    /// # Errors
    ///
    /// Returns [`degenbot_core::errors::AbiDecodeError`] on ABI-encoding
    /// failure (should not happen with valid `commands`).
    pub fn compose(
        &self,
        swap_amounts: &[V4SwapAmounts],
        config: U256,
    ) -> Result<Option<EncodedCall>, degenbot_core::errors::AbiDecodeError> {
        if swap_amounts.len() != 2 {
            return Ok(None);
        }
        let commands = self.encode_v4_v4(&swap_amounts[0], &swap_amounts[1]);
        match commands {
            Some(c) => Ok(Some(encode_execute_call(self.executor, &c, config)?)),
            None => Ok(None),
        }
    }

    /// Encode a 2-pool V4→V4 path into the raw command stream (no ABI wrap).
    /// Uses `V4_SWAP_COMPACT` for both swaps, `V4_TAKE` for profit, and
    /// `V4_SETTLE_DELTA` for the remaining WETH input debt — all inside one
    /// `V4_UNLOCK`.
    #[allow(clippy::too_many_lines)]
    fn encode_v4_v4(&self, swap_a: &V4SwapAmounts, swap_b: &V4SwapAmounts) -> Option<Vec<u8>> {
        let mut at = AddressTable::with_sentinels(
            Some(self.weth),
            Some(self.executor),
            Some(self.pool_manager),
        );
        at.add(swap_a.pool_key.currency0).ok()?;
        at.add(swap_a.pool_key.currency1).ok()?;
        at.add(swap_b.pool_key.currency0).ok()?;
        at.add(swap_b.pool_key.currency1).ok()?;

        let has_native = {
            let addrs = [
                swap_a.pool_key.currency0,
                swap_a.pool_key.currency1,
                swap_b.pool_key.currency0,
                swap_b.pool_key.currency1,
            ];
            addrs.contains(&NATIVE_ADDRESS)
        };
        if has_native {
            at.add(NATIVE_ADDRESS).ok()?;
        }

        let zero_idx = SENTINEL_NATIVE;
        let a_fee = u16::try_from(swap_a.pool_key.fee).ok()?;
        let a_ts = i16::try_from(swap_a.pool_key.tick_spacing).ok()?;
        let b_fee = u16::try_from(swap_b.pool_key.fee).ok()?;
        let b_ts = i16::try_from(swap_b.pool_key.tick_spacing).ok()?;

        // V4_SWAP_COMPACT for pool A.
        let mut inner = encoders::enc_v4_swap_compact(
            at.index_of(swap_a.pool_key.currency0)?,
            at.index_of(swap_a.pool_key.currency1)?,
            a_fee,
            a_ts,
            zero_idx,
            swap_a.zero_for_one,
            swap_a.amount_in,
        )
        .ok()?;
        // V4_SWAP_COMPACT for pool B.
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                at.index_of(swap_b.pool_key.currency0)?,
                at.index_of(swap_b.pool_key.currency1)?,
                b_fee,
                b_ts,
                zero_idx,
                swap_b.zero_for_one,
                swap_b.amount_in,
            )
            .ok()?,
        );

        // Settlement.
        let output_currency_b = if swap_b.zero_for_one {
            swap_b.pool_key.currency1
        } else {
            swap_b.pool_key.currency0
        };
        let input_currency_a = if swap_a.zero_for_one {
            swap_a.pool_key.currency0
        } else {
            swap_a.pool_key.currency1
        };

        if output_currency_b == NATIVE_ADDRESS {
            let native_idx = at.index_of(NATIVE_ADDRESS)?;
            inner.extend_from_slice(
                &encoders::enc_v4_take_compact(
                    native_idx,
                    at.index_of(self.executor)?,
                    swap_b.amount_out,
                )
                .ok()?,
            );
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(at.index_of(self.weth)?));
        } else if input_currency_a == output_currency_b {
            let profit_amount = swap_b.amount_out.saturating_sub(swap_a.amount_in);
            if profit_amount > 0 {
                inner.extend_from_slice(
                    &encoders::enc_v4_take_compact(
                        at.index_of(self.weth)?,
                        at.index_of(self.executor)?,
                        profit_amount,
                    )
                    .ok()?,
                );
            }
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(at.index_of(self.weth)?));
        } else {
            inner.extend_from_slice(
                &encoders::enc_v4_take_compact(
                    at.index_of(output_currency_b)?,
                    at.index_of(self.executor)?,
                    swap_b.amount_out,
                )
                .ok()?,
            );
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(
                at.index_of(input_currency_a)?,
            ));
        }

        let mut commands = encoders::enc_v4_unlock(&inner).ok()?;
        let mut out = encoders::enc_preamble(&at);
        out.append(&mut commands);
        Some(out)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 3-hop composers
// ═══════════════════════════════════════════════════════════════════════════

/// Encode a 3-hop arbitrage path as a `cmd_executor` command stream.
///
/// Dispatches to one of the 27 `three_hop_*` pattern functions based on the
/// hop-type combination.
///
/// Returns `None` for an unknown combination or if any `enc_*` step fails.
#[allow(clippy::too_many_lines)]
#[doc(hidden)]
#[must_use]
#[allow(clippy::too_many_arguments)] // encoder args (path + inputs + executor/pm/weth + opts)
pub fn encode_cmd_3_hop(
    path_info: &PathInfo,
    optimal_input: u128,
    hop_outputs: &[u128],
    consumed_inputs: &[u128],
    executor_address: Address,
    pool_manager_address: Address,
    weth_address: Address,
    opts: EncodeOptions,
) -> Option<Vec<u8>> {
    let hops = &path_info.hops;
    let inputs = ComposerInputs {
        executor_address,
        pool_manager_address,
        weth_address,
        optimal_input,
        hop_outputs,
        consumed_inputs,
        opts,
    };
    if hops.len() != 3 {
        return None;
    }
    match (&hops[0], &hops[1], &hops[2]) {
        (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V2(c)) => three_hop_v2_v2_v2(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V3(c)) => three_hop_v2_v2_v3(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V2(b), HopInfo::V4(c)) => three_hop_v2_v2_v4(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V2(c)) => three_hop_v2_v3_v2(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V3(c)) => three_hop_v2_v3_v3(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V3(b), HopInfo::V4(c)) => three_hop_v2_v3_v4(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V2(c)) => three_hop_v2_v4_v2(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V3(c)) => three_hop_v2_v4_v3(a, b, c, &inputs),
        (HopInfo::V2(a), HopInfo::V4(b), HopInfo::V4(c)) => three_hop_v2_v4_v4(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V2(c)) => three_hop_v3_v2_v2(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V3(c)) => three_hop_v3_v2_v3(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V2(b), HopInfo::V4(c)) => three_hop_v3_v2_v4(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V2(c)) => three_hop_v3_v3_v2(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V3(c)) => three_hop_v3_v3_v3(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V3(b), HopInfo::V4(c)) => three_hop_v3_v3_v4(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V2(c)) => three_hop_v3_v4_v2(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V3(c)) => three_hop_v3_v4_v3(a, b, c, &inputs),
        (HopInfo::V3(a), HopInfo::V4(b), HopInfo::V4(c)) => three_hop_v3_v4_v4(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V2(c)) => three_hop_v4_v2_v2(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V3(c)) => three_hop_v4_v2_v3(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V2(b), HopInfo::V4(c)) => three_hop_v4_v2_v4(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V2(c)) => three_hop_v4_v3_v2(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V3(c)) => three_hop_v4_v3_v3(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V3(b), HopInfo::V4(c)) => three_hop_v4_v3_v4(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V2(c)) => three_hop_v4_v4_v2(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V3(c)) => three_hop_v4_v4_v3(a, b, c, &inputs),
        (HopInfo::V4(a), HopInfo::V4(b), HopInfo::V4(c)) => three_hop_v4_v4_v4(a, b, c, &inputs),
    }
}

/// Shared helper: encode a V4 swap with amount=0 (caller overrides amount).
/// Currently unused by any 3-hop pattern; retained as a reusable primitive.
#[allow(dead_code)]
fn enc_v4_swap_zero(hop: &V4HopInfo, at: &mut AddressTable) -> Option<Vec<u8>> {
    let c0_idx = at.add(hop.currency0_address).ok()?;
    let c1_idx = at.add(hop.currency1_address).ok()?;
    let fee = u16::try_from(hop.fee).ok()?;
    let ts = i16::try_from(hop.tick_spacing).ok()?;
    let hooks_idx = SENTINEL_NATIVE;
    encoders::enc_v4_swap_compact(c0_idx, c1_idx, fee, ts, hooks_idx, hop.zfo, 0).ok()
}

// ── V2-V2-V2 ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v2_v2(
    ha: &V2HopInfo,
    hb: &V2HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;

    // Inside V2c callback: WETH→V2a (creates excess), V2a→V2b, V2b→V2c.
    let mut c_fwd = encoders::enc_erc20_transfer(weth_idx, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2a_idx, ha.zfo, v2b_idx, ha.fee,
    ));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b_idx, hb.zfo, v2c_idx, hb.fee,
    ));

    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, hc.fee, &c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V2-V3 ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v2_v3(
    ha: &V2HopInfo,
    hb: &V2HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    // V3 is the CL hop at idx 2; its swap-in is the solver's CL-clamp
    // executable forward (`consumed_inputs[2]`). Hops 0/1 are V2 (no clamp).
    let c_swap_in = consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let mut c_fwd = encoders::enc_erc20_transfer(weth_idx, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2a_idx, ha.zfo, v2b_idx, ha.fee,
    ));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b_idx, hb.zfo, v3c_idx, hb.fee,
    ));

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, executor_idx, &c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V2-V4 ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v2_v4(
    ha: &V2HopInfo,
    hb: &V2HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let c0_idx = at.add(hc.currency0_address).ok()?;
    let c1_idx = at.add(hc.currency1_address).ok()?;

    let forward_b_addr = if hb.zfo {
        hb.token1_address
    } else {
        hb.token0_address
    };
    let forward_b_idx = at.add(forward_b_addr).ok()?;

    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_sync(forward_b_idx);
    inner.extend_from_slice(&encoders::enc_v4_take_compact(weth_idx, v2a_idx, optimal_input).ok()?);
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2a_idx, ha.zfo, v2b_idx, ha.fee,
    ));
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, hb.zfo, pm_idx, hb.fee));
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_idx, c1_idx, fee_c, ts_c, zero_idx, hc.zfo,
    ));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V3-V2 ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v3_v2(
    ha: &V2HopInfo,
    hb: &V3HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    // Hop1 (V3b) is CL and over-fed — its swap-in = consumed_inputs[1]. `out_a`
    // also feeds the V2a direct output (real). Exit hop2 is V2 — forward stays
    // on `out_c` = hop_outputs[2].
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    at.add(if ha.zfo {
        ha.token1_address
    } else {
        ha.token0_address
    })
    .ok()?;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;

    let mut b_fwd = encoders::enc_erc20_transfer(weth_idx, v2a_idx, optimal_input).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx).ok()?);

    let c_fwd = encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v2c_idx, &b_fwd).ok()?;

    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, hc.fee, &c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V3-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v3_v3(
    ha: &V2HopInfo,
    hb: &V3HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    // Both V3 hops are CL: hop1 swap-in = consumed_inputs[1], hop2 swap-in =
    // consumed_inputs[2]. `out_a` also feeds the V2 direct output (real).
    let b_swap_in = consumed_inputs.get(1).copied()?;
    let c_swap_in = consumed_inputs.get(2).copied()?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let mut v3b_fwd = encoders::enc_erc20_transfer(weth_idx, v2a_idx, optimal_input).ok()?;
    v3b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx).ok()?);

    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v3c_idx, &v3b_fwd).ok()?;

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, executor_idx, &v3c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V3-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v3_v4(
    ha: &V2HopInfo,
    hb: &V3HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // Both mid/third hops are CL: hop1 (V3) swap-in = consumed_inputs[1];
    // hop2 (V4) swap-in = consumed_inputs[2]. `out_a` also feeds the V2 direct
    // output (real); the exit take stays on `out_c`.
    let b_swap_in = consumed_inputs.get(1).copied()?;
    let c_swap_in = consumed_inputs.get(2).copied()?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let c0_idx = at.add(hc.currency0_address).ok()?;
    let c1_idx = at.add(hc.currency1_address).ok()?;

    let forward_b_addr = if hb.zfo {
        hb.token1_address
    } else {
        hb.token0_address
    };
    let forward_b_idx = at.add(forward_b_addr).ok()?;

    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_idx, c1_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in)
            .ok()?,
    );
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(weth_idx, v2a_idx, optimal_input).ok()?);
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(weth_idx, executor_idx, out_c - optimal_input).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(weth_idx));
    // CurrencyNotSettled under-prediction guard: the encoded V4_TAKE amounts
    // are built from the solver's predicted `out_c` (WETH output). When the
    // actual V4 PoolManager swap produces MORE WETH than the solver predicts
    // (residual under-prediction class — see `sim_v4_swap_step_rounding.md`
    // residual addendum + ergo ON5QMD post-fix soak logs), the executor's
    // fixed take amounts leave a positive WETH delta on the PoolManager at
    // unlock exit → `CurrencyNotSettled(WETH, surplus)`. `V4_SETTLE` (bare)
    // is one-directional (pays in owed deltas); `V4_SETTLE_ALL` mirrors
    // `three_hop_v3_v3_v4`'s proven close pattern — it sweeps positive
    // residuals via `PM.take(WETH, self)` for the executor too.
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_b_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V4-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v4_v2(
    ha: &V2HopInfo,
    hb: &V4HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // Hop1 (V4b) is CL and over-fed — its swap-in = consumed_inputs[1]. Hop0
    // V2a feeds via swap_calc (no amount); exit hop2 is V2 — forward stays on
    // `out_b`/`out_c` (hop_outputs).
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let forward_a_idx = at
        .add(if ha.zfo {
            ha.token1_address
        } else {
            ha.token0_address
        })
        .ok()?;
    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;

    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a_idx);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, ha.zfo, pm_idx, ha.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b_idx, v2c_idx, out_b).ok()?);

    let mut c_fwd = encoders::enc_erc20_transfer(weth_idx, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, executor_idx, hc.fee, &c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V4-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v4_v3(
    ha: &V2HopInfo,
    hb: &V4HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    // The V4 hop's swap-in is the solver's executable forward (`consumed_inputs`),
    // which the CL-hop clamp caps at `input_consumed - 1` when the solver
    // over-predicts capacity (path-5000 EMPTY-HALT). The V3 exit take stays on
    // `hop_outputs[1]` — for an over-feeding CL pool output(capacity) == output
    // (over-feed), so the exit amount is unchanged.
    let v4_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let forward_a_idx = at
        .add(if ha.zfo {
            ha.token1_address
        } else {
            ha.token0_address
        })
        .ok()?;
    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a_idx);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, ha.zfo, pm_idx, ha.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, v4_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, out_b).ok()?);

    let mut c_fwd = encoders::enc_erc20_transfer(weth_idx, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, &c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V2-V4-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v2_v4_v4(
    ha: &V2HopInfo,
    hb: &V4HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let forward_a_idx = at
        .add(if ha.zfo {
            ha.token1_address
        } else {
            ha.token0_address
        })
        .ok()?;
    let zero_idx = SENTINEL_NATIVE;

    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;

    let v2a_idx = at.add(ha.pool_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a_idx);
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(weth_idx, v2a_idx, optimal_input).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, ha.zfo, pm_idx, ha.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, out_a)
            .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo,
    ));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&v4_inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V2-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v2_v2(
    ha: &V3HopInfo,
    hb: &V2HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;
    let v3a_idx = at.add(ha.pool_address).ok()?;

    let mut a_fwd = encoders::enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, v2c_idx).ok()?;
    a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx).ok()?,
    );
    a_fwd.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?);

    let commands =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, &a_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V2-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v2_v3(
    ha: &V3HopInfo,
    hb: &V2HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    // V3c is the CL hop at idx 2; its swap-in is the solver's CL-clamp
    // executable forward (`consumed_inputs[2]`). Hop0 V3a feeds optimal_input;
    // hop1 is V2 (no clamp)
    let c_swap_in = consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    // V2_SWAP_CALC (not V2_SWAP_DIRECT): compute the V2 output ON-CHAIN from
    // the ACTUAL input hop0 deposited to the pair (excess balance) rather than
    // the solver's predicted `out_b`. V2_SWAP_DIRECT forces the predicted
    // `out_b` as a fixed amount_out; when the predicted output exceeds what
    // the real (possibly near-empty) pair can fund from the deposited input,
    // the pair's K-invariant reverts (UniswapV2: K). Computing from the actual
    // excess makes the K-check satisfiable by construction and routes the
    // ACTUAL output to v3c (the outermost final V3 hop). Aligns with the
    // prediction→actual settlement class of the V3-V4-V4/V3-V4-V3 fixes.
    let mut v3a_fwd = encoders::enc_v2_swap_calc(v2b_idx, hb.zfo, v3c_idx, hb.fee);
    v3a_fwd
        .extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?);

    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, &v3a_fwd).ok()?;

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, executor_idx, &v3c_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V2-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v2_v4(
    ha: &V3HopInfo,
    hb: &V2HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // V4c is the CL hop at idx 2; its swap-in is the solver's CL-clamp
    // executable forward (`consumed_inputs[2]`). Hop0 V3a feeds optimal_input;
    // hop1 V2 keeps its real output `out_b` (no clamp). Exit take on `out_c`.
    let c_swap_in = consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;

    let forward_b_idx = at
        .add(if hb.zfo {
            hb.token1_address
        } else {
            hb.token0_address
        })
        .ok()?;

    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;

    let mut v4_inner =
        encoders::enc_v4_swap_compact(c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in)
            .ok()?;
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(weth_idx, executor_idx, out_c).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_b_idx));

    let b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let mut a_fwd =
        encoders::enc_v2_swap_compact(v2b_idx, hb.zfo, out_b, executor_idx, hb.fee, &b_fwd).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?);

    let commands =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, &a_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V3-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v3_v2(
    ha: &V3HopInfo,
    hb: &V3HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    // Hop1 (V3b) is CL and over-fed — its swap-in = consumed_inputs[1]. Hop0
    // V3a feeds optimal_input; exit hop2 is V2 — forward stays on `out_c`.
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(optimal_input) || !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2c_idx = at.add(hc.pool_address).ok()?;
    let v3a_idx = at.add(ha.pool_address).ok()?;

    let mut v3a_fwd = encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx).ok()?;
    v3a_fwd
        .extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?);

    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3b_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v3b_idx, &v3a_fwd).ok()?;

    let commands =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v2c_idx, &v3b_fwd).ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V3-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v3_v3(
    ha: &V3HopInfo,
    hb: &V3HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    // All three hops are CL. Hop0 V3a feeds optimal_input; hop1 swap-in =
    // consumed_inputs[1]; hop2 swap-in = consumed_inputs[2].
    let b_swap_in = consumed_inputs.get(1).copied()?;
    let c_swap_in = consumed_inputs.get(2).copied()?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(Some(weth_address), Some(executor_address), None);
    let executor_idx = SENTINEL_SELF;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let v3a_callback: Vec<u8> = Vec::new();
    let v3b_callback =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v3b_idx, &v3a_callback)
            .ok()?;
    let v3c_callback =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v3c_idx, &v3b_callback).ok()?;

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, executor_idx, &v3c_callback)
            .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V3-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v3_v4(
    ha: &V3HopInfo,
    hb: &V3HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // Hop1 (V3b) is CL — swap-in = consumed_inputs[1]. Hop0 V3a feeds
    // optimal_input; hop2 V4c uses V4_SWAP_DYNAMIC (delta ledger, no fixed
    // amount to clamp).
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;

    let forward_b_idx = at
        .add(if hb.zfo {
            hb.token1_address
        } else {
            hb.token0_address
        })
        .ok()?;

    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo,
    ));
    // Pay hop[0]'s V3 pool EXACTLY its owed WETH (`optimal_input`), not the
    // full V4 WETH delta. The V4 swap output (`hop_outputs[2]`) exceeds
    // `optimal_input` by the arbitrage profit; `V4_TAKE_DELTA` (full delta)
    // would send the entire V4 output to the pool and donate the profit to
    // its liquidity providers (V3 has no skim, so the residue is unrecoverable).
    // `V4_TAKE_COMPACT` with `optimal_input` pays the exact debt; the residual
    // WETH delta (== the profit) is then swept to the executor by the
    // following `V4_SETTLE_ALL` (positive WETH delta → `PM.take(WETH, self)`).
    // Mirrors the proven `encode_cmd_v3_v4` 2-hop WETH-debt pattern.
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(weth_idx, v3a_idx, optimal_input).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let a_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let b_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v3b_idx, &a_fwd).ok()?;

    let mut commands = encoders::enc_v4_sync(forward_b_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V4-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v4_v2(
    ha: &V3HopInfo,
    hb: &V4HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.token1_address
        } else {
            ha.token0_address
        })
        .ok()?;
    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;

    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo,
    ));
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(forward_b_idx, v2c_idx));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;
    a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx).ok()?,
    );
    a_fwd.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, &a_fwd).ok()?,
    );

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V4-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v4_v3(
    ha: &V3HopInfo,
    hb: &V4HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.token1_address
        } else {
            ha.token0_address
        })
        .ok()?;
    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;

    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo,
    ));
    // Take EXACTLY `out_b` USDT to v3c, matching v3c.swap(out_b). The take
    // amount must equal the V3 swap's fixed input or v3c's IIA check fails
    // (v3c received more/less USDT than its swap demands). This aligns with
    // the executor's canonical V4→V3 pattern (V2-V4-V3 test: take_compact
    // with the same b_out as the V3 swap), and avoids the silent no-profit
    // from a take_delta(actual) that diverges from the predicted out_b.
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, out_b).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let c_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, &a_fwd).ok()?;

    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, &c_fwd).ok()?,
    );

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V3-V4-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v3_v4_v4(
    ha: &V3HopInfo,
    hb: &V4HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at.add(ha.pool_address).ok()?;

    // forward_a is the V3a output currency; routed into the PoolManager via
    // the V3a swap (recipient = pm_idx) and settled by V4_SYNC/enc_v4_settle
    // so the inner V4 swaps consume the actual delta.
    let forward_a_idx = at
        .add(if ha.zfo {
            ha.token1_address
        } else {
            ha.token0_address
        })
        .ok()?;

    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;

    // Dynamic/delta-driven V4 settlement (W2UWZO/CurrencyNotSettled fix).
    // V3a sends its actual forward_a output to the PoolManager via the
    // callback's V4_SETTLE; the inner V4 swaps consume the ACTUAL deltas
    // (V4_SWAP_DYNAMIC) rather than the solver's predicted hop_outputs —
    // this nets the intermediate currency to zero by construction,
    // eliminating the residual-delta class of CurrencyNotSettled.
    // V4_TAKE_DELTA reads the resulting WETH delta; V4_SETTLE_ALL sweeps
    // any rounding dust.
    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo,
    ));
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo,
    ));
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, executor_idx));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_erc20_transfer(weth_idx, v3a_idx, optimal_input).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    // V4_SYNC must precede the V3a swap: V3's optimistic output transfer
    // delivers forward_a to the PoolManager before V3a's callback runs the
    // V4 unlock, and V4_SYNC(forward_a) is what lets that deposit become a
    // settlable PM delta. V3a routes forward_a to pm_idx (not the executor) so
    // the unlock's V4_SETTLE sees it. Without this, _read_pm_delta(forward_a)
    // is zero and V4_SWAP_DYNAMIC(B) reverts SwapAmountCannotBeZero (0xbe8b8507).
    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, &a_fwd).ok()?,
    );

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V2-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v2_v2(
    ha: &V4HopInfo,
    hb: &V2HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.currency1_address
        } else {
            ha.currency0_address
        })
        .ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;

    let b_cmd = encoders::enc_v2_swap_calc(v2b_idx, hb.zfo, v2c_idx, hb.fee);
    let c_cmd = encoders::enc_v2_swap_calc(v2c_idx, hc.zfo, executor_idx, hc.fee);

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a_idx, v2b_idx, out_a).ok()?);
    inner.extend_from_slice(&b_cmd);
    inner.extend_from_slice(&c_cmd);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V2-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v2_v3(
    ha: &V4HopInfo,
    hb: &V2HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.currency1_address
        } else {
            ha.currency0_address
        })
        .ok()?;
    at.add(if hb.zfo {
        hb.token1_address
    } else {
        hb.token0_address
    })
    .ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;

    let mut v4_inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a_idx, v2b_idx, out_a).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b_idx, hb.zfo, v3c_idx, hb.fee,
    ));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));

    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        hc.zfo,
        out_b,
        executor_idx,
        &encoders::enc_v4_unlock(&v4_inner).ok()?,
    )
    .ok()?;

    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V2-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v2_v4(
    ha: &V4HopInfo,
    hb: &V2HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.currency1_address
        } else {
            ha.currency0_address
        })
        .ok()?;
    at.add(if hb.zfo {
        hb.token1_address
    } else {
        hb.token0_address
    })
    .ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a_idx, v2b_idx, out_a).ok()?);
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b_idx,
        hb.zfo,
        executor_idx,
        hb.fee,
    ));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, out_b)
            .ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V3-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v3_v2(
    ha: &V4HopInfo,
    hb: &V3HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3b_idx = at.add(hb.pool_address).ok()?;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.currency1_address
        } else {
            ha.currency0_address
        })
        .ok()?;
    at.add(if hb.zfo {
        hb.token1_address
    } else {
        hb.token0_address
    })
    .ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;

    let mut b_fwd = encoders::enc_v4_take_compact(forward_a_idx, v3b_idx, out_a).ok()?;
    b_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, executor_idx).ok()?,
    );

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, v2c_idx, &b_fwd).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V3-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v3_v3(
    ha: &V4HopInfo,
    hb: &V3HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    at.add(pool_manager_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.currency1_address
        } else {
            ha.currency0_address
        })
        .ok()?;

    let b_fwd = encoders::enc_v4_take_compact(forward_a_idx, v3b_idx, out_a).ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;

    let inner_v3b = encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, v3c_idx, &b_fwd).ok()?;
    let inner_v3c =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, &inner_v3b).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(&inner_v3c);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V3-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v3_v4(
    ha: &V4HopInfo,
    hb: &V3HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3b_idx = at.add(hb.pool_address).ok()?;

    let forward_a_idx = at
        .add(if ha.zfo {
            ha.currency1_address
        } else {
            ha.currency0_address
        })
        .ok()?;
    let forward_b_idx = at
        .add(if hb.zfo {
            hb.token1_address
        } else {
            hb.token0_address
        })
        .ok()?;

    let b_fwd = encoders::enc_v4_take_compact(forward_a_idx, v3b_idx, out_a).ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_sync(forward_b_idx));
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, out_a, pm_idx, &b_fwd).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, out_b)
            .ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V4-V2 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn three_hop_v4_v4_v2(
    ha: &V4HopInfo,
    hb: &V4HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;

    // Inline add() in Python execution order (forward_b, P2C, V4a currencies, V4b currencies).
    let c_cmd =
        encoders::enc_v2_swap_direct(at.add(hc.pool_address).ok()?, hc.zfo, out_c, executor_idx)
            .ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        at.add(ha.currency0_address).ok()?,
        at.add(ha.currency1_address).ok()?,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        at.add(hb.currency0_address).ok()?,
        at.add(hb.currency1_address).ok()?,
        fee_b,
        ts_b,
        zero_idx,
        hb.zfo,
    ));
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, at.add(hc.pool_address).ok()?, out_b).ok()?,
    );
    inner.extend_from_slice(&c_cmd);
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V4-V3 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v4_v3(
    ha: &V4HopInfo,
    hb: &V4HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_b = hop_outputs[1];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let c_take = encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, out_b).ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;

    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        optimal_input,
    )
    .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo,
    ));
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, executor_idx, &c_take).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── V4-V4-V4 ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // reason: a/b/c hop vars + c0_idx/c1_idx currency-index names are
                                // canonical V4 pool-state vocabulary; renaming reduces clarity.
fn three_hop_v4_v4_v4(
    ha: &V4HopInfo,
    hb: &V4HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let executor_address = inputs.executor_address;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;
    let opts = inputs.opts;

    if hop_outputs.len() < 3 {
        return None;
    }
    let out_a = hop_outputs[0];
    let out_b = hop_outputs[1];
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    // Mid-currencies at each V4↔V4 boundary — detect native-ETH↔WETH gaps.
    let mid_currency_a_out = if ha.zfo {
        ha.currency1_address
    } else {
        ha.currency0_address
    };
    let mid_currency_b_in = if hb.zfo {
        hb.currency0_address
    } else {
        hb.currency1_address
    };
    let mid_currency_b_out = if hb.zfo {
        hb.currency1_address
    } else {
        hb.currency0_address
    };
    let mid_currency_c_in = if hc.zfo {
        hc.currency0_address
    } else {
        hc.currency1_address
    };
    let bridge_ab = CurrencyBridge::at_boundary(mid_currency_a_out, mid_currency_b_in);
    let bridge_bc = CurrencyBridge::at_boundary(mid_currency_b_out, mid_currency_c_in);
    let any_gap = bridge_ab.needs_bridge() || bridge_bc.needs_bridge();

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(executor_address),
        Some(pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    // Determine output (profit) currency of the final V4 swap.
    let output_currency_c = if hc.zfo {
        hc.currency1_address
    } else {
        hc.currency0_address
    };

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;
    let fee_c = u16::try_from(hc.fee).ok()?;
    let ts_c = i16::try_from(hc.tick_spacing).ok()?;

    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;
    let c0_c_idx = at.add(hc.currency0_address).ok()?;
    let c1_c_idx = at.add(hc.currency1_address).ok()?;

    let mut inner = if opts.use_v4_batch && !any_gap {
        let batch = [
            V4BatchEntry {
                c0_idx: c0_a_idx,
                c1_idx: c1_a_idx,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: zero_idx,
                zfo: ha.zfo,
                amount_u96: optimal_input,
            },
            V4BatchEntry {
                c0_idx: c0_b_idx,
                c1_idx: c1_b_idx,
                fee: fee_b,
                tick_spacing: ts_b,
                hooks_idx: zero_idx,
                zfo: hb.zfo,
                amount_u96: 0,
            },
            V4BatchEntry {
                c0_idx: c0_c_idx,
                c1_idx: c1_c_idx,
                fee: fee_c,
                tick_spacing: ts_c,
                hooks_idx: zero_idx,
                zfo: hc.zfo,
                amount_u96: 0,
            },
        ];
        let mut v = encoders::enc_v4_batch(&batch).ok()?;
        // For ERC-20 profit, still need explicit take.
        if output_currency_c != NATIVE_CURRENCY_ADDRESS && output_currency_c != weth_address {
            let profit_idx = at.add(output_currency_c).ok()?;
            v.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, executor_idx));
        }
        v
    } else {
        // Compact path. When a boundary has a native↔WETH gap, emit
        // TAKE + WETH_DEPOSIT/WITHDRAW + explicit V4_SWAP_COMPACT (the PM
        // delta is on the wrong currency for V4_SWAP_DYNAMIC) +
        // V4_SETTLE_DELTA for the bridged input currency. Non-gap boundaries
        // use V4_SWAP_DYNAMIC (reads the PM delta from the prior swap).
        let mut v = encoders::enc_v4_swap_compact(
            c0_a_idx,
            c1_a_idx,
            fee_a,
            ts_a,
            zero_idx,
            ha.zfo,
            optimal_input,
        )
        .ok()?;

        // Hop B — bridge the A→B representation gap if present, else let
        // V4_SWAP_DYNAMIC read the prior swap's PM delta.
        if bridge_ab.needs_bridge() {
            let (take_idx, b_input_idx) = bridge_ab.bridge_indices(weth_idx, SENTINEL_NATIVE);
            emit_currency_bridge(&mut v, bridge_ab, take_idx, out_a)?;
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, out_a,
                )
                .ok()?,
            );
            v.extend_from_slice(&encoders::enc_v4_settle_delta(b_input_idx));
        } else {
            v.extend_from_slice(&encoders::enc_v4_swap_dynamic(
                c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo,
            ));
        }

        // Hop C — same bridge logic as hop B, against the B→C boundary.
        if bridge_bc.needs_bridge() {
            let (take_idx, c_input_idx) = bridge_bc.bridge_indices(weth_idx, SENTINEL_NATIVE);
            emit_currency_bridge(&mut v, bridge_bc, take_idx, out_b)?;
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, out_b,
                )
                .ok()?,
            );
            v.extend_from_slice(&encoders::enc_v4_settle_delta(c_input_idx));
        } else {
            v.extend_from_slice(&encoders::enc_v4_swap_dynamic(
                c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo,
            ));
        }
        v
    };

    // Profit capture.
    if opts.erc6909_profit && output_currency_c == weth_address {
        let profit_amount = out_c - optimal_input;
        if profit_amount > 0 {
            inner.extend_from_slice(
                &encoders::enc_v4_mint_compact(weth_idx, executor_idx, profit_amount).ok()?,
            );
        }
    } else if !opts.use_v4_batch || any_gap {
        if output_currency_c == NATIVE_CURRENCY_ADDRESS {
            let native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
            inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, executor_idx));
        } else if output_currency_c == weth_address {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, executor_idx));
        } else {
            let profit_idx = at.add(output_currency_c).ok()?;
            inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, executor_idx));
        }
    }

    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit tests — CurrencyBridge classifier + emitter
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn currency_bridge_both_native_is_none() {
        let b = CurrencyBridge::at_boundary(NATIVE_CURRENCY_ADDRESS, NATIVE_CURRENCY_ADDRESS);
        assert_eq!(b, CurrencyBridge::None);
        assert!(!b.needs_bridge());
    }

    #[test]
    fn currency_bridge_both_weth_is_none() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let b = CurrencyBridge::at_boundary(weth, weth);
        assert_eq!(b, CurrencyBridge::None);
    }

    #[test]
    fn currency_bridge_native_to_weth_is_wrap() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let b = CurrencyBridge::at_boundary(NATIVE_CURRENCY_ADDRESS, weth);
        assert_eq!(b, CurrencyBridge::Wrap);
        assert!(b.needs_bridge());
    }

    #[test]
    fn currency_bridge_weth_to_native_is_unwrap() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let b = CurrencyBridge::at_boundary(weth, NATIVE_CURRENCY_ADDRESS);
        assert_eq!(b, CurrencyBridge::Unwrap);
        assert!(b.needs_bridge());
    }

    #[test]
    fn currency_bridge_native_to_erc20_is_wrap() {
        // native → any non-native (ERC-20) is a wrap (executor holds ETH, needs WETH/ERC20 representation)
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let b = CurrencyBridge::at_boundary(NATIVE_CURRENCY_ADDRESS, usdc);
        assert_eq!(b, CurrencyBridge::Wrap);
    }

    #[test]
    fn currency_bridge_erc20_to_native_is_unwrap() {
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let b = CurrencyBridge::at_boundary(usdc, NATIVE_CURRENCY_ADDRESS);
        assert_eq!(b, CurrencyBridge::Unwrap);
    }

    #[test]
    fn emit_currency_bridge_none_emits_nothing() {
        let mut inner = Vec::new();
        let result = emit_currency_bridge(&mut inner, CurrencyBridge::None, 0xFE, 1000);
        assert_eq!(result, Some(()));
        assert!(inner.is_empty(), "None bridge must emit zero bytes");
    }

    #[test]
    fn emit_currency_bridge_wrap_emits_take_plus_deposit() {
        let mut inner = Vec::new();
        let native_idx = 0xFF;
        let amount = 1_000_000_000_000_000_000u128;
        emit_currency_bridge(&mut inner, CurrencyBridge::Wrap, native_idx, amount)
            .expect("Wrap bridge encodes");
        // V4_TAKE_COMPACT = 0x52 (1) + currency_idx (1) + recipient_idx (1) + amount_u96 (12) = 15 bytes
        // WETH_DEPOSIT = 0x12 (1) + amount_u256 (32) = 33 bytes
        assert_eq!(inner.len(), 15 + 33);
        assert_eq!(inner[0], 0x52); // CMD_V4_TAKE_COMPACT
        assert_eq!(inner[1], native_idx);
        assert_eq!(inner[2], SENTINEL_SELF);
        assert_eq!(inner[15], 0x12); // CMD_WETH_DEPOSIT
    }

    #[test]
    fn currency_bridge_indices_wrap_takes_native_settles_weth() {
        // Wrap = native out, WETH in: take native from PM, settle WETH after the swap.
        let (take, settle) = CurrencyBridge::Wrap.bridge_indices(SENTINEL_WETH, SENTINEL_NATIVE);
        assert_eq!(take, SENTINEL_NATIVE, "Wrap takes native out of the PM");
        assert_eq!(
            settle, SENTINEL_WETH,
            "Wrap settles WETH (the swap consumed WETH)"
        );
    }

    #[test]
    fn currency_bridge_indices_unwrap_takes_weth_settles_native() {
        // Unwrap = WETH out, native in: take WETH from PM, settle native after the swap.
        let (take, settle) = CurrencyBridge::Unwrap.bridge_indices(SENTINEL_WETH, SENTINEL_NATIVE);
        assert_eq!(take, SENTINEL_WETH, "Unwrap takes WETH out of the PM");
        assert_eq!(
            settle, SENTINEL_NATIVE,
            "Unwrap settles native (the swap consumed native)"
        );
    }

    #[test]
    fn currency_bridge_indices_none_is_unused_but_well_defined() {
        // None never reaches `bridge_indices` (caller guards `needs_bridge()`);
        // the method still returns a deterministic placeholder for safety.
        let (take, settle) = CurrencyBridge::None.bridge_indices(SENTINEL_WETH, SENTINEL_NATIVE);
        assert_eq!((take, settle), (0, 0));
    }

    #[test]
    fn emit_currency_bridge_unwrap_emits_take_plus_withdraw() {
        let mut inner = Vec::new();
        let weth_idx = SENTINEL_WETH;
        let amount = 2_000_000_000_000_000_000u128;
        emit_currency_bridge(&mut inner, CurrencyBridge::Unwrap, weth_idx, amount)
            .expect("Unwrap bridge encodes");
        // V4_TAKE_COMPACT (15) + WETH_WITHDRAW (33) = 48 bytes
        assert_eq!(inner.len(), 15 + 33);
        assert_eq!(inner[0], 0x52); // CMD_V4_TAKE_COMPACT
        assert_eq!(inner[15], 0x13); // CMD_WETH_WITHDRAW
    }

    #[test]
    #[allow(clippy::similar_names)] // canonical a/b/c + c0/c1 V4 currency-index names
    fn three_hop_v2_v4_v3_feeds_clamped_consumed_input_as_v4_swap_in() {
        // Proves the CL-hop clamp reaches the executor: with the V4 hop
        // over-fed (consumed_inputs[1] < hop_outputs[0]), the encoded V4
        // swap-in amount equals consumed_inputs[1], NOT hop_outputs[0].
        // (Cannot use `v4_simulate_swap` here — the encoder only needs the
        // amounts, which the engine clamp already set.)
        let forward = 2_000_000_000u128;
        let clamped = 1_999_999_999u128; // 1-wei CL clamp margin
        let rust = encode_cmd_3_hop(
            &PathInfo::new(vec![
                HopInfo::V2(V2HopInfo {
                    pool_address: address!("1111111111111111111111111111111111111111"),
                    token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                    token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                    fee: 30,
                    zfo: true,
                }),
                HopInfo::V4(V4HopInfo {
                    pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                    pool_id_hex:
                        "0x1111111111111111111111111111111111111111111111111111111111111111"
                            .to_string(),
                    currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                    currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                    fee: 500,
                    tick_spacing: 10,
                    hook_address: address!("0000000000000000000000000000000000000000"),
                    zfo: true,
                }),
                HopInfo::V3(V3HopInfo {
                    pool_address: address!("6666666666666666666666666666666666666666"),
                    token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                    token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                    fee: 3000,
                    zfo: true,
                }),
            ]),
            1_000_000_000_000_000_000u128,
            &[forward, 2_001_000_000_000_000_000u128, 2_001_000_000u128],
            // consumed_inputs = [opt_input, clamped V4 swap-in, V3 input]
            &[1_000_000_000_000_000_000u128, clamped, 2_001_000_000u128],
            address!("DeAd0000000000000000000000000000000000Be"),
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            EncodeOptions::default(),
        )
        .expect("V2-V4-V3 encodes");
        // The V4 swap-in (u96 amount) is the 4th byte after the V4_SWAP_COMPACT
        // opcode at the offset emitted inside the v4_unlock. Locate the opcode
        // sequence (CMD_V4_SWAP_COMPACT) and read the following u96 amount.
        // Simpler: re-derive the expected via the same encoder primitives as
        // the goldens and assert equality on the full stream using clamped.
        let mut at = AddressTable::with_sentinels(
            Some(address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")),
            Some(address!("DeAd0000000000000000000000000000000000Be")),
            Some(address!("000000000004444c5dc75cB358380D2e3dE08A90")),
        );
        let pm_idx = at
            .add(address!("000000000004444c5dc75cB358380D2e3dE08A90"))
            .unwrap();
        let forward_a_idx = at
            .add(address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"))
            .unwrap();
        let forward_b_idx = at
            .add(address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"))
            .unwrap();
        let executor_idx = SENTINEL_SELF;
        let zero_idx = SENTINEL_NATIVE;
        let v2a_idx = at
            .add(address!("1111111111111111111111111111111111111111"))
            .unwrap();
        let v3c_idx = at
            .add(address!("6666666666666666666666666666666666666666"))
            .unwrap();
        let c0_b_idx = at
            .add(address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"))
            .unwrap();
        let c1_b_idx = at
            .add(address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"))
            .unwrap();
        let mut v4_inner = Vec::new();
        v4_inner.extend_from_slice(&encoders::enc_v4_sync(forward_a_idx));
        v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, pm_idx, 30));
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        // V4 swap-in amount = the CL clamp = clamped, NOT forward.
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(c0_b_idx, c1_b_idx, 500, 10, zero_idx, true, clamped)
                .unwrap(),
        );
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, 2_001_000_000_000_000_000)
                .unwrap(),
        );
        let mut c_fwd = Vec::new();
        c_fwd.extend_from_slice(
            &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000)
                .unwrap(),
        );
        c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
        let commands = encoders::enc_v3_swap_compact(
            v3c_idx,
            true,
            2_001_000_000_000_000_000,
            executor_idx,
            &c_fwd,
        )
        .unwrap();
        let mut expected = encoders::enc_preamble(&at);
        expected.extend_from_slice(&commands);
        assert_eq!(
            rust, expected,
            "V4 swap-in must be the clamped consumed_inputs[1], not hop_outputs[0]"
        );
    }
}
