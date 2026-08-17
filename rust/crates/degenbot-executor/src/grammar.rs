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
//! WAYDTL/RVNIPD, and the final hand-written all-V2 emitters (the N-hop
//! speedrail and the former distinct 3-hop layout) were deleted in 4JOWO5.
//! Byte-parity is pinned by the revm runtime matrix (`degenbot-simulation`
//! full_matrix, exact delta), the primitive wire-format layer
//! (`tests/encoders_parity.rs`), and the native bridge byte-golden
//! (`tests/native_eth_3hop_bridge.rs`).
//!
//! **N4TJSZ (SPVEIE + KO5NNB + 4JOWO5):** the all-V2 family (2-hop, 3-hop,
//! any-N) routes through the single `build_walk` pipeline + the
//! [`LedgerValidator`][crate::grammar_ledger::LedgerValidator] gate — D4's "the
//! validator gates the Plan for every family" is literal for all-V2, and the
//! terminal-V2 exact-draw invariant is enforced on the streams the bot actually
//! ships. There is exactly ONE all-V2 producer (the `facts_of_all_v2` arm in
//! `build_walk`); the former distinct all-V2 **3-hop** layout is collapsed to
//! the any-N Plan layout (top-swap-on-pool-A), which is what the revm
//! full_matrix always exercised.
//!
//! The CL-clamp swap-in rule (`V2 → full output; CL → consumed_inputs[i]` +
//! `fits_int128`) is applied directly in the retained builders.

use crate::composers::{ComposerInputs, PathInfo};

/// The generic 2/3-hop dispatcher — delegates every family (all-V2 included
/// since KO5NNB) to [`derive_shape`][crate::grammar_shape::derive_shape].
/// There is **no hand-written backstop**: `derive_shape` either derives the
/// family's bytes or `encode_grammar` returns `None` (byte-parity is held by
/// the revm runtime matrix + the golden suites, not by an adapter oracle).
#[must_use]
pub fn encode_grammar(path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<u8>> {
    // Every 2/3-hop family (all-V2 included) is derived by `derive_shape`.
    crate::grammar_shape::derive_shape(path, inputs)
}
