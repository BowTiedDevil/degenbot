// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ICmdExecutor} from "../src/generated/ICmdExecutor.sol";
import {IFakeUniswapV4PoolManager} from "../src/generated/IFakeUniswapV4PoolManager.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_v4v4.py`.
///
/// V4→V4 shapes over a WBTC/WETH pair space: same-currency WETH→WBTC→WETH
/// netting, V4_BATCH multi-swap with auto-settle (explicit and dynamic
/// amounts), cross-currency legs through native ETH, the USDC-intermediate
/// batch, and the WETH→USDC→WBTC→WETH three-hop triangular arb inside one
/// PoolManager.
///
/// The source module overrides the conftest `v4_pm` fixture with a WBTC/WETH
/// PoolManager (the `executor` fixture is built on it) — preserved in setUp.
contract CmdExecutorV4V4Test is PoolSetup {
    uint256 internal constant AMOUNT_WETH_10 = 10 ether;
    uint256 internal constant AMOUNT_WBTC_1 = 1e8;

    function setUp() public override {
        super.setUp();
        // Source module fixture: the PM is rebuilt over WBTC/WETH, and the
        // executor (which depends on `v4_pm`) is rebuilt with it.
        (address token0, address token1) = _sorted(address(wbtc), address(weth));
        v4Pm = _deployV4PoolManager(token0, token1);
        executor = ICmdExecutor(
            _deploy("CmdExecutor", abi.encode(address(weth), address(v4Pm), address(usdc), address(wbtc)))
        );
        vm.deal(address(this), 10_000 ether);
        executor.initialize{value: 1}();
    }

    // ── Shared helpers ───────────────────────────────────────────────────

    /// Program a fake V4 swap selling `sellCurrency` from a pool over
    /// `(cA, cB)`: `zfo = currency0 == sellCurrency`, exactly the source's
    /// `pool_x_zfo = pool_x_key[0] == <sell currency>`.
    function _setupPool(
        IFakeUniswapV4PoolManager pm,
        address cA,
        address cB,
        uint24 fee,
        int24 tickSpacing,
        address sellCurrency,
        uint256 amountIn,
        uint256 amountOut,
        address outputToken
    ) internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(cA, cB, fee, tickSpacing);
        zfo = key.currency0 == sellCurrency;
        _setupV4Swap(pm, key, amountIn, amountOut, zfo, outputToken);
    }

    function _v4Swap(Cmd.Table memory at, PoolKey memory key, bool zfo, uint8 hooksIdx, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            hooksIdx,
            zfo,
            amount
        );
    }

    function _batchEntry(Cmd.Table memory at, PoolKey memory key, bool zfo, uint8 hooksIdx, uint256 amount)
        internal
        pure
        returns (Cmd.BatchSwap memory)
    {
        return Cmd.BatchSwap({
            c0: Cmd.add(at, key.currency0),
            c1: Cmd.add(at, key.currency1),
            fee: uint16(key.fee),
            tickSpacing: int16(key.tick_spacing),
            hooks: hooksIdx,
            zeroForOne: zfo,
            amount: amount
        });
    }

    // ── TestV4V4SameCurrency ─────────────────────────────────────────────

    /// WETH→WBTC at pool A, then WBTC→WETH at pool B (profitable): the USDC
    /// free-fee pools net WBTC completely and the WETH profit is taken
    /// explicitly.
    function testV4V4AllWethPairs() public {
        (PoolKey memory keyA, bool zfoA) = _setupPool(
            v4Pm, address(weth), address(wbtc), 0, 60, address(weth), AMOUNT_WETH_10, AMOUNT_WBTC_1, address(wbtc)
        );
        (PoolKey memory keyB, bool zfoB) = _setupPool(
            v4Pm, address(weth), address(wbtc), 0, 120, address(wbtc), AMOUNT_WBTC_1, 2 * AMOUNT_WETH_10, address(weth)
        );

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_WETH_10);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_WBTC_1));
        // profit_weth = pool_b_amount_out - pool_a_amount_in
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, executorIdx, AMOUNT_WETH_10));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestV4Batch ──────────────────────────────────────────────────────

    /// V4_BATCH: WETH→WBTC at pool A (explicit amount), WBTC→WETH at pool B
    /// (dynamic — amount 0 reads the delta from the PM). V4_BATCH
    /// auto-settles, so no separate V4_TAKE / V4_SETTLE is needed.
    function testV4BatchSameCurrency() public {
        (PoolKey memory keyA, bool zfoA) = _setupPool(
            v4Pm, address(weth), address(wbtc), 0, 60, address(weth), AMOUNT_WETH_10, AMOUNT_WBTC_1, address(wbtc)
        );
        (PoolKey memory keyB, bool zfoB) = _setupPool(
            v4Pm, address(weth), address(wbtc), 0, 120, address(wbtc), AMOUNT_WBTC_1, 2 * AMOUNT_WETH_10, address(weth)
        );

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        Cmd.add(at, address(weth));
        Cmd.add(at, address(wbtc));
        Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));

        Cmd.BatchSwap[] memory swaps = new Cmd.BatchSwap[](2);
        // Swap 1: WETH→WBTC (explicit amount).
        swaps[0] = _batchEntry(at, keyA, zfoA, zeroIdx, AMOUNT_WETH_10);
        // Swap 2: WBTC→WETH (dynamic — amount derived from swap 1's delta).
        swaps[1] = _batchEntry(at, keyB, zfoB, zeroIdx, 0);

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(Cmd.v4Batch(swaps))));
    }

    /// V4_BATCH cross-currency: WETH→WBTC at pool A (explicit), then
    /// WBTC→native ETH at pool B (dynamic), auto-settled.
    function testV4BatchCrossCurrency() public {
        (PoolKey memory keyA, bool zfoA) = _setupPool(
            v4Pm, address(weth), address(wbtc), 3000, 60, address(weth), AMOUNT_WETH_10, AMOUNT_WBTC_1, address(wbtc)
        );
        (PoolKey memory keyB, bool zfoB) =
            _setupPool(v4Pm, address(0), address(wbtc), 500, 10, address(wbtc), AMOUNT_WBTC_1, 20 ether, address(0));

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        Cmd.add(at, address(weth));
        Cmd.add(at, address(wbtc));
        Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));
        Cmd.add(at, address(0)); // native sentinel

        Cmd.BatchSwap[] memory swaps = new Cmd.BatchSwap[](2);
        // Swap 1: WETH→WBTC (explicit).
        swaps[0] = _batchEntry(at, keyA, zfoA, zeroIdx, AMOUNT_WETH_10);
        // Swap 2: WBTC→ETH native (dynamic).
        swaps[1] = _batchEntry(at, keyB, zfoB, zeroIdx, 0);

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(Cmd.v4Batch(swaps))));
    }

    /// V4_BATCH with a USDC intermediate: WETH→USDC at pool A (explicit),
    /// then USDC→native ETH at pool B (dynamic), auto-settled.
    function testV4BatchUsdcIntermediate() public {
        (PoolKey memory keyA, bool zfoA) = _setupPool(
            v4Pm, address(weth), address(usdc), 3000, 60, address(weth), AMOUNT_WETH, AMOUNT_USDC, address(usdc)
        );
        (PoolKey memory keyB, bool zfoB) =
            _setupPool(v4Pm, address(0), address(usdc), 500, 10, address(usdc), AMOUNT_USDC, 2 ether, address(0));

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        Cmd.add(at, address(weth));
        Cmd.add(at, address(usdc));
        Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));
        Cmd.add(at, address(0)); // native sentinel

        Cmd.BatchSwap[] memory swaps = new Cmd.BatchSwap[](2);
        // Swap 1: WETH→USDC (explicit).
        swaps[0] = _batchEntry(at, keyA, zfoA, zeroIdx, AMOUNT_WETH);
        // Swap 2: USDC→ETH (dynamic).
        swaps[1] = _batchEntry(at, keyB, zfoB, zeroIdx, 0);

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(Cmd.v4Batch(swaps))));
    }

    // ── TestV4V4DifferentCurrency ────────────────────────────────────────

    /// WETH→WBTC at pool A, then WBTC→native ETH at pool B: the native profit
    /// is taken explicitly and the WETH debit settles as a delta.
    function testV4V4WethWbtcEth() public {
        weth.mint(address(executor), 10 ether);
        (PoolKey memory keyA, bool zfoA) =
            _setupPool(v4Pm, address(weth), address(wbtc), 3000, 60, address(weth), 10 ether, 1e8, address(wbtc));
        (PoolKey memory keyB, bool zfoB) =
            _setupPool(v4Pm, address(0), address(wbtc), 500, 10, address(wbtc), 1e8, 20 ether, address(0));

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));
        uint8 nativeIdx = Cmd.add(at, address(0)); // the native sentinel

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, 10 ether);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, 1e8));
        inner = bytes.concat(inner, Cmd.v4Take(nativeIdx, executorIdx, 20 ether));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestV4V4V4ThreeHop ───────────────────────────────────────────────

    /// WETH→USDC→WBTC→WETH triangular arbitrage via three V4 pools inside a
    /// single unlock. All swaps happen in one unlock callback — no nested
    /// callbacks or intermediate custody. Deltas accumulate in the PM's
    /// transient storage and net out: USDC and WBTC cancel completely,
    /// leaving only a net WETH profit.
    ///
    /// The source test carries a class-local `v4_pm` fixture (WETH/USDC) that
    /// shadows the module's WBTC/WETH override, and the `executor` fixture is
    /// rebuilt against it — both preserved here.
    function testV4V4V4ThreeHop() public {
        (address token0, address token1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV4PoolManager pm = _deployV4PoolManager(token0, token1);
        ICmdExecutor ex =
            ICmdExecutor(_deploy("CmdExecutor", abi.encode(address(weth), address(pm), address(usdc), address(wbtc))));
        ex.initialize{value: 1}();

        uint256 amountUsdc = 20_000e6;
        uint256 amountWbtc = 100e8;
        uint256 amountWethProfit = 2 ether; // more WETH out than in

        // V4a: WETH→USDC.
        (PoolKey memory keyA, bool zfoA) =
            _setupPool(pm, address(weth), address(usdc), 3000, 60, address(weth), 10 ether, amountUsdc, address(usdc));
        // V4b: USDC→WBTC.
        (PoolKey memory keyB, bool zfoB) =
            _setupPool(pm, address(usdc), address(wbtc), 500, 10, address(usdc), amountUsdc, amountWbtc, address(wbtc));
        // V4c: WBTC→WETH.
        (PoolKey memory keyC, bool zfoC) = _setupPool(
            pm, address(wbtc), address(weth), 10_000, 200, address(wbtc), amountWbtc, 12 ether, address(weth)
        );

        // Fund the PM with enough WETH for V4c's payout.
        weth.mint(address(pm), 12 ether);

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        Cmd.add(at, address(usdc));
        Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(ex));
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, 10 ether);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, amountUsdc));
        inner = bytes.concat(inner, _v4Swap(at, keyC, zfoC, zeroIdx, amountWbtc));
        // Take the WETH profit; USDC and WBTC net to zero.
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, executorIdx, amountWethProfit));
        // Settle the remaining WETH delta (owed: 10 ether, taken: 2 ether).
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(ex, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestV4V4DifferentCurrency2 ───────────────────────────────────────

    /// WETH→USDC at pool A, then USDC→native ETH at pool B: the native
    /// profit is taken explicitly and the WETH debit settles as a delta.
    function testV4V4UsdcIntermediateWethEth() public {
        weth.mint(address(executor), 1 ether);
        (PoolKey memory keyA, bool zfoA) =
            _setupPool(v4Pm, address(weth), address(usdc), 3000, 60, address(weth), 1 ether, AMOUNT_USDC, address(usdc));
        (PoolKey memory keyB, bool zfoB) =
            _setupPool(v4Pm, address(0), address(usdc), 500, 10, address(usdc), AMOUNT_USDC, 2 ether, address(0));

        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));
        uint8 nativeIdx = Cmd.add(at, address(0)); // the native sentinel

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, 1 ether);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4Take(nativeIdx, executorIdx, 2 ether));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }
}
