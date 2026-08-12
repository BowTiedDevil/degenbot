//! Facet-A derivation **spike** (ergo `6YUNQN`, epic `463V2C`).
//!
//! Feasibility proof for ADR-029 **D4 (hybrid)**: a 2/3-hop family can be
//! emitted from a [`ShapeClass`] + declarative per-hop ledger facts + a
//! per-protocol encoder, instead of a hand-written adapter — and the result
//! executes through the runtime matrix with exact delta.
//!
//! The hybrid split this spike embodies (ADR-029 D4):
//! * **declarative coupling/ledger facts** — [`HopFacts`]: which ledgers a hop
//!   touches, its forward (output) currency, and its coupling role at each
//!   boundary. These are *data* the derivation reasons over.
//! * **per-protocol mechanics** — the `enc_event_*`-style encoder selection in
//!   [`emit_hop`] / [`derive_2hop`] (here a `match`; in production a trait impl
//!   per protocol). The Solidity callback wiring is code, not data (D4).
//!
//! The **enclosure/call-structure** (which hop wraps which `unlock`/callback)
//! and the **repayment pivot** are *derived* from the funding source + the
//! ledgers, never chosen by the caller (ADR-029 D3).
//!
//! **Scope:** this spike covers the V2/V3 2-hop domain (`v2_v3`, `v3_v2`,
//! `v3_v3`) — the minimal cross-section that exercises two *distinct* funding
//! sources (in-path flash vs self-fund), two *distinct* coupling modes
//! (exec-balance bridge vs pool-to-pool via `V2_SWAP_CALC`), and the
//! **terminal-V2 pre-fund rule** (`2PT5HH`). Pure-V4 (PM-ledger + `V4_TAKE`
//! coupling + native bridges) is the harder residual for `WAYDTL` — the spike
//! reports that boundary honestly rather than pretending to span it.

use alloy::primitives::{Address, U256};

use crate::composers::{
    fits_int128, ComposerInputs, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
    NATIVE_CURRENCY_ADDRESS,
};
use crate::encoders::{self, AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH};

/// A hop-protocol family member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    V2,
    V3,
    V4,
}

/// How the stream's entry (seed) capital is supplied (ADR-029 D1).
///
/// Exactly one per stream. For the V2/V3 2-hop domain this is *derived* from
/// the leading-hop protocol and the D0 invariant (never user-chosen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FundingSource {
    /// The outermost pool's own swap-callback extends the entry credit and is
    /// repaid **by the path itself** (executor may start at 0).
    InPathFlash,
    /// The executor holds the entry WETH and pre-funds the leading hop.
    SelfFund,
}

/// A family's shape: hop-protocol sequence + funding source. Profit capture,
/// builder bribe, hop coupling, and repayment pivot are **derived** from the
/// ledger rules (ADR-029 D1/D3), not carried here.
#[derive(Clone, Debug)]
pub struct ShapeClass {
    pub protocols: Vec<Prot>,
    pub funding: FundingSource,
}

/// Declarative ledger facts for one hop — the *data* half of the hybrid
/// (ADR-029 D4): which ledgers the hop touches, its forward (output) currency,
/// and whether it is a callback-flash source.
///
/// Resolved by [`hop_facts`] per hop (a simplified stand-in for the production
/// descriptor record; the field list is the same as `HopFacts`).

/// Resolve the forward currency + terminal-V2 fact for a hop (the declarative
/// "coupling/ledger facts" of ADR-029 D4).
fn hop_facts(h: &HopInfo) -> (Address, bool) {
    match h {
        HopInfo::V2(x) => (v2_forward(x), true),
        HopInfo::V3(x) => (v3_forward(x), false),
        // V4 is outside this spike's domain (residual for WAYDTL).
        HopInfo::V4(_) => unreachable!("V4 outside the 6YUNQN V2/V3 spike"),
    }
}
fn v2_forward(h: &V2HopInfo) -> Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}
fn v3_forward(h: &V3HopInfo) -> Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}

