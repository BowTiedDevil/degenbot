// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
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

    /// fee = 10000 means a 100% fee — all output consumed, nonsense.
    function testFeeExactly10000Reverts() public {
        Outcome memory o = _executeRaw(ex, _calcCommands(10_000));
        assertFalse(o.ok, "Should revert - fee=10000 is invalid");
    }

    /// fee = 20000 > 10000 — would cause underflow in fee_multiplier.
    function testFeeAbove10000Reverts() public {
        Outcome memory o = _executeRaw(ex, _calcCommands(20_000));
        assertFalse(o.ok, "Should revert - fee > 10000 is invalid");
    }

    /// fee = 0 is suspicious — no known V2 pair has zero fee.
    function testFeeZeroReverts() public {
        Outcome memory o = _executeRaw(ex, _calcCommands(0));
        assertFalse(o.ok, "Should revert - fee=0 is invalid");
    }

    /// fee > 10000 in V2_SWAP_COMPACT should revert clearly.
    function testSwapCompactFeeAbove10000Reverts() public {
        Cmd.Table memory at = _emptyTable();
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 exIdx = Cmd.add(at, address(ex));
        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCompact(pairIdx, true, 1000, exIdx, 20_000, ""));
        Outcome memory o = _executeRaw(ex, commands);
        assertFalse(o.ok, "Should revert - fee > 10000 is invalid");
    }
}
