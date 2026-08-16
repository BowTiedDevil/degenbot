//! Tier-3 PancakeSwap V2 pair on-chain accuracy oracle (the fork-fee sub-slice
//! of the V2 family — the case `tier3_v2_pair_swap_vs_revm.rs`'s header calls
//! "a follow-on sub-slice … deferred as gilding"). Deploys the REAL
//! Ethereum-mainnet `PancakePair` from its PINNED on-chain creation bytecode
//! (the live mainnet pair `0x2E8135bE…`, Sourcify `exact_match`, committed
//! under `artifacts/PancakeV2Pair/PancakeV2Pair.json`) via the
//! `PancakeV2SwapOracleHarness` — as real bytecode — and proves the engine's
//! real `PANCAKESWAP_V2` preset byte-exact against it.
//!
//! ## Why the pair is PINNED (not compiled)
//!
//! A local recompile of the pancake swap core source can't reproduce the
//! deployment's embedded 32-byte metadata hash, so the pair's init-code hash
//! (degenbot's `0x57224589…`) only reconciles against the ACTUAL on-chain
//! creation bytecode. Sourcify verifies that bytecode `exact_match`
//! (creation + runtime) against the live mainnet pair, so the harness
//! raw-`create`s the pinned bytes and the oracle exercises the genuine
//! deployed contract — no vendored Etherscan source, no local compile.
//!
//! The oracle logic (deploy → setup → swap probe, the `Swap`-event + post-state
//! assertions, the K-revert-reason assertion, and the proptest strategies) lives
//! in [`tier3_v2_common`](crate::tier3_v2_common), shared verbatim with the
//! Uniswap V2 oracle — removing the copy-paste drift that once split the two
//! families apart. This file declares only the Pancake fork (fee
//! `9975/10000`) and the pinned tests.
//!
//! ## The fork fee — 0.25%, matching the engine preset
//!
//! The DEPLOYED `PancakePair` (Sourcify-verified Ethereum-mainnet pair
//! 0x2E8135bE71230c6B1B4045696d41C09Db0414226) hardcodes its swap fee in the
//! K-check (`balance0Adjusted = balance0.mul(10000).sub(amount0In.mul(25))`),
//! giving a **0.25%** fee → retained fraction `(9975, 10_000)`, and its
//! bytecode PUSH constants (10000/25) independently confirm it. This
//! byte-exactly matches degenbot's `DexVariant::PancakeswapV2` preset
//! (`dex_identity.rs::PANCAKESWAP_V2`, `(9975, 10_000)`, citing the old Python
//! `PancakeswapV2Pool.FEE = Fraction(25, 10000)`). So this oracle proves the
//! engine's real preset against the real deployed bytecode, byte-exact — via
//! the same hardcode-the-bytecode-fee pattern the Uniswap V2 oracle uses
//! (`997`). (The stale `pancake-swap-core` GitHub `master` mirrors a 0.2%
//! `mul(2)/1000` build that was never deployed; the PINNED on-chain bytecode
//! here is the 0.25% one, which is why the oracle is wired at `9975/10000` and
//! passes.) The Pancake fork also stores reserves as the 3-tuple `(uint112,
//! uint112, uint32 blockTimestampLast)` — the `PancakeswapStyle` ABI the
//! engine's `DexVariant::PancakeswapV2` reads — and its K-error string is
//! `Pancake: K`.
//!
//! ## Harness bytecode (committed)
//!
//! Runs in the default `cargo test --workspace` suite. The harness + the
//! PINNED pair bytecode are loaded from the committed `tier3-oracle/artifacts/`
//! tree (no solc/forge needed to RUN). Artifact integrity is enforced two ways:
//! `tier3_harness_artifacts.rs` hashes the tracked sources (toolchain-free), and
//! `tier3-oracle/verify-tier3-artifacts.sh` recompiles every harness and
//! byte-compares it to the committed artifact. After a harness-source edit,
//! regenerate + publish via
//! `tier3-oracle/build-tier3-pancake2-swap-harness.sh`.

#![expect(clippy::doc_markdown)] // repo-consistent tier3 doc lint

mod tier3_v2_common;

use proptest::prelude::*;

use tier3_v2_common::{assert_byte_exact, fork_case_strategy, V2Fork};

/// The PancakeSwap V2 fork descriptor: the DEPLOYED `PancakePair`'s K-check is
/// `balance0Adjusted = balance0.mul(10000).sub(amount0In.mul(25))` → retained
/// `(9975, 10_000)` (0.25% fee), byte-exactly the engine's `PANCAKESWAP_V2`
/// preset. Its K-invariant require-reason is `Pancake: K`.
const FORK: V2Fork = V2Fork {
    harness_sol: "PancakeV2SwapOracleHarness.sol",
    harness_contract: "PancakeV2SwapOracleHarness",
    gamma_numer: 9975,
    fee_denom: 10000,
    k_error: "Pancake: K",
    // The pair is deployed from the PINNED on-chain creation bytecode
    // (Sourcify `exact_match`, artifacts/PancakeV2Pair/) via the harness's raw
    // `create` — not a local compile.
    pair_init_artifact: Some("PancakeV2Pair/PancakeV2Pair.json"),
};

/// Pinned byte-exact oracle at the Pancake fork fee: the engine's `swap`
/// output is the on-chain maximal `amountOut` — `pair.swap` accepts it with
/// matched `Swap` flows + post-state, and rejects `+1` with `Pancake: K`.
#[test]
fn pancake_v2_pair_swap_is_byte_exact_at_fork_fee() {
    // 1000 token0 ↔ 2000 token1 (1e21 / 2e21 wei); swap 100 token0 in.
    let r0: u128 = 1_000_000_000_000_000_000_000;
    let r1: u128 = 2_000_000_000_000_000_000_000;
    let amount_in: u128 = 100_000_000_000_000_000_000;
    let zfo = true;
    assert_byte_exact(&FORK, r0, r1, amount_in, zfo);
}

/// Fixed deterministic edge corpus at the fork fee (1 wei, reserve − 1, tiny
/// and near-`uint112`-max reserves, both directions). Each case runs the full
/// accept/event/post-state/reject-`+1` pipeline.
#[test]
fn pancake_v2_pair_swap_edge_corpus_is_byte_exact() {
    let cases: &[(u128, u128, u128, bool)] = &[
        (1_000_000, 2_000_000, 1, true),
        (1_000_000, 2_000_000, 1, false),
        (1_000_000, 2_000_000, 999_999, true),
        (1_000_000, 2_000_000, 1_999_999, false),
        (1, 1, 1, true),
        (2, 3, 2, true),
        (3, 2, 2, false),
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
    /// Proptest the byte-exact Pancake V2 oracle over the shared fork
    /// strategies (nominal wide range, tiny reserves, near-`uint112`-max
    /// reserves), both directions, running the full pipeline on fresh
    /// pristine harnesses and asserting the `+1` revert reason is `Pancake: K`.
    #[test]
    fn pancake_v2_pair_byte_exact_proptest(
        (r0, r1, amount_in, zfo) in fork_case_strategy(),
    ) {
        assert_byte_exact(&FORK, r0, r1, amount_in, zfo);
    }
}
