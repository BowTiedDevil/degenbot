// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {Vm} from "../src/Vm.sol";
import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {IExttloadComparator} from "../src/generated/IExttloadComparator.sol";
import {ISolReference} from "../src/generated/ISolReference.sol";
import {ITestHarness} from "../src/generated/ITestHarness.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";

/// Focused harness tests for the shared substrate primitives: encoder forms
/// the ports still need (raw V3 0x30, V4_BATCH_OPEN_WETH), the generated
/// executor error vocabulary, balance-snapshot/conservation diffs, tracked
/// transfer accounting, exact revert assertions, and the artifact substrate
/// additions (ExttloadComparator, committed solc-0.7.6 SolReference).
/// Downstream consumers are named in PORTING.md.
///
/// Every negative control here distinguishes the failure modes the shared
/// helpers must separate: success, wrong selector, wrong argument, and an
/// unrelated harness error are each proven NOT to pass as the expected revert.
contract HarnessPrimitivesTest is PoolSetup {
    uint256 internal constant ENCODING_AMOUNT = 0xA5A5A5;

    /// An ERC6909 token id whose numeric value collides with a plausible
    /// address, for the id-topic negative control.
    uint256 internal constant TOKEN_ID_AS_ADDRESS = uint256(uint160(0xC0DE60A5));

    // ═══════════════════════════════════════════════════════════════════
    // Encoder forms
    // ═══════════════════════════════════════════════════════════════════

    function testV3SwapCompactRawBytesAndLength() public {
        Cmd.Table memory at = _emptyTable();
        uint8 poolIdx = Cmd.add(at, address(v4Pm));
        uint8 recipientIdx = Cmd.add(at, address(executor));
        bytes memory forward = hex"deadbeef";

        bytes memory raw =
            Cmd.v3SwapCompactRaw(poolIdx, true, ENCODING_AMOUNT, recipientIdx, forward);

        // [0x30][pool:1][zfo:1][amount:12][recipient:1][forward_len:1][forward]
        bytes memory expected = abi.encodePacked(
            bytes1(0x30),
            poolIdx,
            bytes1(0x01),
            Cmd.be(ENCODING_AMOUNT, 12),
            recipientIdx,
            bytes1(uint8(forward.length)),
            forward
        );
        assertEq(raw, expected, "raw 0x30 form bytes");
        // The raw form carries the direction in-band, one byte more than the
        // direction-specific 0x33/0x34 layouts.
        assertEq(raw.length, 17 + forward.length, "raw 0x30 length is 17 + N");
    }

    function testV3SwapCompactRawDirectionFlagOnly() public {
        Cmd.Table memory at = _emptyTable();
        uint8 poolIdx = Cmd.add(at, address(v4Pm));
        uint8 recipientIdx = Cmd.add(at, address(executor));

        bytes memory zfo = Cmd.v3SwapCompactRaw(poolIdx, true, 1, recipientIdx, "");
        bytes memory ofo = Cmd.v3SwapCompactRaw(poolIdx, false, 1, recipientIdx, "");

        // Same length, same opcode byte; only the in-band flag byte differs.
        assertEq(zfo.length, ofo.length, "direction flag does not change length");
        assertEq(zfo[0], bytes1(0x30), "raw form keeps the 0x30 opcode");
        assertEq(zfo[2], bytes1(0x01), "zero-for-one flag byte");
        assertEq(ofo[2], bytes1(0x00), "one-for-zero flag byte");
        zfo[2] = bytes1(0x00);
        assertEq(zfo, ofo, "streams are identical apart from the flag byte");
    }

    function testV4BatchOpenWethOpAndLayout() public {
        Cmd.BatchSwap[] memory swaps = new Cmd.BatchSwap[](2);
        swaps[0] = Cmd.BatchSwap(1, 2, 3000, 60, 0, true, 100);
        swaps[1] = Cmd.BatchSwap(3, 4, 500, 10, 0, false, 0); // dynamic entry

        bytes memory open = Cmd.v4BatchOpenWeth(swaps);
        bytes memory closed = Cmd.v4Batch(swaps);

        assertEq(open[0], bytes1(0x43), "0x43 V4_BATCH_OPEN_WETH opcode");
        assertEq(closed[0], bytes1(0x42), "0x42 V4_BATCH opcode");
        assertEq(open[1], bytes1(0x02), "swap count byte");
        assertEq(open.length, 2 + 20 * swaps.length, "2-byte header + 20 bytes per entry");

        // Identical layout apart from the opcode byte.
        open[0] = bytes1(0x42);
        assertEq(open, closed, "0x43 body matches the 0x42 body");
    }

    function testMakeConfigFieldWidths() public {
        // (min_profit << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8) | mode
        uint256 config = Cmd.makeConfig(2, 5000, 7, 3);
        assertEq(config, (3 << 32) | (7 << 24) | (5000 << 8) | 2, "config packing");
        assertEq(config & 0xFF, 2, "mode occupies bits 0-7");
        assertEq((config >> 8) & 0xFFFF, 5000, "bribe bips occupies bits 8-23");
        assertEq((config >> 24) & 0xFF, 7, "bribe recipient idx occupies bits 24-31");
        assertEq(config >> 32, 3, "min_profit occupies bits 32-255");
    }

    function testMakeConfigRejectsBadMode() public {
        // Mode 0 is the one the source helper make_bribe_config would produce
        // with its zero mode bits; the executor rejects it loudly.
        (bool ok, bytes memory ret) =
            address(this).call(abi.encodeCall(this.makeConfigNC, (0, 0, 0, 0)));
        assertFalse(ok, "mode 0 must be rejected");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "rejection is a require-string revert");
        assertEq(_revertMessage(ret), "bad mode", "mode rejection message");
    }

    function makeConfigNC(uint256 mode, uint16 bips, uint8 idx, uint256 minProfit)
        external
        pure
        returns (uint256)
    {
        return Cmd.makeConfig(mode, bips, idx, minProfit);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Generated error vocabulary
    // ═══════════════════════════════════════════════════════════════════

    function testGeneratedSelectorsMatchCanonicalSignatures() public pure {
        assertEq(
            ExecutorErrors.UNAUTHORIZED,
            bytes4(keccak256("Unauthorized(address)")),
            "Unauthorized selector"
        );
        assertEq(
            ExecutorErrors.INVALID_CALLBACK,
            bytes4(keccak256("InvalidCallback(address)")),
            "InvalidCallback selector"
        );
        assertEq(
            ExecutorErrors.INSUFFICIENT_BALANCE,
            bytes4(keccak256("InsufficientBalance(uint256,uint256)")),
            "InsufficientBalance selector"
        );
        assertEq(
            ExecutorErrors.INSUFFICIENT_PROFIT,
            bytes4(keccak256("InsufficientProfit(uint256,uint256)")),
            "InsufficientProfit selector"
        );
        assertEq(
            ExecutorErrors.INSUFFICIENT_MINT_DELTA,
            bytes4(keccak256("InsufficientMintDelta(uint256,uint256)")),
            "InsufficientMintDelta selector"
        );
        assertEq(
            ExecutorErrors.INVALID_COMMAND,
            bytes4(keccak256("InvalidCommand(uint256)")),
            "InvalidCommand selector"
        );
        assertEq(
            ExecutorErrors.INVALID_PROFIT_MODE,
            bytes4(keccak256("InvalidProfitMode(uint256)")),
            "InvalidProfitMode selector"
        );
        assertEq(
            ExecutorErrors.BIPS_TOO_HIGH,
            bytes4(keccak256("BipsTooHigh(uint256)")),
            "BipsTooHigh selector"
        );
        assertEq(
            ExecutorErrors.INVALID_MSG_VALUE,
            bytes4(keccak256("InvalidMsgValue(uint256)")),
            "InvalidMsgValue selector"
        );
        assertEq(
            ExecutorErrors.NOT_PLAIN_ETH_TRANSFER,
            bytes4(keccak256("NotPlainEthTransfer()")),
            "NotPlainEthTransfer selector"
        );
    }

    function testNameOfCoversGeneratedVocabulary() public pure {
        assertEq(ExecutorErrors.nameOf(ExecutorErrors.ERROR_STRING), "(Error)", "Error(string)");
        assertEq(ExecutorErrors.nameOf(ExecutorErrors.PANIC), "(Panic)", "Panic(uint256)");
        assertEq(
            ExecutorErrors.nameOf(ExecutorErrors.INVALID_COMMAND), "InvalidCommand", "custom name"
        );
        assertEq(
            ExecutorErrors.nameOf(ExecutorErrors.NOT_PLAIN_ETH_TRANSFER),
            "NotPlainEthTransfer",
            "zero-arg custom name"
        );
        assertEq(
            ExecutorErrors.nameOf(bytes4(0xDEADBEEF)), "unknown", "outside the vocabulary"
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // Exact revert assertions
    // ═══════════════════════════════════════════════════════════════════

    /// Live executor revert: an unassigned opcode in the execution section.
    function testExecutorInvalidCommandExactSelectorAndArgument() public {
        bytes memory commands = bytes.concat(Cmd.preamble(_emptyTable()), hex"77");
        Outcome memory o = _executeRaw(executor, commands, 1);

        _assertRevertData(o, _executorRevert(ExecutorErrors.INVALID_COMMAND, 0x77), "live revert");
        // The same outcome fails an assertion for a different selector.
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(
                this.assertRevertDataNC,
                (
                    false,
                    o.revertData,
                    _executorRevert(ExecutorErrors.UNAUTHORIZED, address(this)),
                    "nc"
                )
            )
        );
        assertFalse(ok, "a different selector must not pass");
        _assertHelperRejection(ret);
    }

    function testAssertRevertDataPassesOnExactMatch() public pure {
        Outcome memory o =
            Outcome(false, _executorRevert(ExecutorErrors.BIPS_TOO_HIGH, 10_001), 0, 0, 0);
        _assertRevertData(o, _executorRevert(ExecutorErrors.BIPS_TOO_HIGH, 10_001), "exact");
        _assertRevertSelector(o, ExecutorErrors.BIPS_TOO_HIGH, "selector-only form");
    }

    function testAssertRevertDataFailsOnWrongSelector() public {
        bytes memory actual = _executorRevert(ExecutorErrors.INVALID_COMMAND, 0x77);
        bytes memory expected = _executorRevert(ExecutorErrors.INVALID_PROFIT_MODE, 0);
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(this.assertRevertDataNC, (false, actual, expected, "nc"))
        );
        assertFalse(ok, "wrong selector must fail");
        _assertHelperRejection(ret);
    }

    function testAssertRevertDataFailsOnWrongArgument() public {
        // Same selector, different argument — the discriminator the
        // "wrong selector" control cannot cover.
        bytes memory actual = _executorRevert(ExecutorErrors.INVALID_COMMAND, 0x77);
        bytes memory expected = _executorRevert(ExecutorErrors.INVALID_COMMAND, 0x78);
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(this.assertRevertDataNC, (false, actual, expected, "nc"))
        );
        assertFalse(ok, "wrong argument must fail");
        _assertHelperRejection(ret);
    }

    function testAssertRevertDataFailsOnSuccess() public {
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(
                this.assertRevertDataNC,
                (
                    true,
                    new bytes(0),
                    _executorRevert(ExecutorErrors.INVALID_COMMAND, 0x77),
                    "nc"
                )
            )
        );
        assertFalse(ok, "a successful call must not pass a revert assertion");
        _assertHelperRejection(ret);
        assertTrue(
            _contains(ret, "expected revert, call succeeded"),
            "the success case is named distinctly from a data mismatch"
        );
    }

    /// An unrelated harness error must never be accepted as the expected
    /// executor revert: the helper rejects it, and its rejection is the
    /// helper's own Error(string), not the unrelated error bubbling through.
    function testUnrelatedRevertNotAcceptedAsExecutorError() public {
        (, bytes memory unrelated) =
            _callExpectRevert(address(this), abi.encodeCall(this.unrelatedHarnessError, (0)));
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(
                this.assertRevertDataNC,
                (false, unrelated, _executorRevert(ExecutorErrors.INVALID_COMMAND, 0x77), "nc")
            )
        );
        assertFalse(ok, "the unrelated revert must not pass");
        _assertHelperRejection(ret);
        assertTrue(
            _contains(ret, "revert data mismatch"),
            "the helper names a data mismatch, not the unrelated error"
        );
    }

    error UnrelatedHarnessError(uint256 payload);

    function unrelatedHarnessError(uint256 payload) external pure {
        revert UnrelatedHarnessError(payload);
    }

    function assertRevertDataNC(
        bool ok,
        bytes memory actualRevertData,
        bytes memory expected,
        string memory label
    ) external pure {
        Outcome memory o = Outcome(ok, actualRevertData, 0, 0, 0);
        _assertRevertData(o, expected, label);
    }

    /// The helper's rejection path is its own Error(string) message — never a
    /// pass-through of the revert under inspection.
    function _assertHelperRejection(bytes memory ret) private pure {
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "helper rejection is Error(string)");
        assertTrue(_contains(ret, "nc"), "rejection carries the assertion label");
    }

    /// Decode an Error(string) revert payload, selector stripped.
    function _revertMessage(bytes memory revertData) private pure returns (string memory) {
        assertTrue(
            revertData.length >= 4 && bytes4(revertData) == ExecutorErrors.ERROR_STRING,
            "not an Error(string) revert"
        );
        bytes memory payload = new bytes(revertData.length - 4);
        for (uint256 i = 4; i < revertData.length; i++) {
            payload[i - 4] = revertData[i];
        }
        return abi.decode(payload, (string));
    }

    function _contains(bytes memory haystack, string memory needle) private pure returns (bool) {
        bytes memory n = bytes(needle);
        if (n.length > haystack.length) return false;
        for (uint256 i = 0; i + n.length <= haystack.length; i++) {
            uint256 j = 0;
            while (j < n.length && haystack[i + j] == n[j]) {
                j++;
            }
            if (j == n.length) return true;
        }
        return false;
    }

    // ═══════════════════════════════════════════════════════════════════
    // Balance snapshots and conservation diffs
    // ═══════════════════════════════════════════════════════════════════

    function testTokenConservationPassesWithinTrackedSet() public {
        address[] memory tokens = _tokens(address(usdc));
        address[] memory accounts = _accounts(address(this), address(v4Pm));
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        usdc.transfer(address(v4Pm), 5e6);

        BalanceSnapshot memory after_ = _snapshotBalances(tokens, accounts);
        _assertTokenConserved(before, after_, 0, "tracked usdc");
    }

    function testTokenConservationFailsOnLeakToUntrackedAddress() public {
        address[] memory tokens = _tokens(address(usdc));
        address[] memory accounts = _accounts(address(this), address(v4Pm));
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        usdc.transfer(address(0xD1E), 5e6); // 0xD1E is NOT tracked

        BalanceSnapshot memory after_ = _snapshotBalances(tokens, accounts);
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(this.assertTokenConservedNC, (before, after_, 0, "leak"))
        );
        assertFalse(ok, "a leak out of the tracked set must fail conservation");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "conservation failure is Error(string)");
        assertTrue(
            _contains(ret, "token conservation violated"),
            "the failure names the conservation check"
        );
    }

    function assertTokenConservedNC(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 tokenIdx,
        string memory label
    ) external pure {
        _assertTokenConserved(beforeSnap, afterSnap, tokenIdx, label);
    }

    /// WETH+ETH is the executor's mode-1 profit domain: a withdrawal of the
    /// combined balance to an untracked address violates WETH+ETH conservation
    /// while the executor profit check still reads the tracked delta exactly.
    function testWethEthConservationAndProfitDelta() public {
        // WETH + NATIVE: the combined mode-1 profit domain.
        address[] memory tokens = new address[](2);
        tokens[0] = address(weth);
        tokens[1] = NATIVE;
        address[] memory accounts = _accounts(address(executor), address(v4Pm), address(this));
        vm.deal(address(executor), 2 ether);

        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);
        executor.withdraw(1 ether, address(this)); // tracked: stays inside the set
        BalanceSnapshot memory afterTracked = _snapshotBalances(tokens, accounts);
        _assertWethEthConserved(before, afterTracked, 0, "tracked withdrawal");
        _assertExecutorProfit(before, afterTracked, 0, 0, -1 ether, "tracked withdrawal");

        // Fresh snapshot for the leak phase: the tracked set is unchanged
        // here, and the next withdrawal leaves it entirely.
        BalanceSnapshot memory beforeLeak = _snapshotBalances(tokens, accounts);
        executor.withdraw(1 ether, address(0xD1E)); // untracked: a leak
        BalanceSnapshot memory afterLeak = _snapshotBalances(tokens, accounts);
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(this.assertWethEthConservedNC, (beforeLeak, afterLeak, 0, "leak"))
        );
        assertFalse(ok, "an untracked withdrawal must fail WETH+ETH conservation");
        assertTrue(_contains(ret, "WETH+ETH conservation violated"), "failure names the domain");
    }

    function assertWethEthConservedNC(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 wethTokenIdx,
        string memory label
    ) external pure {
        _assertWethEthConserved(beforeSnap, afterSnap, wethTokenIdx, label);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Tracked transfer accounting
    // ═══════════════════════════════════════════════════════════════════

    function testCountTransfersInvolvingTracksBothSides() public {
        vm.recordLogs();
        usdc.transfer(address(v4Pm), 5e6);
        usdc.transfer(address(0xD1E), 7e6);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 2, "both transfers counted");
        assertEq(_countTransfersInvolving(logs, address(this)), 2, "sender on both");
        assertEq(_countTransfersInvolving(logs, address(v4Pm)), 1, "receiver of the first");
        assertEq(_countTransfersInvolving(logs, address(0xD1E)), 1, "receiver of the second");
        assertEq(_countTransfersInvolving(logs, address(weth)), 0, "bystander counts nothing");
    }

    /// Negative controls for the ERC6909/erc20 topic shapes: a Swap log is not
    /// a transfer, and an ERC6909 token id that numerically equals a bystander
    /// address must not be read as one.
    function testCountTransfersInvolvingIgnoresSwapAndTokenIdTopics() public {
        // Swap-only logs: a fake pair swap emits Sync + the six-field Swap
        // event. Neither is a Transfer, so nobody's count moves — including
        // the parties named in the Swap.
        IFakeUniswapV2Pair pair = _deployV2Pair(address(weth), address(usdc), 0, 30);
        weth.mint(address(pair), 10 ether);
        usdc.mint(address(pair), 2000e6);
        pair.sync();
        weth.mint(address(pair), 1 ether); // excess input: no callback needed
        bool zfo = pair.token0() == address(weth);
        uint256 amountOut = _v2GetAmountOut(
            1 ether, weth.balanceOf(address(pair)), usdc.balanceOf(address(pair)), 30
        );
        // The swap recipient is an untracked address: the physical output
        // transfer names it and the pair, while the Swap event's indexed
        // topics name the swap caller (address(this)) and the same recipient.
        // Only the Transfer may move counts.
        vm.recordLogs();
        if (zfo) {
            pair.swap(0, amountOut, address(0xD1E), bytes(""));
        } else {
            pair.swap(amountOut, 0, address(0xD1E), bytes(""));
        }
        Vm.Log[] memory swapLogs = vm.getRecordedLogs();
        assertGt(swapLogs.length, 0, "the swap produced logs");
        assertEq(_countTransfers(swapLogs), 1, "only the physical output transfer counts");
        assertEq(_countTransfersInvolving(swapLogs, address(0xD1E)), 1, "transfer receiver");
        assertEq(_countTransfersInvolving(swapLogs, address(pair)), 1, "transfer sender");
        assertEq(
            _countTransfersInvolving(swapLogs, address(this)),
            0,
            "the Swap event's indexed sender must not count as a transfer"
        );

        // ERC6909: mint+burn a token whose id equals a bystander address.
        // from/to are counted from topics 1/2; the id topic (topic 3) must
        // never be interpreted as an address.
        vm.recordLogs();
        v4Pm.unlock("");
        Vm.Log[] memory erc6909Logs = vm.getRecordedLogs();
        assertGe(_countTransfers(erc6909Logs), 2, "mint and burn logged");
        assertEq(
            _countTransfersInvolving(erc6909Logs, address(uint160(TOKEN_ID_AS_ADDRESS))),
            0,
            "a token id equal to an address does not count for it"
        );
        assertEq(
            _countTransfersInvolving(erc6909Logs, address(this)),
            2,
            "ERC6909 to (mint) and from (burn) count"
        );
        assertEq(
            _countTransfersInvolving(erc6909Logs, address(0)),
            2,
            "ERC6909 mint-from and burn-to count"
        );
    }

    /// Called back by the fake PoolManager's unlock for the ERC6909 negative
    /// controls. Mint then burn the bystander-valued token id so the unlock
    /// exits with settled deltas.
    function unlockCallback(bytes calldata) external returns (bytes memory) {
        v4Pm.mint(address(this), TOKEN_ID_AS_ADDRESS, 1);
        v4Pm.burn(address(this), TOKEN_ID_AS_ADDRESS, 1);
        return "";
    }

    // ═══════════════════════════════════════════════════════════════════
    // Vm additions: JSON and hex lanes
    // ═══════════════════════════════════════════════════════════════════

    function testParseJsonTypedGetters() public pure {
        string memory json = '{"release_tag": "v2.0.0", "protocol_major": 2}';
        assertEq(vm.parseJsonString(json, ".release_tag"), "v2.0.0", "parseJsonString");
        assertEq(vm.parseJsonUint(json, ".protocol_major"), 2, "parseJsonUint");
    }

    function testToStringHexForms() public pure {
        bytes32 word = bytes32(uint256(0xABCDEF));
        assertEq(vm.toString(word), vm.toString(abi.encodePacked(word)), "bytes32 and bytes agree");
        assertTrue(
            _contains(bytes(vm.toString(word)), "abcdef"), "bytes32 renders lowercase hex"
        );
        assertEq(vm.toString(abi.encodePacked(hex"dead")), "0xdead", "short bytes");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Artifact substrate additions
    // ═══════════════════════════════════════════════════════════════════

    /// ExttloadComparator is built by the pinned-vyper pipeline; its slot math
    /// is deployable offline (the fork-lane comparisons that need a real
    /// PoolManager live in their own fork-lane port).
    function testExttloadComparatorArtifactSlotMath() public {
        IExttloadComparator comparator =
            IExttloadComparator(_deploy("ExttloadComparator", abi.encode(address(v4Pm), address(v4Pm))));

        (address t0, address t1) = _sorted(address(weth), address(usdc));
        bytes32 expectedSlot = keccak256(
            abi.encodePacked(bytes32(uint256(uint160(t0))), bytes32(uint256(uint160(t1))))
        );
        assertEq(comparator.computeSlot(t0, t1), expectedSlot, "exttload slot derivation");
        assertEq(
            comparator.nativeDeltaSlot(),
            comparator.computeSlot(address(comparator), address(0)),
            "native delta slot keys the comparator's own address"
        );
    }

    /// The committed solc-0.7.6 SolReference artifact and the Vyper TestHarness
    /// agree on the V3 math, the differential property the V3-library ports
    /// fuzz across the input domain.
    function testSolReferenceMatchesVyperHarness() public {
        ITestHarness harness = ITestHarness(_deploy("TestHarness", abi.encode()));
        ISolReference solRef = ISolReference(_deploy("SolReference", abi.encode()));

        assertEq(
            solRef.test_mul_div(type(uint256).max, 3, 5),
            harness.test_mul_div(type(uint256).max, 3, 5),
            "mul_div at the phantom-overflow boundary"
        );
        assertEq(
            solRef.test_mul_div_rounding_up(2 ** 255, 2, 3),
            harness.test_mul_div_rounding_up(2 ** 255, 2, 3),
            "mul_div_rounding_up"
        );
        assertEq(
            solRef.test_div_rounding_up(7, 2),
            harness.test_div_rounding_up(7, 2),
            "div_rounding_up"
        );
        assertEq(
            solRef.test_to_int256(2 ** 255 - 1),
            harness.test_to_int256(2 ** 255 - 1),
            "safe cast to int256"
        );
        assertEq(
            solRef.test_get_sqrt_ratio_at_tick(100),
            harness.test_get_sqrt_ratio_at_tick(100),
            "TickMath at tick 100"
        );
        assertEq(
            solRef.test_get_tick_at_sqrt_ratio(
                solRef.test_get_sqrt_ratio_at_tick(1000)
            ),
            1000,
            "Solidity tick round trip"
        );
        assertEq(
            harness.test_get_tick_at_sqrt_ratio(
                solRef.test_get_sqrt_ratio_at_tick(1000)
            ),
            1000,
            "cross-implementation tick round trip"
        );
        assertEq(
            solRef.test_get_next_sqrt_price_from_input(
                2 * uint160(Q96), 1e18, 1e18, true
            ),
            harness.test_get_next_sqrt_price_from_input(
                2 * uint160(Q96), 1e18, 1e18, true
            ),
            "next sqrt price from input"
        );
        assertEq(
            solRef.test_get_amount0_delta(
                MIN_SQRT_PRICE_X96 + 1000, MAX_SQRT_PRICE_X96 - 1000, 1e18, true
            ),
            harness.test_get_amount0_delta(
                MIN_SQRT_PRICE_X96 + 1000, MAX_SQRT_PRICE_X96 - 1000, 1e18, true
            ),
            "amount0 delta round-up"
        );
    }

    // ── Local builders ────────────────────────────────────────────────────

    function _tokens(address token) private pure returns (address[] memory t) {
        t = new address[](1);
        t[0] = token;
    }

    function _accounts(address a, address b) private pure returns (address[] memory s) {
        s = new address[](2);
        s[0] = a;
        s[1] = b;
    }

    function _accounts(address a, address b, address c) private pure returns (address[] memory s) {
        s = new address[](3);
        s[0] = a;
        s[1] = b;
        s[2] = c;
    }
}
