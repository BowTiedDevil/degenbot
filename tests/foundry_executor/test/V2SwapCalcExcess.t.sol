// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_v2_swap_calc_excess.py`.
///
/// V2_SWAP_CALC with the excess-balance approach:
/// - V2 direct: the executor pre-funds the pair, V2_SWAP_CALC reads the excess
/// - V4→V2: V4_TAKE sends USDC directly to the V2 pair, creating excess
/// - V2→V2: the first swap sends output to the second pair, which reads it
/// - K-invariant: the pair verifies the constant-product invariant at runtime,
///   so amounts computed by V2_SWAP_CALC always satisfy K
contract V2SwapCalcExcessTest is PoolSetup {
    // AMOUNT_WETH (1 ether) / AMOUNT_USDC (2000e6) are the fixture constants.
    uint256 internal constant SWAP_INPUT = 1 ether;

    IFakeUniswapV2Pair internal v2Pair;

    function setUp() public override {
        super.setUp();
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        v2Pair = _deployV2Pair(t0, t1, 0, 30);
    }

    /// V2 direct swap: pre-fund the V2 pair with WETH, then V2_SWAP_CALC reads
    /// the excess and swaps.
    function testV2DirectExcessBalance() public {
        bool v2Zfo = v2Pair.token0() == address(weth);

        usdc.mint(address(v2Pair), AMOUNT_USDC);
        weth.mint(address(v2Pair), AMOUNT_WETH);
        v2Pair.sync();

        uint256 reserveIn = weth.balanceOf(address(v2Pair)); // selling WETH
        uint256 reserveOut = usdc.balanceOf(address(v2Pair)); // buying USDC
        uint256 expectedOut = _v2GetAmountOut(SWAP_INPUT, reserveIn, reserveOut, 30);

        // Deposit input tokens after sync, so reserves do not include them.
        weth.mint(address(v2Pair), SWAP_INPUT);

        Cmd.Table memory at = _emptyTable();
        uint8 v2Idx = Cmd.add(at, address(v2Pair));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(v2Idx, v2Zfo, executorIdx, 30));
        _execute(executor, commands);

        assertGe(usdc.balanceOf(address(executor)), expectedOut, "USDC output covers expected amount");
    }

    /// V2_SWAP_CALC reverts when the pair has no excess balance.
    function testV2DirectExcessBalanceRevertsWithoutDeposit() public {
        bool v2Zfo = v2Pair.token0() == address(weth);

        // Add liquidity only — no excess deposit.
        usdc.mint(address(v2Pair), AMOUNT_USDC);
        weth.mint(address(v2Pair), AMOUNT_WETH);
        v2Pair.sync();

        Cmd.Table memory at = _emptyTable();
        uint8 v2Idx = Cmd.add(at, address(v2Pair));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(v2Idx, v2Zfo, executorIdx, 30));
        Outcome memory o = _executeRaw(executor, commands);
        assertFalse(o.ok, "Should revert - no excess balance");
    }

    /// V2_SWAP_CALC with excess balance calls swap() with data = b"" (no
    /// callback), so the executor's WETH balance never moves — the profit is
    /// USDC, not spent WETH.
    function testV2DirectNoCallbackFires() public {
        bool v2Zfo = v2Pair.token0() == address(weth);

        usdc.mint(address(v2Pair), AMOUNT_USDC);
        weth.mint(address(v2Pair), AMOUNT_WETH);
        v2Pair.sync();

        uint256 reserveIn = weth.balanceOf(address(v2Pair));
        uint256 reserveOut = usdc.balanceOf(address(v2Pair));
        uint256 expectedOut = _v2GetAmountOut(SWAP_INPUT, reserveIn, reserveOut, 30);

        weth.mint(address(v2Pair), SWAP_INPUT);

        uint256 wethBefore = weth.balanceOf(address(executor));

        Cmd.Table memory at = _emptyTable();
        uint8 v2Idx = Cmd.add(at, address(v2Pair));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(v2Idx, v2Zfo, executorIdx, 30));
        _execute(executor, commands);

        assertEq(
            weth.balanceOf(address(executor)),
            wethBefore,
            "no callback fired - executor WETH untouched"
        );
        assertGe(usdc.balanceOf(address(executor)), expectedOut, "USDC output covers expected amount");
    }

    /// V4→V2 direct custody: V4_TAKE sends USDC to the V2 pair → V2_SWAP_CALC
    /// reads the excess.
    function testV4V2DirectCustodyTake() public {
        weth.mint(address(executor), AMOUNT_WETH);
        PoolKey memory key = _poolKey(address(weth), address(usdc), 3000, 60);
        bool poolAZfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, poolAZfo, address(usdc));

        bool v2Zfo = v2Pair.token0() == address(usdc);
        usdc.mint(address(v2Pair), 10_000e6);
        weth.mint(address(v2Pair), 5 ether);
        v2Pair.sync();

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v2Idx = Cmd.add(at, address(v2Pair));
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            zeroIdx,
            poolAZfo,
            AMOUNT_WETH
        );
        inner = bytes.concat(inner, Cmd.v4Take(usdcIdx, v2Idx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v2SwapCalc(v2Idx, v2Zfo, executorIdx, 30));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        // V2 custody: the executor holds tokens for V2 without profit.
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
    }

    /// V4→V2 direct custody via V4_TAKE_DELTA: sends USDC to the V2 pair →
    /// V2_SWAP_CALC reads the excess.
    function testV4V2DirectCustodyTakeDelta() public {
        weth.mint(address(executor), AMOUNT_WETH);
        PoolKey memory key = _poolKey(address(weth), address(usdc), 3000, 60);
        bool poolAZfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, poolAZfo, address(usdc));

        bool v2Zfo = v2Pair.token0() == address(usdc);
        usdc.mint(address(v2Pair), 10_000e6);
        weth.mint(address(v2Pair), 5 ether);
        v2Pair.sync();

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v2Idx = Cmd.add(at, address(v2Pair));
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            zeroIdx,
            poolAZfo,
            AMOUNT_WETH
        );
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, v2Idx));
        inner = bytes.concat(inner, Cmd.v2SwapCalc(v2Idx, v2Zfo, executorIdx, 30));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
    }

    /// V2→V2 chain: the first swap sends USDC directly to the second V2 pair,
    /// whose V2_SWAP_CALC reads it as excess.
    function testV2V2DirectCustody() public {
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        IFakeUniswapV2Pair v2Pair2 = _deployV2Pair(t0, t1, 0, 30);

        // First pair: sell WETH → USDC.
        bool v2_1Zfo = v2Pair.token0() == address(weth);
        usdc.mint(address(v2Pair), 10_000e6);
        weth.mint(address(v2Pair), 5 ether);
        v2Pair.sync();
        // Pre-fund the first pair with WETH (creates excess).
        weth.mint(address(v2Pair), SWAP_INPUT);

        // Second pair: sell USDC → WETH.
        bool v2_2Zfo = v2Pair2.token0() == address(usdc);
        usdc.mint(address(v2Pair2), 20_000e6);
        weth.mint(address(v2Pair2), 10 ether);
        v2Pair2.sync();

        Cmd.Table memory at = _emptyTable();
        uint8 v2_1Idx = Cmd.add(at, address(v2Pair));
        uint8 v2_2Idx = Cmd.add(at, address(v2Pair2));
        uint8 executorIdx = Cmd.add(at, address(executor));

        // V2_SWAP_CALC on pair 1: reads excess WETH, sends USDC to pair 2.
        // V2_SWAP_CALC on pair 2: reads excess USDC, sends WETH to executor.
        bytes memory commands = Cmd.preamble(at);
        commands = bytes.concat(commands, Cmd.v2SwapCalc(v2_1Idx, v2_1Zfo, v2_2Idx, 30));
        commands = bytes.concat(commands, Cmd.v2SwapCalc(v2_2Idx, v2_2Zfo, executorIdx, 30));

        _execute(executor, commands);
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    /// V2_SWAP_CALC reads the total excess when multiple deposits create it.
    function testMultipleDepositsCreateExcess() public {
        bool v2Zfo = v2Pair.token0() == address(weth);

        usdc.mint(address(v2Pair), AMOUNT_USDC);
        weth.mint(address(v2Pair), AMOUNT_WETH);
        v2Pair.sync();

        uint256 deposit1 = 5e17; // 0.5 WETH
        uint256 deposit2 = 5e17; // 0.5 WETH
        uint256 totalDeposit = deposit1 + deposit2; // 1 WETH

        uint256 reserveIn = weth.balanceOf(address(v2Pair));
        uint256 reserveOut = usdc.balanceOf(address(v2Pair));
        uint256 expectedOut = _v2GetAmountOut(totalDeposit, reserveIn, reserveOut, 30);

        // Deposit in two transactions (pairs are stateful).
        weth.mint(address(v2Pair), deposit1);
        weth.mint(address(v2Pair), deposit2);

        Cmd.Table memory at = _emptyTable();
        uint8 v2Idx = Cmd.add(at, address(v2Pair));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(v2Idx, v2Zfo, executorIdx, 30));
        _execute(executor, commands);

        assertGe(usdc.balanceOf(address(executor)), expectedOut, "USDC output covers expected amount");
    }
}
