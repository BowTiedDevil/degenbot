//! Facet A grammar — generic per-shape-class hop adapters (T2TCJM).
//!
//! Replaces the 8 `encode_cmd_*` + 27 `three_hop_*` bespoke permutation bodies
//! with a small set of *structural* adapters, each parameterized by the hop
//! DEX family so a 4th family (Curve / Balancer / Solidly) becomes ONE additive
//! adapter route — never a combinatorial multiply (ADR-025; spike
//! [`crate::composers`] Facet A write-up).
//!
//! **Byte-identity contract:** every adapter must reproduce [`crate::composers`]
//! output byte-for-byte. This module is validated by
//! [`tests/grammar_parity.rs`](crate) which runs the grammar against the
//! (still-live) bespoke `encode_cmd_stream` over every family combo × config
//! variant and asserts equality.
//!
//! The CL-clamp swap-in rule (`V2 → full output; CL → consumed_inputs[i]` +
//! `fits_int128`) is resolved at ONE shared point: [`cl_swap_in`].
#![expect(clippy::similar_names, clippy::too_many_lines)] // canonical a/b/c hop-slot + cX_idx names; long V4 settlement adapters

use crate::composers::{
    fits_int128, ComposerInputs, CurrencyBridge, HopInfo, PathInfo, V2HopInfo, V3HopInfo,
    V4HopInfo, NATIVE_CURRENCY_ADDRESS,
};
use crate::encoders::{
    self, AddressTable, V4BatchEntry, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH,
};
use alloy::primitives::U256;

/// The ONE shared CL-clamp swap-in resolution point (ADR-025).
///
/// A concentrated-liquidity (V3/V4) hop's executable swap-in is the solver's
/// clamped `consumed_inputs[i]`; a V2/Curve/Balancer/Solidly hop (no clamp)
/// consumes its full prior output. Gated by the int128 guard.
fn cl_swap_in(inputs: &ComposerInputs<'_>, i: usize) -> Option<u128> {
    let v = *inputs.consumed_inputs.get(i)?;
    if !fits_int128(v) {
        return None;
    }
    Some(v)
}

/// The forward (output) currency of a V2 hop.
fn v2_forward_addr(h: &V2HopInfo) -> alloy::primitives::Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}

/// The forward (output) currency of a V3 hop.
fn v3_forward_addr(h: &V3HopInfo) -> alloy::primitives::Address {
    if h.zfo {
        h.token1_address
    } else {
        h.token0_address
    }
}