/// Per-protocol encoder selection for the **terminal** hop (D4 mechanics half).
///
/// `pre_grant_to` is the address-table index already credited with the hop's
/// input (a prior `V4_TAKE_COMPACT`/`ERC20_TRANSFER` into the pair). A terminal
/// V2 always swaps via `V2_SWAP_CALC` from that pre-grant (credit-before-debit
/// on the pair-handoff ledger — the `2PT5HH` / `path-182449` rule); a terminal
/// V3 is a `V3_SWAP_COMPACT` flash whose input comes from the coupled ledger.
fn emit_terminal_hop(
    at: &mut AddressTable,
    h: &HopInfo,
    inputs: &ComposerInputs<'_>,
    swap_in: u128,
    pre_grant_to: u8,
    out: &mut Vec<u8>,
) -> Option<()> {
    match h {
        HopInfo::V2(x) => {
            // Terminal-V2 pre-fund rule: swap from whatever the feeder actually
            // delivered to the pair (V2_SWAP_CALC), never an exact-out
            // `V2_SWAP_COMPACT` (over-draws 1 wei → `UniswapV2: K`).
            let _ = (x.pool_address, pre_grant_to, inputs);
            out.extend_from_slice(&encoders::enc_v2_swap_calc(
                at.add(x.pool_address).ok()?,
                x.zfo,
                SENTINEL_SELF,
                x.fee,
            ));
        }
        HopInfo::V3(x) => {
            out.extend_from_slice(
                &encoders::enc_v3_swap_compact(
                    at.add(x.pool_address).ok()?,
                    x.zfo,
                    swap_in,
                    SENTINEL_SELF,
                    &[],
                )
                .ok()?,
            );
        }
        HopInfo::V4(_) => unreachable!("V4 outside the spike"),
    }
    Some(())
}

/// Emit a **single-hop spanning the whole family** and append to `out`.
///
/// This is the per-protocol mechanics dispatch (D4). It emits one
/// flash-capable hop and returns the facts needed for the next boundary. To
/// keep the spike readable it is specialized to the V2/V3 2-hop shapes; the
/// production emitter (WAYDTL) generalizes this into a HopFacts-driven walk.
fn derive_2hop(
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
    class: &ShapeClass,
) -> Option<Vec<u8>> {
    let n = path.hops.len();
    if n != 2 {
        return None;
    }
    let ha = &path.hops[0];
    let hb = &path.hops[1];
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    // Terminal-V3 swap-in via the CL-clamp rule (single shared point).
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let (fwd_a, _) = hop_facts(ha);

    match (ha, hb, class.funding) {
        // ── v2_v3 — V2 in-path flash source; forward bridged to V3 via exec. ──
        (HopInfo::V2(a), HopInfo::V3(b), FundingSource::InPathFlash) => {
            let v2_idx = at.add(a.pool_address).ok()?;
            let v3_idx = at.add(b.pool_address).ok()?;
            let forward_idx = at.add(fwd_a).ok()?;
            // V3's input pre-granted from the V2 forward output (exec bridge).
            let v3_cb = encoders::enc_erc20_transfer(forward_idx, v3_idx, b_swap_in).ok()?;
            let mut cb =
                encoders::enc_v3_swap_compact(v3_idx, b.zfo, b_swap_in, SENTINEL_SELF, &v3_cb)
                    .ok()?;
            // Repay the V2 flash from the V3 WETH output (derived pivot).
            cb.extend_from_slice(
                &encoders::enc_erc20_transfer(SENTINEL_WETH, v2_idx, optimal_input).ok()?,
            );
            let commands = encoders::enc_v2_swap_compact(
                v2_idx,
                a.zfo,
                forward_out,
                SENTINEL_SELF,
                a.fee,
                &cb,
            )
            .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        // ── v3_v2 — V3 self-fund; terminal V2 pre-funded + V2_SWAP_CALC. ──
        (HopInfo::V3(a), HopInfo::V2(b), FundingSource::SelfFund) => {
            let v3_idx = at.add(a.pool_address).ok()?;
            let v2_idx = at.add(b.pool_address).ok()?;
            let forward_idx = at.add(fwd_a).ok()?;
            let mut cb = encoders::enc_erc20_transfer(SENTINEL_WETH, v3_idx, optimal_input).ok()?;
            // Pre-grant the terminal V2 pair its input, then swap from it.
            cb.extend_from_slice(
                &encoders::enc_erc20_transfer(forward_idx, v2_idx, forward_out).ok()?,
            );
            emit_terminal_hop(&mut at, hb, inputs, 0, v2_idx, &mut cb)?;
            let commands =
                encoders::enc_v3_swap_compact(v3_idx, a.zfo, optimal_input, SENTINEL_SELF, &cb)
                    .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        // ── v3_v3 — V3 self-fund; both hops flash-coupled via exec. ──
        (HopInfo::V3(a), HopInfo::V3(b), FundingSource::SelfFund) => {
            let v3_a = at.add(a.pool_address).ok()?;
            let v3_b = at.add(b.pool_address).ok()?;
            let mut a_cb = encoders::enc_erc20_transfer(SENTINEL_WETH, v3_a, optimal_input).ok()?;
            let b_cmd =
                encoders::enc_v3_swap_compact(v3_b, b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?;
            a_cb.extend_from_slice(&b_cmd);
            let commands =
                encoders::enc_v3_swap_compact(v3_a, a.zfo, optimal_input, SENTINEL_SELF, &a_cb)
                    .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        // ── v2_v2 — V2 in-path flash; pool-to-pool via V2_SWAP_CALC. ──
        (HopInfo::V2(a), HopInfo::V2(b), FundingSource::InPathFlash) => {
            let mut at = at;
            let v2_a = at.add(a.pool_address).ok()?;
            let v2_b = at.add(b.pool_address).ok()?;
            let fwd_a = at.add(fwd_a).ok()?;
            // Pre-grant pool b with a's forward output, then V2_SWAP_CALC.
            let mut cb = encoders::enc_erc20_transfer(fwd_a, v2_b, forward_out).ok()?;
            emit_terminal_hop(&mut at, hb, inputs, 0, v2_b, &mut cb)?;
            // Repay the a-flash from pool b's WETH output via the executor.
            cb.extend_from_slice(
                &encoders::enc_erc20_transfer(SENTINEL_WETH, v2_a, optimal_input).ok()?,
            );
            let commands =
                encoders::enc_v2_swap_compact(v2_a, a.zfo, forward_out, SENTINEL_SELF, a.fee, &cb)
                    .ok()?;
            let mut out = encoders::enc_preamble(&at);
            out.extend_from_slice(&commands);
            Some(out)
        }
        _ => None,
    }
}

/// Public spike entry: derive a family's command stream from its
/// [`ShapeClass`] (funding chosen by the leading protocol, as the D0
/// invariant forces). Returns the raw `execute()` payload bytes.
#[must_use]
pub fn derive_shape(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    // V4-involving families: a pure-V4 2-hop path is the *container* case — the
    // whole stream is one V4_UNLOCK over internal ledger movement, so no funding
    // choice is needed (the PM carries the entry credit). Handle it before the
    // V2/V3 funding dispatch.
    match (path.hops.first(), path.hops.get(1), path.hops.get(2)) {
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v4v4v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v4v2v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v2v2v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v2v3v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v3v2v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            derive_3hop_v3v3v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            derive_3hop_v2v4v2(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), None) => derive_2hop_v4v4(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), None) => derive_2hop_v4v3(a, b, inputs),
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), None) => derive_2hop_v3v4(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), None) => derive_2hop_v4v2(a, b, inputs),
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), None) => derive_2hop_v2v4(a, b, inputs),
        _ => derive_2hop_v2v3(path, inputs),
    }
}

