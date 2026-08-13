//! Facet A grammar — the production command-stream encoder (ADR-025 / ADR-029).
//!
//! `encode_grammar` is the **sole** production encoder for every 2/3-hop path
//! (the all-V2 any-N family routes through
//! [`grammar_shape::derive_all_v2`][crate::grammar_shape::derive_all_v2] — the
//! Plan+validator path — reached first by `encode_cmd_stream`). Production
//! delegates byte-emission to
//! [`grammar_shape::derive_shape`][crate::grammar_shape::derive_shape] — the
//! per-shape-class deriver — for every family it handles, with **no hand-written
//! backstop**: a family either derives or it does not encode. The ~32
//! hand-written adapter fns and their `cutover` parity-oracle were retired in
//! WAYDTL/RVNIPD; byte-parity is now pinned by the revm runtime matrix
//! (`degenbot-simulation` full_matrix, exact delta), the primitive wire-format
//! layer (`tests/encoders_parity.rs`), and the native bridge byte-golden
//! (`tests/native_eth_3hop_bridge.rs`).
//!
//! **KO5NNB (epic N4TJSZ) cutover:** the all-V2 family (2-hop, 3-hop, any-N)
//! now routes through `build_all_v2_chain` + the `LedgerValidator` gate — D4's
//! "the validator gates the Plan for every family" is literal for all-V2, and
//! the terminal-V2 exact-draw invariant is enforced on the streams the bot
//! actually ships. The hand-written emitters [`encode_all_v2`] (the N-hop
//! speedrail), `all_v2_walk`, and [`v2_v2_v2`] (the former all-V2 3-hop distinct
//! layout, now COLLAPSED to the N-hop speedrail/Plan layout) are **orphaned**
//! (no production caller) but retained pending the T3 deletion task.
//!
//! The CL-clamp swap-in rule (`V2 → full output; CL → consumed_inputs[i]` +
//! `fits_int128`) is applied directly in the retained emitters/builders.
#![expect(clippy::similar_names)] // canonical v2a/v2b/v2c hop-slot names in the retained v2_v2_v2 emitter

use crate::composers::{ComposerInputs, HopInfo, PathInfo, V2HopInfo};
use crate::encoders::{self, AddressTable, SENTINEL_SELF, SENTINEL_WETH};

/// The forward (output) currency of a V2 hop.
pub(crate) fn v2_forward_addr(h: &V2HopInfo) -> alloy::primitives::Address {
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

    // WE45KC: FundingSource::SelfFund — the executor HOLDS the entry WETH and
    // pre-funds the leading V2 pair, then every hop is a no-callback
    // `V2_SWAP_CALC` (gas-cheaper: no flash-callback overhead, no flash-repay
    // transfer — the economic point of ADR-029 D1). Bytes DIFFER from the
    // default in-path-flash (`V2_SWAP_COMPACT` + callback repay).
    if inputs.opts.funding == crate::grammar_ledger::FundingSource::SelfFund {
        let mut commands =
            encoders::enc_erc20_transfer(weth_idx, pool_indices[0], inputs.optimal_input).ok()?;
        for i in 0..num_hops {
            let hop = v2_hops[i];
            let recipient_idx = if i < num_hops - 1 {
                pool_indices[i + 1]
            } else {
                SENTINEL_SELF
            };
            commands.extend_from_slice(&encoders::enc_v2_swap_calc(
                pool_indices[i],
                hop.zfo,
                recipient_idx,
                hop.fee,
            ));
        }
        let mut out = encoders::enc_preamble(&at);
        out.extend_from_slice(&commands);
        return Some(out);
    }

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

/// The all-V2 N-hop **speedrail** — the former production path for any all-V2
/// route (`encode_cmd_stream`'s short-circuit). **ORPHANED since the KO5NNB
/// cutover**: `encode_cmd_stream` now funnels all-V2 through
/// [`crate::grammar_shape::derive_all_v2`] (the Plan + validator path). Retained
/// only as the byte-parity oracle for [`crate::grammar_shape::build_all_v2_chain`]
/// (the `all_v2_chain_byte_parity_with_speedrail` test, T1) until T3 deletes it.
#[must_use]
pub fn encode_all_v2(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    all_v2_walk(path, inputs)
}

/// Adapt an all-V2 **3-hop** path to the `three_hop_v2_v2_v2` layout (the
/// former distinct 3-hop entry, top-swap-on-pool-C). **ORPHANED since the
/// KO5NNB cutover**: `encode_grammar` no longer dispatches here — the all-V2
/// 3-hop family routes through `derive_shape`'s `(V2,V2,V2)` arm
/// (`build_all_v2_chain`, N-hop speedrail/Plan layout; the distinct layout is
/// COLLAPSED). Retained pending the T3 deletion task.
#[expect(dead_code)]
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

/// The generic 2/3-hop dispatcher — delegates every family (the all-V2
/// routes included since KO5NNB) to
/// [`derive_shape`][crate::grammar_shape::derive_shape]. There is **no
/// hand-written backstop**: `derive_shape` either derives the family's bytes or
/// `encode_grammar` returns `None` (byte-parity is held by the revm runtime
/// matrix + the golden suites, not by an adapter oracle). The former all-V2-3-hop
/// hand-written emitter [`v2_v2_v2`] is orphaned (retained pending T3).
#[must_use]
pub fn encode_grammar(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    // Every 2/3-hop family (all-V2 included) is derived by `derive_shape`.
    crate::grammar_shape::derive_shape(path, inputs)
}