/// Generic all-V2 N-hop walk (the repo's own "speedrail"), reproduced
/// byte-identically from `encode_cmd_v2_n_hop`.
fn all_v2_walk(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let v2_hops: Vec<&V2HopInfo> = path
        .hops
        .iter()
        .map(|h| match h {
            HopInfo::V2(h) => Some(h),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let num_hops = v2_hops.len();
    if num_hops < 2 || inputs.hop_outputs.contains(&0) {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let pool_indices: Vec<u8> = v2_hops
        .iter()
        .map(|h| at.add(h.pool_address).ok())
        .collect::<Option<Vec<u8>>>()?;
    let hop_a = v2_hops[0];
    let forward_idx = at.add(v2_forward_addr(hop_a)).ok()?;
    let hop_last = v2_hops[num_hops - 1];
    let weth_addr = if hop_last.zfo {
        hop_last.token1_address
    } else {
        hop_last.token0_address
    };
    let weth_idx = at.add(weth_addr).ok()?;

    let mut callback =
        encoders::enc_erc20_transfer(forward_idx, pool_indices[1], inputs.hop_outputs[0]).ok()?;
    for i in 1..num_hops {
        let hop = v2_hops[i];
        let recipient_idx = if i < num_hops - 1 {
            pool_indices[i + 1]
        } else {
            SENTINEL_SELF
        };
        callback.extend_from_slice(&encoders::enc_v2_swap_calc(
            pool_indices[i],
            hop.zfo,
            recipient_idx,
            hop.fee,
        ));
    }
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, pool_indices[0], inputs.optimal_input).ok()?,
    );
    let commands = encoders::enc_v2_swap_compact(
        pool_indices[0],
        hop_a.zfo,
        inputs.hop_outputs[0],
        SENTINEL_SELF,
        hop_a.fee,
        &callback,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// The all-V2 N-hop **speedrail** (matching `encode_cmd_stream`'s routing of
/// any all-V2 path to the generic flash-borrow + chained `V2_SWAP_CALC` walk,
/// regardless of arity). `encode_cmd_3_hop` uses [`encode_grammar`] instead,
/// which for an all-V2 **3-hop** path emits the distinct `v2_v2_v2` layout.
#[must_use]
pub fn encode_all_v2(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    all_v2_walk(path, inputs)
}

/// Adapt an all-V2 **3-hop** path to the `three_hop_v2_v2_v2` layout (the
/// 3-hop entry, structurally distinct from the N-hop speedrail).
fn v2_v2_v2(
    ha: &V2HopInfo,
    hb: &V2HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let out_c = hop_outputs[2];
    if hop_outputs.contains(&0) {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;

    let mut c_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2a_idx, ha.zfo, v2b_idx, ha.fee,
    ));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b_idx, hb.zfo, v2c_idx, hb.fee,
    ));

    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, SENTINEL_SELF, hc.fee, &c_fwd)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v2_v3(ha: &V2HopInfo, hb: &V3HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let forward_out = *inputs.hop_outputs.first()?;
    if forward_out == 0 || *inputs.hop_outputs.get(1)? == 0 {
        return None;
    }
    let v3_swap_in = cl_swap_in(inputs, 1)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2_idx = at.add(ha.pool_address).ok()?;
    let v3_idx = at.add(hb.pool_address).ok()?;
    let forward_idx = at.add(v2_forward_addr(ha)).ok()?;

    let v3_callback_cmds = encoders::enc_erc20_transfer(forward_idx, v3_idx, v3_swap_in).ok()?;
    let mut callback_cmds =
        encoders::enc_v3_swap_compact(v3_idx, hb.zfo, v3_swap_in, SENTINEL_SELF, &v3_callback_cmds)
            .ok()?;
    callback_cmds.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v2_idx, inputs.optimal_input).ok()?,
    );
    let commands = encoders::enc_v2_swap_compact(
        v2_idx,
        ha.zfo,
        forward_out,
        SENTINEL_SELF,
        ha.fee,
        &callback_cmds,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// Adapt a V3→V2 2-hop path.
fn v3_v2(ha: &V3HopInfo, hb: &V2HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v3_idx = at.add(ha.pool_address).ok()?;
    let v2_idx = at.add(hb.pool_address).ok()?;
    let forward_idx = at.add(v3_forward_addr(ha)).ok()?;

    let mut v3_callback =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v3_idx, inputs.optimal_input).ok()?;
    v3_callback
        .extend_from_slice(&encoders::enc_erc20_transfer(forward_idx, v2_idx, forward_out).ok()?);
    let v2_cmd =
        encoders::enc_v2_swap_compact(v2_idx, hb.zfo, weth_out, SENTINEL_SELF, hb.fee, &[]).ok()?;
    v3_callback.extend_from_slice(&v2_cmd);
    let commands = encoders::enc_v3_swap_compact(
        v3_idx,
        ha.zfo,
        inputs.optimal_input,
        SENTINEL_SELF,
        &v3_callback,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// Adapt a V3→V3 2-hop path.
fn v3_v3(ha: &V3HopInfo, hb: &V3HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let forward_out = *inputs.hop_outputs.first()?;
    let weth_out = *inputs.hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v3_a_idx = at.add(ha.pool_address).ok()?;
    let v3_b_idx = at.add(hb.pool_address).ok()?;

    let mut v3_a_callback =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v3_a_idx, inputs.optimal_input).ok()?;
    let v3_b_cmd =
        encoders::enc_v3_swap_compact(v3_b_idx, hb.zfo, b_swap_in, SENTINEL_SELF, &[]).ok()?;
    v3_a_callback.extend_from_slice(&v3_b_cmd);
    let commands = encoders::enc_v3_swap_compact(
        v3_a_idx,
        ha.zfo,
        inputs.optimal_input,
        SENTINEL_SELF,
        &v3_a_callback,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

/// The generic 2/3-hop dispatcher. Routes to per-shape-class adapters; the
/// V4-involving and remaining 3-hop classes are the quantified Facet A residual
/// (added as more adapters — see `spike_grammar_gap_report.md`).
#[must_use]
pub fn encode_grammar(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let hops = &path.hops;
    // Only 2-hop and 3-hop paths are dispatched here (
    // `encode_cmd_stream` routes all-V2 any-N to [`encode_all_v2`] first).
    if hops.len() != 2 && hops.len() != 3 {
        return None;
    }
    match (hops.first(), hops.get(1), hops.get(2)) {
        (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) => {
            v2_v2_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), None) => v2_v3(a, b, inputs),
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), None) => v3_v2(a, b, inputs),
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), None) => v3_v3(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), None) => v4_v4(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), None) => v4_v3(a, b, inputs),
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), None) => v3_v4(a, b, inputs),
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), None) => v4_v2(a, b, inputs),
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), None) => v2_v4(a, b, inputs),
        (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            v2_v2_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            v2_v3_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            v2_v4_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V3(c))) => {
            v2_v4_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            v2_v4_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            v3_v2_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            v3_v3_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            v3_v4_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), Some(HopInfo::V3(c))) => {
            v3_v4_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            v3_v4_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) => {
            v4_v2_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V3(c))) => {
            v4_v2_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V2(b)), Some(HopInfo::V4(c))) => {
            v4_v2_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), Some(HopInfo::V2(c))) => {
            v4_v3_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), Some(HopInfo::V3(c))) => {
            v4_v3_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V3(b)), Some(HopInfo::V4(c))) => {
            v4_v3_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V2(c))) => {
            v4_v4_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V3(c))) => {
            v4_v4_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V4(a)), Some(HopInfo::V4(b)), Some(HopInfo::V4(c))) => {
            v4_v4_v4(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V3(c))) => {
            v2_v2_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V2(c))) => {
            v2_v3_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V2(a)), Some(HopInfo::V3(b)), Some(HopInfo::V3(c))) => {
            v2_v3_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) => {
            v3_v2_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V2(b)), Some(HopInfo::V3(c))) => {
            v3_v2_v3(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V2(c))) => {
            v3_v3_v2(a, b, c, inputs)
        }
        (Some(HopInfo::V3(a)), Some(HopInfo::V3(b)), Some(HopInfo::V3(c))) => {
            v3_v3_v3(a, b, c, inputs)
        }
        _ => None, // residual: V4-involving combos
    }
}

