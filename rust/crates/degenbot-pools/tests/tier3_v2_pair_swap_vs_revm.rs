//! Tier-3 V2-pair on-chain accuracy oracle (ergo task `TLBUNW`, epic
//! `UP5NH6` — the V2 family slice of SH6HAK's Tier-3 cutover). Deploys the
//! canonical v2-core `UniswapV2Pair` as real bytecode via the
//! `V2SwapOracleHarness` (solc-0.5.16 compiled), mints reserves + `sync`s so
//! the pair's slot-8 reserves equal the live `balanceOf` (K-check consistency
//! by construction — per ADR-020 D4 the whole-slot-set seeding avoids the
//! production slot-8-vs-balanceOf inconsistency), then drives `pair.swap` with
//! the engine's computed `amountOut` and asserts byte-exactness.
//!
//! The oracle logic (deploy → setup → swap probe, the `Swap`-event + post-state
//! assertions, the K-revert-reason assertion, and the proptest strategies) lives
//! in [`tier3_v2_common`](crate::tier3_v2_common); this file declares the
//! Uniswap V2 fork (hardcoded 0.3% fee `997/1000`) and the pinned tests. Each
//! case proves three things (see the common module), the strongest being that
//! `pair.swap(engine_out + 1)` reverts with exactly `UniswapV2: K` — not just
//! "it reverted" — pinning `engine_out` at the on-chain maximal output
//! byte-for-byte against the real deployed math (not a Rust twin).
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The canonical v2-core
//! bytecode is loaded from the committed `tier3-oracle/artifacts/` tree (no
//! solc/forge needed to RUN). Artifact integrity is enforced two ways:
//! `tier3_harness_artifacts.rs` hashes the tracked sources (toolchain-free),
//! and `tier3-oracle/verify-tier3-artifacts.sh` recompiles every harness and
//! byte-compares it to the committed artifact. After a harness-source edit,
//! regenerate + publish via `tier3-oracle/build-tier3-v2-swap-harness.sh`.

#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::doc_markdown)] // repo-consistent tier3 doc lint

mod tier3_v2_common;

use proptest::prelude::*;

use tier3_v2_common::{assert_byte_exact, fork_case_strategy, V2Fork};

/// The canonical Uniswap V2 fork descriptor: hardcoded 0.3% fee
/// (`gamma_numer = 997, fee_denom = 1000`) and its K-check require-reason.
const FORK: V2Fork = V2Fork {
    harness_sol: "V2SwapOracleHarness.sol",
    harness_contract: "V2SwapOracleHarness",
    gamma_numer: 997,
    fee_denom: 1000,
    k_error: "UniswapV2: K",
    // Uniswap V2's canonical source is a reproducible build, so the harness
    // compiles its own pair (`new UniswapV2Pair()`) — no pinned bytecode.
    pair_init_artifact: None,
};

/// Pinned byte-exact oracle: the engine's `swap` output is the on-chain
/// maximal `getAmountOut` — proven by `pair.swap` accepting it (K-check passes
/// with equality, the emitted `Swap` event matches the engine's flows, post
/// reserves match) and rejecting `+1` with the `UniswapV2: K` error.
#[test]
fn v2_pair_swap_is_byte_exact_to_v2_core_get_amount_out() {
    // 1000 token0 ↔ 2000 token1 (1e21 / 2e21 wei); swap 100 token0 in.
    let r0: u128 = 1_000_000_000_000_000_000_000;
    let r1: u128 = 2_000_000_000_000_000_000_000;
    let amount_in: u128 = 100_000_000_000_000_000_000;
    let zfo = true;
    assert_byte_exact(&FORK, r0, r1, amount_in, zfo);
}

/// Fixed deterministic edge corpus: 1-wei inputs, inputs at reserve − 1, and
/// reserves at both extremes (tiny + near `uint112`-max) — the numerically
/// risky regions where the K-boundary proof is sharpest. Each case runs the
/// full accept/event/post-state/reject-`+1` pipeline.
#[test]
fn v2_pair_swap_edge_corpus_is_byte_exact() {
    let cases: &[(u128, u128, u128, bool)] = &[
        // 1-wei input, both directions.
        (1_000_000, 2_000_000, 1, true),
        (1_000_000, 2_000_000, 1, false),
        // Input at reserve_in − 1, both directions.
        (1_000_000, 2_000_000, 999_999, true),
        (1_000_000, 2_000_000, 1_999_999, false),
        // Tiny reserves.
        (1, 1, 1, true),
        (2, 3, 2, true),
        (3, 2, 2, false),
        // Near uint112-max reserves, wei-scale inputs.
        (
            (1u128 << 112) - 1_000_000,
            (1u128 << 112) - 2_000_000,
            1,
            true,
        ),
        (
            (1u128 << 112) - 1_000_000,
            (1u128 << 112) - 2_000_000,
            500_000,
            false,
        ),
    ];
    for &(r0, r1, amount_in, zfo) in cases {
        assert_byte_exact(&FORK, r0, r1, amount_in, zfo);
    }
}

proptest! {
    /// Proptest the byte-exact V2 oracle over (reserve0, reserve1, amount_in,
    /// direction) drawn from the shared fork strategies (nominal wide range,
    /// tiny reserves, near-uint112-max reserves). Every case runs the full
    /// accept-with-matched-flows / post-state / reject-with-`UniswapV2: K`
    /// pipeline on fresh pristine harnesses.
    #[test]
    fn v2_pair_byte_exact_proptest(
        (r0, r1, amount_in, zfo) in fork_case_strategy(),
    ) {
        assert_byte_exact(&FORK, r0, r1, amount_in, zfo);
    }
}
