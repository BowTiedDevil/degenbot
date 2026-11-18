// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "../src/TestBase.sol";

/// Optional RPC lane: the EIP-2930 access-list comparison the Ape lane's
/// `run_gas_benchmark` / `compute_access_list` helpers used to print
/// (migration manifest dispositions 7 and 11). Isolated from the offline lane
/// three ways:
///
///   - Lane switch. `DEGENBOT_ACCESS_LIST_RPC` is set only by
///     `just test-executor-access-list`; the canonical lanes exclude this suite,
///     so they compile it and never dial an endpoint.
///   - Endpoint. `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` resolves through the
///     same declared layers `test-fork` uses (process env > tests.env), and
///     the recipe probes `eth_chainId` (chain 1) plus `web3_clientVersion`
///     BEFORE forge runs, failing closed on a missing or unreachable node.
///   - Subject. The measured call is a REAL mainnet call object, estimated by
///     the node at one pinned block.
///
/// ## Why the subject is a mainnet router swap, not "the executor's stream"
///
/// The Ape helper could benchmark the executor's own stream because Ape's
/// provider is a local chain: the executor was deployed on the node the
/// `eth_createAccessList` trace ran against. A Foundry test's contracts live
/// only in forge's in-process EVM — the declared endpoint has never heard of
/// them — and forge's local EVM never applies EIP-2930 intrinsic gas, so no
/// local run can produce a with/without-access-list pair at all. Fabricating
/// one from a model would be a claim, not a measurement. So this lane measures
/// node truth on a real call whose cold-account/cold-slot footprint is the
/// same family the executor's pool legs warm (router -> pool -> both tokens,
/// including the token balance slots), and the executor's own per-path gas
/// evidence stays where it belongs: the committed offline snapshots
/// (`.gas-snapshot`, `.gas-snapshot-production`, `.gas-snapshot-evidence`).
///
/// ## What "the same call" means here, and what is reset
///
/// One call object (from, to, data, value) is built once and reused verbatim
/// by `eth_createAccessList` and both `eth_estimateGas` requests; the only
/// difference between the two estimates is the `accessList` field, serialized
/// from the entries the node itself returned. Both estimates name the same
/// pinned block, and every request is bracketed by `vm.snapshotState()` /
/// `vm.revertToState()` so all three are issued from a byte-identical local
/// state. Honest limitation: an `eth_estimateGas` call is a read-only
/// simulation, so there is no node state to replay-reset between runs — the
/// block pin is what holds the remote state fixed. A test that actually sent
/// the two transactions would need a signed sender the declared endpoint
/// accepts, which an archive node does not provide.
///
/// No gas figure here is hardcoded: both values come back from the node on the
/// run, and the assertions only pin relations that must hold for a genuine
/// measurement of this call.
contract AccessListBenchmarkTest is TestBase {
    /// Lane switch, set only by `just test-executor-access-list`.
    string internal constant LANE_ENV = "DEGENBOT_ACCESS_LIST_RPC";
    /// The declared archive endpoint (same resolution layers as `test-fork`).
    string internal constant RPC_ENV = "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI";

    /// Pinned mainnet height: past the V4 deployment and inside the archive
    /// window, so every request in this file reads identical remote state.
    uint256 internal constant AL_BLOCK = 22_000_000;

    /// Canonical mainnet `SwapRouter02`, WETH and USDC. The call sells a small
    /// amount of ETH for USDC through the deepest WETH/USDC pool (fee 500).
    address internal constant SWAP_ROUTER_02 = 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45;
    address internal constant MAINNET_WETH = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
    address internal constant MAINNET_USDC = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
    uint24 internal constant FEE_TIER = 500;
    uint256 internal constant AMOUNT_IN = 1e15;

    /// A real mainnet EOA holding ETH at `AL_BLOCK`, used as the call sender:
    /// the call is real, so its sender must be able to pay the value. The
    /// provenance assertions below (EOA, balance covers the value) make a
    /// repointed endpoint fail closed instead of estimating a call that cannot
    /// execute untouched.
    address internal constant SENDER = 0x28C6c06298d514Db089934071355E5743bf21d60;

    /// `eth_createAccessList`'s result shape as the cheatcode hands it back:
    /// the JSON object's `accessList` entries and its hex-quantity `gasUsed`
    /// (hex-decoded by the cheatcode, so it arrives as bytes).
    struct AlEntry {
        address addr;
        bytes32[] keys;
    }

    struct AlResult {
        AlEntry[] accessList;
        bytes gasUsed;
    }

    event AccessListSummary(uint256 entries, uint256 storageKeys, uint256 tracedGasUsed);
    event AccessListFigures(uint256 gasNoAccessList, uint256 gasWithAccessList);

    /// DSTest-style log surfaces. Forge prints these at `-vv` (plain custom
    /// events only appear in a full trace), so the two figures this lane exists
    /// to report are visible in an ordinary recipe run instead of living only
    /// in the assertions.
    event log_named_uint(string key, uint256 val);
    event log_named_string(string key, string val);

    string internal endpoint;

    function setUp() public {
        if (bytes(vm.envOr(LANE_ENV, "")).length == 0) {
            vm.skip(true);
            return;
        }
        endpoint = vm.envOr(RPC_ENV, "");
        require(bytes(endpoint).length != 0, "access-list lane: no archive endpoint declared");
    }

    // ── Request construction ──────────────────────────────────────────────

    function _rpc(string memory method, string memory params) internal returns (bytes memory) {
        return vm.rpc(endpoint, method, params);
    }

    /// Quantity results (`eth_estimateGas`, `eth_getBalance`, `eth_chainId`,
    /// and the `gasUsed` field) arrive hex-decoded; read them big-endian.
    function _quantity(bytes memory raw) internal pure returns (uint256 value) {
        for (uint256 i; i < raw.length; ++i) {
            value = (value << 8) | uint8(raw[i]);
        }
    }

    /// Minimal hex quantity (`0x…`, no leading zeros), the JSON-RPC encoding
    /// the endpoint expects for `value` and block numbers.
    function _hexUint(uint256 value) internal pure returns (string memory) {
        if (value == 0) return "0x0";
        bytes memory digits = new bytes(64);
        uint256 used;
        while (value != 0) {
            uint256 nibble = value & 0xf;
            digits[63 - used] = nibble < 10
                ? bytes1(uint8(48 + nibble))
                : bytes1(uint8(87 + nibble));
            value >>= 4;
            ++used;
        }
        bytes memory out = new bytes(2 + used);
        out[0] = "0";
        out[1] = "x";
        for (uint256 i; i < used; ++i) {
            out[2 + i] = digits[64 - used + i];
        }
        return string(out);
    }

    /// `SwapRouter02.exactInputSingle((tokenIn,tokenOut,fee,recipient,amountIn,
    /// amountOutMinimum,sqrtPriceLimitX96))` — ETH in, USDC out, output back to
    /// the sender. A struct parameter is ABI-encoded as its flat tuple, so the
    /// fields are passed directly.
    function _swapCalldata() internal pure returns (bytes memory) {
        return abi.encodeWithSelector(
            bytes4(keccak256("exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))")),
            MAINNET_WETH,
            MAINNET_USDC,
            FEE_TIER,
            SENDER,
            AMOUNT_IN,
            uint256(0),
            uint256(0)
        );
    }

    /// The one call object under test, without its closing brace so the
    /// `accessList` field can be appended for the treated run.
    function _txBody() internal pure returns (string memory) {
        return string.concat(
            '{"from":"',
            vm.toString(SENDER),
            '","to":"',
            vm.toString(SWAP_ROUTER_02),
            '","data":"',
            vm.toString(_swapCalldata()),
            '","value":"',
            _hexUint(AMOUNT_IN),
            '"'
        );
    }

    function _params(string memory txBody, string memory blockJson)
        internal
        pure
        returns (string memory)
    {
        return string.concat("[", txBody, "}", ",", blockJson, "]");
    }

    /// Serialize the decoded entries back to JSON-RPC form, so the treated
    /// run uses exactly what the node returned (a corrupted key would make the
    /// treated estimate fail rather than silently measuring something else).
    function _alJson(AlResult memory result) internal pure returns (string memory out) {
        out = "[";
        for (uint256 i; i < result.accessList.length; ++i) {
            if (i != 0) out = string.concat(out, ",");
            out = string.concat(
                out,
                '{"address":"',
                vm.toString(result.accessList[i].addr),
                '","storageKeys":['
            );
            for (uint256 j; j < result.accessList[i].keys.length; ++j) {
                if (j != 0) out = string.concat(out, ",");
                out = string.concat(out, '"', vm.toString(result.accessList[i].keys[j]), '"');
            }
            out = string.concat(out, "]}");
        }
        return string.concat(out, "]");
    }

    // ── Response validation ───────────────────────────────────────────────

    /// Exact shape checks on what the node returned: a non-empty entry set,
    /// no zero address, at least one warmed slot, and a hex-quantity gasUsed
    /// no wider than a uint256. Anything else is not the object this benchmark
    /// claims to re-serialize.
    function _validateAlResult(AlResult memory result)
        internal
        returns (uint256 entries, uint256 keys)
    {
        assertGt(result.accessList.length, 0, "createAccessList returned no entries");
        assertGt(result.gasUsed.length, 0, "createAccessList returned an empty gasUsed");
        assertLe(result.gasUsed.length, 32, "gasUsed is wider than a uint256 quantity");
        for (uint256 i; i < result.accessList.length; ++i) {
            assertTrue(
                result.accessList[i].addr != address(0),
                "access-list entry carries the zero address"
            );
            keys += result.accessList[i].keys.length;
        }
        assertGt(keys, 0, "access list warms no storage slot");
        entries = result.accessList.length;
        emit AccessListSummary(entries, keys, _quantity(result.gasUsed));
    }

    // ── Cases ─────────────────────────────────────────────────────────────

    /// Node identity, from inside the suite: the lane's benchmark claims must
    /// be about Ethereum mainnet, and a client that cannot name itself cannot
    /// back any claim at all.
    function testRpcLaneEndpointIdentity() public {
        assertEq(_quantity(_rpc("eth_chainId", "[]")), 1, "lane needs Ethereum mainnet");
        assertGt(
            _rpc("web3_clientVersion", "[]").length,
            0,
            "endpoint answered no client version"
        );
    }

    /// The access-list comparison: the same call estimated bare and with the
    /// node's returned list, at one pinned block.
    function testAccessListGasComparison() public {
        string memory blockJson = string.concat('"', _hexUint(AL_BLOCK), '"');

        // Sender/router provenance at the pinned block.
        uint256 senderBalance = _quantity(
            _rpc(
                "eth_getBalance",
                string.concat('["', vm.toString(SENDER), '",', blockJson, "]")
            )
        );
        assertGe(senderBalance, AMOUNT_IN, "pinned sender cannot cover the call value");
        assertEq(
            _rpc(
                "eth_getCode",
                string.concat('["', vm.toString(SENDER), '",', blockJson, "]")
            ).length,
            0,
            "pinned sender must be an EOA"
        );
        assertGt(
            _rpc(
                "eth_getCode",
                string.concat('["', vm.toString(SWAP_ROUTER_02), '",', blockJson, "]")
            ).length,
            0,
            "router has no code at the pinned block"
        );

        string memory txParams = _params(_txBody(), blockJson);

        // Run 1: the node traces the call and returns its optimal access list.
        uint256 snapshot = vm.snapshotState();
        AlResult memory result = abi.decode(_rpc("eth_createAccessList", txParams), (AlResult));
        vm.revertToState(snapshot);
        (uint256 entries, uint256 keys) = _validateAlResult(result);
        emit log_named_string("access-list/endpoint", endpoint);
        emit log_named_uint("access-list/block", AL_BLOCK);
        emit log_named_uint("access-list/entries", entries);
        emit log_named_uint("access-list/storage-keys", keys);
        emit log_named_uint("access-list/traced-gas-used", _quantity(result.gasUsed));

        // Run 2: the SAME call object, no access list.
        snapshot = vm.snapshotState();
        uint256 gasNoAccessList = _quantity(_rpc("eth_estimateGas", txParams));
        vm.revertToState(snapshot);

        // Run 3: the SAME call object, plus exactly the returned entries.
        string memory withAlBody =
            string.concat(_txBody(), ',"accessList":', _alJson(result));
        snapshot = vm.snapshotState();
        uint256 gasWithAccessList =
            _quantity(_rpc("eth_estimateGas", _params(withAlBody, blockJson)));
        vm.revertToState(snapshot);

        emit AccessListFigures(gasNoAccessList, gasWithAccessList);
        emit GasEvidence("access-list/no-access-list", gasNoAccessList);
        emit GasEvidence("access-list/with-access-list", gasWithAccessList);
        emit log_named_uint("access-list/gas-without-list", gasNoAccessList);
        emit log_named_uint("access-list/gas-with-list", gasWithAccessList);
        emit log_named_uint(
            "access-list/saved", gasNoAccessList - gasWithAccessList
        );

        // Both figures are total transaction gas for one identical call.
        assertGe(gasNoAccessList, 21_000, "bare estimate below the intrinsic cost");
        assertGe(gasWithAccessList, 21_000, "listed estimate below the intrinsic cost");
        // The traced execution gas is one call frame inside the estimate that
        // carries it, so it can never exceed that estimate.
        assertLe(
            _quantity(result.gasUsed),
            gasWithAccessList,
            "traced gas exceeds the access-list estimate"
        );
        // Measured on the declared endpoint at the pinned block: warming the
        // accounts and slots this call touches costs less than the cold
        // accesses it removes. A call with a thinner footprint can go the
        // other way — that is a property of the call, not of access lists.
        assertLt(gasWithAccessList, gasNoAccessList, "access list did not reduce the estimate");
    }
}
