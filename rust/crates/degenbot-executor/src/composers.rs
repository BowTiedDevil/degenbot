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

// `emit_currency_bridge` (the only user of these) survives RVNIPD only as a
// unit-test fixture.
#[cfg(test)]
use crate::encoders::{self, SENTINEL_SELF};
use alloy::primitives::{Address, U256};
use degenbot_abi::abi_types::AbiValue;
use degenbot_abi::encoder::encode_rust;

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
/// crate docs + `executor/tests/test_cmd_executor_v4v4_wrap_unwrap.py`
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
/// encode (uint96 overflow — the walker's int128 guard bounds the amounts).
/// [`CurrencyBridge::None`] emits nothing.
#[cfg(test)]
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
///
/// **Per-path output axes (ADR-029 D1, WE45KC):** `funding`, `capture`, and
/// `bribe` carry the runtime economic choices the strategy/operator makes per
/// path. Whether a family's builder actually branches an axis IN THE STREAM is
/// **declared per family** on the family→producer dispatch row
/// ([`crate::grammar_shape::family_axis_support`]) — read that, not builder
/// bodies. Today: `funding` is branched only by `v2_v3` + any-N all-V2 (their
/// rows declare `{funding}`); every other family derives it (`InPathFlash`).
/// `capture` is branched only by the pure-V4 families `v4_v4` / `v4_v4_v4`
/// (their rows declare `{capture}`); V2/V3-only and V4-involving-but-not-
/// pure-V4 streams reach `capture` only via the on-chain `check_mode` config
/// (a different seam), NOT the stream bytes. `bribe` is branched by no family
/// (it rides `pack_config`, never the stream). Spreading an axis across more
/// families is separate post-WE45KC work, not this surface's claim. The legacy
/// `erc6909_profit` bool is kept as a backwards-compatible alias for
/// `capture = ProfitCapture::Erc6909` (see [`resolve_axes`] for the precedence
/// rule).
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeOptions {
    /// If `true`, use `V4_MINT_COMPACT` instead of `V4_TAKE_DELTA` for profit
    /// capture on pure-V4 paths (saves ~20K gas; needs `check_mode=2`).
    /// Legacy alias for `capture = ProfitCapture::Erc6909` (see [`resolve_axes`]).
    pub erc6909_profit: bool,
    /// If `true`, use `V4_BATCH` instead of individual `V4_SWAP_COMPACT`/`_DYNAMIC`
    /// for pure-V4 paths (single PM extcall).
    pub use_v4_batch: bool,
    /// Declared origin of the stream's entry (seed) capital (ADR-029 D1).
    /// Branched IN THE STREAM only by the families whose dispatch row declares
    /// `funding` ([`crate::grammar_shape::family_axis_support`]: `v2_v3` and
    /// any-N all-V2); every other family derives `InPathFlash`. Honoring it as
    /// a runtime economic knob across ALL families is separate post-WE45KC work.
    pub funding: crate::grammar_ledger::FundingSource,
    /// Declared destination of the stream's terminal profit (ADR-029 D1).
    /// Honored via [`resolve_axes`] (takes precedence over the legacy
    /// `erc6909_profit` bool only when that bool is `false`).
    pub capture: crate::grammar_ledger::ProfitCapture,
    /// Whether/how a builder bribe is paid (ADR-029 D1/Q3). Not yet honored by
    /// the encoder; wiring lands in a subsequent WE45KC increment.
    pub bribe: crate::grammar_ledger::Bribe,
}