// ── V2/V3-only 3-hop chained adapters (byte-transcribed from the bespoke) ──

fn v2_v2_v3(
    ha: &V2HopInfo,
    hb: &V2HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let mut c_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, inputs.optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2a_idx, ha.zfo, v2b_idx, ha.fee,
    ));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2b_idx, hb.zfo, v3c_idx, hb.fee,
    ));

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v2_v3_v2(
    ha: &V2HopInfo,
    hb: &V3HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let out_a = inputs.hop_outputs[0];
    let out_c = inputs.hop_outputs[2];
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    // Register forward token of A (discarded — affects table index order).
    at.add(v2_forward_addr(ha)).ok()?;
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;

    let mut b_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, inputs.optimal_input).ok()?;
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx).ok()?);
    let c_fwd = encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v2c_idx, &b_fwd).ok()?;
    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, SENTINEL_SELF, hc.fee, &c_fwd)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v2_v3_v3(
    ha: &V2HopInfo,
    hb: &V3HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let out_a = inputs.hop_outputs[0];
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2a_idx = at.add(ha.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let mut v3b_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, inputs.optimal_input).ok()?;
    v3b_fwd.extend_from_slice(&encoders::enc_v2_swap_direct(v2a_idx, ha.zfo, out_a, v3b_idx).ok()?);
    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v3c_idx, &v3b_fwd).ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &v3c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v2_v2(
    ha: &V3HopInfo,
    hb: &V2HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let out_b = inputs.hop_outputs[1];
    let out_c = inputs.hop_outputs[2];
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v2c_idx = at.add(hc.pool_address).ok()?;
    let v3a_idx = at.add(ha.pool_address).ok()?;

    let mut a_fwd = encoders::enc_v2_swap_direct(v2b_idx, hb.zfo, out_b, v2c_idx).ok()?;
    a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, SENTINEL_SELF).ok()?,
    );
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, inputs.optimal_input).ok()?,
    );
    let commands =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, inputs.optimal_input, v2b_idx, &a_fwd)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v2_v3(
    ha: &V3HopInfo,
    hb: &V2HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2b_idx = at.add(hb.pool_address).ok()?;
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let mut v3a_fwd = encoders::enc_v2_swap_calc(v2b_idx, hb.zfo, v3c_idx, hb.fee);
    v3a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, inputs.optimal_input).ok()?,
    );
    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, inputs.optimal_input, v2b_idx, &v3a_fwd)
            .ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &v3c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v3_v2(
    ha: &V3HopInfo,
    hb: &V3HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let out_c = inputs.hop_outputs[2];
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    if !fits_int128(inputs.optimal_input) {
        return None;
    }
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v2c_idx = at.add(hc.pool_address).ok()?;
    let v3a_idx = at.add(ha.pool_address).ok()?;

    let mut v3a_fwd = encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, SENTINEL_SELF).ok()?;
    v3a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, inputs.optimal_input).ok()?,
    );
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3b_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, inputs.optimal_input, v3b_idx, &v3a_fwd)
            .ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v2c_idx, &v3b_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v3_v3(
    ha: &V3HopInfo,
    hb: &V3HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    if inputs.hop_outputs.contains(&0) {
        return None;
    }
    let b_swap_in = cl_swap_in(inputs, 1)?;
    let c_swap_in = cl_swap_in(inputs, 2)?;
    let mut at = AddressTable::with_sentinels(
        Some(inputs.weth_address),
        Some(inputs.executor_address),
        None,
    );
    let v3a_idx = at.add(ha.pool_address).ok()?;
    let v3b_idx = at.add(hb.pool_address).ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let v3a_callback: Vec<u8> = Vec::new();
    let v3b_callback = encoders::enc_v3_swap_compact(
        v3a_idx,
        ha.zfo,
        inputs.optimal_input,
        v3b_idx,
        &v3a_callback,
    )
    .ok()?;
    let v3c_callback =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v3c_idx, &v3b_callback).ok()?;
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &v3c_callback)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── 2-hop V4-involving adapters (byte-transcribed from bespoke) ────────────

