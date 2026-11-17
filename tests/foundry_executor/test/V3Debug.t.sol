// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";

/// Port of `tests/executor/test_v3_debug.py`: the focused `v3c_zfo` debug
/// stream — a fake V3 pool priced so a WBTC-in swap pays out WETH, executed
/// end-to-end through the executor with the same price, liquidity, direction,
/// and recipient wiring as the source.
contract V3DebugTest is PoolSetup {
    // AMOUNT_WBTC (100e8) is the ExecutorFixture constant.
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    function testV3cZfo() public {
        (address t0, address t1) = _sorted(address(wbtc), address(weth));
        IFakeUniswapV3Pool v3c = _deployV3Pool(t0, t1, 0, 3000);
        bool zfo = v3c.token0() == address(wbtc);

        uint256 priceScaled;
        if (zfo) {
            // AMOUNT_WETH_PROFIT * Q96^2 * 10^18 / (AMOUNT_WBTC * 10^8). The
            // product overflows uint256, but the divisor divides
            // AMOUNT_WETH_PROFIT * 10^18 exactly, so factoring the division
            // first is the same integer.
            uint256 numerator = AMOUNT_WETH_PROFIT * 10 ** 18;
            uint256 divisor = AMOUNT_WBTC * 10 ** 8;
            assertEq(numerator % divisor, 0, "the factored division is exact");
            priceScaled = (numerator / divisor) * Q96 * Q96;
        } else {
            priceScaled = ((AMOUNT_WBTC * Q96) * Q96 * 10 ** 8) / (AMOUNT_WETH_PROFIT * 10 ** 18);
        }
        v3c.initialize(uint160(_isqrt(priceScaled)));

        wbtc.mint(address(v3c), AMOUNT_WBTC * 100);
        weth.mint(address(v3c), AMOUNT_WETH_PROFIT * 100);
        v3c.add_liquidity();

        wbtc.mint(address(executor), AMOUNT_WBTC);

        Cmd.Table memory at = Cmd.newTable(address(weth), address(executor), address(0));
        uint8 v3cIdx = Cmd.add(at, address(v3c));
        uint8 ownerIdx = Cmd.add(at, address(this));

        bytes memory full = bytes.concat(
            Cmd.preamble(at), Cmd.v3SwapCompact(v3cIdx, zfo, AMOUNT_WBTC, ownerIdx, "")
        );
        Outcome memory o = _executeRaw(executor, full, 1);
        assertTrue(o.ok, "V3c zfo swap failed!");
    }
}