/// Resolve the per-path output axes (ADR-029 D1, WE45KC) from [`EncodeOptions`],
/// collapsing the legacy `erc6909_profit` bool into the `capture` axis.
///
/// **Precedence (backwards-compatible):** `erc6909_profit: true` forces
/// `ProfitCapture::Erc6909` regardless of the `capture` field — so every
/// existing caller that sets the legacy bool keeps today's bytes. A caller that
/// leaves `erc6909_profit: false` (the default) and sets `capture` directly is
/// honored.
///
/// `funding` and `bribe` are passed through unchanged (the encoder does not yet
/// read them; they are carried for the subsequent WE45KC increments).
#[must_use]
pub fn resolve_axes(
    opts: EncodeOptions,
) -> (
    crate::grammar_ledger::FundingSource,
    crate::grammar_ledger::ProfitCapture,
    crate::grammar_ledger::Bribe,
) {
    let capture = if opts.erc6909_profit {
        crate::grammar_ledger::ProfitCapture::Erc6909
    } else {
        opts.capture
    };
    (opts.funding, capture, opts.bribe)
}

// ═══════════════════════════════════════════════════════════════════════════
// Encode intake: EncodeContext (session) + EncodeRequest (per path, ADR-033)
// ═══════════════════════════════════════════════════════════════════════════

/// The session-scoped deployment addresses shared by every encode request in
/// one session (ADR-033). Built once per session (the strategy), never per
/// path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeContext {
    /// The `cmd_executor` contract the stream executes on.
    pub executor: Address,
    /// The Uniswap-V4 `PoolManager` (the pool-key / delta-claim home).
    pub pool_manager: Address,
    /// The session's WETH (the seed + wrap/unwrap bridge currency).
    pub weth: Address,
}

impl EncodeContext {
    #[must_use]
    pub fn new(executor: Address, pool_manager: Address, weth: Address) -> Self {
        Self {
            executor,
            pool_manager,
            weth,
        }
    }
}

/// The per-path encode intake (ADR-033): the path + the solver's amount
/// triple + the declared axes, as one unit.
///
/// Built exactly once at each producing site (the strategy's candidate
/// projection; the declarative harness chain runners) and handed to
/// [`encode_cmd_stream`] together with an [`EncodeContext`]. A request
/// without its path is the shape that lets amounts be synthesized blind to
/// what the path constrains — so path and amounts are one unit.
///
/// **The CL overfeed-clamp invariant (UO3JM4 / path-5000 EMPTY-HALT) attaches
/// to this value**: `consumed_inputs[i]` is the *executable* input fed to hop
/// `i`. For a non-over-fed CL hop (and for V2/Balancer/Curve/Solidly hops) it
/// equals `hop_outputs[i − 1]`; for an over-fed CL hop the producer clamps it
/// to `input_consumed − 1` (the solver's `clamp_cl_hop_capacity`, whose bound
/// is the pools-layer `exact_input_clamp_bound` rule) so the on-chain
/// exact-in loop terminates on `amountRemaining == 0` instead of marching
/// empty bitmap words. Building the request is where that invariant is owned;
/// it is not re-derived by the encoder.
#[derive(Clone, Debug)]
pub struct EncodeRequest {
    /// The resolved path hops (the encode's shape selector).
    pub path: PathInfo,
    /// The flash/optimal input amount (u128; the `cmd_executor` int128
    /// convention).
    pub optimal_input: u128,
    /// Per-hop output amounts. `hop_outputs[i]` = the output after hop `i`
    /// (`[forward_out, final_output]` for a 2-hop path).
    pub hop_outputs: Vec<u128>,
    /// Per-hop executable input amounts (the CL-clamp swap-in — see the type
    /// doc for the invariant).
    pub consumed_inputs: Vec<u128>,
    /// The declared per-path axes (funding / capture / bribe + the opcode
    /// toggles).
    pub opts: EncodeOptions,
}

