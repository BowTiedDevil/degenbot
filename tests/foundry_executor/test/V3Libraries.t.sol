// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "../src/TestBase.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {ITestHarness} from "../src/generated/ITestHarness.sol";
import {ISolReference} from "../src/generated/ISolReference.sol";

/// Port of `tests/executor/test_v3_libraries.py`: the Vyper V3 library ports
/// (FullMath, UnsafeMath, SafeCast, SqrtPriceMath, TickMath) against the
/// Solidity reference values the source captured from real v3-core. The
/// committed solc-0.7.6 `SolReference` artifact cross-checks the same math
/// where it strengthens a case; it never replaces a source expectation.
contract V3LibrariesTest is TestBase {
    uint256 internal constant Q96 = 79228162514264337593543950336;

    ITestHarness internal harness;
    ISolReference internal solRef;

    function setUp() public {
        harness = ITestHarness(_deploy("TestHarness", abi.encode()));
        solRef = ISolReference(_deploy("SolReference", abi.encode()));
    }

    // ── Revert helpers ────────────────────────────────────────────────────

    function _callHarness(bytes memory data)
        private
        returns (bool ok, bytes memory revertData)
    {
        (ok, revertData) = _callExpectRevert(address(harness), data);
    }

    /// The Vyper libraries expose their revert identity through `assert`
    /// messages; pin the exact Error(string) payload rather than any revert.
    function _assertErrorMessage(bytes memory revertData, string memory expected) private pure {
        assertTrue(
            revertData.length >= 4 && bytes4(revertData) == ExecutorErrors.ERROR_STRING,
            "expected an Error(string) revert"
        );
        bytes memory payload = new bytes(revertData.length - 4);
        for (uint256 i = 4; i < revertData.length; i++) {
            payload[i - 4] = revertData[i];
        }
        assertEq(abi.decode(payload, (string)), expected, "revert message");
    }

    // ═════════════════════════════════════════════════════════════════════
    // FullMath
    // ═════════════════════════════════════════════════════════════════════

    function testMulDivSimple() public {
        assertEq(harness.test_mul_div(7, 11, 3), 25, "mul_div(7, 11, 3)");
    }

    function testMulDivSimple2() public {
        assertEq(harness.test_mul_div(5, 7, 2), 17, "mul_div(5, 7, 2)");
    }

    function testMulDivExact() public {
        assertEq(harness.test_mul_div(6, 7, 2), 21, "exact division");
    }

    function testMulDivTruncation() public {
        // 77 / 3 = 25.666... truncates to 25
        assertEq(harness.test_mul_div(7, 11, 3), 25, "truncation");
    }

    function testMulDivPhantomOverflowU128() public {
        // (2^128-1)^2 overflows 256 bits but the quotient fits.
        uint256 u128Max = (1 << 128) - 1;
        assertEq(harness.test_mul_div(u128Max, u128Max, u128Max), u128Max, "u128 max self");
        assertEq(
            solRef.test_mul_div(u128Max, u128Max, u128Max),
            u128Max,
            "Solidity reference agrees"
        );
    }

    function testMulDivPhantomOverflowPow2() public {
        uint256 u128 = 1 << 128;
        assertEq(harness.test_mul_div(u128, u128, u128), u128, "2^128 * 2^128 / 2^128");
    }

    function testMulDivPhantomOverflowNearMax() public {
        uint256 u128 = 1 << 128;
        assertEq(harness.test_mul_div(u128 + 1, 3, u128), 3, "(2^128+1) * 3 / 2^128");
        assertEq(solRef.test_mul_div(u128 + 1, 3, u128), 3, "Solidity reference agrees");
    }

    function testMulDivPhantomOverflowU255() public {
        uint256 u255 = 1 << 255;
        assertEq(harness.test_mul_div(u255 + 1, 3, u255), 3, "(2^255+1) * 3 / 2^255");
    }

    function testMulDivOneMaxOne() public {
        assertEq(harness.test_mul_div(1, type(uint256).max, 1), type(uint256).max, "1 * max / 1");
    }

    function testMulDivDenominatorOne() public {
        uint256 a = (1 << 128) - 1;
        assertEq(harness.test_mul_div(a, 1, 1), a, "a * 1 / 1");
    }

    function testMulDivZeroNumerator() public {
        assertEq(harness.test_mul_div(0, 12345, 7), 0, "0 * x / y");
    }

    function testMulDivZeroDenominatorReverts() public {
        (bool ok, bytes memory revertData) =
            _callHarness(abi.encodeCall(ITestHarness.test_mul_div, (1, 1, 0)));
        assertFalse(ok, "division by zero must revert");
        _assertErrorMessage(revertData, "FullMath: division by zero");
    }

    function testMulDivRoundingUpSimple() public {
        // 77 / 3 = 25.666... rounds up to 26
        assertEq(harness.test_mul_div_rounding_up(7, 11, 3), 26, "rounding up");
        assertEq(
            solRef.test_mul_div_rounding_up(7, 11, 3), 26, "Solidity reference agrees"
        );
    }

    function testMulDivRoundingUpExact() public {
        uint256 u128Max = (1 << 128) - 1;
        assertEq(
            harness.test_mul_div_rounding_up(u128Max, u128Max, u128Max),
            u128Max,
            "exact division rounds to the floor value"
        );
    }

    function testMulDivRoundingUpTruncates() public {
        assertEq(harness.test_mul_div(1, 1, 2), 0, "floor");
        assertEq(harness.test_mul_div_rounding_up(1, 1, 2), 1, "rounding up");
    }

    // ═════════════════════════════════════════════════════════════════════
    // UnsafeMath
    // ═════════════════════════════════════════════════════════════════════

    function testDivRoundingUpExact() public {
        assertEq(harness.test_div_rounding_up(10, 5), 2, "exact division");
    }

    function testDivRoundingUpTruncates() public {
        assertEq(harness.test_div_rounding_up(10, 3), 4, "10 / 3 rounds up to 4");
        assertEq(solRef.test_div_rounding_up(10, 3), 4, "Solidity reference agrees");
    }

    function testDivRoundingUpOneOver() public {
        assertEq(harness.test_div_rounding_up(1, 2), 1, "1 / 2 rounds up to 1");
    }

    function testDivRoundingUpZero() public {
        assertEq(harness.test_div_rounding_up(0, 5), 0, "0 / 5");
    }

    function testDivRoundingUpLarge() public {
        assertEq(
            harness.test_div_rounding_up(type(uint256).max, 1), type(uint256).max, "max / 1"
        );
    }

    // ═════════════════════════════════════════════════════════════════════
    // SafeCast
    // ═════════════════════════════════════════════════════════════════════

    function testToUint160Small() public {
        assertEq(harness.test_to_uint160(100), uint160(100), "small value");
    }

    function testToUint160Max() public {
        assertEq(
            harness.test_to_uint160(type(uint160).max),
            type(uint160).max,
            "uint160 max passes through"
        );
    }

    function testToUint160OverflowReverts() public {
        (bool ok, bytes memory revertData) =
            _callHarness(abi.encodeCall(ITestHarness.test_to_uint160, (1 << 160)));
        // Vyper's checked uint256→uint160 conversion reverts bare before the
        // SafeCast assert is reached, so the exposed identity is the empty payload.
        assertFalse(ok, "uint160 overflow must revert");
        assertEq(revertData.length, 0, "bare revert from the checked conversion");
    }

    function testToInt128Positive() public {
        assertEq(harness.test_to_int128(100), 100, "small positive");
    }

    function testToInt128Negative() public {
        assertEq(harness.test_to_int128(-100), -100, "small negative");
    }

    function testToInt128Max() public {
        assertEq(harness.test_to_int128(type(int128).max), type(int128).max, "int128 max");
    }

    function testToInt128Min() public {
        assertEq(harness.test_to_int128(type(int128).min), type(int128).min, "int128 min");
    }

    function testToInt128OverflowReverts() public {
        (bool ok, bytes memory revertData) =
            _callHarness(abi.encodeCall(ITestHarness.test_to_int128, (2 ** 127)));
        // Same bare checked-conversion revert as the uint160 direction.
        assertFalse(ok, "int128 overflow must revert");
        assertEq(revertData.length, 0, "bare revert from the checked conversion");
    }

    function testToInt256Small() public {
        assertEq(harness.test_to_int256(0), 0, "zero");
    }

    function testToInt256Positive() public {
        assertEq(harness.test_to_int256(100), 100, "small positive");
    }

    function testToInt256Max() public {
        assertEq(harness.test_to_int256(2 ** 255 - 1), type(int256).max, "int256 max");
    }

    function testToInt256OverflowReverts() public {
        (bool ok, bytes memory revertData) =
            _callHarness(abi.encodeCall(ITestHarness.test_to_int256, (2 ** 255)));
        assertFalse(ok, "int256 overflow must revert");
        _assertErrorMessage(revertData, "SafeCast: overflow");
    }

    // ═════════════════════════════════════════════════════════════════════
    // SqrtPriceMath
    // ═════════════════════════════════════════════════════════════════════

    function testGetNextSqrtPriceFromInputZfo() public {
        // Selling token0 halves the price: Q96 -> Q96 / 2.
        assertEq(
            harness.test_get_next_sqrt_price_from_input(uint160(Q96), 10 ** 18, 10 ** 18, true),
            39614081257132168796771975168,
            "price decreases for zfo input"
        );
        assertEq(
            solRef.test_get_next_sqrt_price_from_input(uint160(Q96), 10 ** 18, 10 ** 18, true),
            39614081257132168796771975168,
            "Solidity reference agrees"
        );
    }

    function testGetNextSqrtPriceFromInputNotZfo() public {
        // Selling token1 doubles the price: Q96 -> 2 * Q96.
        assertEq(
            harness.test_get_next_sqrt_price_from_input(uint160(Q96), 10 ** 18, 10 ** 18, false),
            158456325028528675187087900672,
            "price increases for one-for-zero input"
        );
    }

    function testGetNextSqrtPriceFromInputZeroAmount() public {
        assertEq(
            harness.test_get_next_sqrt_price_from_input(uint160(Q96), 10 ** 18, 0, true),
            uint160(Q96),
            "zero amount leaves the price unchanged"
        );
    }

    function testGetNextSqrtPriceFromOutputZfo() public {
        assertEq(
            harness.test_get_next_sqrt_price_from_output(uint160(Q96), 10 ** 18, 10 ** 17, true),
            71305346262837903834189555302,
            "price decreases toward the output"
        );
    }

    function testGetNextSqrtPriceFromOutputNotZfo() public {
        assertEq(
            harness.test_get_next_sqrt_price_from_output(uint160(Q96), 10 ** 18, 10 ** 17, false),
            88031291682515930659493278152,
            "price increases toward the output"
        );
    }

    function testGetAmount0DeltaRoundUp() public {
        assertEq(
            harness.test_get_amount0_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, true),
            500000000000000000,
            "token0 amount over [Q96, 2*Q96]"
        );
        assertEq(
            solRef.test_get_amount0_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, true),
            500000000000000000,
            "Solidity reference agrees"
        );
    }

    function testGetAmount0DeltaRoundDown() public {
        assertEq(
            harness.test_get_amount0_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, false),
            500000000000000000,
            "rounding down agrees for an exact division"
        );
    }

    function testGetAmount1DeltaRoundUp() public {
        assertEq(
            harness.test_get_amount1_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, true),
            1000000000000000000,
            "token1 amount over [Q96, 2*Q96]"
        );
    }

    function testGetAmount1DeltaRoundDown() public {
        assertEq(
            harness.test_get_amount1_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, false),
            1000000000000000000,
            "rounding down agrees for an exact division"
        );
    }

    function testGetAmount0DeltaReversedInputs() public {
        uint256 forward =
            harness.test_get_amount0_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, true);
        uint256 reversed =
            harness.test_get_amount0_delta(uint160(Q96 * 2), uint160(Q96), 10 ** 18, true);
        assertEq(reversed, forward, "reversed bounds give the same amount0");
    }

    function testGetAmount1DeltaReversedInputs() public {
        uint256 forward =
            harness.test_get_amount1_delta(uint160(Q96), uint160(Q96 * 2), 10 ** 18, true);
        uint256 reversed =
            harness.test_get_amount1_delta(uint160(Q96 * 2), uint160(Q96), 10 ** 18, true);
        assertEq(reversed, forward, "reversed bounds give the same amount1");
    }

    // ═════════════════════════════════════════════════════════════════════
    // TickMath
    // ═════════════════════════════════════════════════════════════════════

    function testGetSqrtRatioAtTickZero() public {
        assertEq(harness.test_get_sqrt_ratio_at_tick(0), uint160(Q96), "tick 0 is 1:1");
    }

    function testGetSqrtRatioAtTickOne() public {
        assertEq(
            harness.test_get_sqrt_ratio_at_tick(1),
            79232123823359799118286999568,
            "tick 1"
        );
    }

    function testGetSqrtRatioAtTick100() public {
        assertEq(
            harness.test_get_sqrt_ratio_at_tick(100),
            79625275426524748796330556128,
            "tick 100"
        );
        assertEq(
            solRef.test_get_sqrt_ratio_at_tick(100),
            79625275426524748796330556128,
            "Solidity reference agrees"
        );
    }

    function testGetSqrtRatioAtTick100000() public {
        assertEq(
            harness.test_get_sqrt_ratio_at_tick(100000),
            11755562826496067164730007768450,
            "tick 100000"
        );
    }

    function testGetTickAtSqrtRatioQ96() public {
        assertEq(harness.test_get_tick_at_sqrt_ratio(uint160(Q96)), 0, "Q96 is tick 0");
    }

    function testGetTickAtSqrtRatioMin() public {
        assertEq(harness.test_get_tick_at_sqrt_ratio(4295128739), -887272, "MIN_SQRT_RATIO");
        assertEq(
            solRef.test_get_tick_at_sqrt_ratio(4295128739), -887272, "Solidity reference agrees"
        );
    }

    function testRoundTripTick100() public {
        uint160 price = harness.test_get_sqrt_ratio_at_tick(100);
        assertEq(harness.test_get_tick_at_sqrt_ratio(price), 100, "round trip tick 100");
    }

    function testRoundTripTick1000() public {
        uint160 price = harness.test_get_sqrt_ratio_at_tick(1000);
        assertEq(harness.test_get_tick_at_sqrt_ratio(price), 1000, "round trip tick 1000");
    }

    function testRoundTripTickNegative() public {
        uint160 price = harness.test_get_sqrt_ratio_at_tick(-1000);
        assertEq(harness.test_get_tick_at_sqrt_ratio(price), -1000, "round trip tick -1000");
    }

    function testGetSqrtRatioAtTickRevertsPastMax() public {
        (bool ok, bytes memory revertData) =
            _callHarness(abi.encodeCall(ITestHarness.test_get_sqrt_ratio_at_tick, (887273)));
        assertFalse(ok, "ticks past MAX_TICK must revert");
        _assertErrorMessage(revertData, "T");
    }
}
