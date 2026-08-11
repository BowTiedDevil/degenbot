// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./StubToken.sol";

/// Full math — copied verbatim from v3-core `FullMath.sol` (the proven
/// 512-bit mulDiv), used by the V3 price math below.
library FullMath {
    /// `mulDiv` — the whole body is `unchecked` so the 512-bit Newton/`inv`
    /// multiplications wrap (the audited v3-core `FullMath.sol` relies on
    /// wrapping; a checked `prod0 * inv` panics `Panic(0x11)` for large V3
    /// numerators like `liquidity << 96`).
    function mulDiv(uint256 a, uint256 b, uint256 denominator)
        internal pure returns (uint256 result)
    {
        unchecked {
        uint256 prod0;
        uint256 prod1;
        assembly {
            let mm := mulmod(a, b, not(0))
            prod0 := mul(a, b)
            prod1 := sub(sub(mm, prod0), lt(mm, prod0))
        }
        if (prod1 == 0) {
            require(denominator > 0);
            assembly { result := div(prod0, denominator) }
            return result;
        }
        require(denominator > prod1);
        uint256 remainder;
        assembly {
            remainder := mulmod(a, b, denominator)
            prod1 := sub(prod1, gt(remainder, prod0))
            prod0 := sub(prod0, remainder)
        }
        uint256 twos = denominator & (~denominator + 1);
        assembly { denominator := div(denominator, twos) }
        assembly { prod0 := div(prod0, twos) }
        assembly { twos := add(div(sub(0, twos), twos), 1) }
        prod0 |= prod1 * twos;
        uint256 inv = (3 * denominator) ^ 2;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        result = prod0 * inv;
        return result;
        }
    }

    function mulDivRoundingUp(uint256 a, uint256 b, uint256 denominator)
        internal pure returns (uint256 result)
    {
        result = mulDiv(a, b, denominator);
        if (mulmod(a, b, denominator) > 0) {
            require(result < type(uint256).max);
            result++;
        }
    }
}

library UnsafeMath {
    function divRoundingUp(uint256 x, uint256 y) internal pure returns (uint256 result) {
        assembly { result := add(div(x, y), gt(mod(x, y), 0)) }
    }
}

library FixedPoint96 { uint8 internal constant RESOLUTION = 96; uint256 internal constant Q96 = 0x1000000000000000000000000; }

