// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./StubToken.sol";


interface IUniswapV2Callee {
    function uniswapV2Call(address sender, uint256 amount0, uint256 amount1, bytes calldata data)
        external;
}

/// Minimal Uniswap-V2-compatible pair (0.3% fee) faithfully reproducing
/// `v2-core/contracts/UniswapV2Pair.sol::swap` — the K-check, the optimistic
/// output transfer, the `uniswapV2Call` callback when `data.length > 0`, and
/// the `transferFrom(msg.sender,…)` input pull. Minimal only in that it omits
/// liquidity/mint/burn/transfer (unused by the executor); the swap path is
/// byte-faithful so the executor's `V2_SWAP_*` runs against it exactly as it
/// would against a real pair.
contract Pair {
    address public token0;
    address public token1;

    uint112 private reserve0;
    uint112 private reserve1;
    uint32 private blockTimestampLast;

    event Swap(
        address indexed sender,
        uint256 amount0In,
        uint256 amount1In,
        uint256 amount0Out,
        uint256 amount1Out,
        address indexed to
    );
    event Sync(uint112 reserve0, uint112 reserve1);

    function initialize(address tokenA, address tokenB) external {
        require(reserve0 == 0 && reserve1 == 0, "P:init");
        (token0, token1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
    }

    function getReserves() external view returns (uint112, uint112, uint32) {
        return (reserve0, reserve1, blockTimestampLast);
    }

    function sync() external {
        reserve0 = uint112(Token(token0).balanceOf(address(this)));
        reserve1 = uint112(Token(token1).balanceOf(address(this)));
        emit Sync(reserve0, reserve1);
    }

    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data)
        external
    {
        require(amount0Out > 0 || amount1Out > 0, "U:IOA");
        uint112 _r0 = reserve0;
        uint112 _r1 = reserve1;
        require(amount0Out < _r0 && amount1Out < _r1, "U:IL");
        require(to != token0 && to != token1, "U:IT");

        if (amount0Out > 0) Token(token0).transfer(to, amount0Out);
        if (amount1Out > 0) Token(token1).transfer(to, amount1Out);
        if (data.length > 0) {
            IUniswapV2Callee(to).uniswapV2Call(msg.sender, amount0Out, amount1Out, data);
        }

        uint256 balance0 = Token(token0).balanceOf(address(this));
        uint256 balance1 = Token(token1).balanceOf(address(this));
        uint256 amount0In =
            balance0 > _r0 - amount0Out ? balance0 - (_r0 - amount0Out) : 0;
        uint256 amount1In =
            balance1 > _r1 - amount1Out ? balance1 - (_r1 - amount1Out) : 0;
        require(amount0In > 0 || amount1In > 0, "U:IIA");

        uint256 balance0Adjusted = balance0 * 1000 - amount0In * 3;
        uint256 balance1Adjusted = balance1 * 1000 - amount1In * 3;
        require(
            balance0Adjusted * balance1Adjusted >= uint256(_r0) * uint256(_r1) * 1000 ** 2,
            "UniswapV2: K"
        );

        reserve0 = uint112(balance0);
        reserve1 = uint112(balance1);
        emit Swap(msg.sender, amount0In, amount1In, amount0Out, amount1Out, to);
    }
}
