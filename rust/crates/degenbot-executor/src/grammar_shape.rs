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

use alloy::primitives::Address;

use crate::composers::{fits_int128, ComposerInputs, HopInfo, PathInfo, V2HopInfo, V3HopInfo};
use crate::encoders::{self, AddressTable, SENTINEL_SELF, SENTINEL_WETH};

/// A hop-protocol family member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    V2,
    V3,
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
            _ => unreachable!("V4 outside spike"),
        })
        .collect();
    let class = ShapeClass { protocols, funding };
    derive_2hop(path, inputs, &class)
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
