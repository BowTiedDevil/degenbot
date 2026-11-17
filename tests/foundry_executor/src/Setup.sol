// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ExecutorFixture} from "./TestBase.sol";
import {Cmd} from "./Cmd.sol";
import {IFakeErc20} from "./generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "./generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "./generated/IFakeUniswapV3Pool.sol";
import {IFakeUniswapV4PoolManager} from "./generated/IFakeUniswapV4PoolManager.sol";
import {IFakeWeth} from "./generated/IFakeWeth.sol";
import {PoolKey} from "./generated/Types.sol";

/// Pool setup helpers — the port of `conftest_shared.py`'s `_setup_*` family.
abstract contract PoolSetup is ExecutorFixture {
    uint256 internal constant Q96 = 79228162514264337593543950336;
    uint160 internal constant MIN_SQRT_PRICE_X96 = 4295128739;
    uint160 internal constant MAX_SQRT_PRICE_X96 =
        1461446703485210103287273052203988822378723970342;

    // ── Math ─────────────────────────────────────────────────────────────

    function _isqrt(uint256 n) internal pure returns (uint256) {
        if (n < 2) return n;
        uint256 x = n;
        uint256 y = (x + 1) / 2;
        while (y < x) {
            x = y;
            y = (x + n / x) / 2;
        }
        return x;
    }

    function _v2GetAmountOut(uint256 amountIn, uint256 reserveIn, uint256 reserveOut, uint16 fee)
        internal
        pure
        returns (uint256)
    {
        uint256 fm = 10_000 - fee;
        return (amountIn * fm * reserveOut) / (reserveIn * 10_000 + amountIn * fm);
    }

    function _v2ReservesForCannedSwap(
        uint256 amountIn,
        uint256 amountOut,
        uint16 fee,
        uint256 liquidityFactor
    ) internal pure returns (uint256 reserveIn, uint256 reserveOut) {
        uint256 fm = 10_000 - fee;
        reserveOut = amountOut * liquidityFactor;
        reserveIn = (amountIn * fm * (reserveOut - amountOut)) / (amountOut * 10_000);
    }

    // ── Deployments ──────────────────────────────────────────────────────

    function _deployErc20(string memory name, string memory symbol, uint8 decimals, uint256 supply)
        internal
        returns (IFakeErc20)
    {
        return IFakeErc20(_deploy("FakeErc20", abi.encode(name, symbol, decimals, supply)));
    }

    function _deployV2Pair(address token0, address token1, uint256 callbackVariant, uint256 fee)
        internal
        returns (IFakeUniswapV2Pair)
    {
        return IFakeUniswapV2Pair(
            _deploy("FakeUniswapV2Pair", abi.encode(token0, token1, callbackVariant, fee))
        );
    }

    function _deployV3Pool(address token0, address token1, uint256 callbackVariant, uint24 fee)
        internal
        returns (IFakeUniswapV3Pool)
    {
        return IFakeUniswapV3Pool(
            _deploy("FakeUniswapV3Pool", abi.encode(token0, token1, callbackVariant, fee))
        );
    }

    function _deployV4PoolManager(address token0, address token1)
        internal
        returns (IFakeUniswapV4PoolManager)
    {
        return IFakeUniswapV4PoolManager(
            _deploy("FakeUniswapV4PoolManager", abi.encode(token0, token1))
        );
    }

    // ── Pool setup ───────────────────────────────────────────────────────

    /// Fund the pair, sync reserves, and return the K-invariant-consistent output.
    function _setupV2Pair(
        IFakeUniswapV2Pair pair,
        IFakeErc20 inputToken,
        IFakeErc20 outputToken,
        uint256 amountIn,
        uint16 fee
    ) internal returns (bool zfo, uint256 amountOut) {
        inputToken.mint(address(pair), amountIn * 100);
        outputToken.mint(address(pair), amountIn * 100);
        pair.sync();

        zfo = pair.token0() == address(inputToken);
        uint256 reserveIn = inputToken.balanceOf(address(pair));
        uint256 reserveOut = outputToken.balanceOf(address(pair));
        amountOut = _v2GetAmountOut(amountIn, reserveIn, reserveOut, fee);
    }

    /// Initialize a V3 pool at the price implied by `amountIn`/`amountOut` and
    /// provide `liquidityFactor`x liquidity.
    function _setupV3(
        IFakeUniswapV3Pool pool,
        IFakeErc20 inputToken,
        IFakeErc20 outputToken,
        uint256 amountIn,
        uint256 amountOut,
        uint256 liquidityFactor
    ) internal returns (bool zfo, uint256 actualOut) {
        zfo = pool.token0() == address(inputToken);
        uint256 sqrtPriceX96;
        if (zfo) {
            sqrtPriceX96 = _isqrt(amountOut * Q96 * Q96 / amountIn);
        } else {
            sqrtPriceX96 = _isqrt(amountIn * Q96 * Q96 / amountOut);
        }
        pool.initialize(uint160(sqrtPriceX96));

        inputToken.mint(address(pool), amountIn * liquidityFactor);
        outputToken.mint(address(pool), amountOut * liquidityFactor);
        pool.add_liquidity();

        actualOut = pool.get_amount_out(amountIn, zfo);
    }

    function _setupV3(
        IFakeUniswapV3Pool pool,
        IFakeErc20 inputToken,
        IFakeErc20 outputToken,
        uint256 amountIn,
        uint256 amountOut
    ) internal returns (bool, uint256) {
        return _setupV3(pool, inputToken, outputToken, amountIn, amountOut, 100);
    }

    /// Program the fake PoolManager's next swap and fund the output side.
    function _setupV4Swap(
        IFakeUniswapV4PoolManager pm,
        PoolKey memory key,
        uint256 amountIn,
        uint256 amountOut,
        bool zfo,
        address outputToken
    ) internal {
        if (outputToken == address(0)) {
            vm.deal(address(pm), address(pm).balance + amountOut);
        } else {
            IFakeErc20(outputToken).mint(address(pm), amountOut);
        }
        pm.set_next_swap(key, amountIn, amountOut, zfo, "");
    }

    function _emptyTable() internal pure returns (Cmd.Table memory t) {
        // default-constructed memory struct: every field zero, no table entries
    }
}
