
// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;



import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";



/// Port of `tests/executor/test_sentinel_failclosed.py`.
///
/// Each command that accepts a sentinel byte or a bounded index must reject
/// out-of-range values with InvalidCommand rather than silently no-op'ing,
/// bounds-reverting opaquely, or wrapping. The V4 hook-sentinel slot is the
/// exception: invalid hook sentinels are already fail-closed through the Vyper
/// `t_addresses` bounds check (empty returndata) — no explicit guard sits on
/// the hot V4-swap hooks lookup. These tests pin the existing fail-closed
/// behavior: an empty-returndata revert, never success and never a silent
/// mis-resolution.
///
/// All revert assertions run through the module-local `_expectRevert` /
/// `_expectAccepted` ports of the source `_expect_revert` / `_expect_accepted`
/// helpers; the meta-tests pin the helpers' own failure modes.
contract SentinelFailClosedTest is PoolSetup {
    /// The identified downstream revert of the native-sentinel swap: the fake
    /// PoolManager's `assert self.t_unlocked, "ManagerLocked"` bubbles an
    /// Error(string) payload out of the swap's PM call.
    function _managerLocked() private pure returns (bytes memory) {
        return abi.encodeWithSignature("Error(string)", "ManagerLocked");
    }



    // ── Ports of the source revert helpers ────────────────────────────────



    /// Port of `_expect_revert`: the outcome must revert with exactly one
    /// pinned identity — exact revert data. A custom-error guard pins the
    /// generated selector and its arguments (`_executorRevert`); an
    /// empty-data guard pins the zero-length payload. A successful call, a
    /// revert carrying any other identity, and a revert carrying none all
    /// fail.
    function _expectRevert(Outcome memory o, bytes memory expectedRevertData, string memory label)
        internal
        pure
    {
        _assertRevertData(o, expectedRevertData, label);
    }



    /// Port of `_expect_accepted`: the guard under test must pass — success,
    /// or exactly the identified downstream revert, a failure downstream of
    /// the guard. Any other revert, and any revert carrying no identity,
    /// fails.
    function _expectAccepted(
        Outcome memory o,
        bytes memory downstreamRevertData,
        string memory label
    )
        internal
        pure
    {
        if (o.ok) return;
        assertTrue(
            downstreamRevertData.length > 0,
            string.concat(label, ": expected the guard to pass")
        );
        assertEq(
            o.revertData,
            downstreamRevertData,
            string.concat(label, ": expected success or the identified downstream revert")
        );
    }



    // ── M5: V4_TAKE_DELTA zero-delta no-op ────────────────────────────────



    /// V4_TAKE_DELTA(WETH, self) inside an unlock with no prior swap sees
    /// delta == 0 and skips the take, so the unlock exits cleanly instead of
    /// PM.take(0) bubbling an opaque revert. Runs with the default config 1;
    /// the dedicated overload tests exercise both ABI selectors.
    function testTakeDeltaZeroDeltaNoOpNotOpaqueRevert() public {
        Cmd.Table memory at = _emptyTable();
        bytes memory commands = Cmd.v4Unlock(Cmd.v4TakeDelta(Cmd.SENTINEL_WETH, Cmd.SENTINEL_SELF));
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectAccepted(o, bytes(""), "zero-delta V4_TAKE_DELTA");
    }



    // ── M6: hook sentinels fail closed through the bounds check ───────────



    function testV4SwapCompactPmHookSentinelReverts() public {
        /// hooks_idx = 0xFC (PM sentinel) — not a valid hook contract.
        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        bytes memory commands =
            Cmd.v4SwapCompact(wethIdx, usdcIdx, 3000, 60, Cmd.SENTINEL_PM, true, 1 ether);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectRevert(o, bytes(""), "PM hook sentinel");
    }



    function testV4SwapCompactWethHookSentinelReverts() public {
        /// hooks_idx = 0xFE (WETH sentinel) — not a valid hook contract.
        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        bytes memory commands =
            Cmd.v4SwapCompact(wethIdx, usdcIdx, 3000, 60, Cmd.SENTINEL_WETH, true, 1 ether);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectRevert(o, bytes(""), "WETH hook sentinel");
    }



    function testV4SwapDynamicSelfHookSentinelReverts() public {
        /// V4_SWAP_DYNAMIC with hooks_idx = 0xFD (SELF sentinel) — not a valid
        /// hook contract.
        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        bytes memory commands =
            Cmd.v4SwapDynamic(wethIdx, usdcIdx, 3000, 60, Cmd.SENTINEL_SELF, true);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectRevert(o, bytes(""), "SELF hook sentinel");
    }



    /// hooks_idx = 0xFF (NATIVE sentinel = no hooks) is the one valid
    /// sentinel: the hook lookup must pass. The swap then runs at top level,
    /// outside a V4 unlock, so the PoolManager's lock guard is the identified
    /// downstream revert; success would mean the pool took the swap. Either
    /// way the hooks sentinel guard must not fire.
    function testV4SwapCompactNativeHookSentinelAllowed() public {
        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        bytes memory commands =
            Cmd.v4SwapCompact(wethIdx, usdcIdx, 3000, 60, Cmd.SENTINEL_NATIVE, true, 1 ether);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectAccepted(o, _managerLocked(), "native hook sentinel");
    }



    // ── M7: V4_SETTLE_DELTA sentinels raise InvalidCommand ────────────────



    function testSettleDeltaPmSentinelReverts() public {
        Cmd.Table memory at = _emptyTable();
        bytes memory commands = Cmd.v4Unlock(Cmd.v4SettleDelta(Cmd.SENTINEL_PM));
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectRevert(
            o,
            _executorRevert(ExecutorErrors.INVALID_COMMAND, Cmd.SENTINEL_PM),
            "PM sentinel settle"
        );
    }



    function testSettleDeltaSelfSentinelReverts() public {
        Cmd.Table memory at = _emptyTable();
        bytes memory commands = Cmd.v4Unlock(Cmd.v4SettleDelta(Cmd.SENTINEL_SELF));
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        _expectRevert(
            o,
            _executorRevert(ExecutorErrors.INVALID_COMMAND, Cmd.SENTINEL_SELF),
            "SELF sentinel settle"
        );
    }



    // ── M3: bribe recipient index bounds ──────────────────────────────────



    /// A minimal valid execution stream for the config guard under test.
    /// A stream whose execution section is empty reverts in the dispatch loop
    /// before any config handling, so the guard needs one benign command:
    /// V4_TAKE_DELTA on a zero delta is a clean no-op and moves no value, so
    /// profit stays zero.
    function _noOpStream() internal pure returns (bytes memory) {
        bytes memory commands = Cmd.v4Unlock(Cmd.v4TakeDelta(Cmd.SENTINEL_WETH, Cmd.SENTINEL_SELF));
        return bytes.concat(Cmd.preamble(_emptyTable()), commands);
    }



    function testBribeRecipientIdx32Reverts() public {
        // bips=1, idx=32, mode=1 — idx=32 is out of bounds.
        uint256 config = (uint256(1) << 8) | (uint256(32) << 24) | 1;
        Outcome memory o = _executeRaw(executor, _noOpStream(), config);
        _expectRevert(o, _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), "bribe recipient idx 32");
    }



    function testBribeRecipientIdxSentinelReverts() public {
        // idx = 0xFC (PM sentinel) — not a meaningful bribe target.
        uint256 config = (uint256(1) << 8) | (uint256(Cmd.SENTINEL_PM) << 24) | 1;
        Outcome memory o = _executeRaw(executor, _noOpStream(), config);
        _expectRevert(
            o,
            _executorRevert(ExecutorErrors.INVALID_COMMAND, Cmd.SENTINEL_PM),
            "bribe recipient idx sentinel"
        );
    }



    /// idx=31 is the max valid table slot and must pass the guard. The
    /// stream earns no profit, so no bribe transfer is attempted and the
    /// transaction must succeed end to end.
    function testBribeRecipientIdx31Accepted() public {
        uint256 config = (uint256(1) << 8) | (uint256(31) << 24) | 1;
        Outcome memory o = _executeRaw(executor, _noOpStream(), config);
        _expectAccepted(o, bytes(""), "bribe recipient idx 31");
    }



    // ── Meta-tests: the revert helpers themselves ─────────────────────────



    /// The helpers must fail on success, on reverts carrying no identity, on
    /// reverts carrying the wrong identity, and must never swallow an
    /// unrelated error — the behaviors a catch-all handler destroys. Each
    /// negative control runs through a self-call so the helper's own
    /// rejection is observed without aborting the test.

    function expectRevertNC(
        bool ok,
        bytes memory actualRevertData,
        bytes memory expectedRevertData,
        string memory label
    )
        public
        pure
    {
        _expectRevert(Outcome(ok, actualRevertData, 0, 0, 0), expectedRevertData, label);
    }



    function expectAcceptedNC(
        bool ok,
        bytes memory actualRevertData,
        bytes memory downstreamRevertData,
        string memory label
    )
        public
        pure
    {
        _expectAccepted(Outcome(ok, actualRevertData, 0, 0, 0), downstreamRevertData, label);
    }



    function _runExpectRevertNC(
        bool ok,
        bytes memory actualRevertData,
        bytes memory expectedRevertData,
        string memory label
    )
        private
        returns (bytes memory ret)
    {
        (, ret) = _callExpectRevert(
            address(this),
            abi.encodeCall(this.expectRevertNC, (ok, actualRevertData, expectedRevertData, label))
        );
    }



    function _runExpectAcceptedNC(
        bool ok,
        bytes memory actualRevertData,
        bytes memory downstreamRevertData,
        string memory label
    )
        private
        returns (bytes memory ret)
    {
        (, ret) = _callExpectRevert(
            address(this),
            abi.encodeCall(this.expectAcceptedNC, (ok, actualRevertData, downstreamRevertData, label))
        );
    }



    /// A success must never pass a revert assertion, and the failure must be
    /// the helper's own Error(string) — not a silent pass.
    function testExpectRevertFailsOnSuccess() public {
        bytes memory ret =
            _runExpectRevertNC(true, bytes(""), _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(_contains(ret, "expected revert, call succeeded"), "the success case is named");
    }



    /// A revert carrying no data has no identity to pin — the analogue of the
    /// source's silent reverted receipt.
    function testExpectRevertFailsOnSilentRevertedReceipt() public {
        bytes memory ret =
            _runExpectRevertNC(false, bytes(""), _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(_contains(ret, "revert data mismatch"), "the missing identity is named");
    }



    /// An unrelated error must never be accepted as the expected revert: the
    /// helper rejects it, and its rejection is the helper's own Error(string),
    /// not the unrelated error bubbling through.
    function testExpectRevertPropagatesUnrelatedException() public {
        bytes memory ret = _runExpectRevertNC(
            false, _unrelatedHarnessError(), _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), "nc"
        );
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "rejection is the helper's, not the unrelated error");
        assertTrue(_contains(ret, "revert data mismatch"), "the unrelated error is named a mismatch");
    }



    /// A revert carrying a different identified payload fails the exact
    /// custom-error pin.
    function testExpectRevertFailsOnWrongError() public {
        bytes memory ret =
            _runExpectRevertNC(false, _managerLocked(), _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(_contains(ret, "revert data mismatch"), "the wrong error is named");
    }



    /// Same custom error, different argument — the discriminator the
    /// wrong-error control cannot cover.
    function testExpectRevertFailsOnWrongOpcode() public {
        bytes memory ret = _runExpectRevertNC(
            false,
            _executorRevert(ExecutorErrors.INVALID_COMMAND, 7),
            _executorRevert(ExecutorErrors.INVALID_COMMAND, 32),
            "nc"
        );
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(_contains(ret, "revert data mismatch"), "the wrong opcode is named");
    }



    /// A revert carrying an identity fails the empty-data pin.
    function testExpectRevertFailsOnWrongMessage() public {
        bytes memory ret =
            _runExpectRevertNC(false, _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), bytes(""), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(_contains(ret, "revert data mismatch"), "the unexpected identity is named");
    }



    function testExpectAcceptedFailsOnGuardRevert() public {
        bytes memory ret =
            _runExpectAcceptedNC(false, _executorRevert(ExecutorErrors.INVALID_COMMAND, 32), bytes(""), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(_contains(ret, "expected the guard to pass"), "the guard revert is named");
    }



    function testExpectAcceptedFailsOnUnidentifiedRevert() public {
        bytes memory ret = _runExpectAcceptedNC(false, bytes(""), _managerLocked(), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "the helper rejects, it does not pass");
        assertTrue(
            _contains(ret, "expected success or the identified downstream revert"),
            "the unidentified revert is named"
        );
    }



    function testExpectAcceptedPropagatesUnrelatedException() public {
        bytes memory ret =
            _runExpectAcceptedNC(false, _unrelatedHarnessError(), _managerLocked(), "nc");
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "rejection is the helper's, not the unrelated error");
        assertTrue(
            _contains(ret, "expected success or the identified downstream revert"),
            "the unrelated error is named a mismatch"
        );
    }



    function testExpectAcceptedAcceptsSuccess() public pure {
        expectAcceptedNC(true, bytes(""), _managerLocked(), "nc");
    }



    function testExpectAcceptedAcceptsIdentifiedDownstream() public pure {
        expectAcceptedNC(false, _managerLocked(), _managerLocked(), "nc");
    }



    // ── Local helpers ─────────────────────────────────────────────────────



    error UnrelatedHarnessError(uint256 payload);



    function _unrelatedHarnessError() private pure returns (bytes memory) {
        return abi.encodeWithSelector(UnrelatedHarnessError.selector, uint256(1));
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
}
