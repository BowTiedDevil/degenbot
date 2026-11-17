// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";

/// Port of `tests/executor/test_token_sentinels.py`.
contract TokenSentinelsTest is PoolSetup {
    uint8 internal constant SELF = 0xFD;

    function _takeDeltaQuote(uint8 currencyIdx) internal {
        Cmd.Table memory at = _emptyTable();
        bytes memory commands = Cmd.v4Unlock(Cmd.v4TakeDelta(currencyIdx, SELF));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
    }

    function testUsdcSentinelResolvesWithoutTableEntries() public {
        _takeDeltaQuote(0xFA);
    }

    function testWbtcSentinelResolvesWithoutTableEntries() public {
        _takeDeltaQuote(0xFB);
    }

    function _settleAllIncludesSentinel(IFakeErc20 token, uint8 expectedIdx) internal {
        uint256 amountIn = 1 ether;
        uint256 amountOut = 2_000_000_000;
        PoolKey memory key = _poolKey(address(weth), address(token), 3000, 60);
        bool zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, amountIn, amountOut, zfo, address(token));
        weth.mint(address(executor), amountIn);

        Cmd.Table memory at = Cmd.tokenTable(address(usdc), address(wbtc));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 tokenIdx = Cmd.add(at, address(token));
        uint8 hooksIdx = Cmd.add(at, address(0));
        assertEq(uint256(tokenIdx), uint256(expectedIdx), "user sentinel index");

        bytes memory inner = Cmd.v4SwapCompact(
            key.currency0 == address(weth) ? wethIdx : tokenIdx,
            key.currency1 == address(token) ? tokenIdx : wethIdx,
            uint16(key.fee),
            int16(key.tick_spacing),
            hooksIdx,
            zfo,
            amountIn
        );
        inner = bytes.concat(inner, Cmd.v4SettleAll());

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(token.balanceOf(address(executor)), amountOut, "settle_all delivered the output");
    }

    function testSettleAllIncludesUsdcSentinel() public {
        _settleAllIncludesSentinel(usdc, 0xFA);
    }

    function testSettleAllIncludesWbtcSentinel() public {
        _settleAllIncludesSentinel(wbtc, 0xFB);
    }

    function testUnassignedReservedIndexReverts() public {
        Cmd.Table memory at = _emptyTable();
        bytes memory commands = Cmd.v4Unlock(Cmd.v4TakeDelta(0xF9, SELF));
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        // 0xF9 is below the sentinel threshold and unassigned: the address
        // index is out of bounds for the 32-slot table, fail-closed with no
        // revert data (the source's NO_REVERT_DATA identity).
        _assertRevertData(o, bytes(""), "unassigned reserved index");
    }
}
