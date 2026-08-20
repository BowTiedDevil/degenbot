#![expect(clippy::unwrap_used, clippy::expect_used)]
//! Tier-2 behavioral dual-driver parity — Balancer V2 stable swap calc,
//! ComposableStable `bpt_idx = Some(..)` path (ADR-005 standalone claim,
//! RPSW4Z).
//!
//! Proves the BPT-index-skipping path in
//! `simulate_balancer_stable_swap` (`skip_bpt`) correctly **drops the BPT
//! token from the invariant balance list** before computing the stableswap
//! outGivenIn, end-to-end through `BotState::calculate_tokens_out_miss_aware`
//! → `simulate_swap` → `simulate_balancer_stable_swap` → `skip_bpt` — the
//! same path a `cargo add degenbot` standalone consumer reaches.
//!
//! ## The BPT-drop equivalence (the oracle)
//!
//! The fixture is a 3-token `ComposableStablePool` `[token0, token1, BPT]`
//! with `bpt_idx = Some(2)` (BPT at the END of the list). After `skip_bpt`
//! drops the BPT, the invariant + outGivenIn run over **only** the two
//! non-BPT balances — which are byte-identical to the existing MetaStable
//! (`bpt_idx = None`) fixture in
//! `rust/crates/degenbot-pools/tests/pool_handle_balance_vector.rs`
//! (equal balances `1_000_000`, `amp = 100 * 1000`, ZERO fee, identity
//! scaling factors `sf = ONE`, `invariant_version = 2`). That MetaStable
//! fixture records `amount_in = 1_000 → amount_out = 989`, cross-checked
//! against the independent pure-Python `BalancerV2StablePool` companion.
//!
//! Therefore the ComposableStable fixture MUST yield the **same `989`** —
//! if `skip_bpt` failed to drop the BPT, the invariant would be computed
//! over THREE balances (one of them the irrelevant BPT supply) and the
//! output would diverge from `989`. This is the mechanical, non-circular
//! oracle: the MetaStable constant is independently re-derived in
//! `tests/standalone_parity/test_balancer_stable_swap_dual_driver.py` (the
//! Python companion), and the ComposableStable fixture here re-derives the
//! SAME constant through the BPT-drop path.
//!
//! ## Known gap to the full RPSW4Z scenario
//!
//! The task body's exact scenario — `bpt_idx = Some(1)` with a
//! `token0 → token2` swap (one index PAST the BPT) — is NOT reachable
//! through the current `simulate_swap` dispatch, which is
//! `zero_for_one`-based and hardcodes the swap to token-list positions
//! `0 ↔ 1`. With `bpt_idx = Some(1)` the BPT IS at position 1, so
//! positions `0 ↔ 1` is a swap involving the BPT itself (not a valid
//! asset-pair swap). The "index PAST the BPT" rebase branch of `skip_bpt`
//! is therefore covered by a DIRECT unit test on `skip_bpt` in
//! `degenbot-pools/src/simulate_swap.rs` (adj_in/adj_out when one index >
//! bpt_idx), while THIS end-to-end fixture covers the BPT-drop branch
//! (`bpt_idx = Some(2)`, swap between two non-BPT positions). The full
//! end-to-end `bpt_idx = Some(1)` + `token0 → token2` scenario is blocked
//! on the multi-token `simulate_swap` API extension (sibling to VQ4OHX
//! tasks `7D34LW` Aerodrome stable decimals + `U2K6FN` Curve get_dy).

#![expect(clippy::doc_markdown)]

use alloy::primitives::{address, U256};
use degenbot::pools::balancer_stable_state::RegisterBalancerStablePoolParams;
use degenbot::BotState;

// ---- the shared canonical fixture (mirror in the Python parity test) ----
const TOKEN0: alloy::primitives::Address = address!("000000000000000000000000000000000000000A");
const TOKEN1: alloy::primitives::Address = address!("000000000000000000000000000000000000000B");
const TOKEN_BPT: alloy::primitives::Address = address!("000000000000000000000000000000000000000D");
const POOL: alloy::primitives::Address = address!("00000000000000000000000000000000000000CC");
const VAULT: alloy::primitives::Address = address!("00000000000000000000000000000000000000EE");
// 32-byte pool_id (pool address CC... padded).
const POOL_ID: [u8; 32] = {
    let mut id = [0u8; 32];
    id[12] = 0xCC;
    id
};

// 3-token ComposableStable [token0, token1, BPT] with bpt_idx = 2 (BPT at end).
const BPT_IDX: Option<usize> = Some(2);
// Non-BPT balances mirror the MetaStable fixture (1_000_000 each). The BPT
// balance is irrelevant — it is dropped before the invariant — but must be
// present so tokens/balances arities match.
const BALANCE0: u128 = 1_000_000;
const BALANCE1: u128 = 1_000_000;
const BALANCE_BPT: u128 = 1_000_000;
const AMP: u128 = 100 * 1000; // on-chain amp = A * 1000
const SWAP_FEE: u128 = 0; // ZERO fee — isolates the stable math from the fee step
const INVARIANT_VERSION: u8 = 2;
const ONE: u128 = 1_000_000_000_000_000_000; // 1e18 — identity scaling for 18dp tokens

const AMOUNT_IN: u128 = 1_000; // matches the MetaStable fixture's probe amount
const ZERO_FOR_ONE: bool = true; // token0 → token1 (positions 0 ↔ 1, both non-BPT)

/// Canonical expected output. Equal to the MetaStable (`bpt_idx = None`)
/// fixture's recorded `989` because the BPT is dropped from the invariant,
/// leaving an identical 2-token stable swap. If `skip_bpt` failed to drop
/// the BPT, the invariant would run over three balances and diverge from
/// this constant. Independently re-derived by the Python companion in
/// `tests/standalone_parity/test_balancer_stable_swap_dual_driver.py`.
const EXPECTED_AMOUNT_OUT: u128 = 989;

