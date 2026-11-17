// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Vm} from "./Vm.sol";
import {ExecutorErrors} from "./generated/Errors.sol";
import {Cmd} from "./Cmd.sol";
import {ICmdExecutor} from "./generated/ICmdExecutor.sol";
import {IFakeErc20} from "./generated/IFakeErc20.sol";
import {IFakeWeth} from "./generated/IFakeWeth.sol";
import {IFakeUniswapV2Pair} from "./generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "./generated/IFakeUniswapV3Pool.sol";
import {IFakeUniswapV4PoolManager} from "./generated/IFakeUniswapV4PoolManager.sol";
import {PoolKey, ExecuteResult} from "./generated/Types.sol";

/// Minimal balanceOf surface for conservation snapshots. Any token the tests
/// track — the fake ERC20s, WETH, or a pool — satisfies it.
interface TokenLike {
    function balanceOf(address account) external view returns (uint256);
}


/// Base harness: artifact deployment, assertions, and execution helpers.
///
/// The test contract is the owner EOA (the analogue of Ape's `accounts[0]`):
/// every owner-side action is a plain call from `address(this)`, so `msg.sender`
/// is the owner at the top level and the executor itself inside callbacks —
/// exactly the call graph Ape's `sender=owner_account` produces.
abstract contract TestBase {
    Vm internal constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    address internal constant NATIVE = address(0);
    bytes4 internal constant EXECUTE_SELECTOR = bytes4(keccak256("execute(bytes,uint256)"));
    int24 internal constant MIN_TICK = -887272;
    int24 internal constant MAX_TICK = 887272;

    /// The test contract stands in for the owner EOA, so it must be able to
    /// receive ETH (WETH.withdraw, executor payouts, bribes).
    receive() external payable {}

    // ── Deployment from the pinned-vyper artifacts ────────────────────────

    function _artifact(string memory name) internal view returns (string memory) {
        return vm.readFile(string.concat("./artifacts/", name, ".json"));
    }

    function _deploy(string memory name, bytes memory ctorArgs) internal returns (address addr) {
        bytes memory code = vm.parseJsonBytes(_artifact(name), ".bytecode");
        bytes memory init = bytes.concat(code, ctorArgs);
        assembly {
            addr := create(0, add(init, 0x20), mload(init))
        }
        require(addr != address(0), string.concat("deploy failed: ", name));
    }

    function _runtime(string memory name) internal view returns (bytes memory) {
        return vm.parseJsonBytes(_artifact(name), ".deployedBytecode");
    }

    // ── Execution helpers ─────────────────────────────────────────────────

    struct Outcome {
        bool ok;
        bytes revertData;
        uint256 gasUsed;
        uint256 grossProfit;
        uint256 bribePaid;
    }

    /// Call `execute` and report the outcome without bubbling the revert.
    function _executeRaw(ICmdExecutor ex, bytes memory commands, uint256 config, uint256 value)
        internal
        returns (Outcome memory o)
    {
        bytes memory data = abi.encodeWithSelector(EXECUTE_SELECTOR, commands, config);
        uint256 before = gasleft();
        (o.ok, o.revertData) = address(ex).call{value: value}(data);
        o.gasUsed = before - gasleft();
        if (o.ok && o.revertData.length >= 64) {
            ExecuteResult memory r = abi.decode(o.revertData, (ExecuteResult));
            o.grossProfit = r.gross_profit;
            o.bribePaid = r.bribe_paid;
        }
    }

    /// Call `execute` and require success.
    function _execute(ICmdExecutor ex, bytes memory commands, uint256 config, uint256 value)
        internal
        returns (Outcome memory o)
    {
        o = _executeRaw(ex, commands, config, value);
        if (!o.ok) {
            revert(
                string.concat(
                    "execute reverted: ", _hexPreview(o.revertData), " ", _revertName(o.revertData)
                )
            );
        }
    }

    function _execute(ICmdExecutor ex, bytes memory commands, uint256 config)
        internal
        returns (Outcome memory)
    {
        return _execute(ex, commands, config, 0);
    }

    /// The Vyper signature's declared default (`config: uint256 = 1`), which a
    /// Solidity interface cannot express — the port must state it explicitly.
    function _execute(ICmdExecutor ex, bytes memory commands) internal returns (Outcome memory) {
        return _execute(ex, commands, 1, 0);
    }

    function _executeRaw(ICmdExecutor ex, bytes memory commands) internal returns (Outcome memory) {
        return _executeRaw(ex, commands, 1, 0);
    }

    /// Raw execution with the Vyper default `config = 1` made explicit.
    function _executeRaw(ICmdExecutor ex, bytes memory commands, uint256 config)
        internal
        returns (Outcome memory)
    {
        return _executeRaw(ex, commands, config, 0);
    }

    /// eth_call-equivalent: run `execute` and roll the state back.
    function _executeSimulated(ICmdExecutor ex, bytes memory commands, uint256 config)
        internal
        returns (Outcome memory o)
    {
        uint256 snapshot = vm.snapshotState();
        o = _executeRaw(ex, commands, config, 0);
        vm.revertToState(snapshot);
    }

    // ── Log helpers (port of `tests/executor/verify.py`) ─────────────────

    bytes32 internal constant TRANSFER_TOPIC = keccak256("Transfer(address,address,uint256)");
    bytes32 internal constant ERC6909_TRANSFER_TOPIC =
        keccak256("Transfer(address,address,address,uint256,uint256)");
    /// The canonical V2 `Swap` event is the six-field shape (with the trailing
    /// `to`), so the fake pair — which logs the canonical signature through the
    /// `IUniswapV2Pair` interface — shares the canonical topic0. The name only
    /// records where the port uses it: the fake pair's Swap emissions.
    bytes32 internal constant FAKE_V2_SWAP_TOPIC =
        keccak256("Swap(address,uint256,uint256,uint256,uint256,address)");
    bytes32 internal constant V2_SYNC_TOPIC = keccak256("Sync(uint112,uint112)");

    /// Count physical ERC20 transfers plus ERC6909 `Transfer` emissions.
    function _countTransfers(Vm.Log[] memory logs) internal pure returns (uint256 n) {
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics.length == 0) continue;
            if (logs[i].topics[0] == TRANSFER_TOPIC || logs[i].topics[0] == ERC6909_TRANSFER_TOPIC)
            {
                n++;
            }
        }
    }

    /// Physical transfers to/from `who`, per raw topic0 — the tracked-transfer
    /// accounting the port of `tests/executor/verify.py` builds its count
    /// assertions on (each ERC20/ERC6909 `Transfer` is one physical transfer).
    ///
    /// Only the two Transfer topics participate. The fake V2 `Swap` event is
    /// not a transfer and must not move any count. The ERC6909 topic layout is
    /// `[signature, from, to, id]` — the trailing id topic is a token id that
    /// can collide with an address's integer value, so it is never compared.
    function _countTransfersInvolving(Vm.Log[] memory logs, address who)
        internal
        pure
        returns (uint256 n)
    {
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics.length < 3) continue;
            bytes32 topic0 = logs[i].topics[0];
            if (topic0 != TRANSFER_TOPIC && topic0 != ERC6909_TRANSFER_TOPIC) {
                continue;
            }
            // ERC20 Transfer: from = topic1, to = topic2. ERC6909 Transfer:
            // from = topic1, to = topic2 (topic3 is the token id).
            if (
                address(uint160(uint256(logs[i].topics[1]))) == who
                    || address(uint160(uint256(logs[i].topics[2]))) == who
            ) {
                n++;
            }
        }
    }

    /// Balance snapshot over (token, account) pairs — the port of
    /// `verify.py`'s `snapshot_balances`. A `NATIVE` token entry records the
    /// account's ETH balance; every other entry reads `balanceOf`.
    struct BalanceSnapshot {
        address[] tokens;
        address[] accounts;
        uint256[] balances; // tokens x accounts, row-major
    }

    function _snapshotBalances(address[] memory tokens, address[] memory accounts)
        internal
        view
        returns (BalanceSnapshot memory s)
    {
        s.tokens = tokens;
        s.accounts = accounts;
        s.balances = new uint256[](tokens.length * accounts.length);
        for (uint256 i = 0; i < tokens.length; i++) {
            for (uint256 j = 0; j < accounts.length; j++) {
                s.balances[i * accounts.length + j] = tokens[i] == NATIVE
                    ? accounts[j].balance
                    : TokenLike(tokens[i]).balanceOf(accounts[j]);
            }
        }
    }

    /// Signed balance change of one (token, account) pair between snapshots.
    function _balanceDelta(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 tokenIdx,
        uint256 accountIdx
    )
        internal
        pure
        returns (int256)
    {
        uint256 w = beforeSnap.accounts.length;
        uint256 k = tokenIdx * w + accountIdx;
        return int256(afterSnap.balances[k]) - int256(beforeSnap.balances[k]);
    }

    /// Sum of balance changes for one token across ALL tracked accounts. A
    /// nonzero sum means tokens entered or left the tracked set — the leak
    /// signal `test_conservation.py` asserts on.
    function _tokenDeltaSum(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 tokenIdx
    ) internal pure returns (int256 total) {
        for (uint256 j = 0; j < beforeSnap.accounts.length; j++) {
            total += _balanceDelta(beforeSnap, afterSnap, tokenIdx, j);
        }
    }

    /// Combined WETH+ETH delta of one account — the executor profit domain
    /// (profit mode 1).
    function _combinedWethEthDelta(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 wethTokenIdx,
        uint256 accountIdx
    )
        internal
        pure
        returns (int256)
    {
        uint256 nativeIdx = _tokenIdx(beforeSnap, NATIVE);
        return _balanceDelta(beforeSnap, afterSnap, wethTokenIdx, accountIdx)
            + _balanceDelta(beforeSnap, afterSnap, nativeIdx, accountIdx);
    }

    function _tokenIdx(BalanceSnapshot memory s, address token) private pure returns (uint256) {
        for (uint256 i = 0; i < s.tokens.length; i++) {
            if (s.tokens[i] == token) return i;
        }
        revert("token not in snapshot");
    }

    function _assertTokenConserved(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 tokenIdx,
        string memory label
    )
        internal
        pure
    {
        int256 total = _tokenDeltaSum(beforeSnap, afterSnap, tokenIdx);
        if (total != 0) {
            revert(
                string.concat(
                    label,
                    ": token conservation violated, sum of tracked balance changes = ",
                    _toStringSigned(total)
                )
            );
        }
    }

    function _assertWethEthConserved(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 wethTokenIdx,
        string memory label
    )
        internal
        pure
    {
        uint256 nativeIdx = _tokenIdx(beforeSnap, NATIVE);
        int256 total = _tokenDeltaSum(beforeSnap, afterSnap, wethTokenIdx)
            + _tokenDeltaSum(beforeSnap, afterSnap, nativeIdx);
        if (total != 0) {
            revert(
                string.concat(
                    label,
                    ": WETH+ETH conservation violated, combined tracked change = ",
                    _toStringSigned(total)
                )
            );
        }
    }

    /// The executor profit check: combined WETH+ETH delta of one account must
    /// equal the expectation exactly (profit mode 1 semantics).
    function _assertExecutorProfit(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        uint256 wethTokenIdx,
        uint256 accountIdx,
        int256 expected,
        string memory label
    )
        internal
        pure
    {
        int256 actual = _combinedWethEthDelta(beforeSnap, afterSnap, wethTokenIdx, accountIdx);
        if (actual != expected) {
            revert(
                string.concat(
                    label,
                    ": executor WETH+ETH profit mismatch, expected ",
                    _toStringSigned(expected),
                    ", got ",
                    _toStringSigned(actual)
                )
            );
        }
    }

    function _toStringSigned(int256 value) private pure returns (string memory) {
        if (value < 0) return string.concat("-", vmToString(uint256(-value)));
        return vmToString(uint256(value));
    }

    // ── Exact revert assertions (generated vocabulary) ─────────────────────

    /// Require an execution outcome to have reverted with exactly
    /// `expectedRevertData` — selector and arguments both. `ExecutorErrors`
    /// (generated from the artifact ABI + the release error map) supplies the
    /// selectors, so no test hand-writes one.
    function _assertRevertData(
        Outcome memory o,
        bytes memory expectedRevertData,
        string memory label
    )
        internal
        pure
    {
        assertFalse(o.ok, string.concat(label, ": expected revert, call succeeded"));
        if (keccak256(o.revertData) != keccak256(expectedRevertData)) {
            revert(
                string.concat(
                    label,
                    ": revert data mismatch, got ",
                    _hexPreview(o.revertData),
                    " (",
                    _revertName(o.revertData),
                    "), expected ",
                    _hexPreview(expectedRevertData),
                    " (",
                    _revertName(expectedRevertData),
                    ")"
                )
            );
        }
    }

    /// Selector-only form, for reverts whose arguments are not pinned.
    function _assertRevertSelector(
        Outcome memory o,
        bytes4 selector,
        string memory label
    )
        internal
        pure
    {
        assertFalse(o.ok, string.concat(label, ": expected revert, call succeeded"));
        assertTrue(
            o.revertData.length >= 4 && bytes4(o.revertData) == selector,
            string.concat(
                label,
                ": revert selector mismatch, got ",
                _hexPreview(o.revertData),
                " (",
                _revertName(o.revertData),
                ")"
            )
        );
    }

    /// ABI-encode an expected executor revert from the generated vocabulary.
    function _executorRevert(bytes4 selector) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(selector);
    }

    function _executorRevert(bytes4 selector, uint256 a) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(selector, a);
    }

    function _executorRevert(bytes4 selector, uint256 a, uint256 b)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodeWithSelector(selector, a, b);
    }

    function _executorRevert(bytes4 selector, address a) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(selector, a);
    }


    function _countTopic(Vm.Log[] memory logs, bytes32 topic) internal pure returns (uint256 n) {
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics.length > 0 && logs[i].topics[0] == topic) n++;
        }
    }

    /// Call `target` with raw calldata, requiring success.
    function _call(address target, bytes memory data) internal returns (bytes memory) {
        (bool ok, bytes memory ret) = target.call(data);
        require(ok, string.concat("call reverted: ", _hexPreview(ret)));
        return ret;
    }

    /// Call `target` with raw calldata; return the raw revert payload.
    function _callExpectRevert(address target, bytes memory data)
        internal
        returns (bool ok, bytes memory revertData)
    {
        (ok, revertData) = target.call(data);
    }

    function _hexPreview(bytes memory data) internal pure returns (string memory) {
        bytes memory alphabet = "0123456789abcdef";
        uint256 n = data.length < 36 ? data.length : 36;
        bytes memory out = new bytes(2 + 2 * n);
        out[0] = "0";
        out[1] = "x";
        for (uint256 i = 0; i < n; i++) {
            out[2 + 2 * i] = alphabet[uint8(data[i]) >> 4];
            out[3 + 2 * i] = alphabet[uint8(data[i]) & 0x0f];
        }
        return string(out);
    }

    /// Name a revert from the GENERATED vocabulary
    /// (`src/generated/Errors.sol`) — there is no second hand-written table.
    function _revertName(bytes memory data) internal pure returns (string memory) {
        if (data.length < 4) return "(empty)";
        string memory named = ExecutorErrors.nameOf(bytes4(data));
        if (keccak256(bytes(named)) == keccak256(bytes("unknown"))) {
            return string.concat("selector ", _hexPreview(abi.encodePacked(bytes4(data))));
        }
        return named;
    }

    // ── Assertions ────────────────────────────────────────────────────────

    function fail(string memory message) internal pure {
        revert(message);
    }

    function assertTrue(bool value, string memory message) internal pure {
        if (!value) revert(message);
    }

    function assertTrue(bool value) internal pure {
        assertTrue(value, "assertTrue failed");
    }

    function assertFalse(bool value, string memory message) internal pure {
        if (value) revert(message);
    }

    function assertEq(uint256 a, uint256 b, string memory message) internal pure {
        if (a != b) {
            revert(
                string.concat(
                    message, ": ", vmToString(a), " != ", vmToString(b)
                )
            );
        }
    }

    function assertEq(uint256 a, uint256 b) internal pure {
        assertEq(a, b, "assertEq(uint256)");
    }

    function assertEq(int256 a, int256 b, string memory message) internal pure {
        if (a != b) revert(message);
    }

    function assertEq(int256 a, int256 b) internal pure {
        assertEq(a, b, "assertEq(int256)");
    }

    function assertEq(address a, address b, string memory message) internal pure {
        if (a != b) revert(message);
    }

    function assertEq(address a, address b) internal pure {
        assertEq(a, b, "assertEq(address)");
    }

    function assertEq(bytes32 a, bytes32 b, string memory message) internal pure {
        if (a != b) revert(message);
    }

    function assertEq(bytes32 a, bytes32 b) internal pure {
        assertEq(a, b, "assertEq(bytes32)");
    }

    function assertEq(bytes memory a, bytes memory b, string memory message) internal pure {
        if (keccak256(a) != keccak256(b)) revert(message);
    }

    function assertEq(bytes memory a, bytes memory b) internal pure {
        assertEq(a, b, "assertEq(bytes)");
    }

    function assertEq(bool a, bool b, string memory message) internal pure {
        if (a != b) revert(message);
    }

    function assertEq(string memory a, string memory b, string memory message) internal pure {
        if (keccak256(bytes(a)) != keccak256(bytes(b))) {
            revert(string.concat(message, ": \"", a, "\" != \"", b, "\""));
        }
    }

    function assertEq(string memory a, string memory b) internal pure {
        assertEq(a, b, "assertEq(string)");
    }

    function assertGt(uint256 a, uint256 b, string memory message) internal pure {
        if (!(a > b)) revert(message);
    }

    function assertGe(uint256 a, uint256 b, string memory message) internal pure {
        if (!(a >= b)) revert(message);
    }

    function assertLt(uint256 a, uint256 b, string memory message) internal pure {
        if (!(a < b)) revert(message);
    }

    function assertLe(uint256 a, uint256 b, string memory message) internal pure {
        if (!(a <= b)) revert(message);
    }

    function vmToString(uint256 value) internal pure returns (string memory) {
        if (value == 0) return "0";
        uint256 temp = value;
        uint256 digits;
        while (temp != 0) {
            digits++;
            temp /= 10;
        }
        bytes memory buffer = new bytes(digits);
        while (value != 0) {
            digits--;
            buffer[digits] = bytes1(uint8(48 + (value % 10)));
            value /= 10;
        }
        return string(buffer);
    }
}