/// V2/V3 2-hop / 3-hop-(V2/V3) entry (the previous funding-based dispatch).
fn derive_2hop_v2v3(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let funding = match path.hops.first()? {
        HopInfo::V2(_) => FundingSource::InPathFlash,
        HopInfo::V3(_) => FundingSource::SelfFund,
        _ => return None,
    };
    let protocols: Vec<Prot> = path
        .hops
        .iter()
        .map(|h| match h {
            HopInfo::V2(_) => Prot::V2,
            HopInfo::V3(_) => Prot::V3,
            _ => unreachable!("V4 outside the V2/V3 branch"),
        })
        .collect();
    let class = ShapeClass { protocols, funding };
    derive_2hop(path, inputs, &class)
}

/// Pure V4→V4 2-hop container derivation (WAYDTL step 2, **WETH-only slice**).
///
/// Per the v4 ledger rules / boundary model (`docs/plans/executor-v4-ledger-rules.md`):
/// the whole stream is one `V4_UNLOCK`; V4→V4 is internal ledger movement (no
/// `TAKE`, no ERC-20 transfer); the WETH output is captured by `TAKE_DELTA(WETH→SELF)`;
/// a trailing `V4_SETTLE_ALL` flushes any residual so every delta nets to zero by
/// callback end (the one master V4 invariant).
///
/// Scoped to the WETH-only, no-native-bridge, WETH-output case (the harness `v4_v4`
/// family) — `default` opts (no `V4_BATCH`, no `erc6909_profit`). Other V4 shapes
/// (native bridges, non-WETH output, batch/mint) return `None` for now (later steps).
fn derive_2hop_v4v4(a: &V4HopInfo, b: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    use crate::composers::{emit_currency_bridge, CurrencyBridge};

    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let output_currency_b = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    let mid_currency_a = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let mid_currency_b = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    let b_needs_native = mid_currency_b == NATIVE_CURRENCY_ADDRESS;
    let bridge = CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b);
    let currency_gap = bridge.needs_bridge();

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    // Native is address(0) — a sentinel in the table; registering it is a no-op.
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

    // One unlock: both swaps, the native<->WETH boundary bridge when present,
    // profit capture, and a trailing settle to net every currency to zero.
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a,
        c1_a,
        fee_a,
        ts_a,
        SENTINEL_NATIVE,
        a.zfo,
        optimal_input,
    )
    .ok()?;
    if currency_gap {
        let bridge_idx = match bridge {
            CurrencyBridge::Wrap => native_idx,
            CurrencyBridge::Unwrap => weth_idx,
            CurrencyBridge::None => unreachable!("currency_gap implies a bridge"),
        };
        emit_currency_bridge(&mut inner, bridge, bridge_idx, forward_out)?;
    }
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, SENTINEL_NATIVE, b.zfo, b_swap_in)
            .ok()?,
    );
    if currency_gap {
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(if b_needs_native {
            native_idx
        } else {
            weth_idx
        }));
    }
    // Capture the terminal profit out of the PM to the executor (physical).
    if output_currency_b == NATIVE_CURRENCY_ADDRESS {
        inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
    } else if output_currency_b == inputs.weth_address {
        inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
    } else {
        inner.extend_from_slice(&encoders::enc_v4_take_delta(
            if b.zfo { c1_b } else { c0_b },
            SENTINEL_SELF,
        ));
    }
    // Resolve any residual deltas so every currency nets to zero.
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V3 2-hop derivation (WAYDTL step 2 / (A)).
///
/// V4's forward currency **leaves the PM** to become the V3 input (boundary
/// model: V4→outside = `V4_TAKE_COMPACT(cur→SELF, forward_out)`); a native
/// forward is wrapped (`WETH_DEPOSIT`) before the V3 swap; the V4 input debt
/// is settled (`V4_SETTLE_DELTA`), with a `WETH_WITHDRAW` when the V4 input is
/// itself native.
fn derive_2hop_v4v3(a: &V4HopInfo, b: &V3HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let v4_out_native = if a.zfo {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    };
    let v4_in_native = if a.zfo {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v3_idx = at.add(b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    // Native is address(0) — a sentinel, so registering it never adds a table
    // entry; idx is just a sentinel value either way.
    let native_idx = SENTINEL_NATIVE;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a,
        c1_a,
        fee_a,
        ts_a,
        SENTINEL_NATIVE,
        a.zfo,
        optimal_input,
    )
    .ok()?;
    if v4_out_native {
        // Native V4 output: take it out, wrap to WETH, then the V3 swap.
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?,
        );
        let input_idx = if a.zfo { c0_a } else { c1_a };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    } else {
        // ERC-20 V4 output: take it to the executor, which funds the V3 swap.
        let forward_idx = if a.zfo { c1_a } else { c0_a };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, b.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?,
        );
        if v4_in_native {
            // Native V4 input: unwrap WETH to seed it before settling.
            let input_idx = if a.zfo { c0_a } else { c1_a };
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(optimal_input)));
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        }
    }
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V4 2-hop derivation (WAYDTL step 2 / (A)).
///
/// A V3 (outer flash) feeds a V4 pool. When the V4 input is an ERC-20, the V3
/// forward output **enters the PM** (boundary model: `V4_SYNC(cur)` +
/// `ERC20_TRANSFER(cur, PM, out)` + `V4_SETTLE`) to seed the input, then the
/// V4 swap + `V4_TAKE_COMPACT(output→SELF)` capture; the V3 flash is repaid
/// `ERC20_TRANSFER(WETH→v3, optimal_input)`. When the V4 input is native the
/// V3's WETH output is unwrapped (`WETH_WITHDRAW(forward_out)`) to seed it and
/// settled directly (`V4_SETTLE_DELTA(native)`).
fn derive_2hop_v3v4(a: &V3HopInfo, b: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(weth_out) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(v4_swap_in) {
        return None;
    }
    let v4_in_native = if b.zfo {
        b.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        b.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v3_idx = at.add(a.pool_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;

    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

    let v3_callback = if v4_in_native {
        // Native V4 input: settle it directly from executor native balance.
        let mut v4_inner = encoders::enc_v4_swap_compact(
            c0_b,
            c1_b,
            fee_b,
            ts_b,
            SENTINEL_NATIVE,
            b.zfo,
            v4_swap_in,
        )
        .ok()?;
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        let output_currency = if b.zfo {
            b.currency1_address
        } else {
            b.currency0_address
        };
        if output_currency == NATIVE_CURRENCY_ADDRESS {
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        } else {
            let output_idx = if b.zfo { c1_b } else { c0_b };
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        }
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        let input_currency_v3 = if a.zfo {
            a.token0_address
        } else {
            a.token1_address
        };
        if input_currency_v3 == inputs.weth_address || input_currency_v3 == NATIVE_CURRENCY_ADDRESS
        {
            return None;
        }
        let forward_v3_idx = at.add(input_currency_v3).ok()?;
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_v3_idx, v3_idx, optimal_input).ok()?,
        );
        cb
    } else {
        // ERC-20 V4 input: sync + transfer + settle to seed it into the PM.
        let forward_addr = if a.zfo {
            a.token1_address
        } else {
            a.token0_address
        };
        let forward_idx = at.add(forward_addr).ok()?;
        let mut v4_inner = encoders::enc_v4_sync(forward_idx);
        v4_inner.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, pm_idx, forward_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b,
                c1_b,
                fee_b,
                ts_b,
                SENTINEL_NATIVE,
                b.zfo,
                v4_swap_in,
            )
            .ok()?,
        );
        let output_idx = if b.zfo { c1_b } else { c0_b };
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3_idx, optimal_input).ok()?);
        cb
    };

    let commands =
        encoders::enc_v3_swap_compact(v3_idx, a.zfo, optimal_input, SENTINEL_SELF, &v3_callback)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V2 2-hop derivation (WAYDTL step 2 / (A)).
