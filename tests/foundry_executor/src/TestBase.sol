// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Vm} from "./Vm.sol";
import {Cmd} from "./Cmd.sol";
import {ICmdExecutor} from "./generated/ICmdExecutor.sol";
import {IFakeErc20} from "./generated/IFakeErc20.sol";
import {IFakeWeth} from "./generated/IFakeWeth.sol";
import {IFakeUniswapV2Pair} from "./generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "./generated/IFakeUniswapV3Pool.sol";
import {IFakeUniswapV4PoolManager} from "./generated/IFakeUniswapV4PoolManager.sol";
import {PoolKey, ExecuteResult} from "./generated/Types.sol";

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
    /// The fake pair's `Swap` carries a trailing `to` field, so its topic0 does
    /// NOT match the canonical Uniswap V2 `Swap(address,uint256,uint256,uint256,uint256)`.
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

    function _revertName(bytes memory data) internal pure returns (string memory) {
        if (data.length < 4) return "";
        bytes4 selector = bytes4(data);
        if (selector == 0x08c379a0) return "(Error)";
        if (selector == 0x4e487b71) return "(Panic)";
        return string.concat("selector ", _hexPreview(abi.encodePacked(selector)));
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
