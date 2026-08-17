//! Enclosure-shape bodies for `derive_plan` (ADR-031 D6 structural split,
//! arch-review epic PZBGP7). One module per enclosure block; the gate
//! ordering is owned by `derive_plan` in the parent file.
pub(crate) mod all_v2_chain;
pub(crate) mod tag_residual;
pub(crate) mod three_hop;
pub(crate) mod two_hop_seed_v4;
pub(crate) mod two_hop_uniswap_only;
pub(crate) mod two_hop_v4_led;