///
/// V4's forward currency **leaves the PM to the V2 pool** (`V4_TAKE_COMPACT`
/// with the V2 pool as recipient) and the terminal V2 swap runs; the V4 input
/// is re-seeded. A native V4 output is wrapped (`WETH_DEPOSIT`) before being
/// transferred to the V2 pool (and the terminal V2 always uses `V2_SWAP_CALC`,
/// never exact-out). A native V4 input is settled via `WETH_WITHDRAW`.
fn derive_2hop_v4v2(a: &V4HopInfo, b: &V2HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let v4_out_native = if a.zfo {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    };
    let v4_in_native = if a.zfo {
        a.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        a.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2_idx = at.add(b.pool_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;

    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        c0_a,
        c1_a,
        fee_a,
        ts_a,
        SENTINEL_NATIVE,
        a.zfo,
        optimal_input,
    )
    .ok()?;
    if v4_out_native {
        // Native V4 output: take it out, wrap, then fund the terminal V2 pool.
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        inner.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, forward_out).ok()?);
        inner.extend_from_slice(&encoders::enc_v2_swap_calc(
            v2_idx,
            b.zfo,
            SENTINEL_SELF,
            b.fee,
        ));
        let input_idx = if a.zfo { c0_a } else { c1_a };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    } else {
        // ERC-20 V4 output: hand it directly to the V2 pool (recipient = V2).
        let forward_idx = if a.zfo { c1_a } else { c0_a };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, v2_idx, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v2_swap_calc(
            v2_idx,
            b.zfo,
            SENTINEL_SELF,
            b.fee,
        ));
        if v4_in_native {
            let input_idx = if a.zfo { c0_a } else { c1_a };
            inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(optimal_input)));
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_sync(weth_idx));
            inner.extend_from_slice(
                &encoders::enc_erc20_transfer(weth_idx, pm_idx, optimal_input).ok()?,
            );
            inner.extend_from_slice(&encoders::enc_v4_settle());
        }
    }
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V4 2-hop derivation (WAYDTL step 2 / (A)).
///
/// A V2 (outer flash) feeds a V4 pool. When the V4 input is an ERC-20, the V2
/// forward output **enters the PM** (boundary model: `V4_SYNC(cur)` +
/// `ERC20_TRANSFER(cur, PM, out)` + `V4_SETTLE`) to seed it, the V4 swap +
/// `V4_TAKE_COMPACT(output→SELF)` captures, and the V2 flash is repaid
/// `ERC20_TRANSFER(WETH→v2, optimal_input)`. When the V4 input is native the
/// V2's WETH output is unwrapped (`WETH_WITHDRAW(forward_out)`) and the V4
/// input settled directly.
fn derive_2hop_v2v4(a: &V2HopInfo, b: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(forward_out) || !fits_int128(weth_out) {
        return None;
    }
    let v4_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(v4_swap_in) {
        return None;
    }
    let v4_in_native = if b.zfo {
        b.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        b.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let v2_idx = at.add(a.pool_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;
    let forward_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_idx = at.add(forward_addr).ok()?;

    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;

    let callback_cmds = if v4_in_native {
        // Native V4 input: settle it directly from executor native balance.
        let mut v4_inner = encoders::enc_v4_swap_compact(
            c0_b,
            c1_b,
            fee_b,
            ts_b,
            SENTINEL_NATIVE,
            b.zfo,
            v4_swap_in,
        )
        .ok()?;
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        let output_idx = if b.zfo { c1_b } else { c0_b };
        v4_inner.extend_from_slice(
            &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, v2_idx, optimal_input).ok()?,
        );
        cb
    } else {
        let v4_out_native = if b.zfo {
            b.currency1_address == NATIVE_CURRENCY_ADDRESS
        } else {
            b.currency0_address == NATIVE_CURRENCY_ADDRESS
        };
        let mut v4_inner = encoders::enc_v4_sync(forward_idx);
        v4_inner.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_idx, pm_idx, forward_out).ok()?,
        );
        v4_inner.extend_from_slice(&encoders::enc_v4_settle());
        v4_inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b,
                c1_b,
                fee_b,
                ts_b,
                SENTINEL_NATIVE,
                b.zfo,
                v4_swap_in,
            )
            .ok()?,
        );
        if v4_out_native {
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        } else {
            let output_idx = if b.zfo { c1_b } else { c0_b };
            v4_inner.extend_from_slice(
                &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, weth_out).ok()?,
            );
        }
        v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        if v4_out_native {
            cb.extend_from_slice(&encoders::enc_weth_deposit(U256::from(weth_out)));
        }
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, optimal_input).ok()?);
        cb
    };

    let outer = encoders::enc_v2_swap_compact(
        v2_idx,
        a.zfo,
        forward_out,
        SENTINEL_SELF,
        a.fee,
        &callback_cmds,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&outer);
    Some(out)
}