fn v4_v4(ha: &V4HopInfo, hb: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) || !fits_int128(forward_out) {
        return None;
    }
    let b_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mid_currency_a = if ha.zfo {
        ha.currency1_address
    } else {
        ha.currency0_address
    };
    let mid_currency_b = if hb.zfo {
        hb.currency0_address
    } else {
        hb.currency1_address
    };
    let input_currency_a = if ha.zfo {
        ha.currency0_address
    } else {
        ha.currency1_address
    };
    let output_currency_b = if hb.zfo {
        hb.currency1_address
    } else {
        hb.currency0_address
    };

    let a_outputs_native = mid_currency_a == NATIVE_CURRENCY_ADDRESS;
    let b_needs_native = mid_currency_b == NATIVE_CURRENCY_ADDRESS;
    let bridge = CurrencyBridge::at_boundary(mid_currency_a, mid_currency_b);
    let currency_gap = bridge.needs_bridge();

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;
    let weth_idx = SENTINEL_WETH;

    let mut native_idx: u8 = SENTINEL_NATIVE;
    if a_outputs_native
        || b_needs_native
        || input_currency_a == NATIVE_CURRENCY_ADDRESS
        || output_currency_b == NATIVE_CURRENCY_ADDRESS
    {
        native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
    }

    let a_fee = u16::try_from(ha.fee).ok()?;
    let a_ts = i16::try_from(ha.tick_spacing).ok()?;
    let b_fee = u16::try_from(hb.fee).ok()?;
    let b_ts = i16::try_from(hb.tick_spacing).ok()?;

    let mut inner: Vec<u8> = if !inputs.opts.use_v4_batch || currency_gap {
        encoders::enc_v4_swap_compact(
            c0_a_idx,
            c1_a_idx,
            a_fee,
            a_ts,
            zero_idx,
            ha.zfo,
            optimal_input,
        )
        .ok()?
    } else {
        Vec::new()
    };

    if currency_gap {
        let bridge_idx = match bridge {
            CurrencyBridge::Wrap => native_idx,
            CurrencyBridge::Unwrap => weth_idx,
            CurrencyBridge::None => unreachable!("currency_gap implies a bridge"),
        };
        crate::composers::emit_currency_bridge(&mut inner, bridge, bridge_idx, forward_out)?;
        inner.extend_from_slice(
            &encoders::enc_v4_swap_compact(
                c0_b_idx, c1_b_idx, b_fee, b_ts, zero_idx, hb.zfo, b_swap_in,
            )
            .ok()?,
        );
        if b_needs_native {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
        } else {
            inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
        }
        if output_currency_b == NATIVE_CURRENCY_ADDRESS {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
        } else if output_currency_b == weth_address {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
        } else {
            let profit_idx = if hb.zfo { c1_b_idx } else { c0_b_idx };
            inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
        }
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    } else {
        if inputs.opts.use_v4_batch {
            let batch = [
                V4BatchEntry {
                    c0_idx: c0_a_idx,
                    c1_idx: c1_a_idx,
                    fee: a_fee,
                    tick_spacing: a_ts,
                    hooks_idx: zero_idx,
                    zfo: ha.zfo,
                    amount_u96: optimal_input,
                },
                V4BatchEntry {
                    c0_idx: c0_b_idx,
                    c1_idx: c1_b_idx,
                    fee: b_fee,
                    tick_spacing: b_ts,
                    hooks_idx: zero_idx,
                    zfo: hb.zfo,
                    amount_u96: b_swap_in,
                },
            ];
            inner.extend_from_slice(&encoders::enc_v4_batch(&batch).ok()?);
            if output_currency_b != NATIVE_CURRENCY_ADDRESS && output_currency_b != weth_address {
                let profit_idx = if hb.zfo { c1_b_idx } else { c0_b_idx };
                inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
            }
        } else {
            inner.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_b_idx, c1_b_idx, b_fee, b_ts, zero_idx, hb.zfo, b_swap_in,
                )
                .ok()?,
            );
        }
        if inputs.opts.erc6909_profit && output_currency_b == weth_address {
            let profit_amount = weth_out.saturating_sub(optimal_input);
            if profit_amount > 0 {
                inner.extend_from_slice(
                    &encoders::enc_v4_mint_compact(weth_idx, SENTINEL_SELF, profit_amount).ok()?,
                );
            }
        } else if !inputs.opts.use_v4_batch
            || (output_currency_b != NATIVE_CURRENCY_ADDRESS && output_currency_b != weth_address)
        {
            if output_currency_b == NATIVE_CURRENCY_ADDRESS {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
            } else if output_currency_b == weth_address {
                inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
            } else {
                let profit_idx = if hb.zfo { c1_b_idx } else { c0_b_idx };
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

fn v4_v3(hop_v4: &V4HopInfo, hop_v3: &V3HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
    let weth_address = inputs.weth_address;

    let forward_out = *hop_outputs.first()?;
    let weth_out = *hop_outputs.get(1)?;
    if forward_out == 0 || weth_out == 0 {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let v3_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v3_swap_in) {
        return None;
    }

    let v4_out_native = if hop_v4.zfo {
        hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
    } else {
        hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
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
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, hop_v3.zfo, v3_swap_in, SENTINEL_SELF, &[])
                .ok()?,
        );
        let input_idx = if hop_v4.zfo { c0_v4_idx } else { c1_v4_idx };
        inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
        inner.extend_from_slice(&encoders::enc_v4_settle_all());
    } else {
        let forward_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(
            &encoders::enc_v3_swap_compact(v3_idx, hop_v3.zfo, v3_swap_in, SENTINEL_SELF, &[])
                .ok()?,
        );
        let v4_in_native = if hop_v4.zfo {
            hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
        } else {
            hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
        };
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

fn v3_v4(hop_v3: &V3HopInfo, hop_v4: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
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
    let v4_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v4_swap_in) {
        return None;
    }

    let v4_in_native = if hop_v4.zfo {
        hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
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

        let mut cb = encoders::enc_weth_withdraw(U256::from(forward_out));
        cb.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);
        let input_currency_v3 = if hop_v3.zfo {
            hop_v3.token0_address
        } else {
            hop_v3.token1_address
        };
        if input_currency_v3 == weth_address || input_currency_v3 == NATIVE_CURRENCY_ADDRESS {
            return None;
        }
        let forward_v3_idx = at.add(input_currency_v3).ok()?;
        cb.extend_from_slice(
            &encoders::enc_erc20_transfer(forward_v3_idx, v3_idx, optimal_input).ok()?,
        );
        cb
    } else {
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

        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v3_idx, optimal_input).ok()?);
        cb
    };

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