/// The shared fixture graph from `tests/executor/conftest.py`.
abstract contract ExecutorFixture is TestBase {
    IFakeErc20 internal usdc;
    IFakeWeth internal weth;
    IFakeErc20 internal wbtc;
    IFakeUniswapV4PoolManager internal v4Pm;
    ICmdExecutor internal executor;
    Cmd.Table internal table;

    uint256 internal constant AMOUNT_WETH = 1 ether;
    uint256 internal constant AMOUNT_USDC = 2000e6;
    uint256 internal constant AMOUNT_WBTC = 100e8;
    uint16 internal constant V2_FEE = 30;

    function setUp() public virtual {
        usdc = IFakeErc20(
            _deploy(
                "FakeErc20",
                abi.encode("Fake USD Coin", "USDC", uint8(6), uint256(100_000_000))
            )
        );
        weth = IFakeWeth(
            _deploy(
                "FakeWeth",
                abi.encode("Fake Wrapped Ether", "WETH", uint8(18), uint256(100_000_000))
            )
        );
        wbtc = IFakeErc20(
            _deploy(
                "FakeErc20",
                abi.encode("Fake Wrapped Bitcoin", "WBTC", uint8(8), uint256(100_000_000))
            )
        );
        vm.deal(address(weth), 1_000_000 ether);

        (address token0, address token1) = _sorted(address(weth), address(usdc));
        v4Pm = IFakeUniswapV4PoolManager(
            _deploy("FakeUniswapV4PoolManager", abi.encode(token0, token1))
        );

        executor = ICmdExecutor(
            _deploy(
                "CmdExecutor",
                abi.encode(address(weth), address(v4Pm), address(usdc), address(wbtc))
            )
        );
        vm.deal(address(this), 10_000 ether);
        executor.initialize{value: 1}();

        table = Cmd.newTable(address(weth), address(executor), address(v4Pm));
    }

    function _sorted(address a, address b) internal pure returns (address, address) {
        return uint160(a) < uint160(b) ? (a, b) : (b, a);
    }

    function _poolKey(address a, address b, uint24 fee, int24 tickSpacing)
        internal
        pure
        returns (PoolKey memory)
    {
        (address c0, address c1) = _sorted(a, b);
        return PoolKey(c0, c1, fee, tickSpacing, address(0));
    }

    function _poolId(PoolKey memory key) internal pure returns (bytes32) {
        return keccak256(abi.encode(key));
    }
}