impl EncodeRequest {
    /// Build a request, checking the hop-alignment invariants.
    ///
    /// `hop_outputs` and `consumed_inputs` are per-hop arrays: each must have
    /// exactly one entry per hop in `path`, or the encode would index
    /// misaligned amounts silently.
    ///
    /// # Panics
    ///
    /// If `hop_outputs.len()` or `consumed_inputs.len()` differs from
    /// `path.hops.len()` (a programmer error — the arrays are per-hop, aligned
    /// with the path). The panic names the mismatched arrays.
    #[must_use]
    pub fn new(
        path: PathInfo,
        optimal_input: u128,
        hop_outputs: Vec<u128>,
        consumed_inputs: Vec<u128>,
        opts: EncodeOptions,
    ) -> Self {
        let n = path.hops.len();
        assert_eq!(
            hop_outputs.len(),
            n,
            "EncodeRequest: hop_outputs has {} entries for a {}-hop path",
            hop_outputs.len(),
            n
        );
        assert_eq!(
            consumed_inputs.len(),
            n,
            "EncodeRequest: consumed_inputs has {} entries for a {}-hop path",
            consumed_inputs.len(),
            n
        );
        Self {
            path,
            optimal_input,
            hop_outputs,
            consumed_inputs,
            opts,
        }
    }
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
/// The intake contract is the pair [`EncodeContext`] (session-scoped
/// deployment addresses) + [`EncodeRequest`] (per path: the path + the
/// solver's amount triple + the declared axes — ADR-033). The CL
/// overfeed-clamp invariant attaches to the request (`consumed_inputs[i]`
/// is the executable input to hop `i`) — it is owned where the request is
/// built, not re-derived here. Bribes never ride the stream: the caller
/// passes them through `pack_config` at the call site.
///
/// Returns `None` if encoding declines for this path type. A validator
/// `Reject` (a Plan was built but violated the ledger invariants) is fatal
/// by contract (ADR-030) — it panics rather than folding into `None`.
///
/// # Path-type routing
///
/// * all-V2 hops (≥2): [`crate::grammar_shape::derive_all_v2`] — the Plan +
///   validator path (KO5NNB cutover)
/// * every other 2/3-hop mix: the shape-class walker
///   ([`crate::grammar_shape::derive_shape`])
#[must_use]
pub fn encode_cmd_stream(ctx: &EncodeContext, req: &EncodeRequest) -> Option<Vec<u8>> {
    let num_hops = req.path.hops.len();
    let inputs = ComposerInputs {
        executor_address: ctx.executor,
        pool_manager_address: ctx.pool_manager,
        weth_address: ctx.weth,
        optimal_input: req.optimal_input,
        hop_outputs: &req.hop_outputs,
        consumed_inputs: &req.consumed_inputs,
        opts: req.opts,
    };

    // Facet A (T2TCJM): a generic per-shape-class hop-grammar walk replaces the
    // former 8 two-hop + 27 three-hop bespoke permutation bodies, producing
    // byte-identical output (validated by the golden corpus). All-V2 any-N uses
    // the Plan + validator path (`derive_all_v2` → `build_walk` → gate
    // → `plan_to_bytes`, KO5NNB); other 2/3-hop paths use the combo grammar walk.
    if num_hops >= 2 && req.path.hops.iter().all(|h| matches!(h, HopInfo::V2(_))) {
        crate::grammar_shape::derive_all_v2(&req.path, &inputs)
    } else {
        crate::grammar_shape::derive_shape(&req.path, &inputs)
    }
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

/// Build the `execute(bytes,uint256)` `config` uint256 matching an
/// [`EncodeOptions`] (the axis-aware config builder, WE45KC). Reads the full
/// per-path axis set:
///   - `capture` → `check_mode`: `Erc6909` = 2 (verify via PM.balanceOf),
///     `SweepToAddress` = 3 (SWEEP — defeats the assert), every other capture
///     (`Custody`/`Native`/`Owner`/`BalancerVault`) = 1 (WETH+ETH combined
///     balance assert — active by default, U3WVLL). Resolved through
///     [`resolve_axes`] so the legacy `erc6909_profit` bool is collapsed into
///     `capture` (backwards-compatible: `erc6909_profit: true` forces
///     `Erc6909`).
///   - `bribe` → `bribe_bips` + `bribe_recipient_idx`: `None` = (0, 0) (no bribe);
///     `Some{bips, recipient_idx}` is forwarded (recipient_idx 0 = block.coinbase).
///   - `expected_value` is IGNORED (kept in the signature for ABI compat; the
///     U3WVLL contract fix made the executor read its OWN combined balance at
///     start+end, so the operator no longer supplies the pre-tx balance).
///
/// This is the single axis-aware config builder. Production
/// (`degenbot-arbitrage`'s `simulate_path_on_evm`, Q35IJN) packs every
/// `execute(bytes, uint256)` call through it, and the declarative harness
/// (`run_path_with_*`, SMOZG3) mirrors it — so the on-chain profit check
/// (check_mode 1/2/3) runs under production exactly as it runs under tests.
/// Only the offline calldata-dump examples use the raw zero config.
///
/// # Errors
///
/// Returns [`crate::encoders::EncoderError`] if the resolved
/// `bribe` axis is out of range (`bips > 10_000` or `recipient_idx >= 32`);
/// `check_mode` is always in range (statically resolved from `ProfitCapture`).
pub fn config_for_options(
    opts: EncodeOptions,
    expected_value: U256,
) -> Result<U256, crate::config::ConfigError> {
    let _ = expected_value; // U3WVLL: ignored — the contract reads its own balance.
    let (_, capture, bribe) = resolve_axes(opts);
    // U3WVLL defect fix: the profit assert is active by default. Non-erc6909
    // captures use check_mode=1 (WETH+ETH combined balance assert — the
    // on-chain money-loss protection the operator wants active "nearly
    // always"); Erc6909 uses check_mode=2 (ERC6909 WETH). check_mode=0 (fast
    // path, no assert) is no longer the default — it was the footgun that
    // silently skipped the profit check.
    let check_mode = match capture {
        crate::grammar_ledger::ProfitCapture::Erc6909 => 2u8,
        crate::grammar_ledger::ProfitCapture::SweepToAddress => 3u8,
        _ => 1u8,
    };
    let (bribe_bips, bribe_recipient_idx) = match bribe {
        crate::grammar_ledger::Bribe::None => (0u16, 0u8),
        crate::grammar_ledger::Bribe::Some {
            bips,
            recipient_idx,
        } => (bips, recipient_idx),
    };
    crate::config::pack_config(check_mode, U256::ZERO, bribe_bips, bribe_recipient_idx)
}

/// Wrap a command-stream `commands` payload in the `execute(bytes, uint256)`
/// ABI call to the `cmd_executor` contract.
///
/// `config` is the packed `execute()` config uint256 (see
/// [`config::pack_config`]); `0` = skip profit check, no bribe.
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
// 3-hop entry point (grammar-delegating, kept for API/tests)
// ═══════════════════════════════════════════════════════════════════════════

/// Encode a 3-hop arbitrage path as a `cmd_executor` command stream.
///
/// Facet A (T2TCJM): delegates to the generic per-shape-class grammar walk
/// ([`crate::grammar_shape`]), which dispatches to per-family hop adapters — the
/// same byte-identical engine `encode_cmd_stream` now uses. Retained as a thin
/// 3-hop convenience entry (public, `#[doc(hidden)]`) for callers/tests that
/// previously reached the 27 `three_hop_*` dispatcher directly.
///
/// Returns `None` for an unknown combination or if any `enc_*` step fails.
#[doc(hidden)]
#[must_use]
#[expect(clippy::too_many_arguments)] // 3-hop entry carries executor/pm/weth + opts (matches bespoke signature)
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
    let inputs = ComposerInputs {
        executor_address,
        pool_manager_address,
        weth_address,
        optimal_input,
        hop_outputs,
        consumed_inputs,
        opts,
    };
    crate::grammar_shape::derive_shape(path_info, &inputs)
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit tests — CurrencyBridge classifier + emitter
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::encoders::{self, AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};
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
    #[expect(clippy::similar_names)] // canonical a/b/c + c0/c1 V4 currency-index names
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
            // Exact-match: the V4 take carries consumed_inputs[2] (the v3c exit
            // swap-in), NOT the solver's over-predictable out_b (path-73385).
            &encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, 2_001_000_000).unwrap(),
        );
        // The CL clamp caps the V4 swap-in below the settled V2 forward, leaving
        // a residual on the settled currency (forward_a). Sweep it back so the
        // unlock nets to zero (else CurrencyNotSettled at unlock exit).
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a_idx));
        let mut c_fwd = Vec::new();
        c_fwd.extend_from_slice(
            &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000)
                .unwrap(),
        );
        c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
        let commands = encoders::enc_v3_swap_compact(
            v3c_idx,
            true,
            // exact-match: the v3c exit swap-in = consumed_inputs[2]
            2_001_000_000,
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

    /// The V4 exit take in `three_hop_v3_v4_v3` must use `consumed_inputs[2]`
    /// (the byte-exact V4 output, path-73385 twin) — NOT the solver's raw
    /// `hop_outputs[1]`, which can over-predict the V4 output by a few wei and
    /// over-take the pool, stranding a residual delta that the trailing
    /// V4_SETTLE_ALL repays via a failing `USDT.transfer(PM, …)` (0xfe halt).
    #[test]
    fn three_hop_v3_v4_v3_take_uses_consumed_inputs2_not_hop_outputs1() {
        // Path-73385 numbers: solver predicted V4 output 85097884 (hop_outputs[1])
        // but the pool's byte-exact twin output is 85097881 (consumed_inputs[2]).
        let optimal_input = 44_421_383_036_608_956u128;
        let v4_predicted = 85_097_884u128;
        let v4_actual = 85_097_881u128;
        let take_from = |consumed2| {
            let rust = encode_cmd_3_hop(
                &PathInfo::new(vec![
                    HopInfo::V3(V3HopInfo {
                        pool_address: address!("E0554a476A092703abdB3Ef35c80e0D76d32939F"),
                        token0_address: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                        token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                        fee: 100,
                        zfo: false,
                    }),
                    HopInfo::V4(V4HopInfo {
                        pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                        pool_id_hex:
                            "0x8aa4e11cbdf30eedc92100f4c8a31ff748e201d44712cc8c90d189edaa8e4e47"
                                .to_string(),
                        currency0_address: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                        currency1_address: address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
                        fee: 10,
                        tick_spacing: 1,
                        hook_address: address!("0000000000000000000000000000000000000000"),
                        zfo: true,
                    }),
                    HopInfo::V3(V3HopInfo {
                        pool_address: address!("c7bBeC68d12a0d1830360F8Ec58fA599bA1b0e9b"),
                        token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                        token1_address: address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
                        fee: 100,
                        zfo: false,
                    }),
                ]),
                optimal_input,
                &[85_060_245, v4_predicted, 44_421_879_564_949_974],
                // consumed_inputs = [opt, V4 swap-in, exact V4 output]
                &[optimal_input, 85_060_245, consumed2],
                address!("DeAd0000000000000000000000000000000000Be"),
                address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                EncodeOptions::default(),
            )
            .expect("V3-V4-V3 encodes");
            // Locate the single V4_TAKE_COMPACT (0x52) and read the 12-byte amount.
            let found = rust
                .windows(15)
                .find(|w| w[0] == 0x52 && w[3..].iter().any(|b| *b != 0))
                .map(|w| {
                    let mut a = [0u8; 16];
                    a[4..].copy_from_slice(&w[3..15]); // 12 bytes at offset 3
                    u128::from_be_bytes(a)
                });
            found
        };
        // With the exact twin amount in consumed_inputs[2], the take = that amount.
        assert_eq!(take_from(v4_actual), Some(v4_actual));
        // And it is NOT the solver's over-predicted hop_outputs[1].
        assert_eq!(take_from(v4_actual), Some(v4_actual));
        assert_ne!(take_from(v4_actual), Some(v4_predicted));
    }
}