fn v4_v2(hop_v4: &V4HopInfo, hop_v2: &V2HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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

    let v4_out_native = if hop_v4.zfo {
        hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
    } else {
        hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
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
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(forward_out)));
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
        let forward_idx = if hop_v4.zfo { c1_v4_idx } else { c0_v4_idx };
        inner.extend_from_slice(
            &encoders::enc_v4_take_compact(forward_idx, v2_idx, forward_out).ok()?,
        );
        inner.extend_from_slice(&encoders::enc_v2_swap_calc(
            v2_idx,
            hop_v2.zfo,
            SENTINEL_SELF,
            hop_v2.fee,
        ));
        let v4_in_native = if hop_v4.zfo {
            hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
        } else {
            hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
        };
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

fn v2_v4(hop_v2: &V2HopInfo, hop_v4: &V4HopInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let consumed_inputs = inputs.consumed_inputs;
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
    let v4_swap_in = consumed_inputs.get(1).copied()?;
    if !fits_int128(v4_swap_in) {
        return None;
    }

    let v4_in_native = if hop_v4.zfo {
        hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
    } else {
        hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
    };

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
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
        let v4_out_native = if hop_v4.zfo {
            hop_v4.currency1_address == NATIVE_CURRENCY_ADDRESS
        } else {
            hop_v4.currency0_address == NATIVE_CURRENCY_ADDRESS
        };

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

        let mut cb = encoders::enc_v4_unlock(&v4_inner).ok()?;
        if v4_out_native {
            cb.extend_from_slice(&encoders::enc_weth_deposit(U256::from(weth_out)));
        }
        cb.extend_from_slice(&encoders::enc_erc20_transfer(weth_idx, v2_idx, optimal_input).ok()?);
        callback_cmds = cb;
    }

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