/// SqrtPriceMath — the V3 price/amount functions, copied verbatim from
/// v3-core `SqrtPriceMath.sol` (minus the signed-liquidity overloads the swap
/// path doesn't need).
library SqrtPriceMath {
    function getNextSqrtPriceFromAmount0RoundingUp(
        uint160 sqrtPX96, uint128 liquidity, uint256 amount, bool add
    ) internal pure returns (uint160) {
        if (amount == 0) return sqrtPX96;
        uint256 numerator1 = uint256(liquidity) << FixedPoint96.RESOLUTION;
        if (add) {
            uint256 product;
            if ((product = amount * sqrtPX96) / amount == sqrtPX96) {
                uint256 denominator = numerator1 + product;
                if (denominator >= numerator1)
                    return uint160(FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator));
            }
            return uint160(UnsafeMath.divRoundingUp(numerator1, (numerator1 / sqrtPX96) + amount));
        } else {
            uint256 product;
            require((product = amount * sqrtPX96) / amount == sqrtPX96 && numerator1 > product);
            uint256 denominator = numerator1 - product;
            return uint160(FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator));
        }
    }

    function getNextSqrtPriceFromAmount1RoundingDown(
        uint160 sqrtPX96, uint128 liquidity, uint256 amount, bool add
    ) internal pure returns (uint160) {
        if (add) {
            uint256 quotient = amount <= type(uint160).max
                ? (amount << FixedPoint96.RESOLUTION) / liquidity
                : FullMath.mulDiv(amount, FixedPoint96.Q96, liquidity);
            return uint160(uint256(sqrtPX96) + quotient);
        } else {
            uint256 quotient = amount <= type(uint160).max
                ? UnsafeMath.divRoundingUp(amount << FixedPoint96.RESOLUTION, liquidity)
                : FullMath.mulDivRoundingUp(amount, FixedPoint96.Q96, liquidity);
            require(sqrtPX96 > quotient);
            return uint160(sqrtPX96 - quotient);
        }
    }

    function getNextSqrtPriceFromInput(uint160 sqrtPX96, uint128 liquidity, uint256 amountIn, bool zeroForOne)
        internal pure returns (uint160)
    {
        require(sqrtPX96 > 0); require(liquidity > 0);
        return zeroForOne
            ? getNextSqrtPriceFromAmount0RoundingUp(sqrtPX96, liquidity, amountIn, true)
            : getNextSqrtPriceFromAmount1RoundingDown(sqrtPX96, liquidity, amountIn, true);
    }

    function getAmount0Delta(uint160 sqrtRatioAX96, uint160 sqrtRatioBX96, uint128 liquidity, bool roundUp)
        internal pure returns (uint256 amount0)
    {
        if (sqrtRatioAX96 > sqrtRatioBX96) (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96);
        uint256 numerator1 = uint256(liquidity) << FixedPoint96.RESOLUTION;
        uint256 numerator2 = sqrtRatioBX96 - sqrtRatioAX96;
        require(sqrtRatioAX96 > 0);
        return roundUp
            ? UnsafeMath.divRoundingUp(FullMath.mulDivRoundingUp(numerator1, numerator2, sqrtRatioBX96), sqrtRatioAX96)
            : FullMath.mulDiv(numerator1, numerator2, sqrtRatioBX96) / sqrtRatioAX96;
    }

    function getAmount1Delta(uint160 sqrtRatioAX96, uint160 sqrtRatioBX96, uint128 liquidity, bool roundUp)
        internal pure returns (uint256 amount1)
    {
        if (sqrtRatioAX96 > sqrtRatioBX96) (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96);
        return roundUp
            ? FullMath.mulDivRoundingUp(liquidity, sqrtRatioBX96 - sqrtRatioAX96, FixedPoint96.Q96)
            : FullMath.mulDiv(liquidity, sqrtRatioBX96 - sqrtRatioAX96, FixedPoint96.Q96);
    }
}

interface IUniswapV3Callback {
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external;
}