// ── WE45KC: resolve_axes (ADR-029 D1) ────────────────────────────────

#[test]
fn resolve_axes_default_is_custody_no_bribe() {
    let (funding, capture, bribe) = resolve_axes(EncodeOptions::default());
    assert_eq!(funding, crate::grammar_ledger::FundingSource::InPathFlash);
    assert_eq!(capture, crate::grammar_ledger::ProfitCapture::Custody);
    assert_eq!(bribe, crate::grammar_ledger::Bribe::None);
}

#[test]
fn resolve_axes_legacy_erc6909_bool_forces_erc6909_capture() {
    // Backwards-compat: erc6909_profit: true wins over the capture field.
    for capture in [
        crate::grammar_ledger::ProfitCapture::Custody,
        crate::grammar_ledger::ProfitCapture::Owner,
        crate::grammar_ledger::ProfitCapture::Native,
    ] {
        let opts = EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            capture,
            ..Default::default()
        };
        assert_eq!(
            resolve_axes(opts).1,
            crate::grammar_ledger::ProfitCapture::Erc6909,
            "legacy erc6909_profit:true must force Erc6909 even with capture={capture:?}"
        );
    }
}

#[test]
fn resolve_axes_capture_field_honored_when_legacy_bool_false() {
    for capture in [
        crate::grammar_ledger::ProfitCapture::Custody,
        crate::grammar_ledger::ProfitCapture::Owner,
        crate::grammar_ledger::ProfitCapture::Native,
        crate::grammar_ledger::ProfitCapture::Erc6909,
    ] {
        let opts = EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
            capture,
            ..Default::default()
        };
        assert_eq!(resolve_axes(opts).1, capture);
    }
}

