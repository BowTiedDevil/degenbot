// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IUniswapV2Factory} from "../src/generated/IUniswapV2Factory.sol";
import {IUniswapV2Pair} from "../src/generated/IUniswapV2Pair.sol";
import {IUniswapV3Factory} from "../src/generated/IUniswapV3Factory.sol";
import {IUniswapV3Pool} from "../src/generated/IUniswapV3Pool.sol";

/// The executor against CANONICAL Uniswap bytecode: `UniswapV2Pair` (solc
/// 0.5.16) and `UniswapV3Pool` (solc 0.7.6), compiled one-off by
/// `build_solidity_artifacts.py` with the pinned binaries and deployed here from
/// their artifacts. No forge solc manager, no proxy contract in the path.
contract RealLibrariesTest is PoolSetup {
    uint256 internal constant ONE_WETH = 1 ether;

    // ── V2: canonical UniswapV2Pair ──────────────────────────────────────

    function testExecutorDrivesCanonicalV2Pair() public {
        // The canonical pair asks its factory for `feeTo()` (protocol-fee
        // accounting), so it needs the real factory behind it — deploying the
        // pair directly would make this test contract the factory and the
        // `mint` path would revert on the missing `feeTo()` selector.
        IUniswapV2Factory factory = IUniswapV2Factory(
            _deploy("UniswapV2Factory", abi.encode(address(this)))
        );
        IUniswapV2Pair pair =
            IUniswapV2Pair(factory.createPair(address(usdc), address(weth)));

        usdc.mint(address(pair), 200_000e6);
        weth.mint(address(pair), 100 ether);
        pair.mint(address(this));

        uint256 amountIn = 2_000e6;
        bool zfo = pair.token0() == address(usdc);
        (uint112 r0, uint112 r1,) = pair.getReserves();
        uint256 amountOut = _v2GetAmountOut(amountIn, zfo ? r0 : r1, zfo ? r1 : r0, 30);
        assertGt(amountOut, 0, "canonical pair quotes a nonzero output");

        usdc.mint(address(executor), amountIn);

        Cmd.Table memory at = _emptyTable();
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands = Cmd.erc20Transfer(usdcIdx, pairIdx, amountIn);
        commands = bytes.concat(
            commands, Cmd.v2SwapCompact(pairIdx, zfo, amountOut, executorIdx, 30, "")
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);
        assertEq(weth.balanceOf(address(executor)), amountOut, "canonical pair paid the executor");
    }

    // ── V3: canonical UniswapV3Factory + UniswapV3Pool ───────────────────

    function uniswapV3MintCallback(uint256 amount0Owed, uint256 amount1Owed, bytes calldata)
        external
    {
        if (amount0Owed > 0) {
            IFakeErc20(IUniswapV3Pool(msg.sender).token0()).transfer(msg.sender, amount0Owed);
        }
        if (amount1Owed > 0) {
            IFakeErc20(IUniswapV3Pool(msg.sender).token1()).transfer(msg.sender, amount1Owed);
        }
    }

    function testExecutorDrivesCanonicalV3Pool() public {
        IUniswapV3Factory factory = IUniswapV3Factory(_deploy("UniswapV3Factory", ""));
        address poolAddr = factory.createPool(address(usdc), address(weth), uint24(3000));
        IUniswapV3Pool pool = IUniswapV3Pool(poolAddr);

        bool zfo = pool.token0() == address(usdc);
        uint256 sqrtP = zfo
            ? _isqrt((ONE_WETH * Q96 * Q96) / 2_000e6)
            : _isqrt((2_000e6 * Q96 * Q96) / ONE_WETH);
        pool.initialize(uint160(sqrtP));

        (uint160 sp, int24 tick,,,,,) = pool.slot0();
        assertGt(uint256(sp), 0, "pool priced");
        int24 spacing = pool.tickSpacing();
        int24 base = (tick / spacing) * spacing;
        usdc.mint(address(this), 1e30);
        weth.mint(address(this), 1e30);
        pool.mint(address(this), base - 600, base + 600, uint128(1e18), "");
        assertGt(pool.liquidity(), 0, "canonical pool has liquidity");

        uint256 amountIn = 2_000e6 / 10;
        usdc.mint(address(executor), amountIn);
        uint256 wethBefore = weth.balanceOf(address(executor));

        Cmd.Table memory at = _emptyTable();
        uint8 poolIdx = Cmd.add(at, address(pool));
        uint8 executorIdx = Cmd.add(at, address(executor));

        // Empty forward_data = the executor's V3 auto-pay path: the pool's
        // callback delta is paid from the executor's own balance.
        bytes memory commands = Cmd.v3SwapCompact(poolIdx, zfo, amountIn, executorIdx, "");
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);

        assertGt(weth.balanceOf(address(executor)), wethBefore, "canonical pool paid WETH out");
        assertEq(usdc.balanceOf(address(executor)), 0, "executor paid its USDC input");
    }
}
