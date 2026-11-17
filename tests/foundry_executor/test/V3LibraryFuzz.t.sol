// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "../src/TestBase.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {ITestHarness} from "../src/generated/ITestHarness.sol";
import {ISolReference} from "../src/generated/ISolReference.sol";

/// Port of `tests/executor/test_v3_library_fuzz.py`: cross-implementation
/// fuzzing of the Vyper V3 library ports (TestHarness) against the committed
/// solc-0.7.6 Solidity reference (SolReference).
///
/// The source drives Hypothesis sweeps (200-500 examples per property) over a
/// deployed Vyper/`SolReference` pair and compares verdicts. The Forge port
/// runs the same sweeps deterministically: every test reseeds a fixed-seed
/// splitmix64 stream and generates a fixed number of examples with explicit
/// bounds, so the sweep is reproducible run to run without touching the
/// lane-wide forge fuzz configuration. Any disagreement is a bug in the Vyper
/// port.
///
/// Verdict parity (`_parity`) distinguishes exactly what the source's
/// `_expect_parity` distinguishes: both sides returning equal values, both
/// reverting with the same exposed revert identity, bilateral bare reverts,
/// and one-sided bare reverts (a revert that exposes no identity cannot
/// disagree with an annotated one). Anything else — value mismatch, a one-sided
/// revert, exposed reverts that disagree, an undecodable return payload (the
/// analogue of Ape's missing-method error) or a capture-layer failure (the
/// analogue of an unrelated Python exception) — fails the test.
contract V3LibraryFuzzTest is TestBase {
    uint256 internal constant MIN_SQRT_RATIO = 4295128739;
    uint256 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342;

    uint256 internal constant EXAMPLES = 200;
    uint256 internal constant TICK_EXAMPLES = 500;
    uint256 internal constant SWAP_STEP_EXAMPLES = 500;

    /// Fixed seed for every sweep (splitmix64 stream, reseeded per test).
    uint64 internal constant FUZZ_SEED = 0xa6729880;

    uint64 internal _state;

    ITestHarness internal harness;
    ISolReference internal solRef;

    function setUp() public {
        harness = ITestHarness(_deploy("TestHarness", abi.encode()));
        solRef = ISolReference(_deploy("SolReference", abi.encode()));
    }

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

    /// A full 256-bit draw assembled from four 64-bit words.
    function _next256() private returns (uint256) {
        return (uint256(_next()) << 192) | (uint256(_next()) << 128) | (uint256(_next()) << 64)
            | uint256(_next());
    }

    /// Reduce a draw into [lo, hi]. The range never approaches the draw's
    /// width except in the full-range callers, where the modulo bias is
    /// below 2^-96 and irrelevant for a math differential.
    function _bounded(uint256 x, uint256 lo, uint256 hi) private pure returns (uint256) {
        return lo + (x % (hi - lo + 1));
    }

    // ── Verdict capture and parity engine ─────────────────────────────────

    /// One side's call outcome: returndata on success, revert data otherwise.
    /// `threw` models a capture-layer failure — Ape's `capture_revert` only
    /// converts contract reverts into values; any other exception propagates.
    struct Call {
        bool ok;
        bool threw;
        bytes payload;
    }

    enum Shape {
        U256,
        I256,
        Quad
    }

    enum Verdict {
        Parity,
        ResultsDisagree,
        VerdictsDisagree,
        RevertsDisagree,
        SurfaceFailure
    }

    function _capture(address target, bytes memory data) private returns (Call memory c) {
        (c.ok, c.payload) = target.call(data);
    }

    function _verdictName(Verdict v) private pure returns (string memory) {
        if (v == Verdict.Parity) return "Parity";
        if (v == Verdict.ResultsDisagree) return "ResultsDisagree";
        if (v == Verdict.VerdictsDisagree) return "VerdictsDisagree";
        if (v == Verdict.RevertsDisagree) return "RevertsDisagree";
        return "SurfaceFailure";
    }

    function _intToString(int256 v) private view returns (string memory) {
        if (v >= 0) return vm.toString(uint256(v));
        // Two's-complement magnitude; correct for type(int256).min too.
        return string.concat("-", vm.toString(~uint256(v) + 1));
    }

    /// Compare the two success payloads under the expected return shape.
    /// An undecodable payload (e.g. a codeless target's empty returndata —
    /// the raw-call analogue of Ape's missing-method error) is a surface
    /// failure, never parity.
    function _valuesAgree(bytes memory pa, bytes memory pb, Shape shape)
        private
        view
        returns (bool ok, string memory why, string memory av, string memory bv)
    {
        if (shape == Shape.U256) {
            if (pa.length != 32 || pb.length != 32) {
                return (false, "call surface failure: undecodable return payload", "", "");
            }
            uint256 a = abi.decode(pa, (uint256));
            uint256 b = abi.decode(pb, (uint256));
            return (a == b, "", vm.toString(a), vm.toString(b));
        }
        if (shape == Shape.I256) {
            if (pa.length != 32 || pb.length != 32) {
                return (false, "call surface failure: undecodable return payload", "", "");
            }
            int256 a = abi.decode(pa, (int256));
            int256 b = abi.decode(pb, (int256));
            return (a == b, "", _intToString(a), _intToString(b));
        }
        if (pa.length != 128 || pb.length != 128) {
            return (false, "call surface failure: undecodable return payload", "", "");
        }
        (uint256 a0, uint256 a1, uint256 a2, uint256 a3) =
            abi.decode(pa, (uint256, uint256, uint256, uint256));
        (uint256 b0, uint256 b1, uint256 b2, uint256 b3) =
            abi.decode(pb, (uint256, uint256, uint256, uint256));
        bool equal = a0 == b0 && a1 == b1 && a2 == b2 && a3 == b3;
        return (
            equal,
            "",
            string.concat(
                "(",
                vm.toString(a0),
                ", ",
                vm.toString(a1),
                ", ",
                vm.toString(a2),
                ", ",
                vm.toString(a3),
                ")"
            ),
            string.concat(
                "(",
                vm.toString(b0),
                ", ",
                vm.toString(b1),
                ", ",
                vm.toString(b2),
                ", ",
                vm.toString(b3),
                ")"
            )
        );
    }

    /// Whether a success payload can be decoded under the expected shape.
    /// A codeless target's empty returndata (the raw-call analogue of Ape's
    /// missing-method error) fails here and must surface, never pass.
    function _shapeOk(bytes memory payload, Shape shape) private pure returns (bool) {
        if (shape == Shape.U256 || shape == Shape.I256) {
            return payload.length == 32;
        }
        return payload.length == 128;
    }

    /// The parity verdict for one fuzz case, mirroring the source's
    /// `_expect_parity` + `reverts_match` semantics.
    function _parity(Call memory a, Call memory b, Shape shape)
        private
        view
        returns (Verdict v, string memory reason)
    {
        if (a.threw || b.threw) {
            return (Verdict.SurfaceFailure, "capture-layer failure propagated");
        }
        // Surface failures on the ok sides are detected before any verdict
        // comparison: a missing method / undecodable returndata propagates
        // regardless of what the other side did.
        if (a.ok && !_shapeOk(a.payload, shape)) {
            return (Verdict.SurfaceFailure, "call surface failure: undecodable return payload");
        }
        if (b.ok && !_shapeOk(b.payload, shape)) {
            return (Verdict.SurfaceFailure, "call surface failure: undecodable return payload");
        }
        if (a.ok && b.ok) {
            (bool agree, string memory why, string memory av, string memory bval) =
                _valuesAgree(a.payload, b.payload, shape);
            if (bytes(why).length != 0) {
                return (Verdict.SurfaceFailure, why);
            }
            if (!agree) {
                return (
                    Verdict.ResultsDisagree,
                    string.concat("results disagree: Vyper = ", av, ", Sol = ", bval)
                );
            }
            return (Verdict.Parity, "");
        }
        if (a.ok != b.ok) {
            return (
                Verdict.VerdictsDisagree,
                string.concat(
                    "verdicts disagree: Vyper = ",
                    a.ok ? "returned" : _revertName(a.payload),
                    ", Sol = ",
                    b.ok ? "returned" : _revertName(b.payload)
                )
            );
        }
        // Both reverted: compare the exposed revert identity. A revert that
        // exposes nothing (empty payload, the Vyper checked-conversion or
        // bare-require shape) cannot disagree with an annotated one.
        bool aExposed = a.payload.length >= 4;
        bool bExposed = b.payload.length >= 4;
        if (aExposed && bExposed) {
            if (keccak256(a.payload) == keccak256(b.payload)) {
                return (Verdict.Parity, "");
            }
            return (
                Verdict.RevertsDisagree,
                string.concat(
                    "reverts disagree: Vyper = ",
                    _revertName(a.payload),
                    " / ",
                    vm.toString(a.payload),
                    ", Sol = ",
                    _revertName(b.payload),
                    " / ",
                    vm.toString(b.payload)
                )
            );
        }
        return (Verdict.Parity, "");
    }

    /// Assert one fuzz case reaches the same verdict on both implementations.
    /// `ctx` names the concrete inputs, so a failure prints them.
    function _expectParity(Call memory a, Call memory b, Shape shape, string memory ctx)
        private
        view
    {
        (Verdict v, string memory reason) = _parity(a, b, shape);
        if (v != Verdict.Parity) {
            fail(string.concat(ctx, " [", reason, "]"));
        }
    }

    // ── Shared call shapes ────────────────────────────────────────────────

    function _harness(address h, bytes memory data) private returns (Call memory) {
        return _capture(h, data);
    }

    function _mulDivCase(uint256 a, uint256 b, uint256 d) private {
        string memory ctx =
            string.concat("mul_div(", vm.toString(a), ", ", vm.toString(b), ", ", vm.toString(d), ")");
        _expectParity(
            _harness(address(harness), abi.encodeCall(ITestHarness.test_mul_div, (a, b, d))),
            _harness(address(solRef), abi.encodeCall(ISolReference.test_mul_div, (a, b, d))),
            Shape.U256,
            ctx
        );
    }

    function _mulDivUpCase(uint256 a, uint256 b, uint256 d) private {
        string memory ctx = string.concat(
            "mul_div_rounding_up(", vm.toString(a), ", ", vm.toString(b), ", ", vm.toString(d), ")"
        );
        _expectParity(
            _harness(address(harness), abi.encodeCall(ITestHarness.test_mul_div_rounding_up, (a, b, d))),
            _harness(
                address(solRef), abi.encodeCall(ISolReference.test_mul_div_rounding_up, (a, b, d))
            ),
            Shape.U256,
            ctx
        );
    }

    function testMulDivParity() public {
        _state = FUZZ_SEED + 1;
        // Preserved phantom-overflow and boundary examples from the source's
        // deterministic suite, run through the same parity verdict.
        _mulDivCase((1 << 128) - 1, (1 << 128) - 1, (1 << 128) - 1);
        _mulDivCase(1 << 128, 1 << 128, 1 << 128);
        _mulDivCase((1 << 128) + 1, 3, 1 << 128);
        _mulDivCase((1 << 255) + 1, 3, 1 << 255);
        _mulDivCase(1, type(uint256).max, 1);
        _mulDivCase(0, 12345, 7);
        _mulDivCase(type(uint256).max, type(uint256).max, 1);
        _mulDivCase(1, 1, 0); // zero denominator: bare revert vs annotated assert
        for (uint256 i = 0; i < EXAMPLES; i++) {
            _mulDivCase(_next256(), _next256(), 1 + (_next256() % type(uint256).max));
        }
    }

    function testMulDivRoundingUpParity() public {
        _state = FUZZ_SEED + 2;
        _mulDivUpCase((1 << 128) - 1, (1 << 128) - 1, (1 << 128) - 1);
        _mulDivUpCase(1, 1, 2);
        _mulDivUpCase(1, type(uint256).max, 1);
        _mulDivUpCase(1, 1, 0);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            _mulDivUpCase(_next256(), _next256(), 1 + (_next256() % type(uint256).max));
        }
    }

    function testGetNextSqrtPriceFromInputParity() public {
        _state = FUZZ_SEED + 3;
        _fromInputCase(uint160(MIN_SQRT_RATIO), 10 ** 18, 10 ** 18, true);
        _fromInputCase(uint160(MIN_SQRT_RATIO), 10 ** 18, 0, true);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint160 sqrtP = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint128 liquidity = uint128(1 + (_next256() % ((1 << 128) - 1)));
            uint256 amount = _next256() % (1 << 128);
            bool zeroForOne = _next() % 2 == 1;
            _fromInputCase(sqrtP, liquidity, amount, zeroForOne);
        }
    }

    function _fromInputCase(uint160 sqrtP, uint128 liquidity, uint256 amount, bool zeroForOne)
        private
    {
        string memory ctx = string.concat(
            "from_input(",
            vm.toString(sqrtP),
            ", ",
            vm.toString(liquidity),
            ", ",
            vm.toString(amount),
            ", ",
            zeroForOne ? "true" : "false",
            ")"
        );
        _expectParity(
            _harness(
                address(harness),
                abi.encodeCall(
                    ITestHarness.test_get_next_sqrt_price_from_input,
                    (sqrtP, liquidity, amount, zeroForOne)
                )
            ),
            _harness(
                address(solRef),
                abi.encodeCall(
                    ISolReference.test_get_next_sqrt_price_from_input,
                    (sqrtP, liquidity, amount, zeroForOne)
                )
            ),
            Shape.U256,
            ctx
        );
    }

    function testGetNextSqrtPriceFromOutputParity() public {
        _state = FUZZ_SEED + 4;
        _fromOutputCase(uint160(MIN_SQRT_RATIO), 10 ** 18, 10 ** 17, true);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint160 sqrtP = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint128 liquidity = uint128(1 + (_next256() % ((1 << 128) - 1)));
            uint256 amount = 1 + (_next256() % ((1 << 128) - 1));
            bool zeroForOne = _next() % 2 == 1;
            _fromOutputCase(sqrtP, liquidity, amount, zeroForOne);
        }
    }

    function _fromOutputCase(uint160 sqrtP, uint128 liquidity, uint256 amount, bool zeroForOne)
        private
    {
        string memory ctx = string.concat(
            "from_output(",
            vm.toString(sqrtP),
            ", ",
            vm.toString(liquidity),
            ", ",
            vm.toString(amount),
            ", ",
            zeroForOne ? "true" : "false",
            ")"
        );
        _expectParity(
            _harness(
                address(harness),
                abi.encodeCall(
                    ITestHarness.test_get_next_sqrt_price_from_output,
                    (sqrtP, liquidity, amount, zeroForOne)
                )
            ),
            _harness(
                address(solRef),
                abi.encodeCall(
                    ISolReference.test_get_next_sqrt_price_from_output,
                    (sqrtP, liquidity, amount, zeroForOne)
                )
            ),
            Shape.U256,
            ctx
        );
    }

    function testGetAmount0DeltaParity() public {
        _state = FUZZ_SEED + 5;
        _amount0Case(uint160(MIN_SQRT_RATIO), uint160(MIN_SQRT_RATIO) * 2, 10 ** 18, true);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint160 sqrtA = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint160 sqrtB = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint128 liquidity = uint128(1 + (_next256() % ((1 << 128) - 1)));
            bool roundUp = _next() % 2 == 1;
            _amount0Case(sqrtA, sqrtB, liquidity, roundUp);
        }
    }

    function _amount0Case(uint160 sqrtA, uint160 sqrtB, uint128 liquidity, bool roundUp) private {
        string memory ctx = string.concat(
            "amount0(",
            vm.toString(sqrtA),
            ", ",
            vm.toString(sqrtB),
            ", ",
            vm.toString(liquidity),
            ", ",
            roundUp ? "true" : "false",
            ")"
        );
        _expectParity(
            _harness(
                address(harness),
                abi.encodeCall(ITestHarness.test_get_amount0_delta, (sqrtA, sqrtB, liquidity, roundUp))
            ),
            _harness(
                address(solRef),
                abi.encodeCall(ISolReference.test_get_amount0_delta, (sqrtA, sqrtB, liquidity, roundUp))
            ),
            Shape.U256,
            ctx
        );
    }

    function testGetAmount1DeltaParity() public {
        _state = FUZZ_SEED + 6;
        _amount1Case(uint160(MIN_SQRT_RATIO), uint160(MIN_SQRT_RATIO) * 2, 10 ** 18, true);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            uint160 sqrtA = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint160 sqrtB = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint128 liquidity = uint128(1 + (_next256() % ((1 << 128) - 1)));
            bool roundUp = _next() % 2 == 1;
            _amount1Case(sqrtA, sqrtB, liquidity, roundUp);
        }
    }

    function _amount1Case(uint160 sqrtA, uint160 sqrtB, uint128 liquidity, bool roundUp) private {
        string memory ctx = string.concat(
            "amount1(",
            vm.toString(sqrtA),
            ", ",
            vm.toString(sqrtB),
            ", ",
            vm.toString(liquidity),
            ", ",
            roundUp ? "true" : "false",
            ")"
        );
        _expectParity(
            _harness(
                address(harness),
                abi.encodeCall(ITestHarness.test_get_amount1_delta, (sqrtA, sqrtB, liquidity, roundUp))
            ),
            _harness(
                address(solRef),
                abi.encodeCall(ISolReference.test_get_amount1_delta, (sqrtA, sqrtB, liquidity, roundUp))
            ),
            Shape.U256,
            ctx
        );
    }

    function testGetSqrtRatioAtTickParity() public {
        _state = FUZZ_SEED + 7;
        _sqrtRatioCase(-887272);
        _sqrtRatioCase(0);
        _sqrtRatioCase(887272);
        for (uint256 i = 0; i < TICK_EXAMPLES; i++) {
            int24 tick = int24(int256(_bounded(_next256(), 0, 887272 * 2)) - 887272);
            _sqrtRatioCase(tick);
        }
    }

    function _sqrtRatioCase(int24 tick) private {
        string memory ctx =
            string.concat("get_sqrt_ratio_at_tick(", _intToString(int256(tick)), ")");
        _expectParity(
            _harness(address(harness), abi.encodeCall(ITestHarness.test_get_sqrt_ratio_at_tick, (tick))),
            _harness(address(solRef), abi.encodeCall(ISolReference.test_get_sqrt_ratio_at_tick, (tick))),
            Shape.U256,
            ctx
        );
    }

    function testGetTickAtSqrtRatioParity() public {
        _state = FUZZ_SEED + 8;
        _tickAtCase(uint160(MIN_SQRT_RATIO));
        _tickAtCase(uint160(MAX_SQRT_RATIO - 1));
        for (uint256 i = 0; i < TICK_EXAMPLES; i++) {
            uint160 sqrtP = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            _tickAtCase(sqrtP);
        }
    }

    function _tickAtCase(uint160 sqrtP) private {
        string memory ctx = string.concat("get_tick_at_sqrt_ratio(", vm.toString(sqrtP), ")");
        _expectParity(
            _harness(
                address(harness), abi.encodeCall(ITestHarness.test_get_tick_at_sqrt_ratio, (sqrtP))
            ),
            _harness(
                address(solRef), abi.encodeCall(ISolReference.test_get_tick_at_sqrt_ratio, (sqrtP))
            ),
            Shape.I256,
            ctx
        );
    }

    /// tick -> price -> tick is the identity away from the ±887272 boundary
    /// (harness-only, as in the source).
    function testTickRoundTrip() public {
        _state = FUZZ_SEED + 9;
        _roundTripCase(-887271);
        _roundTripCase(1000);
        _roundTripCase(-1000);
        for (uint256 i = 0; i < EXAMPLES; i++) {
            int24 tick = int24(int256(_bounded(_next256(), 0, 887271 * 2)) - 887271);
            _roundTripCase(tick);
        }
    }

    function _roundTripCase(int24 tick) private {
        uint160 price = harness.test_get_sqrt_ratio_at_tick(tick);
        int24 recovered = harness.test_get_tick_at_sqrt_ratio(price);
        if (recovered != tick) {
            fail(
                string.concat(
                    "round trip: tick ",
                    _intToString(int256(tick)),
                    " -> price ",
                    vm.toString(price),
                    " -> tick ",
                    _intToString(int256(recovered))
                )
            );
        }
    }

    function testComputeSwapStepParity() public {
        _state = FUZZ_SEED + 10;
        _swapStepCase(uint160(MIN_SQRT_RATIO), uint160(MIN_SQRT_RATIO) * 2, 10 ** 18, 10 ** 18, 3000);
        for (uint256 i = 0; i < SWAP_STEP_EXAMPLES; i++) {
            uint160 sqrtCurrent = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint160 sqrtTarget = uint160(_bounded(_next256(), MIN_SQRT_RATIO, MAX_SQRT_RATIO - 1));
            uint128 liquidity = uint128(1 + (_next256() % ((1 << 128) - 1)));
            int256 amountRemaining = int256(_next256());
            uint24 feePips = uint24(_next256() % 1_000_000);
            _swapStepCase(sqrtCurrent, sqrtTarget, liquidity, amountRemaining, feePips);
        }
    }

    function _swapStepCase(
        uint160 sqrtCurrent,
        uint160 sqrtTarget,
        uint128 liquidity,
        int256 amountRemaining,
        uint24 feePips
    ) private {
        string memory ctx = string.concat(
            "compute_swap_step(",
            vm.toString(sqrtCurrent),
            ", ",
            vm.toString(sqrtTarget),
            ", ",
            vm.toString(liquidity),
            ", ",
            _intToString(amountRemaining),
            ", ",
            vm.toString(feePips),
            ")"
        );
        _expectParity(
            _harness(
                address(harness),
                abi.encodeCall(
                    ITestHarness.test_compute_swap_step,
                    (sqrtCurrent, sqrtTarget, liquidity, amountRemaining, feePips)
                )
            ),
            _harness(
                address(solRef),
                abi.encodeCall(
                    ISolReference.test_compute_swap_step,
                    (sqrtCurrent, sqrtTarget, liquidity, amountRemaining, feePips)
                )
            ),
            Shape.Quad,
            ctx
        );
    }

    // ═════════════════════════════════════════════════════════════════════
    // Meta-tests: the parity helper itself (port of TestRevertParityHelpers).
    // Chain-free: the Call values are stubs, so these pin the failure modes a
    // catch-all `except` destroys without deploying anything.
    // ═════════════════════════════════════════════════════════════════════

    function _value(uint256 v) private pure returns (Call memory) {
        return Call({ok: true, threw: false, payload: abi.encode(v)});
    }

    function _revertWith(bytes memory data) private pure returns (Call memory) {
        return Call({ok: false, threw: false, payload: data});
    }

    /// Capture-layer failure: the analogue of the source stubs raising a
    /// RuntimeError that `capture_revert` must not swallow.
    function _threw() private pure returns (Call memory) {
        return Call({ok: false, threw: true, payload: ""});
    }

    function _bareRevert() private pure returns (Call memory) {
        return _revertWith("");
    }

    function _expectVerdict(Verdict actual, Verdict expected, string memory label) private pure {
        assertTrue(
            actual == expected,
            string.concat(label, ": expected ", _verdictName(expected), ", got ", _verdictName(actual))
        );
    }

    function testEqualResultsAreParity() public {
        (Verdict v, ) = _parity(_value(7), _value(7), Shape.U256);
        _expectVerdict(v, Verdict.Parity, "equal results");
    }

    function testResultMismatchFails() public {
        (Verdict v, string memory reason) = _parity(_value(7), _value(8), Shape.U256);
        _expectVerdict(v, Verdict.ResultsDisagree, "result mismatch");
        assertTrue(bytes(reason).length != 0, "mismatch must carry a reason");
    }

    function testResultVsRevertFails() public {
        (Verdict v, ) = _parity(_value(7), _bareRevert(), Shape.U256);
        _expectVerdict(v, Verdict.VerdictsDisagree, "result vs revert");
    }

    function testRevertVsResultFails() public {
        (Verdict v, ) = _parity(_bareRevert(), _value(7), Shape.U256);
        _expectVerdict(v, Verdict.VerdictsDisagree, "revert vs result");
    }

    function testMatchingRevertMessagesAreParity() public {
        bytes memory payload = abi.encodeWithSignature("Error(string)", "FullMath: overflow");
        (Verdict v, ) = _parity(_revertWith(payload), _revertWith(payload), Shape.U256);
        _expectVerdict(v, Verdict.Parity, "matching revert messages");
    }

    function testBareRevertsAreParity() public {
        (Verdict v, ) = _parity(_bareRevert(), _bareRevert(), Shape.U256);
        _expectVerdict(v, Verdict.Parity, "bare reverts");
    }

    /// A revert that exposes no identity cannot disagree: the Vyper port
    /// annotates guards the Solidity original leaves bare (`assert ...,
    /// "FullMath: overflow"` vs bare `require`).
    function testAnnotatedRevertMatchesBareRevert() public {
        bytes memory annotated = abi.encodeWithSignature("Error(string)", "FullMath: overflow");
        (Verdict v, ) = _parity(_revertWith(annotated), _bareRevert(), Shape.U256);
        _expectVerdict(v, Verdict.Parity, "annotated vs bare");
    }

    function testDifferentRevertMessagesFail() public {
        (Verdict v, ) = _parity(
            _revertWith(abi.encodeWithSignature("Error(string)", "T")),
            _revertWith(abi.encodeWithSignature("Error(string)", "R")),
            Shape.U256
        );
        _expectVerdict(v, Verdict.RevertsDisagree, "different messages");
    }

    function testMatchingCustomErrorsAreParity() public {
        bytes memory payload = abi.encodeWithSelector(ExecutorErrors.INVALID_COMMAND, uint256(32));
        (Verdict v, ) = _parity(_revertWith(payload), _revertWith(payload), Shape.U256);
        _expectVerdict(v, Verdict.Parity, "matching custom errors");
    }

    function testDifferentCustomErrorFieldsFail() public {
        (Verdict v, ) = _parity(
            _revertWith(abi.encodeWithSelector(ExecutorErrors.INVALID_COMMAND, uint256(7))),
            _revertWith(abi.encodeWithSelector(ExecutorErrors.INVALID_COMMAND, uint256(32))),
            Shape.U256
        );
        _expectVerdict(v, Verdict.RevertsDisagree, "different custom error fields");
    }

    function testDifferentCustomErrorNamesFail() public {
        // Fabricated stub selectors with identical fields: only the name
        // (selector) differs, as in the source's stub CustomError ABIs.
        bytes4 selA = bytes4(keccak256("StubErrorA(uint256)"));
        bytes4 selB = bytes4(keccak256("StubErrorB(uint256)"));
        (Verdict v, ) = _parity(
            _revertWith(abi.encodeWithSelector(selA, uint256(32))),
            _revertWith(abi.encodeWithSelector(selB, uint256(32))),
            Shape.U256
        );
        _expectVerdict(v, Verdict.RevertsDisagree, "different custom error names");
    }

    function testUnrelatedExceptionOnVyperSidePropagates() public {
        (Verdict v, ) = _parity(_threw(), _value(7), Shape.U256);
        _expectVerdict(v, Verdict.SurfaceFailure, "unrelated exception on Vyper side");
    }

    function testUnrelatedExceptionOnSoliditySidePropagates() public {
        (Verdict v, ) = _parity(_value(7), _threw(), Shape.U256);
        _expectVerdict(v, Verdict.SurfaceFailure, "unrelated exception on Solidity side");
    }

    /// A missing method surfaces in the raw-call capture as a success with an
    /// undecodable (empty) return payload — the analogue of Ape's
    /// ApeAttributeError — and must fail loudly rather than pass as parity,
    /// whatever the other side's verdict: against a returned value, a bare
    /// revert, or an exposed revert.
    function testMissingMethodPropagates() public {
        Call memory missing =
            _capture(address(0), abi.encodeCall(ITestHarness.test_mul_div, (1, 1, 1)));
        assertTrue(missing.ok, "a codeless target must not revert");
        (Verdict v, ) = _parity(missing, _value(7), Shape.U256);
        _expectVerdict(v, Verdict.SurfaceFailure, "missing method vs result");
        (v, ) = _parity(_value(7), missing, Shape.U256);
        _expectVerdict(v, Verdict.SurfaceFailure, "result vs missing method");
        (v, ) = _parity(missing, _bareRevert(), Shape.U256);
        _expectVerdict(v, Verdict.SurfaceFailure, "missing method vs bare revert");
        (v, ) = _parity(
            missing, _revertWith(abi.encodeWithSignature("Error(string)", "T")), Shape.U256
        );
        _expectVerdict(v, Verdict.SurfaceFailure, "missing method vs exposed revert");
    }
}
