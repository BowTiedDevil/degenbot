// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "../src/TestBase.sol";

/// Port of `tests/executor/test_v2_fee_denominator_fuzz.py`: does the fee
/// denominator matter for the V2 fee formulas?
///
/// Comparing two representations of the SAME 0.3% fee:
///
///   Old (base-1000):  feeMultiplier = 1000 - 3  = 997   -> 997 / 1000
///   New (base-10000): feeMultiplier = 10000 - 30 = 9970 -> 9970 / 10000
///
/// These are rationally identical (9970/10000 = 997/1000), but integer
/// truncation could in principle diverge. It cannot: the new numerator is
/// exactly 10x the old and the new denominator exactly 10x the old, so
/// floor(newNum / newDen) = floor(N / D). The tests verify that proof (and
/// the base-1000/10000 comparison for the PancakeSwap 0.25% fee, where the
/// old formula truncates to 0.2%) over the same explicit uint112 bounds and
/// preserved examples the source's Hypothesis sweeps use.
///
/// No contract deployment: the source runs the same formulas in pure Python,
/// and every quantity below fits uint256 comfortably within the uint112
/// bounds (the largest product, amountIn x 9970 x reserveOut, is below
/// 2^239), so the source's Hypothesis overflow `assume`s are subsumed by the
/// explicit bounds themselves.
///
/// Like the V3 fuzz port, the sweeps are deterministic: a fixed-seed
/// splitmix64 stream with a fixed example count, so results are reproducible
/// run to run without touching the lane-wide forge fuzz configuration.
contract V2FeeDenominatorFuzzTest is TestBase {
    uint256 internal constant UINT256_MAX = type(uint256).max;
    uint256 internal constant U112_MAX = (1 << 112) - 1;
    uint256 internal constant EXAMPLES = 200;

    /// Fixed seed for every sweep (splitmix64 stream, reseeded per test).
    uint64 internal constant FUZZ_SEED = 0xa6729880;

    uint64 internal _state;

    // ── Deterministic example generation ──────────────────────────────────

    function _next() private returns (uint256 z) {
        // splitmix64 is defined modulo 2^64, so the multiplies must wrap.
        unchecked {
            _state += 0x9E3779B97F4A7C15;
            uint64 x = _state;
            x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9;
            x = (x ^ (x >> 27)) * 0x94D049BB133111EB;
            z = uint256(x ^ (x >> 31));
        }
    }

    /// A uint112 draw within the source's explicit bound.
    function _next112() private returns (uint256) {
        return 1 + (_next() % U112_MAX);
    }

    // ── Pure-math formula reimplementations (generic V2) ──────────────────

    function _getAmountOut(
        uint256 amountIn,
        uint256 reserveIn,
        uint256 reserveOut,
        uint256 feeMultiplier,
        uint256 denominator
    ) private pure returns (uint256) {
        uint256 aif = amountIn * feeMultiplier;
        uint256 num = aif * reserveOut;
        uint256 den = reserveIn * denominator + aif;
        if (den == 0) return 0;
        return num / den;
    }

    function _getAmountIn(
        uint256 amountOut,
        uint256 reserveIn,
        uint256 reserveOut,
        uint256 feeMultiplier,
        uint256 denominator
    ) private pure returns (uint256) {
        uint256 num = reserveIn * amountOut * denominator;
        uint256 den = (reserveOut - amountOut) * feeMultiplier;
        if (den == 0) return UINT256_MAX;
        return num / den + 1;
    }

    function _kCheck(
        uint256 balance0,
        uint256 balance1,
        uint256 reserve0,
        uint256 reserve1,
        uint256 amount0In,
        uint256 amount1In,
        uint256 feeSubtracted,
        uint256 denominator
    ) private pure returns (bool) {
        uint256 b0adj = balance0 * denominator - amount0In * feeSubtracted;
        uint256 b1adj = balance1 * denominator - amount1In * feeSubtracted;
        return b0adj * b1adj >= reserve0 * reserve1 * denominator * denominator;
    }

    // ── Failing-input reporting ───────────────────────────────────────────

    function _failEq(uint256 a, uint256 b, string memory what, string memory ctx) private pure {
        if (a != b) {
            fail(
                string.concat(
                    ctx, " [", what, " differ: old=", vm.toString(a), ", new=", vm.toString(b), "]"
                )
            );
        }
    }

    // ═════════════════════════════════════════════════════════════════════
    // Standard 0.3% fee: old = 997/1000, new = 9970/10000.
    // These MUST produce identical results for all inputs.
    // ═════════════════════════════════════════════════════════════════════

    /// 997/1000 vs 9970/10000 produce identical getAmountOut.
    function testGetAmountOutIdentical() public {
        _state = FUZZ_SEED + 1;
        // Preserved source examples (boundaries of the uint112 range).
        _amountOutCase(1, 1, 1);
        _amountOutCase(U112_MAX, 1, 1);
        _amountOutCase(1, U112_MAX, 1);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            _amountOutCase(_next112(), _next112(), _next112());
        }
    }

    function _amountOutCase(uint256 amountIn, uint256 reserveIn, uint256 reserveOut) private pure {
        string memory ctx = string.concat(
            "get_amount_out(",
            vm.toString(amountIn),
            ", ",
            vm.toString(reserveIn),
            ", ",
            vm.toString(reserveOut),
            ", fee 997/1000 vs 9970/10000)"
        );
        _failEq(
            _getAmountOut(amountIn, reserveIn, reserveOut, 997, 1000),
            _getAmountOut(amountIn, reserveIn, reserveOut, 9970, 10000),
            "old and new",
            ctx
        );
    }

    /// 997/1000 vs 9970/10000 produce identical getAmountIn.
    function testGetAmountInIdentical() public {
        _state = FUZZ_SEED + 2;
        _amountInCase(1, 1, 2);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint256 amountOut = _next112();
            uint256 reserveIn = _next112();
            uint256 reserveOut = _next112();
            // The source's `assume(amount_out < reserve_out)`.
            if (amountOut >= reserveOut) continue;
            _amountInCase(amountOut, reserveIn, reserveOut);
        }
    }

    function _amountInCase(uint256 amountOut, uint256 reserveIn, uint256 reserveOut) private pure {
        string memory ctx = string.concat(
            "get_amount_in(",
            vm.toString(amountOut),
            ", ",
            vm.toString(reserveIn),
            ", ",
            vm.toString(reserveOut),
            ", fee 997/1000 vs 9970/10000)"
        );
        _failEq(
            _getAmountIn(amountOut, reserveIn, reserveOut, 997, 1000),
            _getAmountIn(amountOut, reserveIn, reserveOut, 9970, 10000),
            "old and new",
            ctx
        );
    }

    /// K-invariant checks are equivalent for div-10 fees.
    function testKCheckEquivalent() public {
        _state = FUZZ_SEED + 3;
        _kCheckCase(1, 1, 1);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            _kCheckCase(_next112(), _next112(), _next112());
        }
    }

    function _kCheckCase(uint256 amountIn, uint256 reserveIn, uint256 reserveOut) private pure {
        uint256 amountOut = _getAmountOut(amountIn, reserveIn, reserveOut, 9970, 10000);
        if (amountOut == 0) return;

        uint256 balance0 = reserveIn + amountIn;
        uint256 balance1 = reserveOut - amountOut;

        string memory ctx = string.concat(
            "k_check(",
            vm.toString(amountIn),
            ", ",
            vm.toString(reserveIn),
            ", ",
            vm.toString(reserveOut),
            ", amount_out=",
            vm.toString(amountOut),
            ")"
        );
        bool passesOld = _kCheck(balance0, balance1, reserveIn, reserveOut, amountIn, 0, 3, 1000);
        bool passesNew = _kCheck(balance0, balance1, reserveIn, reserveOut, amountIn, 0, 30, 10000);
        if (passesOld != passesNew) {
            fail(
                string.concat(
                    ctx,
                    " [K-check differs: old=",
                    passesOld ? "pass" : "fail",
                    ", new=",
                    passesNew ? "pass" : "fail",
                    "]"
                )
            );
        }
    }

    // ═════════════════════════════════════════════════════════════════════
    // PancakeSwap 0.25% fee: old = 998/1000 (= 0.2%), new = 9975/10000
    // (= 0.25%). The old formula CANNOT represent 0.25% — it truncates
    // 25/10 = 2, giving an effective fee of 0.2% instead of 0.25%.
    // ═════════════════════════════════════════════════════════════════════

    /// Old 998/1000 gives >= output vs new 9975/10000 (fee undercharged).
    function testGetAmountOutOldGivesMore() public {
        _state = FUZZ_SEED + 4;
        _amountOutMoreCase(1, 1, 1);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            _amountOutMoreCase(_next112(), _next112(), _next112());
        }
    }

    function _amountOutMoreCase(uint256 amountIn, uint256 reserveIn, uint256 reserveOut)
        private
        pure
    {
        string memory ctx = string.concat(
            "get_amount_out(",
            vm.toString(amountIn),
            ", ",
            vm.toString(reserveIn),
            ", ",
            vm.toString(reserveOut),
            ", fee 998/1000 vs 9975/10000)"
        );
        uint256 oldOut = _getAmountOut(amountIn, reserveIn, reserveOut, 998, 1000);
        uint256 newOut = _getAmountOut(amountIn, reserveIn, reserveOut, 9975, 10000);
        if (oldOut < newOut) {
            fail(
                string.concat(
                    ctx,
                    " [old should give >= output: old=",
                    vm.toString(oldOut),
                    ", new=",
                    vm.toString(newOut),
                    "]"
                )
            );
        }
    }

    /// Old 998/1000 requires <= input vs new 9975/10000 (fee undercharged).
    function testGetAmountInOldGivesLess() public {
        _state = FUZZ_SEED + 5;
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint256 amountOut = _next112();
            uint256 reserveIn = _next112();
            uint256 reserveOut = _next112();
            if (amountOut >= reserveOut) continue;
            string memory ctx = string.concat(
                "get_amount_in(",
                vm.toString(amountOut),
                ", ",
                vm.toString(reserveIn),
                ", ",
                vm.toString(reserveOut),
                ", fee 998/1000 vs 9975/10000)"
            );
            uint256 oldIn = _getAmountIn(amountOut, reserveIn, reserveOut, 998, 1000);
            uint256 newIn = _getAmountIn(amountOut, reserveIn, reserveOut, 9975, 10000);
            if (oldIn > newIn) {
                fail(
                    string.concat(
                        ctx,
                        " [old should require <= input: old=",
                        vm.toString(oldIn),
                        ", new=",
                        vm.toString(newIn),
                        "]"
                    )
                );
            }
        }
    }

    /// If the new 10000-base K-check passes, the old 1000-base K-check also
    /// passes: old subtracts LESS fee, making adjusted balances higher and
    /// the check easier. The executor's amounts (computed with the
    /// 10000-base formula) are therefore safe against real 1000-base pairs.
    function testNewKPassImpliesOldKPass() public {
        _state = FUZZ_SEED + 6;
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint256 amountIn = _next112();
            uint256 reserveIn = _next112();
            uint256 reserveOut = _next112();

            uint256 amountOut = _getAmountOut(amountIn, reserveIn, reserveOut, 9975, 10000);
            if (amountOut == 0) continue;

            uint256 balance0 = reserveIn + amountIn;
            uint256 balance1 = reserveOut - amountOut;

            bool passesNew = _kCheck(balance0, balance1, reserveIn, reserveOut, amountIn, 0, 25, 10000);
            if (!passesNew) continue;

            bool passesOld = _kCheck(balance0, balance1, reserveIn, reserveOut, amountIn, 0, 2, 1000);
            if (!passesOld) {
                fail(
                    string.concat(
                        "new K-check passed but old K-check failed (amounts could fail ",
                        "against real V2 pairs): amount_in=",
                        vm.toString(amountIn),
                        ", amount_out=",
                        vm.toString(amountOut),
                        ", reserve_in=",
                        vm.toString(reserveIn),
                        ", reserve_out=",
                        vm.toString(reserveOut)
                    )
                );
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════════
    // Fees below 10 bps (fee < 10): the old 1000-base formula charges ZERO
    // fee — the most dramatic divergence, silently dropping sub-10bps fees.
    // ═════════════════════════════════════════════════════════════════════

    /// For fees 1-9 (sub-10bps), the old formula is the zero-fee output.
    function testOldChargesZeroFeeSub10bps() public {
        _state = FUZZ_SEED + 7;
        // Preserved source examples.
        _zeroFeeCase(10 ** 18, 10 ** 24, 10 ** 24, 1);
        _zeroFeeCase(10 ** 18, 10 ** 24, 10 ** 24, 9);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint256 fee = 1 + (_next() % 9);
            _zeroFeeCase(_next112(), _next112(), _next112(), fee);
        }
    }

    function _zeroFeeCase(uint256 amountIn, uint256 reserveIn, uint256 reserveOut, uint256 fee)
        private
        pure
    {
        string memory ctx = string.concat(
            "sub10bps fee=",
            vm.toString(fee),
            ", amount_in=",
            vm.toString(amountIn),
            ", reserve_in=",
            vm.toString(reserveIn),
            ", reserve_out=",
            vm.toString(reserveOut)
        );
        uint256 oldFeeMultiplier = 1000 - fee / 10; // fee < 10 => 1000 - 0 = 1000
        if (oldFeeMultiplier != 1000) {
            fail(string.concat(ctx, " [old should give the zero-fee multiplier 1000]"));
        }

        uint256 zeroFeeResult = _getAmountOut(amountIn, reserveIn, reserveOut, 1000, 1000);
        uint256 oldResult = _getAmountOut(amountIn, reserveIn, reserveOut, oldFeeMultiplier, 1000);
        uint256 newResult = _getAmountOut(amountIn, reserveIn, reserveOut, 10000 - fee, 10000);

        // Old = zero-fee output, which is more than the correctly-charged new.
        if (oldResult != zeroFeeResult) {
            fail(
                string.concat(
                    ctx,
                    " [old != zero-fee output: old=",
                    vm.toString(oldResult),
                    ", zero_fee=",
                    vm.toString(zeroFeeResult),
                    "]"
                )
            );
        }
        if (oldResult < newResult) {
            fail(
                string.concat(
                    ctx,
                    " [old should give >= new: old=",
                    vm.toString(oldResult),
                    ", new=",
                    vm.toString(newResult),
                    "]"
                )
            );
        }
    }
}
