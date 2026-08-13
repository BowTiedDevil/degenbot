//! Facet A grammar — the production command-stream encoder (ADR-025 / ADR-029).
//!
//! `encode_grammar` is the **sole** production encoder for every 2/3-hop path
//! (the all-V2 any-N speedrail lives in [`encode_all_v2`], reached first by
//! `encode_cmd_stream`). Production delegates byte-emission to
//! [`grammar_shape::derive_shape`][crate::grammar_shape::derive_shape] — the
//! per-shape-class deriver — for every family it handles, with **no hand-written
//! backstop**: a family either derives or it does not encode (the
//! `cutover`/`debug_assert` oracle and the ~32 proven adapter fns it guarded
//! were retired in WAYDTL once `derive_shape` covered every family
//! byte-identically; byte-parity is now held by the golden-master suites in
//! `tests/composers_parity.rs` / `tests/composers_3hop_parity.rs`).
//!
//! The single retained hand-written emitter is [`v2_v2_v2`] — the all-V2
//! **3-hop** layout, structurally distinct from the N-hop speedrail (per the
//! `6ZIE5X` decision: emitters are per-protocol code; this is **not** a backstop,
//! it is the deliberate primary path for that routing split, reached only via the
//! test-only `encode_cmd_3_hop` entry). `encode_all_v2` (the speedrail) is the
//! production path for any-N all-V2.
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
///
/// Reached only via the test-only `encode_cmd_3_hop` entry (production
/// `encode_cmd_stream` routes any-N all-V2 to the speedrail first). Retained as a
/// hand-written emitter per the `6ZIE5X` decision (emitters are per-protocol
/// code); this is **not** a derive_shape backstop.
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

/// The generic 2/3-hop dispatcher — the **sole** production entry for the
/// non-speedrail families.
///
/// Every family except the all-V2 3-hop path delegates byte-emission to
/// [`derive_shape`][crate::grammar_shape::derive_shape]. There is **no
/// hand-written backstop**: `derive_shape` either derives the family's bytes or
/// `encode_grammar` returns `None` (byte-parity is held by the golden-master
/// suites, not by an adapter oracle). The single retained hand-written emitter
/// is [`v2_v2_v2`] (the deliberate all-V2-3-hop routing split; see the
/// `6ZIE5X` decision).
#[must_use]
pub fn encode_grammar(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    // The all-V2 3-hop path: the deliberate speedrail/routing split, reached
    // only via the test-only `encode_cmd_3_hop` entry. Emitted by the retained
    // hand-written `v2_v2_v2` adapter (NOT derive_shape; the split is
    // structural — `encode_cmd_stream` routes any-N all-V2 to the speedrail).
    if let (Some(HopInfo::V2(a)), Some(HopInfo::V2(b)), Some(HopInfo::V2(c))) =
        (path.hops.first(), path.hops.get(1), path.hops.get(2))
    {
        return v2_v2_v2(a, b, c, inputs);
    }
    // Every other 2/3-hop family: derived. No backstop.
    crate::grammar_shape::derive_shape(path, inputs)
}