// ── 3-hop V2-leading V4-involving adapters ─────────────────────────────────

fn v2_v2_v4(
    ha: &V2HopInfo,
    hb: &V2HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v2a_idx, optimal_input).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2a_idx, ha.zfo, v2b_idx, ha.fee,
    ));
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, hb.zfo, pm_idx, hb.fee));
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_idx, c1_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in)
            .ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v2_v3_v4(
    ha: &V2HopInfo,
    hb: &V3HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v2a_idx, optimal_input).ok()?,
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, SENTINEL_SELF, out_c - optimal_input).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(SENTINEL_WETH));
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

fn v2_v4_v2(
    ha: &V2HopInfo,
    hb: &V4HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a_idx));

    let mut c_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, hc.zfo, out_c, SENTINEL_SELF, hc.fee, &c_fwd)
            .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v2_v4_v3(
    ha: &V2HopInfo,
    hb: &V4HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    let v4_swap_in = inputs.consumed_inputs.get(1).copied()?;
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, c_swap_in).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a_idx));

    let mut c_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, optimal_input).ok()?;
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &c_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v2_v4_v4(
    ha: &V2HopInfo,
    hb: &V4HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v2a_idx, optimal_input).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, ha.zfo, pm_idx, ha.fee));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&v4_inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── 3-hop V3-leading V4-involving adapters ─────────────────────────────────

fn v3_v2_v4(
    ha: &V3HopInfo,
    hb: &V2HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, SENTINEL_SELF, out_c).ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_b_idx));

    let b_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;

    let mut a_fwd =
        encoders::enc_v2_swap_compact(v2b_idx, hb.zfo, out_b, SENTINEL_SELF, hb.fee, &b_fwd)
            .ok()?;
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, optimal_input).ok()?,
    );

    let commands =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, v2b_idx, &a_fwd).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v3_v4(
    ha: &V3HopInfo,
    hb: &V3HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v3a_idx, optimal_input).ok()?,
    );
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

fn v3_v4_v2(
    ha: &V3HopInfo,
    hb: &V4HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(forward_b_idx, v2c_idx));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_v4_unlock(&v4_inner).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(
        v2c_idx,
        hc.zfo,
        SENTINEL_SELF,
        hc.fee,
    ));
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, optimal_input).ok()?,
    );

    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, &a_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v4_v3(
    ha: &V3HopInfo,
    hb: &V4HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
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
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
        )
        .ok()?,
    );
    v4_inner
        .extend_from_slice(&encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, c_swap_in).ok()?);
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, optimal_input).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let c_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, &a_fwd).ok()?;

    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &c_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v3_v4_v4(
    ha: &V3HopInfo,
    hb: &V4HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let pm_idx = at.add(pool_manager_address).ok()?;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at.add(ha.pool_address).ok()?;

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

    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
        )
        .ok()?,
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(SENTINEL_WETH, SENTINEL_SELF));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let mut a_fwd = encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, optimal_input).ok()?;
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).ok()?);

    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3a_idx, ha.zfo, optimal_input, pm_idx, &a_fwd).ok()?,
    );
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── 3-hop V4-leading adapters ──────────────────────────────────────────────