#[test]
fn resolve_axes_bribe_passes_through() {
    let opts = EncodeOptions {
        bribe: crate::grammar_ledger::Bribe::Some {
            bips: 50,
            recipient_idx: 0,
        },
        ..Default::default()
    };
    assert_eq!(
        resolve_axes(opts).2,
        crate::grammar_ledger::Bribe::Some {
            bips: 50,
            recipient_idx: 0
        }
    );
}

#[test]
fn resolve_axes_funding_passes_through() {
    for funding in [
        crate::grammar_ledger::FundingSource::SelfFund,
        crate::grammar_ledger::FundingSource::PmLedger,
        crate::grammar_ledger::FundingSource::ExternalLender,
        crate::grammar_ledger::FundingSource::Erc6909BurnToSettle,
    ] {
        let opts = EncodeOptions {
            funding,
            ..Default::default()
        };
        assert_eq!(resolve_axes(opts).0, funding);
    }
}

// ── WE45KC: config_for_options axis→config mapping ───────────────────

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_default_is_check_mode_1() {
    // U3WVLL defect fix: default (Custody, no bribe) → check_mode=1 (WETH+ETH
    // profit assert active). The contract reads its own combined balance at
    // start+end and asserts combined_after >= combined_before. This is the
    // "profit assert active nearly always" protection the operator wants.
    let cfg = config_for_options(EncodeOptions::default(), U256::ZERO).unwrap();
    assert_eq!(
        cfg & U256::from(255u64),
        U256::from(1u64),
        "default → check_mode=1"
    );
    assert_eq!(
        (cfg >> 8) & U256::from(65535u64),
        U256::ZERO,
        "no bribe by default"
    );
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_capture_erc6909_sets_check_mode_2() {
    let opts = EncodeOptions {
        capture: crate::grammar_ledger::ProfitCapture::Erc6909,
        ..Default::default()
    };
    let cfg = config_for_options(opts, U256::ZERO).unwrap();
    assert_eq!(
        cfg & U256::from(255u64),
        U256::from(2u64),
        "Erc6909 → check_mode=2"
    );
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_capture_native_is_check_mode_1() {
    // U3WVLL: Native capture also uses check_mode=1 (WETH+ETH combined assert;
    // the in-stream WETH_WITHDRAW leaves the profit as ETH, still counted in
    // the combined balance). The profit assert is active for Native capture too.
    let opts = EncodeOptions {
        capture: crate::grammar_ledger::ProfitCapture::Native,
        ..Default::default()
    };
    let cfg = config_for_options(opts, U256::ZERO).unwrap();
    assert_eq!(
        cfg & U256::from(255u64),
        U256::from(1u64),
        "Native → check_mode=1 (assert active)"
    );
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_legacy_erc6909_bool_forces_check_mode_2() {
    // Backwards-compat: the legacy `erc6909_profit: true` bool forces
    // Erc6909 (via resolve_axes precedence) → check_mode=2.
    let opts = EncodeOptions {
        erc6909_profit: true,
        capture: crate::grammar_ledger::ProfitCapture::Custody, // overridden
        ..Default::default()
    };
    let cfg = config_for_options(opts, U256::ZERO).unwrap();
    assert_eq!(
        cfg & U256::from(255u64),
        U256::from(2u64),
        "legacy bool forces check_mode=2"
    );
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_bribe_packs_bips_and_recipient() {
    let opts = EncodeOptions {
        bribe: crate::grammar_ledger::Bribe::Some {
            bips: 500,
            recipient_idx: 3,
        },
        ..Default::default()
    };
    let cfg = config_for_options(opts, U256::ZERO).unwrap();
    // bits 8-23: bribe_bips = 500
    assert_eq!((cfg >> 8) & U256::from(65535u64), U256::from(500u64));
    // bits 24-31: bribe_recipient_idx = 3
    assert_eq!((cfg >> 24) & U256::from(255u64), U256::from(3u64));
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_expected_value_is_ignored() {
    // U3WVLL: expected_value is IGNORED (the contract reads its own combined
    // balance at start+end). The high bits are always 0 regardless of the
    // operator-supplied expected_value.
    let ev = U256::from(0xBEEFu64);
    let cfg = config_for_options(EncodeOptions::default(), ev).unwrap();
    assert_eq!(
        cfg >> 32,
        U256::ZERO,
        "expected_value ignored by the builder"
    );
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_combines_all_axes() {
    // Erc6909 check + 5% bribe to coinbase.
    let opts = EncodeOptions {
        capture: crate::grammar_ledger::ProfitCapture::Erc6909,
        bribe: crate::grammar_ledger::Bribe::Some {
            bips: 500,
            recipient_idx: 0,
        },
        ..Default::default()
    };
    let cfg = config_for_options(opts, U256::from(1_000_000u64)).unwrap();
    assert_eq!(cfg & U256::from(255u64), U256::from(2u64)); // check_mode=2
    assert_eq!((cfg >> 8) & U256::from(65535u64), U256::from(500u64)); // bips
    assert_eq!((cfg >> 24) & U256::from(255u64), U256::ZERO); // recipient=0 (coinbase)
    assert_eq!(cfg >> 32, U256::ZERO); // expected_value ignored (U3WVLL)
}

#[test]
#[expect(clippy::unwrap_used)] // test asserts config bits; unwrap is fine
fn config_for_options_capture_sweep_to_address_sets_check_mode_3() {
    // U3WVLL follow-up (767TN5): ProfitCapture::SweepToAddress routes to
    // check_mode=3 (SWEEP) — the only way to defeat the profit assert.
    let opts = EncodeOptions {
        capture: crate::grammar_ledger::ProfitCapture::SweepToAddress,
        ..Default::default()
    };
    let cfg = config_for_options(opts, U256::ZERO).unwrap();
    assert_eq!(
        cfg & U256::from(255u64),
        U256::from(3u64),
        "SweepToAddress → check_mode=3 (SWEEP)"
    );
}
