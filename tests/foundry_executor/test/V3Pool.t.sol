// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";

/// Port of `tests/executor/test_v3_pool.py`: the fake V3 pool computes swap
/// outputs with the real V3 computeSwapStep (SqrtPriceMath + FullMath) rather
/// than pre-configured amounts, and every swap goes through the executor —
/// the pool always invokes the callback on msg.sender, so direct calls from
/// an EOA revert exactly like the real UniswapV3Pool.
contract V3PoolTest is PoolSetup {
    uint256 internal constant FEE_30BIPS = 3000;
    uint256 internal constant WETH_LIQUIDITY = 100 ether;
    uint256 internal constant USDC_LIQUIDITY = 200_000e6;

    IFakeUniswapV3Pool internal pool;

    function setUp() public override {
        super.setUp();
        (address t0, address t1) = _sorted(address(weth), address(usdc));
        pool = _deployV3Pool(t0, t1, 0, uint24(FEE_30BIPS));

        // 2000 USDC per WETH: token1/token0 in raw units is 1e18 / 2000e6 = 5e8.
        // The source derives sqrtPriceX96 through Python float math; the exact
        // integer isqrt of 5e8 * Q96^2 is the same price without that rounding.
        if (pool.token0() == address(usdc)) {
            pool.initialize(uint160(_isqrt(uint256(5e8) * Q96 * Q96)));
        } else {
            uint256 primarySqrt = _isqrt(uint256(5e8) * Q96 * Q96);
            pool.initialize(uint160((uint256(Q96) * Q96) / primarySqrt));
        }

        weth.mint(address(pool), WETH_LIQUIDITY);
        usdc.mint(address(pool), USDC_LIQUIDITY);
        pool.add_liquidity();
    }

    function testInitializeAndLiquidity() public view {
        assertGt(uint256(pool.sqrt_price_x96()), 0, "sqrt price set");
        assertGt(pool.liquidity(), 0, "liquidity added");
        assertEq(pool.fee(), uint24(FEE_30BIPS), "fee");
    }

    function testSwapWethForUsdc() public {
        bool swapZfo = pool.token0() == address(weth);

        uint160 sqrtPriceBefore = pool.sqrt_price_x96();
        uint256 poolUsdcBefore = usdc.balanceOf(address(pool));
        uint256 poolWethBefore = weth.balanceOf(address(pool));

        Cmd.Table memory at = Cmd.newTable(address(weth), address(executor), address(0));
        uint8 poolIdx = Cmd.add(at, address(pool));
        uint8 ownerIdx = Cmd.add(at, address(this));

        weth.mint(address(executor), AMOUNT_WETH);
        bytes memory commands = Cmd.v3SwapCompact(poolIdx, swapZfo, AMOUNT_WETH, ownerIdx, "");
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        // Price moved in the correct direction.
        uint160 sqrtPriceAfter = pool.sqrt_price_x96();
        if (swapZfo) {
            assertLt(sqrtPriceAfter, sqrtPriceBefore, "selling token0 moves price down");
        } else {
            assertGt(sqrtPriceAfter, sqrtPriceBefore, "selling token1 moves price up");
        }

        // Pool received WETH.
        assertGt(weth.balanceOf(address(pool)), poolWethBefore, "pool received WETH");

        // Pool sent USDC (~1974 USDC for 1 WETH at 0.3% fee); the 2% window
        // matches the source's Python-reference tolerance.
        uint256 usdcOutput = poolUsdcBefore - usdc.balanceOf(address(pool));
        assertGt(usdcOutput, 0, "pool sent USDC");

        uint256 expectedApprox =
            (AMOUNT_WETH * 2000 * (1_000_000 - FEE_30BIPS) * 987) / (1_000_000 * 10 ** 12 * 1000);
        uint256 tolerance = (expectedApprox * 2) / 100;
        uint256 diff =
            usdcOutput > expectedApprox ? usdcOutput - expectedApprox : expectedApprox - usdcOutput;
        assertTrue(diff < tolerance, "USDC output not within 2% of expected");
    }
}