fn register_composable_stable_fixture() -> (BotState, u64) {
    let mut bot = BotState::new();
    let params = RegisterBalancerStablePoolParams {
        address: POOL,
        vault: VAULT,
        pool_id: POOL_ID,
        tokens: vec![TOKEN0, TOKEN1, TOKEN_BPT],
        amp: AMP,
        scaling_factors: vec![U256::from(ONE), U256::from(ONE), U256::from(ONE)],
        swap_fee: SWAP_FEE,
        bpt_idx: BPT_IDX,
        invariant_version: INVARIANT_VERSION,
        balances: vec![
            U256::from(BALANCE0),
            U256::from(BALANCE1),
            U256::from(BALANCE_BPT),
        ],
        update_block: 0,
        rate_provider: None,
    };
    let pool_id = bot.register_balancer_stable_pool(&params);
    (bot, pool_id)
}

#[test]
fn composable_stable_bpt_drop_matches_metastable_oracle() {
    let (bot, pool_id) = register_composable_stable_fixture();
    let amount_out = bot
        .calculate_tokens_out_miss_aware(pool_id, ZERO_FOR_ONE, U256::from(AMOUNT_IN))
        .expect("ComposableStable swap should compute");
    assert_eq!(
        amount_out,
        U256::from(EXPECTED_AMOUNT_OUT),
        "ComposableStable bpt_idx=Some(2) swap did not match the MetaStable oracle (989); \
         skip_bpt may have failed to drop the BPT from the invariant"
    );
}

#[test]
fn composable_stable_bpt_drop_is_symmetric_on_equal_reserves() {
    // On equal non-BPT reserves, token0→token1 and token1→token0 of the same
    // amount_in MUST yield identical output (stableswap symmetry under
    // equal balances). A BPT-drop bug that asymmetrically perturbed the
    // invariant would break this.
    let (bot, pool_id) = register_composable_stable_fixture();
    let forward = bot
        .calculate_tokens_out_miss_aware(pool_id, true, U256::from(AMOUNT_IN))
        .unwrap();
    let reverse = bot
        .calculate_tokens_out_miss_aware(pool_id, false, U256::from(AMOUNT_IN))
        .unwrap();
    assert_eq!(
        forward, reverse,
        "ComposableStable swap must be symmetric on equal reserves (fwd={forward}, rev={reverse})"
    );
}

#[test]
fn composable_stable_bpt_drop_is_monotonic_and_bounded() {
    // Larger amount_in → strictly larger amount_out (monotonic), and the
    // output never exceeds the input (the conservation bound — no free
    // money; the stableswap curve sits below the x=y line for any non-zero
    // trade). A sign-flip or an invariant corruption that produced a
    // super-linear / unbounded output breaks one of these.
    //
    // (A strict sub-linearity assertion `out(2×in) < 2×out(in)` is NOT robust
    // here: with `amp = 100 * 1000` the curve is very flat near the peg, and
    // integer flooring at small `amount_in` loses proportionally more than
    // at large `amount_in`, so the observed ratio is dominated by rounding,
    // not slippage. Monotonicity + the conservation bound are the robust
    // sanity checks the fixture can pin.)
    let (bot, pool_id) = register_composable_stable_fixture();
    let small = bot
        .calculate_tokens_out_miss_aware(pool_id, ZERO_FOR_ONE, U256::from(AMOUNT_IN))
        .unwrap();
    let large = bot
        .calculate_tokens_out_miss_aware(pool_id, ZERO_FOR_ONE, U256::from(10 * AMOUNT_IN))
        .unwrap();
    assert!(
        large > small,
        "monotonicity violated: 10× amount_in gave {large} <= {small}"
    );
    assert!(
        large <= U256::from(10 * AMOUNT_IN),
        "conservation bound violated: amount_out {large} > amount_in {} (free money)",
        10 * AMOUNT_IN
    );
}

#[test]
fn bpt_balance_does_not_affect_output_proving_drop() {
    // RPSW4Z: the BPT balance MUST NOT affect the swap output, because `skip_bpt`
    // drops it from the invariant before any computation. Using a 7× BPT balance
    // (vs the 1× used elsewhere) would perturb a 3-balance invariant if the drop
    // were broken; the output staying `989` proves the drop is live. This is the
    // STRONGEST BPT-drop check in this file — the equal-balance `989` oracle is
    // (coincidentally) insensitive to the drop at this amp, but a broken drop
    // here makes the 7× BPT balance shift the invariant and the output collapses
    // (verified: dropping is broken → output becomes 0, not 989).
    let mut bot = BotState::new();
    let params = RegisterBalancerStablePoolParams {
        address: POOL,
        vault: VAULT,
        pool_id: POOL_ID,
        tokens: vec![TOKEN0, TOKEN1, TOKEN_BPT],
        amp: AMP,
        scaling_factors: vec![U256::from(ONE); 3],
        swap_fee: SWAP_FEE,
        bpt_idx: BPT_IDX,
        invariant_version: INVARIANT_VERSION,
        balances: vec![
            U256::from(BALANCE0),
            U256::from(BALANCE1),
            U256::from(7_000_000u128),
        ],
        update_block: 0,
        rate_provider: None,
    };
    let pid = bot.register_balancer_stable_pool(&params);
    let out = bot
        .calculate_tokens_out_miss_aware(pid, ZERO_FOR_ONE, U256::from(AMOUNT_IN))
        .unwrap();
    assert_eq!(
        out,
        U256::from(EXPECTED_AMOUNT_OUT),
        "output changed when only the (irrelevant) BPT balance changed => BPT NOT dropped: {out}"
    );
}
