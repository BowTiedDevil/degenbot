// SPDX-License-Identifier: MIT
// Generated from artifacts/UniswapV3Pool.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IUniswapV3Pool {
    event Burn(address indexed owner, int24 indexed tickLower, int24 indexed tickUpper, uint128 amount, uint256 amount0, uint256 amount1);
    event Collect(address indexed owner, address recipient, int24 indexed tickLower, int24 indexed tickUpper, uint128 amount0, uint128 amount1);
    event CollectProtocol(address indexed sender, address indexed recipient, uint128 amount0, uint128 amount1);
    event Flash(address indexed sender, address indexed recipient, uint256 amount0, uint256 amount1, uint256 paid0, uint256 paid1);
    event IncreaseObservationCardinalityNext(uint16 observationCardinalityNextOld, uint16 observationCardinalityNextNew);
    event Initialize(uint160 sqrtPriceX96, int24 tick);
    event Mint(address sender, address indexed owner, int24 indexed tickLower, int24 indexed tickUpper, uint128 amount, uint256 amount0, uint256 amount1);
    event SetFeeProtocol(uint8 feeProtocol0Old, uint8 feeProtocol1Old, uint8 feeProtocol0New, uint8 feeProtocol1New);
    event Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick);
    function burn(int24 tickLower, int24 tickUpper, uint128 amount) external returns (uint256 amount0, uint256 amount1);
    function collect(address recipient, int24 tickLower, int24 tickUpper, uint128 amount0Requested, uint128 amount1Requested) external returns (uint128 amount0, uint128 amount1);
    function collectProtocol(address recipient, uint128 amount0Requested, uint128 amount1Requested) external returns (uint128 amount0, uint128 amount1);
    function factory() external view returns (address r0);
    function fee() external view returns (uint24 r0);
    function feeGrowthGlobal0X128() external view returns (uint256 r0);
    function feeGrowthGlobal1X128() external view returns (uint256 r0);
    function flash(address recipient, uint256 amount0, uint256 amount1, bytes calldata data) external;
    function increaseObservationCardinalityNext(uint16 observationCardinalityNext) external;
    function initialize(uint160 sqrtPriceX96) external;
    function liquidity() external view returns (uint128 r0);
    function maxLiquidityPerTick() external view returns (uint128 r0);
    function mint(address recipient, int24 tickLower, int24 tickUpper, uint128 amount, bytes calldata data) external returns (uint256 amount0, uint256 amount1);
    function observations(uint256 a0) external view returns (uint32 blockTimestamp, int56 tickCumulative, uint160 secondsPerLiquidityCumulativeX128, bool initialized);
    function observe(uint32[] calldata secondsAgos) external view returns (int56[] memory tickCumulatives, uint160[] memory secondsPerLiquidityCumulativeX128s);
    function positions(bytes32 a0) external view returns (uint128 liquidity, uint256 feeGrowthInside0LastX128, uint256 feeGrowthInside1LastX128, uint128 tokensOwed0, uint128 tokensOwed1);
    function protocolFees() external view returns (uint128 token0, uint128 token1);
    function setFeeProtocol(uint8 feeProtocol0, uint8 feeProtocol1) external;
    function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);
    function snapshotCumulativesInside(int24 tickLower, int24 tickUpper) external view returns (int56 tickCumulativeInside, uint160 secondsPerLiquidityInsideX128, uint32 secondsInside);
    function swap(address recipient, bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96, bytes calldata data) external returns (int256 amount0, int256 amount1);
    function tickBitmap(int16 a0) external view returns (uint256 r0);
    function tickSpacing() external view returns (int24 r0);
    function ticks(int24 a0) external view returns (uint128 liquidityGross, int128 liquidityNet, uint256 feeGrowthOutside0X128, uint256 feeGrowthOutside1X128, int56 tickCumulativeOutside, uint160 secondsPerLiquidityOutsideX128, uint32 secondsOutside, bool initialized);
    function token0() external view returns (address r0);
    function token1() external view returns (address r0);
}
