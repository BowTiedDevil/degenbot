// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {ICmdExecutor} from "../src/generated/ICmdExecutor.sol";

/// Port of `tests/executor/test_v2_fee_bounds.py`.
///
/// V2 fee is a fraction of 10000. Values >= 10000 are invalid because
/// 10000 - fee would underflow (0 or negative); fee = 0 is rejected as a
/// suspicious no-fee pair. Both V2_SWAP_CALC and V2_SWAP_COMPACT must revert
/// on invalid fee values rather than execute a nonsense swap.
contract V2FeeBoundsTest is PoolSetup {
    IFakeUniswapV2Pair internal pair;
    ICmdExecutor internal ex;

    function setUp() public override {
        super.setUp();
        // Unfunded pair with fee=30 — the fee bounds checks fire before any
        // liquidity or excess balance matters.
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        pair = _deployV2Pair(t0, t1, 0, 30);
        ex = ICmdExecutor(
            _deploy(
                "CmdExecutor",
                abi.encode(address(weth), address(pair), address(weth), address(weth))
            )
        );
    }

    function _calcCommands(uint16 fee) internal view returns (bytes memory) {
        Cmd.Table memory at = _emptyTable();
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 exIdx = Cmd.add(at, address(ex));
        return bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(pairIdx, true, exIdx, fee));
    }

    /// fee = 10000 means a 100% fee: `fee_multiplier` is zero, so the calc path
    /// asks the pair for a zero output. Both commands refuse to execute a
    /// nonsense swap — pinned here to the identity each one actually carries.
    /// Neither revert is a fee-bound guard: the executor deliberately removed
    /// the up-front fee validation and documents that `fee >= 10000` is caught
    /// downstream (see `_v2_get_amount_out`'s and the V2_SWAP_CALC /
    /// V2_SWAP_COMPACT doc comments), so the port pins what the contract does
    /// rather than a guard it does not have.
    ///
    /// The pair is unfunded in these two cases: V2_SWAP_CALC reads its input as
    /// `balanceOf(pair) - reserves`, which underflows on an empty pair and
    /// reverts with Vyper's bare (empty-data) checked-subtraction panic BEFORE
    /// the fee math runs. The funded cases at the end of this file exercise the
    /// fee math itself.
    function testFeeExactly10000Reverts() public {
        Outcome memory o = _executeRaw(ex, _calcCommands(10_000));
        assertFalse(o.ok, "Should revert - fee=10000 is invalid");
        assertEq(o.revertData, "", "unfunded pair: the excess-balance read underflows");
    }

    /// fee = 20000 > 10000: on the unfunded pair the same excess-balance read
    /// underflows first, so the identity is the same empty-data panic. The
    /// funded case below reaches the wrapped `fee_multiplier`.
    function testFeeAbove10000Reverts() public {
        Outcome memory o = _executeRaw(ex, _calcCommands(20_000));
        assertFalse(o.ok, "Should revert - fee > 10000 is invalid");
        assertEq(o.revertData, "", "unfunded pair: the excess-balance read underflows");
    }

    /// fee = 0: the executor's documented posture is that a zero fee is a VALID
    /// fee-free swap, not a suspicious pair — the source's expectation of a
    /// revert is a symptom of its unfunded fixture, not contract behavior. This
    /// case keeps the source's stream and pins the real identity (the inflowing
    /// read underflow); `testFundedPairFeeZeroExecutesAsFeeFreeSwap` pins the
    /// fee-free execution.
    function testFeeZeroReverts() public {
        Outcome memory o = _executeRaw(ex, _calcCommands(0));
        assertFalse(o.ok, "unfunded pair: no excess balance to swap");
        assertEq(o.revertData, "", "unfunded pair: the excess-balance read underflows");
    }

    /// fee > 10000 in V2_SWAP_COMPACT: the compact command carries the output
    /// amount explicitly, so it reaches the pair, and the pair's own output
    /// guard is what refuses it — pinned to the pair's exact Error(string).
    function testSwapCompactFeeAbove10000Reverts() public {
        Cmd.Table memory at = _emptyTable();
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 exIdx = Cmd.add(at, address(ex));
        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCompact(pairIdx, true, 1000, exIdx, 20_000, ""));
        Outcome memory o = _executeRaw(ex, commands);
        assertFalse(o.ok, "Should revert - fee > 10000 is invalid");
        assertEq(
            o.revertData,
            abi.encodeWithSignature("Error(string)", "UniswapV2: INSUFFICIENT_LIQUIDITY"),
            "the pair refuses an output larger than its reserves"
        );
    }

    // ── Funded pair: the fee math itself ──────────────────────────────────

    /// A pair with real reserves plus a deposited excess of the input token,
    /// which is exactly the shape V2_SWAP_CALC reads its input from.
    function _fundPairWithExcess(uint256 excessIn)
        internal
        returns (address outputToken, uint256 reserveIn, uint256 reserveOut)
    {
        usdc.mint(address(pair), 200_000e6);
        weth.mint(address(pair), 100 ether);
        pair.sync();
        (uint112 r0, uint112 r1,) = pair.getReserves();
        // `_calcCommands` swaps zero_for_one, so the input is token0.
        outputToken = pair.token1();
        (reserveIn, reserveOut) = (r0, r1);
        IFakeErc20(pair.token0()).mint(address(pair), excessIn);
    }

    /// fee = 0 on a funded pair: the executor computes a fee-free
    /// constant-product output and the swap executes. The executor's own doc
    /// comment is the authority ("fee == 0 means a fee-free swap (valid, just
    /// unusual)"), so the port asserts the executed output instead of the
    /// source's assumed revert.
    function testFundedPairFeeZeroExecutesAsFeeFreeSwap() public {
        uint256 excessIn = 1_000e6;
        (address outputToken, uint256 reserveIn, uint256 reserveOut) =
            _fundPairWithExcess(excessIn);

        // `_calcCommands` sends the output to the file's own executor instance
        // (`ex`), not the fixture's.
        uint256 outBefore = IFakeErc20(outputToken).balanceOf(address(ex));
        Outcome memory o = _executeRaw(ex, _calcCommands(0));
        assertTrue(o.ok, "a fee-free swap executes on a funded pair");
        uint256 delivered = IFakeErc20(outputToken).balanceOf(address(ex)) - outBefore;
        assertGt(delivered, 0, "the executor received the swap output");

        // The pair's own post-swap reserves prove which swap ran: the deposited
        // excess became the input reserve, and the output reserve fell by
        // exactly what the executor received.
        (uint112 r0After, uint112 r1After,) = pair.getReserves();
        assertEq(uint256(r0After), reserveIn + excessIn, "the excess became the input reserve");
        assertEq(uint256(r1After), reserveOut - delivered, "the output reserve fell by the payout");
    }

    /// fee = 20000 on a funded pair reaches the fee math with a wrapped
    /// `fee_multiplier`; the checked multiplication added for exactly this class
    /// of nonsense converts it into a clean revert (empty data), never a silent
    /// under-output swap.
    function testFundedPairFeeAbove10000RevertsInTheFeeMath() public {
        (address outputToken,,) = _fundPairWithExcess(1_000e6);
        // `_calcCommands` pays the file-local `ex`, so that is the balance an
        // under-output swap would move.
        uint256 outBefore = IFakeErc20(outputToken).balanceOf(address(ex));
        Outcome memory o = _executeRaw(ex, _calcCommands(20_000));
        assertFalse(o.ok, "fee = 20000 must not execute");
        assertEq(o.revertData, "", "the wrapped fee product hits the checked multiplication");
        assertEq(
            IFakeErc20(outputToken).balanceOf(address(ex)),
            outBefore,
            "no output was delivered"
        );
    }

    /// fee = 10000 on a funded pair computes a zero output and the pair's own
    /// guard refuses it — the nonsense swap never lands.
    function testFundedPairFeeExactly10000RevertsAtThePair() public {
        (address outputToken,,) = _fundPairWithExcess(1_000e6);
        uint256 outBefore = IFakeErc20(outputToken).balanceOf(address(ex));
        Outcome memory o = _executeRaw(ex, _calcCommands(10_000));
        assertFalse(o.ok, "fee = 10000 must not execute");
        assertEq(
            o.revertData,
            abi.encodeWithSignature("Error(string)", "UniswapV2: INSUFFICIENT_OUTPUT_AMOUNT"),
            "a zero output is refused by the pair"
        );
        assertEq(
            IFakeErc20(outputToken).balanceOf(address(ex)),
            outBefore,
            "no output was delivered"
        );
    }
}