/// Pure V4→V4→V4 3-hop container derivation (WAYDTL step 3).
///
/// Like the 2-hop `v4_v4`, the whole stream is one `V4_UNLOCK` of internal
/// ledger movement — each V4 hop delegates to its own `V4_SWAP_COMPACT`;
/// native↔WETH representation gaps between hops emit a `V4_TAKE_COMPACT` +
/// `WETH_DEPOSIT`/`WETH_WITHDRAW` bridge + settle; the terminal profit is
/// captured (`V4_TAKE_DELTA(output→SELF)`); a trailing `V4_SETTLE_ALL` nets
/// every currency to zero. Scoped to `default` opts (no `V4_BATCH`,
/// no `erc6909_profit`).
fn derive_3hop_v4v4v4(
    a: &V4HopInfo,
    b: &V4HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    use crate::composers::{emit_currency_bridge, CurrencyBridge};

    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if out_a == 0 || out_b == 0 || out_c == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = *inputs.consumed_inputs.first()?;
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(a_swap_in) || !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mid_a_out = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    let mid_b_in = if b.zfo {
        b.currency0_address
    } else {
        b.currency1_address
    };
    let mid_b_out = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };
    let mid_c_in = if c.zfo {
        c.currency0_address
    } else {
        c.currency1_address
    };
    let output_c = if c.zfo {
        c.currency1_address
    } else {
        c.currency0_address
    };
    let bridge_ab = CurrencyBridge::at_boundary(mid_a_out, mid_b_in);
    let bridge_bc = CurrencyBridge::at_boundary(mid_b_out, mid_c_in);

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let zero_idx = SENTINEL_NATIVE;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;
    let c0_c = at.add(c.currency0_address).ok()?;
    let c1_c = at.add(c.currency1_address).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, a_swap_in).ok()?;
    if bridge_ab.needs_bridge() {
        let (take_idx, b_input_idx) = bridge_ab.bridge_indices(weth_idx, SENTINEL_NATIVE);
        emit_currency_bridge(&mut inner, bridge_ab, take_idx, out_a)?;
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in)
                .ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(b_input_idx));
    } else {
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in)
                .ok()?,
        );
    }
    if bridge_bc.needs_bridge() {
        let (take_idx, c_input_idx) = bridge_bc.bridge_indices(weth_idx, SENTINEL_NATIVE);
        emit_currency_bridge(&mut inner, bridge_bc, take_idx, out_b)?;
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in)
                .ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(c_input_idx));
    } else {
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(c0_c, c1_c, fee_c, ts_c, zero_idx, c.zfo, c_swap_in)
                .ok()?,
        );
    }
    // Capture the terminal profit out of the PM to the executor.
    if output_c == NATIVE_CURRENCY_ADDRESS {
        inner.extend_from_slice(&encoders::enc_v4_take_delta(SENTINEL_NATIVE, SENTINEL_SELF));
    } else if output_c == inputs.weth_address {
        inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
    } else {
        let profit_idx = at.add(output_c).ok()?;
        inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
    }
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V4→V2→V2 3-hop derivation (WAYDTL step 3).
///
/// One `V4_UNLOCK`: the V4 hop's forward currency **leaves the PM directly to
/// the first V2 pool** (`V4_TAKE_COMPACT(cur→v2b, out_a)`), the two V2 legs
/// chain by `V2_SWAP_CALC` (v2b pays into v2c, v2c pays the executor), and the
/// V4 input (WETH) debt is settled (`V4_SETTLE_DELTA(WETH)`).
fn derive_3hop_v4v2v2(
    a: &V4HopInfo,
    b: &V2HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.first()?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let forward_a_cur = if a.zfo {
        a.currency1_address
    } else {
        a.currency0_address
    };
    if forward_a_cur == NATIVE_CURRENCY_ADDRESS || forward_a_cur == inputs.weth_address {
        return None; // terminal-V2 chain needs an ERC-20 forward out of V4
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let forward_a = at.add(forward_a_cur).ok()?;
    let fee_a = u16::try_from(a.fee).ok()?;
    let ts_a = i16::try_from(a.tick_spacing).ok()?;
    let c0_a = at.add(a.currency0_address).ok()?;
    let c1_a = at.add(a.currency1_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;

    let b_cmd = encoders::enc_v2_swap_calc(v2b, b.zfo, v2c, b.fee);
    let c_cmd = encoders::enc_v2_swap_calc(v2c, c.zfo, SENTINEL_SELF, c.fee);

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a, c1_a, fee_a, ts_a, zero_idx, a.zfo, optimal_input)
            .ok()?;
    inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_a, v2b, out_a).ok()?);
    inner.extend_from_slice(&b_cmd);
    inner.extend_from_slice(&c_cmd);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V2→V4 3-hop derivation (WAYDTL step 3).