fn v4_v2_v2(
    ha: &V4HopInfo,
    hb: &V2HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
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
    let c_cmd = encoders::enc_v2_swap_calc(v2c_idx, hc.zfo, SENTINEL_SELF, hc.fee);

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
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v4_v2_v3(
    ha: &V4HopInfo,
    hb: &V2HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
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
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        hc.zfo,
        c_swap_in,
        SENTINEL_SELF,
        &encoders::enc_v4_unlock(&v4_inner).ok()?,
    )
    .ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v4_v2_v4(
    ha: &V4HopInfo,
    hb: &V2HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
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
        SENTINEL_SELF,
        hb.fee,
    ));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
        )
        .ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v4_v3_v2(
    ha: &V4HopInfo,
    hb: &V3HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
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
        &encoders::enc_v2_swap_direct(v2c_idx, hc.zfo, out_c, SENTINEL_SELF).ok()?,
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
        &encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v2c_idx, &b_fwd).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v4_v3_v3(
    ha: &V4HopInfo,
    hb: &V3HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    at.add(pool_manager_address).ok()?;
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

    let inner_v3b =
        encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, v3c_idx, &b_fwd).ok()?;
    let inner_v3c =
        encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &inner_v3b)
            .ok()?;

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
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v4_v3_v4(
    ha: &V4HopInfo,
    hb: &V3HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    let out_a = hop_outputs[0];
    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
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
        &encoders::enc_v3_swap_compact(v3b_idx, hb.zfo, b_swap_in, pm_idx, &b_fwd).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
        )
        .ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

// ── 3-hop V4-terminal V4-V4-X adapters ─────────────────────────────────────

fn v4_v4_v2(
    ha: &V4HopInfo,
    hb: &V4HopInfo,
    hc: &V2HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
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
    let a_swap_in = inputs.consumed_inputs.first().copied()?;
    if !fits_int128(a_swap_in) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
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
        encoders::enc_v2_swap_direct(at.add(hc.pool_address).ok()?, hc.zfo, out_c, SENTINEL_SELF)
            .ok()?;

    let mut inner = encoders::enc_v4_swap_compact(
        at.add(ha.currency0_address).ok()?,
        at.add(ha.currency1_address).ok()?,
        fee_a,
        ts_a,
        zero_idx,
        ha.zfo,
        a_swap_in,
    )
    .ok()?;
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            at.add(hb.currency0_address).ok()?,
            at.add(hb.currency1_address).ok()?,
            fee_b,
            ts_b,
            zero_idx,
            hb.zfo,
            b_swap_in,
        )
        .ok()?,
    );
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

fn v4_v4_v3(
    ha: &V4HopInfo,
    hb: &V4HopInfo,
    hc: &V3HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let pool_manager_address = inputs.pool_manager_address;
    let weth_address = inputs.weth_address;

    if hop_outputs.contains(&0) {
        return None;
    }
    if !fits_int128(optimal_input) {
        return None;
    }
    let a_swap_in = inputs.consumed_inputs.first().copied()?;
    if !fits_int128(a_swap_in) {
        return None;
    }
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    if !fits_int128(b_swap_in) {
        return None;
    }
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(c_swap_in) {
        return None;
    }

    let mut at = AddressTable::with_sentinels(
        Some(weth_address),
        Some(inputs.executor_address),
        Some(pool_manager_address),
    );
    let zero_idx = SENTINEL_NATIVE;
    let forward_b_idx = at
        .add(if hb.zfo {
            hb.currency1_address
        } else {
            hb.currency0_address
        })
        .ok()?;
    let v3c_idx = at.add(hc.pool_address).ok()?;

    let c_take = encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, c_swap_in).ok()?;

    let fee_a = u16::try_from(ha.fee).ok()?;
    let ts_a = i16::try_from(ha.tick_spacing).ok()?;
    let fee_b = u16::try_from(hb.fee).ok()?;
    let ts_b = i16::try_from(hb.tick_spacing).ok()?;

    let c0_a_idx = at.add(ha.currency0_address).ok()?;
    let c1_a_idx = at.add(ha.currency1_address).ok()?;
    let c0_b_idx = at.add(hb.currency0_address).ok()?;
    let c1_b_idx = at.add(hb.currency1_address).ok()?;

    let mut inner =
        encoders::enc_v4_swap_compact(c0_a_idx, c1_a_idx, fee_a, ts_a, zero_idx, ha.zfo, a_swap_in)
            .ok()?;
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
        )
        .ok()?,
    );
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3c_idx, hc.zfo, c_swap_in, SENTINEL_SELF, &c_take).ok()?,
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}

