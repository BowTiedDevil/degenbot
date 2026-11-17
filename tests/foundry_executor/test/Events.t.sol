// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Vm} from "../src/Vm.sol";
import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";

/// Port of `tests/executor/test_events.py`.
contract EventsTest is PoolSetup {
    uint16 internal constant EVENTS_V2_FEE = 30;

    function _record() internal {
        vm.recordLogs();
    }

    function testErc20TransferEvent() public {
        _record();
        usdc.transfer(address(0xB1), 1000);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "one Transfer");
    }

    function testErc20MintEvent() public {
        _record();
        usdc.mint(address(0xB1), 1000);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "mint emits one Transfer from the zero address");
    }

    function testErc20BurnEvent() public {
        _record();
        usdc.burn(1000);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "burn emits one Transfer to the zero address");
    }

    function testWethDepositEvent() public {
        _record();
        weth.deposit{value: 1 ether}();
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "deposit emits one Transfer");
    }

    function testWethWithdrawEvent() public {
        weth.deposit{value: 1 ether}();
        _record();
        weth.withdraw(1 ether);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "withdraw emits one Transfer");
    }

    function testV2SwapEvents() public {
        IFakeUniswapV2Pair v2 =
            _deployV2Pair(address(weth), address(usdc), 0, uint256(EVENTS_V2_FEE));
        usdc.mint(address(v2), 2000e6);
        weth.mint(address(v2), 10 ether);
        v2.sync();

        uint256 amountIn = 1 ether;
        uint256 amountOut = _v2GetAmountOut(
            amountIn, weth.balanceOf(address(v2)), usdc.balanceOf(address(v2)), V2_FEE
        );

        weth.transfer(address(v2), amountIn);
        _record();
        v2.swap(0, amountOut, address(this), "");
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 1, "one Transfer (USDC pair -> owner)");
        assertEq(_countTopic(logs, V2_SYNC_TOPIC), 1, "one Sync");
        assertEq(_countTopic(logs, FAKE_V2_SWAP_TOPIC), 1, "one Swap");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Event-surface additions on the withdrawal exit paths (documented
    // additions, not source cases): ERC6909 Transfer counted exactly once,
    // V4 takes never double-counted, and the native/physical/ERC6909
    // distinctions kept explicit.
    // ═══════════════════════════════════════════════════════════════════

    uint256 internal constant EVENTS_AMOUNT_WETH = 1 ether;

    /// The withdrawal AddressTable() order: pm, weth, usdc, executor, zero,
    /// owner — every address an appended index, the zero address the native
    /// sentinel.
    struct ETable {
        Cmd.Table at;
        uint8 pmIdx;
        uint8 wethIdx;
        uint8 usdcIdx;
        uint8 executorIdx;
        uint8 zeroIdx;
        uint8 ownerIdx;
    }

    function _eventsWithdrawalTable() internal view returns (ETable memory t) {
        t.at = _emptyTable();
        t.pmIdx = Cmd.add(t.at, address(v4Pm));
        t.wethIdx = Cmd.add(t.at, address(weth));
        t.usdcIdx = Cmd.add(t.at, address(usdc));
        t.executorIdx = Cmd.add(t.at, address(executor));
        t.zeroIdx = Cmd.add(t.at, address(0));
        t.ownerIdx = Cmd.add(t.at, address(this));
    }

    function _eventsWethUsdcPool() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, EVENTS_AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));
    }

    function _eventsNativeWethPool() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(0), address(weth), 500, 10);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, EVENTS_AMOUNT_WETH, EVENTS_AMOUNT_WETH, zfo, address(0));
    }

    /// A V4 take is ONE physical transfer: the token's own Transfer log. The
    /// ERC6909 topic never appears and the take is not counted twice.
    function testV4TakeEmitsExactlyOnePhysicalTransfer() public {
        vm.deal(address(executor), EVENTS_AMOUNT_WETH);
        PoolKey memory key = _poolKey(address(0), address(usdc), 500, 10);
        bool zfo = true; // native is always currency0
        _setupV4Swap(v4Pm, key, EVENTS_AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));

        ETable memory t = _eventsWithdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 nativeIdx = Cmd.add(at, address(0));

        bytes memory inner =
            Cmd.v4SwapCompact(nativeIdx, usdcIdx, 500, 10, zeroIdx, zfo, EVENTS_AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Take(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(nativeIdx));

        _record();
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 1, "exactly one transfer-topic log for the take");
        assertEq(_countTopic(logs, TRANSFER_TOPIC), 1, "one physical ERC20 Transfer");
        assertEq(_countTopic(logs, ERC6909_TRANSFER_TOPIC), 0, "no ERC6909 Transfer");
    }

    /// Converting a delta into an ERC6909 claim (V4_MINT_COMPACT) logs the
    /// ERC6909 Transfer exactly once, alongside the physical WETH payment.
    function testErc6909MintCountsExactlyOnce() public {
        weth.mint(address(executor), EVENTS_AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _eventsWethUsdcPool();
        ETable memory t = _eventsWithdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 pmIdx = t.pmIdx;
        uint8 wethIdx = t.wethIdx;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;

        bytes memory inner = _eventsV4Swap(at, key, zfo, zeroIdx, EVENTS_AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Sync(wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(wethIdx, pmIdx, EVENTS_AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, Cmd.v4MintCompact(usdcIdx, executorIdx, AMOUNT_USDC));

        _record();
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 2, "one physical + one ERC6909");
        assertEq(_countTopic(logs, TRANSFER_TOPIC), 1, "WETH executor->PM physical transfer");
        assertEq(_countTopic(logs, ERC6909_TRANSFER_TOPIC), 1, "mint logged exactly once");
        assertEq(_countTransfersInvolving(logs, address(executor)), 2, "executor on both sides");
    }

    /// The ERC6909-to-physical exit: burn (one ERC6909 Transfer), take_delta
    /// (one physical transfer PM->executor), xfer-balance (one physical
    /// transfer executor->owner) — three transfer-topic logs, no double count.
    function testErc6909ToPhysicalExitEventCounts() public {
        weth.mint(address(executor), EVENTS_AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _eventsWethUsdcPool();
        ETable memory t = _eventsWithdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 pmIdx = t.pmIdx;
        uint8 wethIdx = t.wethIdx;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;

        bytes memory inner = _eventsV4Swap(at, key, zfo, zeroIdx, EVENTS_AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Sync(wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(wethIdx, pmIdx, EVENTS_AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, Cmd.v4MintCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);

        bytes memory inner2 = Cmd.v4BurnCompact(usdcIdx, AMOUNT_USDC);
        inner2 = bytes.concat(inner2, Cmd.v4TakeDelta(usdcIdx, executorIdx));
        bytes memory commands =
            bytes.concat(Cmd.v4Unlock(inner2), Cmd.erc20XferBalance(usdcIdx, ownerIdx));

        _record();
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 3, "burn + take + xfer-balance");
        assertEq(_countTopic(logs, TRANSFER_TOPIC), 2, "two physical transfers");
        assertEq(_countTopic(logs, ERC6909_TRANSFER_TOPIC), 1, "one ERC6909 burn");
        assertEq(_countTransfersInvolving(logs, address(executor)), 3, "executor in all three");
        assertEq(_countTransfersInvolving(logs, address(this)), 1, "owner receives once");
    }

    /// The native ETH exit: burn logs one ERC6909 Transfer, but the take of
    /// native currency and SEND_ETH_ALL move ETH with no Transfer log at all —
    /// the native/physical distinction the counter must not blur.
    function testNativeErc6909ExitCounts() public {
        weth.mint(address(executor), EVENTS_AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _eventsNativeWethPool();
        ETable memory t = _eventsWithdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 wethIdx = t.wethIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;
        uint8 nativeIdx = Cmd.add(at, address(0));

        bytes memory inner = _eventsV4Swap(at, key, zfo, zeroIdx, EVENTS_AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4MintCompact(nativeIdx, executorIdx, EVENTS_AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);

        bytes memory inner2 = Cmd.v4BurnCompact(nativeIdx, EVENTS_AMOUNT_WETH);
        inner2 = bytes.concat(inner2, Cmd.v4TakeDelta(nativeIdx, executorIdx));
        bytes memory commands = bytes.concat(Cmd.v4Unlock(inner2), Cmd.sendEthAll(ownerIdx));

        _record();
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 1, "only the ERC6909 burn");
        assertEq(_countTopic(logs, TRANSFER_TOPIC), 0, "no physical transfer: native ETH moves silently");
        assertEq(_countTopic(logs, ERC6909_TRANSFER_TOPIC), 1, "one ERC6909 burn");
    }

    /// SEND_ETH_ALL moves native ETH with no events of any kind.
    function testSendEthAllEmitsNoEvents() public {
        vm.deal(address(executor), 1 ether);
        ETable memory t = _eventsWithdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 ownerIdx = t.ownerIdx;

        _record();
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.sendEthAll(ownerIdx)), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 0, "native ETH exit emits no events");
    }

    function _eventsV4Swap(
        Cmd.Table memory at,
        PoolKey memory key,
        bool zfo,
        uint8 hooksIdx,
        uint256 amount
    )
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
}