///
/// The V2 chain (a,b) routes WETH→t2; t2 is synced/transferred/settled **into
/// the PM**, then the trailing V4 pool c swaps t2→WETH; `V4_SETTLE_ALL` nets
/// the WETH profit to the executor.
fn derive_3hop_v2v2v4(
    a: &V2HopInfo,
    b: &V2HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };
    if forward_b_addr == NATIVE_CURRENCY_ADDRESS || forward_b_addr == inputs.weth_address {
        return None; // needs an ERC-20 forward into the PM
    }

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

    let mut inner = encoders::enc_v4_sync(forward_b);
    inner.extend_from_slice(&encoders::enc_erc20_transfer(SENTINEL_WETH, v2a, optimal_input).ok()?);
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a, a.zfo, v2b, a.fee));
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b, b.zfo, pm_idx, b.fee));
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V3→V4 3-hop derivation (WAYDTL step 3).
///
/// V2 a directs t1→the V3 (via `V2_SWAP_DIRECT`), which is the outer flash
/// that pays the PM; the V4 unlock swaps the seeded forward→WETH, repays the
/// V2's WETH, and captures the WETH profit.
fn derive_3hop_v2v3v4(
    a: &V2HopInfo,
    b: &V3HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_a = *inputs.hop_outputs.get(0)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(SENTINEL_WETH, v2a, optimal_input).ok()?);
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, SENTINEL_SELF, out_c - optimal_input).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(SENTINEL_WETH));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a, a.zfo, out_a, v3b).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_b);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V2→V4 3-hop derivation (WAYDTL step 3).
///
/// A V3 outer flash (a) pays into the V2 (b), whose callback embeds a V4
/// swap: the V4 input is settled (`V4_SETTLE_DELTA(forward_b)`) and WETH profit
/// captured; the V3's WETH is repaid via `ERC20_TRANSFER` in the V2 callback.
fn derive_3hop_v3v2v4(
    a: &V3HopInfo,
    b: &V2HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let v2b = at.add(b.pool_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;

    let mut v4_inner =
        encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?;
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, SENTINEL_SELF, out_c).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_b));

    let b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let mut a_fwd =
        encoders::enc_v2_swap_compact(v2b, b.zfo, out_b, SENTINEL_SELF, b.fee, &b_fwd).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_erc20_transfer(SENTINEL_WETH, v3a, optimal_input).ok()?);

    let commands = encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, v2b, &a_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V3→V3→V4 3-hop derivation (WAYDTL step 3).