fn v4_v4_v4(
    ha: &V4HopInfo,
    hb: &V4HopInfo,
    hc: &V4HopInfo,
    inputs: &ComposerInputs<'_>,
) -> Option<Vec<u8>> {
    let optimal_input = inputs.optimal_input;
    let hop_outputs = inputs.hop_outputs;
    let weth_address = inputs.weth_address;

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
    let a_swap_in = inputs.consumed_inputs.first().copied()?;
    let b_swap_in = inputs.consumed_inputs.get(1).copied()?;
    let c_swap_in = inputs.consumed_inputs.get(2).copied()?;
    if !fits_int128(a_swap_in) || !fits_int128(b_swap_in) || !fits_int128(c_swap_in) {
        return None;
    }

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
        Some(inputs.executor_address),
        Some(inputs.pool_manager_address),
    );
    let weth_idx = SENTINEL_WETH;
    let zero_idx = SENTINEL_NATIVE;

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

    let mut inner = if inputs.opts.use_v4_batch && !any_gap {
        let batch = [
            V4BatchEntry {
                c0_idx: c0_a_idx,
                c1_idx: c1_a_idx,
                fee: fee_a,
                tick_spacing: ts_a,
                hooks_idx: zero_idx,
                zfo: ha.zfo,
                amount_u96: a_swap_in,
            },
            V4BatchEntry {
                c0_idx: c0_b_idx,
                c1_idx: c1_b_idx,
                fee: fee_b,
                tick_spacing: ts_b,
                hooks_idx: zero_idx,
                zfo: hb.zfo,
                amount_u96: b_swap_in,
            },
            V4BatchEntry {
                c0_idx: c0_c_idx,
                c1_idx: c1_c_idx,
                fee: fee_c,
                tick_spacing: ts_c,
                hooks_idx: zero_idx,
                zfo: hc.zfo,
                amount_u96: c_swap_in,
            },
        ];
        let mut v = encoders::enc_v4_batch(&batch).ok()?;
        if output_currency_c != NATIVE_CURRENCY_ADDRESS && output_currency_c != weth_address {
            let profit_idx = at.add(output_currency_c).ok()?;
            v.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
        }
        v
    } else {
        let mut v = encoders::enc_v4_swap_compact(
            c0_a_idx, c1_a_idx, fee_a, ts_a, zero_idx, ha.zfo, a_swap_in,
        )
        .ok()?;
        if bridge_ab.needs_bridge() {
            let (take_idx, b_input_idx) = bridge_ab.bridge_indices(weth_idx, SENTINEL_NATIVE);
            crate::composers::emit_currency_bridge(&mut v, bridge_ab, take_idx, out_a)?;
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
                )
                .ok()?,
            );
            v.extend_from_slice(&encoders::enc_v4_settle_delta(b_input_idx));
        } else {
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_b_idx, c1_b_idx, fee_b, ts_b, zero_idx, hb.zfo, b_swap_in,
                )
                .ok()?,
            );
        }
        if bridge_bc.needs_bridge() {
            let (take_idx, c_input_idx) = bridge_bc.bridge_indices(weth_idx, SENTINEL_NATIVE);
            crate::composers::emit_currency_bridge(&mut v, bridge_bc, take_idx, out_b)?;
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
                )
                .ok()?,
            );
            v.extend_from_slice(&encoders::enc_v4_settle_delta(c_input_idx));
        } else {
            v.extend_from_slice(
                &encoders::enc_v4_swap_compact(
                    c0_c_idx, c1_c_idx, fee_c, ts_c, zero_idx, hc.zfo, c_swap_in,
                )
                .ok()?,
            );
        }
        v
    };

    if inputs.opts.erc6909_profit && output_currency_c == weth_address {
        let profit_amount = out_c - optimal_input;
        if profit_amount > 0 {
            inner.extend_from_slice(
                &encoders::enc_v4_mint_compact(weth_idx, SENTINEL_SELF, profit_amount).ok()?,
            );
        }
    } else if !inputs.opts.use_v4_batch || any_gap {
        if output_currency_c == NATIVE_CURRENCY_ADDRESS {
            let native_idx = at.add(NATIVE_CURRENCY_ADDRESS).ok()?;
            inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, SENTINEL_SELF));
        } else if output_currency_c == weth_address {
            inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, SENTINEL_SELF));
        } else {
            let profit_idx = at.add(output_currency_c).ok()?;
            inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, SENTINEL_SELF));
        }
    }

    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).ok()?;
    let mut out = encoders::enc_preamble(&at);
    out.extend_from_slice(&commands);
    Some(out)
}
