// SPDX-License-Identifier: MIT
pragma solidity ^0.7.0;

// Tier-3 Balancer weighted/stable swap on-chain accuracy oracle harness (ergo
// task EZLECC, epic UP5NH6 — family 3/3 of SH6HAK's Tier-3 cutover).
//
// Unlike the Curve harness (whose canonical source is Vyper, absent here), the
// Balancer math cores ARE Solidity and are compiled VERBATIM from the canonical
// balancer-v2-monorepo (pinned commit f8b6f44) — FixedPoint.sol + LogExpMath.sol
// (the fixed-point pow/log-exp engine), WeightedMath.sol and StableMath.sol —
// vendored under `lib/balancer-src/`. This harness is a thin glass box that
// calls those canonical internal library functions, wrapping them with the
// exact fee/scaling/direction sequence the Rust `degenbot-balancer-math` engine
// uses (`simulate_balancer_weighted_swap` / `simulate_balancer_stable_swap`).
//
//   - weightedOutGivenIn*: subtractSwapFee → mulDown-scale balances+amount →
//     WeightedMath._calcOutGivenIn (canonical; the Rust PowVersion::V2 fast
//     paths match canonical FixedPoint.powDown/powUp) → divDown descale.
//   - stableOutGivenIn*: subtractSwapFee → mulDown upscale all balances +
//     amount → StableMath._calculateInvariant (canonical V1, INVARIANT_V1) →
//     StableMath._calcOutGivenIn → divDown descale. Mirrors the engine's
//     invariant_version == 1 path (bpt_idx = None).
//
// Direction is split into two explicit entry points (`*0to1` / `*1to0`) rather
// than a bool + ternaries to keep the 0.7 stack depth under the limit — the
// Rust engine only models 2-token pools, so this is not a loss of coverage.
//
// The deployed-contract V2 invariant (`calculate_invariant_deployed` /
// INVARIANT_V2) is an older MetaStable/ComposableStable inline revision, not
// present in the current canonical StableMath library; that engine variant is
// a follow-on slice tracked in the ergo task body.

import "@balancer-labs/v2-solidity-utils/contracts/math/FixedPoint.sol";
import "@balancer-labs/v2-solidity-utils/contracts/math/WeightedMath.sol";
import "@balancer-labs/v2-solidity-utils/contracts/math/StableMath.sol";

// Max coins the stable harness accepts (matches StableMath `_MAX_STABLE_TOKENS`).
uint256 constant MAX_STABLE_TOKENS = 5;

contract BalancerSwapOracleHarness {
    using FixedPoint for uint256;

    function weightedOutGivenIn0to1(
        uint256 amountIn,
        uint256 swapFee,
        uint256 balance0,
        uint256 balance1,
        uint256 weight0,
        uint256 weight1,
        uint256 sf0,
        uint256 sf1
    ) external pure returns (uint256) {
        if (amountIn == 0) return 0;
        // subtractSwapFeeAmount: amount - mulUp(amount, feePercentage).
        uint256 amountInLessFee = amountIn.sub(amountIn.mulUp(swapFee));
        uint256 sbIn = balance0.mulDown(sf0);
        uint256 sbOut = balance1.mulDown(sf1);
        uint256 saIn = amountInLessFee.mulDown(sf0);
        uint256 saOut = WeightedMath._calcOutGivenIn(sbIn, weight0, sbOut, weight1, saIn);
        return saOut.divDown(sf1);
    }

    function weightedOutGivenIn1to0(
        uint256 amountIn,
        uint256 swapFee,
        uint256 balance0,
        uint256 balance1,
        uint256 weight0,
        uint256 weight1,
        uint256 sf0,
        uint256 sf1
    ) external pure returns (uint256) {
        if (amountIn == 0) return 0;
        uint256 amountInLessFee = amountIn.sub(amountIn.mulUp(swapFee));
        uint256 sbIn = balance1.mulDown(sf1);
        uint256 sbOut = balance0.mulDown(sf0);
        uint256 saIn = amountInLessFee.mulDown(sf1);
        uint256 saOut = WeightedMath._calcOutGivenIn(sbIn, weight1, sbOut, weight0, saIn);
        return saOut.divDown(sf0);
    }

    function stableOutGivenIn0to1(
        uint256 amountIn,
        uint256 swapFee,
        uint256 amp,
        uint256[MAX_STABLE_TOKENS] memory balances,
        uint256[MAX_STABLE_TOKENS] memory scalingFactors,
        uint256 tokenCount
    ) external pure returns (uint256) {
        if (amountIn == 0) return 0;
        require(tokenCount >= 2 && tokenCount <= MAX_STABLE_TOKENS, "tokenCount");

        uint256 amountInLessFee = amountIn.sub(amountIn.mulUp(swapFee));
        uint256[] memory ub = new uint256[](tokenCount);
        for (uint256 i = 0; i < tokenCount; i++) {
            ub[i] = balances[i].mulDown(scalingFactors[i]);
        }
        uint256 saIn = amountInLessFee.mulDown(scalingFactors[0]);
        uint256 invariant = StableMath._calculateInvariant(amp, ub);
        uint256 saOut = StableMath._calcOutGivenIn(amp, ub, 0, 1, saIn, invariant);
        return saOut.divDown(scalingFactors[1]);
    }

    function stableOutGivenIn1to0(
        uint256 amountIn,
        uint256 swapFee,
        uint256 amp,
        uint256[MAX_STABLE_TOKENS] memory balances,
        uint256[MAX_STABLE_TOKENS] memory scalingFactors,
        uint256 tokenCount
    ) external pure returns (uint256) {
        if (amountIn == 0) return 0;
        require(tokenCount >= 2 && tokenCount <= MAX_STABLE_TOKENS, "tokenCount");

        uint256 amountInLessFee = amountIn.sub(amountIn.mulUp(swapFee));
        uint256[] memory ub = new uint256[](tokenCount);
        for (uint256 i = 0; i < tokenCount; i++) {
            ub[i] = balances[i].mulDown(scalingFactors[i]);
        }
        uint256 saIn = amountInLessFee.mulDown(scalingFactors[1]);
        uint256 invariant = StableMath._calculateInvariant(amp, ub);
        uint256 saOut = StableMath._calcOutGivenIn(amp, ub, 1, 0, saIn, invariant);
        return saOut.divDown(scalingFactors[0]);
    }
}