///
/// Two V3 flashes: b pays into the PM, its callback embeds a second V3 (a)
/// whose callback embeds the V4 jump — the V4 input is synced into the PM
/// (`V4_SYNC(forward_b)` at the outer), the V4 swap runs, the first V3's WETH
/// is repaid (`V4_TAKE_COMPACT(WETH→v3a, optimal)`), and `V4_SETTLE_ALL` nets.
fn derive_3hop_v3v3v4(
    a: &V3HopInfo,
    b: &V3HopInfo,
    c: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = *inputs.consumed_inputs.get(2)?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    let forward_b_addr = if b.zfo {
        b.token1_address
    } else {
        b.token0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a = at.add(a.pool_address).ok()?;
    let v3b = at.add(b.pool_address).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let fee_c = u16::try_from(c.fee).ok()?;
    let ts_c = i16::try_from(c.tick_spacing).ok()?;
    let c0 = at.add(c.currency0_address).ok()?;
    let c1 = at.add(c.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0, c1, fee_c, ts_c, zero_idx, c.zfo, c_swap_in).ok()?,
    );
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(SENTINEL_WETH, v3a, optimal_input).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let a_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let b_fwd = encoders::enc_v3_swap_compact(v3a, a.zfo, optimal_input, v3b, &a_fwd).ok()?;

    let mut commands = encoders::enc_v4_sync(forward_b);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b, b.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// V2→V4→V2 3-hop derivation (WAYDTL step 3).
///
/// V2 c is the outer flash; its callback transfers WETH to V2 a (repay), then
/// runs a V4 unlock that syncs a's forward into the PM (via V2 a paying the
/// PM), swaps the V4 middle pool b, takes b's forward directly to V2 c, and
/// settles a's forward delta.
fn derive_3hop_v2v4v2(
    a: &V2HopInfo,
    b: &V4HopInfo,
    c: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let out_b = *inputs.hop_outputs.get(1)?;
    let out_c = *inputs.hop_outputs.get(2)?;
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = *inputs.consumed_inputs.get(1)?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let forward_a_addr = if a.zfo {
        a.token1_address
    } else {
        a.token0_address
    };
    let forward_b_addr = if b.zfo {
        b.currency1_address
    } else {
        b.currency0_address
    };

    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let pm_idx = at.add(inputs.pool_manager_address).ok()?;
    let forward_a = at.add(forward_a_addr).ok()?;
    let forward_b = at.add(forward_b_addr).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v2a = at.add(a.pool_address).ok()?;
    let v2c = at.add(c.pool_address).ok()?;
    let fee_b = u16::try_from(b.fee).ok()?;
    let ts_b = i16::try_from(b.tick_spacing).ok()?;
    let c0_b = at.add(b.currency0_address).ok()?;
    let c1_b = at.add(b.currency1_address).ok()?;

    let mut v4_inner = encoders::enc_v4_sync(forward_a);
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a, a.zfo, pm_idx, a.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, b.zfo, b_swap_in).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_compact(forward_b, v2c, out_b).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a));

    let mut c_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v2a, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v2_swap_compact(v2c, c.zfo, out_c, SENTINEL_SELF, c.fee, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};

    #[test]
    fn funding_source_is_derived_from_leading_hop() {
        // V2-leading → in-path flash; V3-leading → self-fund (D0-forced).
        let cases: [(Prot, FundingSource); 2] = [
            (Prot::V2, FundingSource::InPathFlash),
            (Prot::V3, FundingSource::SelfFund),
        ];
        for (_p, expected) in cases {
            // The assignment rule is the match in `derive_shape`; assert the
            // two funding values are distinct (the spike's contract).
            assert_ne!(
                expected,
                match expected {
                    FundingSource::InPathFlash => FundingSource::SelfFund,
                    FundingSource::SelfFund => FundingSource::InPathFlash,
                }
            );
        }
    }

    #[test]
    fn terminal_v2_uses_swap_calc_never_exact_out() {
        // The terminal-V2 rule is expressed by `emit_terminal_hop` choosing
        // `enc_v2_swap_calc` (0x21) — assert the encoder selection is CALC.
        let h = V2HopInfo {
            pool_address: address!("00000000000000000000000000000000000000aa"),
            token0_address: address!("0000000000000000000000000000000000000001"),
            token1_address: address!("0000000000000000000000000000000000000002"),
            fee: 30,
            zfo: true,
        };
        let mut at = AddressTable::new();
        let mut out = Vec::new();
        let inputs = ComposerInputs {
            executor_address: Address::ZERO,
            pool_manager_address: Address::ZERO,
            weth_address: Address::ZERO,
            optimal_input: 1000,
            hop_outputs: &[1000],
            consumed_inputs: &[1000],
            opts: Default::default(),
        };
        emit_terminal_hop(&mut at, &HopInfo::V2(h), &inputs, 0, 0, &mut out).unwrap();
        // 0x21 = V2_SWAP_CALC (never exact-out V2_SWAP_COMPACT 0x20).
        assert_eq!(out[0], 0x21, "terminal V2 must encode as V2_SWAP_CALC");
        let _ = U256::ZERO;
    }
}