/// Minimal Uniswap-V3-faithful pool (single active tick range) for the executor
/// grammar harness (UQOAHA). Reproduces the exact-input `swap` custody + the
/// `uniswapV3SwapCallback` sign/IIA contract of the real pool (v3-core) using
/// the proven `SqrtPriceMath`; liquidity is constant (one full-range bin, no
/// tick crossing). The swap MATH is already proven by the tier-3 V3 oracle, so
/// this stub only needs to be executor-faithful in ordering + callbacks.
contract PoolV3 {
    address public token0;
    address public token1;
    uint24 public fee;
    uint160 public sqrtPriceX96;
    uint128 public liquidity;

    constructor() {}

    function initialize(address t0, address t1, uint24 fee_) external {
        require(t0 != address(0));
        token0 = t0; token1 = t1; fee = fee_;
    }
    // Seeding hooks (the tier-3 oracles seed via setup; here the harness sets
    // the price + liquidity directly then `sync`-style no-ops).
    function setPrice(uint160 p) external { sqrtPriceX96 = p; }
    function setLiquidity(uint128 l) external { liquidity = l; }

    function slot0() external view returns (
        uint160, int24, uint16, uint16, uint16, uint8, bool
    ) {
        return (sqrtPriceX96, 0, 0, 0, 0, 0, true);
    }

    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1) {
        require(amountSpecified > 0, "AS"); // harness drives only exact-input
        uint160 current = sqrtPriceX96;
        require(zeroForOne ? sqrtPriceLimitX96 < current : sqrtPriceLimitX96 > current, "SPL");

        uint160 target = sqrtPriceLimitX96;
        uint128 liq = liquidity;
        uint256 amountRemaining = uint256(amountSpecified);
        uint256 amountRemainingLessFee = FullMath.mulDiv(amountRemaining, 1e6 - fee, 1e6);

        uint160 next;
        uint256 amountIn;
        uint256 amountOut;
        if (zeroForOne) {
            amountIn = SqrtPriceMath.getAmount0Delta(target, current, liq, true);
            next = amountRemainingLessFee >= amountIn
                ? target
                : SqrtPriceMath.getNextSqrtPriceFromInput(current, liq, amountRemainingLessFee, true);
            amountIn = SqrtPriceMath.getAmount0Delta(next, current, liq, true);
            amountOut = SqrtPriceMath.getAmount1Delta(next, current, liq, false);
        } else {
            amountIn = SqrtPriceMath.getAmount1Delta(current, target, liq, true);
            next = amountRemainingLessFee >= amountIn
                ? target
                : SqrtPriceMath.getNextSqrtPriceFromInput(current, liq, amountRemainingLessFee, false);
            amountIn = SqrtPriceMath.getAmount1Delta(current, next, liq, true);
            amountOut = SqrtPriceMath.getAmount0Delta(current, next, liq, false);
        }

        // exact-input, no tick crossing → we always consume the full input.
        // amount0/amount1 signed per v3 convention (positive = pool's input).
        if (zeroForOne) {
            amount0 = int256(amountSpecified);
            amount1 = -int256(amountOut);
            Token(token1).transfer(recipient, amountOut); // pool sends output
        } else {
            amount0 = -int256(amountOut);
            amount1 = int256(amountSpecified);
            Token(token0).transfer(recipient, amountOut);
        }

        sqrtPriceX96 = next;

        bytes memory balanceCheck = abi.encodeWithSignature("balanceOf(address)", address(this));
        uint256 balBefore = _erc20Balance(token0, balanceCheck);
        IUniswapV3Callback(recipient).uniswapV3SwapCallback(amount0, amount1, data);
        uint256 balAfter = _erc20Balance(token0, balanceCheck);
        // IIA: the callback must have paid the input into the pool.
        uint256 expected = zeroForOne ? uint256(amount0) : 0;
        require(balBefore + expected <= balAfter, "IIA");
        emit SwapV3(zeroForOne ? token0 : token1, amountOut, sqrtPriceX96, amountSpecified);
    }

    event SwapV3(address inputToken, uint256 amountOut, uint160 sqrtPriceX96, int256 amountSpecified);

    function _erc20Balance(address tok, bytes memory sel) internal view returns (uint256) {
        (bool ok, bytes memory ret) = tok.staticcall(sel);
        require(ok && ret.length >= 32);
        return abi.decode(ret, (uint256));
    }
}

/// Diagnostic driver that calls a `PoolV3` directly, acting as the swap
/// recipient + paying the pool back via `uniswapV3SwapCallback` — isolates the
/// stub's swap math from any executor ambiguity. Typed call so a `Panic`
/// propagates unchanged.
interface IV3ExecLike {
    function swap(address, bool, int256, uint160, bytes calldata) external returns (int256, int256);
    function token0() external view returns (address);
    function token1() external view returns (address);
}
contract TestV3SwapDriver {
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata) external {
        address t0 = IV3ExecLike(msg.sender).token0();
        address t1 = IV3ExecLike(msg.sender).token1();
        if (amount0Delta > 0) {
            Token(t0).transfer(msg.sender, uint256(amount0Delta));
        } else {
            Token(t1).transfer(msg.sender, uint256(amount1Delta));
        }
    }
    function doSwap(address pool, bool zfo, int256 amount, bool zfoLimit) external returns (int256, int256) {
        // MIN_SQRT_RATIO+1 for zfo / MAX_SQRT_RATIO-1 otherwise (same as the executor).
        uint160 lim = zfoLimit
            ? 4295128740
            : 1461446703485210103287273052203988822378723970341;
        return IV3ExecLike(pool).swap(address(this), zfo, amount, lim, abi.encodePacked(uint8(0)));
    }
}
