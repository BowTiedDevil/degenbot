// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {Vm} from "../src/Vm.sol";
import {PoolSetup} from "../src/Setup.sol";
import {ConservationOps} from "../src/ConservationOps.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_three_hop_production_calldata.py`.
///
/// The optimized matrix proves the contract mechanics with streams assembled by
/// `Cmd.sol`. This suite closes the other half of the contract: the exact
/// command stream the production composer
/// (`degenbot_executor::composers::encode_cmd_3_hop`) emits must execute with
/// its documented production transfer shape and conserve every tracked token.
/// No stream is rebuilt here — the Rust encoder is the sole producer and its
/// output is executed verbatim.
///
/// ## Lane
///
/// `vm.ffi` is enabled only by `forge test --ffi`, and the encoder is a Cargo
/// artifact, so the suite runs only through
/// `just test-executor-production-calldata`. That recipe builds
/// `encode_three_hop` (`just build-executor-three-hop-encoder`), enables FFI,
/// and sets `DEGENBOT_THREE_HOP_PRODUCTION_FFI=1`. Without that switch `setUp`
/// skips the contract, so the FFI-disabled ordinary tree
/// (`just test-executor-tree`) never shells out and stays green.
///
/// The encoder path resolves the Cargo target dir exactly as the justfile does
/// (`CARGO_TARGET_DIR`, else `rust/target`) and reads
/// `<target-dir>/debug/encode_three_hop` from the repository root, which is
/// `../..` relative to this forge project. The request documents carry the
/// actual deployed token, pool, PoolManager and executor addresses, so the
/// encoder's own address table resolves them and the emitted preamble is
/// complete.
///
/// ## Rows
///
/// One test per V2/V3/V4 permutation, so the test name is the row label. Each
/// row funds the three hop pools at the source's pinned prices, then asserts
/// the emitted stream's physical ERC20/ERC6909 transfer count
/// (`_expectedTransfers`), tracked-account conservation through the shared
/// `ConservationOps` helper (WETH+ETH combined, USDC and WBTC summed to zero),
/// and the source's profit mode (mode 1 everywhere; V4-V4-V4 is the single
/// mode-2 MINT-capture row).
///
/// Five further tests pin the encoder's CLI refusals through `vm.tryFfi`: exact
/// exit code, empty stdout, and the exact stderr text.
contract CmdExecutorThreeHopProductionCalldataTest is PoolSetup, ConservationOps {
    /// The trusted FFI lane's opt-in switch, set only by
    /// `just test-executor-production-calldata`.
    string internal constant FFI_LANE_ENV = "DEGENBOT_THREE_HOP_PRODUCTION_FFI";

    /// The repository root as seen from this forge project
    /// (`tests/foundry_executor`), the working directory `vm.ffi` commands run
    /// in.
    string internal constant REPO_ROOT = "../..";

    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;
    uint24 internal constant V3_FEE = 3000;
    uint24 internal constant V4_FEE = 3000;
    /// Shared by the encoder request (a decimal in JSON) and the pool key (an
    /// `int24`); the value is positive, so one `uint256` constant serves both.
    uint256 internal constant V4_TICK_SPACING = 60;

    uint8 internal constant FAMILY_V2 = 0;
    uint8 internal constant FAMILY_V3 = 1;
    uint8 internal constant FAMILY_V4 = 2;

    /// The five encoder refusals, spelled as the binary writes them to stderr:
    /// `main` returns `Result<(), Box<dyn Error>>`, so each is `Error: <debug>`
    /// followed by a newline.
    string internal constant STDERR_MISSING_REQUEST_ARGUMENT =
        "Error: \"usage: encode_three_hop '<request-json>'\"\n";
    string internal constant STDERR_MALFORMED_JSON =
        "Error: Error(\"EOF while parsing an object\", line: 1, column: 1)\n";
    string internal constant STDERR_UNSUPPORTED_FAMILY = "Error: \"unsupported family V5\"\n";
    string internal constant STDERR_MISSING_TICK_SPACING = "Error: \"V4 requires tick_spacing\"\n";
    string internal constant STDERR_U16_FEE_OVERFLOW = "Error: TryFromIntError(PosOverflow)\n";

    IFakeUniswapV2Pair internal v2A; // WETH/USDC
    IFakeUniswapV2Pair internal v2B; // USDC/WBTC
    IFakeUniswapV2Pair internal v2C; // WBTC/WETH
    IFakeUniswapV3Pool internal v3A; // WETH/USDC
    IFakeUniswapV3Pool internal v3B; // USDC/WBTC
    IFakeUniswapV3Pool internal v3C; // WBTC/WETH

    /// Absolute-or-repo-relative path of the Rust encoder binary.
    string internal encoder;

    /// One hop of a row: the fixture prepared on chain plus the facts the
    /// encoder request carries.
    struct HopFixture {
        uint8 family;
        address pool;
        address token0;
        address token1;
        uint256 fee;
        bool zfo;
        bool hasTickSpacing;
        uint256 tickSpacing;
        uint256 amountOut;
    }

    function setUp() public override {
        super.setUp();
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        v2A = _deployV2Pair(a0, a1, 0, V2_FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        v2B = _deployV2Pair(b0, b1, 0, V2_FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        v2C = _deployV2Pair(c0, c1, 0, V2_FEE);
        v3A = _deployV3Pool(a0, a1, 0, V3_FEE);
        v3B = _deployV3Pool(b0, b1, 0, V3_FEE);
        v3C = _deployV3Pool(c0, c1, 0, V3_FEE);

        if (bytes(vm.envOr(FFI_LANE_ENV, "")).length == 0) {
            // FFI is off (or the lane is not the trusted one): skip the whole
            // contract instead of shelling out. The lane recipe is the only
            // sanctioned entry point and builds the encoder first.
            vm.skip(true);
            return;
        }
        encoder = _encoderPath();
    }

    // ── Encoder location ─────────────────────────────────────────────────

    /// `CARGO_TARGET_DIR`, else `rust/target` — the justfile's `target_dir`
    /// convention, resolved against the repository root unless already
    /// absolute. A Cargo target dir outside the repo therefore keeps working.
    function _encoderPath() internal view returns (string memory) {
        string memory targetDir = vm.envOr("CARGO_TARGET_DIR", "rust/target");
        if (_isAbsolute(targetDir)) {
            return string.concat(targetDir, "/debug/encode_three_hop");
        }
        return string.concat(REPO_ROOT, "/", targetDir, "/debug/encode_three_hop");
    }

    function _isAbsolute(string memory path) internal pure returns (bool) {
        bytes memory raw = bytes(path);
        return raw.length > 0 && raw[0] == bytes1("/");
    }

    // ── Encoder invocation ───────────────────────────────────────────────

    /// Run the production encoder and execute its bytes verbatim.
    ///
    /// `vm.ffi` reverts on a non-zero exit and carries the encoder's stderr in
    /// the revert, so a failure names the row (the test name) and the encoder
    /// diagnostic. A valid-hex stdout is hex-decoded by the cheatcode, so the
    /// encoder's one hex line arrives here as the raw command stream — which is
    /// why the shape guard reads the leading `SET_ADDRESS` opcode, not a digit.
    function _productionCommands(string memory request, string memory label)
        internal
        returns (bytes memory)
    {
        string[] memory command = new string[](2);
        command[0] = encoder;
        command[1] = request;
        bytes memory commands = vm.ffi(command);
        if (commands.length == 0) {
            revert(string.concat(label, ": the production encoder returned an empty stream"));
        }
        if (commands[0] != bytes1(uint8(0x00))) {
            revert(
                string.concat(
                    label,
                    ": the emitted stream does not start with its SET_ADDRESS preamble: ",
                    _hexPreview(commands)
                )
            );
        }
        return commands;
    }

    function _runEncoder(string[] memory command) internal returns (Vm.FfiResult memory) {
        return vm.tryFfi(command);
    }

    // ── Fixture preparation (the source's per-hop setup family) ───────────

    /// Fund a V2 pair at the price that makes V2_SWAP_DIRECT's pinned output
    /// reachable (the source `_setup_v2_for_calc`).
    function _setupV2ForCalc(
        IFakeUniswapV2Pair pool,
        IFakeErc20 inputToken,
        IFakeErc20 outputToken,
        uint256 amountIn,
        uint256 amountOut
    ) internal returns (bool zfo) {
        inputToken.mint(address(pool), amountIn * 100);
        outputToken.mint(address(pool), amountOut * 100);
        pool.sync();
        zfo = pool.token0() == address(inputToken);
    }

    /// Prepare hop `index` of a row: the pool state, the pool's output for the
    /// row's chained input, and the facts the encoder request repeats.
    function _prepareHop(
        uint256 index,
        uint8 family,
        IFakeErc20 inputToken,
        IFakeErc20 outputToken,
        uint256 amountIn,
        uint256 desiredOut
    ) internal returns (HopFixture memory hop) {
        hop.family = family;
        hop.fee = V2_FEE;
        if (family == FAMILY_V2) {
            IFakeUniswapV2Pair pair = index == 0 ? v2A : index == 1 ? v2B : v2C;
            hop.zfo = _setupV2ForCalc(pair, inputToken, outputToken, amountIn, desiredOut);
            hop.amountOut = _v2GetAmountOut(
                amountIn,
                inputToken.balanceOf(address(pair)),
                outputToken.balanceOf(address(pair)),
                V2_FEE
            );
            hop.pool = address(pair);
            hop.token0 = pair.token0();
            hop.token1 = pair.token1();
        } else if (family == FAMILY_V3) {
            IFakeUniswapV3Pool pool = index == 0 ? v3A : index == 1 ? v3B : v3C;
            (hop.zfo, hop.amountOut) = _setupV3(pool, inputToken, outputToken, amountIn, desiredOut);
            hop.pool = address(pool);
            hop.token0 = pool.token0();
            hop.token1 = pool.token1();
            hop.fee = V3_FEE;
        } else {
            PoolKey memory key =
                _poolKey(address(inputToken), address(outputToken), V4_FEE, int24(int256(V4_TICK_SPACING)));
            hop.zfo = key.currency0 == address(inputToken);
            _setupV4Swap(v4Pm, key, amountIn, desiredOut, hop.zfo, address(outputToken));
            hop.amountOut = desiredOut;
            hop.pool = address(v4Pm);
            hop.token0 = key.currency0;
            hop.token1 = key.currency1;
            hop.fee = key.fee;
            hop.hasTickSpacing = true;
            hop.tickSpacing = V4_TICK_SPACING;
        }
    }

    // ── Row execution + verification (the source `_run_and_verify`) ───────

    function _runPermutation(uint8[3] memory combo) internal {
        string memory label = string.concat("production ", _comboLabel(combo));
        uint256[3] memory desired = [AMOUNT_USDC, AMOUNT_WBTC, AMOUNT_WETH_PROFIT];
        IFakeErc20[3] memory tokens = [IFakeErc20(address(weth)), usdc, wbtc];
        HopFixture[3] memory hops;
        uint256[3] memory consumed;
        uint256[3] memory outputs;
        address[3] memory hopPools;

        uint256 amountIn = AMOUNT_WETH;
        for (uint256 index = 0; index < 3; index++) {
            consumed[index] = amountIn;
            hops[index] = _prepareHop(
                index,
                combo[index],
                tokens[index],
                tokens[(index + 1) % 3],
                amountIn,
                desired[index]
            );
            amountIn = hops[index].amountOut;
            outputs[index] = amountIn;
            hopPools[index] = hops[index].pool;
        }

        bytes memory commands = _productionCommands(_requestDocument(hops, outputs, consumed), label);
        _runAndVerify(label, commands, hopPools, _expectedTransfers(combo), _profitMode(combo));
    }

    /// Snapshot, execute the FFI-produced bytes with the row's profit mode,
    /// assert the exact transfer count from on-chain events, then verify
    /// tracked-account conservation.
    ///
    /// The source leaves `expected_weth_delta` unset on every row, so the
    /// executor-profit pin is off and only the conservation invariants run —
    /// the same shape the source's `_verify_conservation` takes.
    function _runAndVerify(
        string memory label,
        bytes memory commands,
        address[3] memory hopPools,
        uint256 expectedTransfers,
        uint256 mode
    ) internal {
        address[] memory tokens = _tokens();
        address[] memory accounts = _trackedAccounts(hopPools);
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        vm.recordLogs();
        Outcome memory outcome = _executeRaw(executor, commands, mode, 0);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        if (!outcome.ok) {
            revert(
                string.concat(
                    label,
                    ": production stream reverted: ",
                    _revertName(outcome.revertData),
                    " ",
                    _hexPreview(outcome.revertData)
                )
            );
        }

        assertEq(
            _countTransfers(logs), expectedTransfers, string.concat(label, ": transfer count mismatch")
        );

        BalanceSnapshot memory afterSnap = _snapshotBalances(tokens, accounts);
        _verifyConservation(before, afterSnap, address(executor), false, 0, label);
    }

    function _tokens() internal view returns (address[] memory t) {
        t = new address[](4);
        t[0] = address(weth);
        t[1] = address(usdc);
        t[2] = address(wbtc);
        t[3] = NATIVE;
    }

    /// The tracked account set: the executor plus every hop pool.
    ///
    /// The source snapshots into a `(token, account)`-keyed mapping, so a
    /// repeated PoolManager (the V4 hops all address it) collapses to one
    /// entry. The port deduplicates for the same accounting, because the
    /// positional snapshot would otherwise count its delta twice.
    function _trackedAccounts(address[3] memory hopPools) internal view returns (address[] memory) {
        address[4] memory candidates = [address(executor), hopPools[0], hopPools[1], hopPools[2]];
        bool[4] memory duplicate;
        uint256 count;
        for (uint256 i = 0; i < 4; i++) {
            if (duplicate[i]) continue;
            count++;
            for (uint256 j = i + 1; j < 4; j++) {
                if (candidates[j] == candidates[i]) duplicate[j] = true;
            }
        }
        address[] memory accounts = new address[](count);
        uint256 next;
        for (uint256 i = 0; i < 4; i++) {
            if (duplicate[i]) continue;
            accounts[next++] = candidates[i];
        }
        return accounts;
    }

    // ── The source's expected transfer counts and profit modes ───────────

    /// The production stream's physical ERC20/ERC6909 transfer count per
    /// permutation — the source's `EXPECTED_TRANSFERS` table.
    ///
    /// V4-V4-V4 is the pure delta-netting row: one MINT capture, no physical
    /// transfer. The five rows with a netting V4-V4 tail (V2-V4-V4, V3-V4-V4,
    /// V4-V3-V4, V4-V4-V2, V4-V4-V3) run three. V2-V2-V2 is self-funded (the
    /// in-path flash option is a separate axis) and V3-V2-V4 uses its
    /// forward-flash/final-repayment arm, each one transfer above the
    /// equivalent contract-mechanics stream, so they run five. Every remaining
    /// row runs four.
    function _expectedTransfers(uint8[3] memory combo) internal pure returns (uint256) {
        if (combo[0] == FAMILY_V4 && combo[1] == FAMILY_V4 && combo[2] == FAMILY_V4) return 1;
        if (_isThreeTransferRow(combo)) return 3;
        if (combo[0] == FAMILY_V2 && combo[1] == FAMILY_V2 && combo[2] == FAMILY_V2) return 5;
        if (combo[0] == FAMILY_V3 && combo[1] == FAMILY_V2 && combo[2] == FAMILY_V4) return 5;
        return 4;
    }

    function _isThreeTransferRow(uint8[3] memory combo) internal pure returns (bool) {
        return _isRow(combo, FAMILY_V2, FAMILY_V4, FAMILY_V4)
            || _isRow(combo, FAMILY_V3, FAMILY_V4, FAMILY_V4)
            || _isRow(combo, FAMILY_V4, FAMILY_V3, FAMILY_V4)
            || _isRow(combo, FAMILY_V4, FAMILY_V4, FAMILY_V2)
            || _isRow(combo, FAMILY_V4, FAMILY_V4, FAMILY_V3);
    }

    function _isRow(uint8[3] memory combo, uint8 a, uint8 b, uint8 c) internal pure returns (bool) {
        return combo[0] == a && combo[1] == b && combo[2] == c;
    }

    /// The source's `profit_mode`: V4-V4-V4 captures the net WETH profit as an
    /// ERC6909 claim (mode 2); every other row measures physical WETH+ETH.
    function _profitMode(uint8[3] memory combo) internal pure returns (uint256) {
        return _isRow(combo, FAMILY_V4, FAMILY_V4, FAMILY_V4) ? 2 : 1;
    }

    // ── Request documents ────────────────────────────────────────────────

    function _requestDocument(
        HopFixture[3] memory hops,
        uint256[3] memory outputs,
        uint256[3] memory consumed
    ) internal view returns (string memory) {
        return string.concat(
            "{\"executor\":\"",
            vm.toString(address(executor)),
            "\",\"pool_manager\":\"",
            vm.toString(address(v4Pm)),
            "\",\"weth\":\"",
            vm.toString(address(weth)),
            "\",\"optimal_input\":",
            vmToString(AMOUNT_WETH),
            ",\"outputs\":",
            _jsonUintArray(outputs),
            ",\"consumed_inputs\":",
            _jsonUintArray(consumed),
            ",\"hops\":[",
            _hopJson(hops[0]),
            ",",
            _hopJson(hops[1]),
            ",",
            _hopJson(hops[2]),
            "]}"
        );
    }

    function _hopJson(HopFixture memory hop) internal pure returns (string memory) {
        string memory zfoText = hop.zfo ? "true" : "false";
        string memory tickSpacingText = hop.hasTickSpacing ? vmToString(hop.tickSpacing) : "null";
        return string.concat(
            "{\"family\":\"",
            _familyName(hop.family),
            "\",\"address\":\"",
            vm.toString(hop.pool),
            "\",\"token0\":\"",
            vm.toString(hop.token0),
            "\",\"token1\":\"",
            vm.toString(hop.token1),
            "\",\"fee\":",
            vmToString(hop.fee),
            ",\"zfo\":",
            zfoText,
            ",\"tick_spacing\":",
            tickSpacingText,
            "}"
        );
    }

    function _jsonUintArray(uint256[3] memory values) internal pure returns (string memory) {
        return string.concat(
            "[",
            vmToString(values[0]),
            ",",
            vmToString(values[1]),
            ",",
            vmToString(values[2]),
            "]"
        );
    }

    /// A request that decodes cleanly, so each refusal test isolates the one
    /// rule it drives: the hop families and hop 0's fee are the only knobs the
    /// callers vary, and no hop carries a tick spacing — which is exactly what
    /// the V4 arm of the family override needs to be refused.
    function _refusalDocument(string[3] memory families, uint256 firstFee)
        internal
        pure
        returns (string memory)
    {
        return string.concat(
            "{\"executor\":\"0x1111111111111111111111111111111111111111\",",
            "\"pool_manager\":\"0x2222222222222222222222222222222222222222\",",
            "\"weth\":\"0x3333333333333333333333333333333333333333\",",
            "\"optimal_input\":1000,\"outputs\":[900,800,1000],",
            "\"consumed_inputs\":[1000,900,800],\"hops\":[",
            _refusalHop(families[0], firstFee),
            ",",
            _refusalHop(families[1], V2_FEE),
            ",",
            _refusalHop(families[2], V2_FEE),
            "]}"
        );
    }

    function _refusalHop(string memory family, uint256 fee)
        internal
        pure
        returns (string memory)
    {
        return string.concat(
            "{\"family\":\"",
            family,
            "\",\"address\":\"0x4444444444444444444444444444444444444444\",",
            "\"token0\":\"0x5555555555555555555555555555555555555555\",",
            "\"token1\":\"0x6666666666666666666666666666666666666666\",",
            "\"fee\":",
            vmToString(fee),
            ",\"zfo\":true,\"tick_spacing\":null}"
        );
    }

    // ── Encoder CLI refusals ─────────────────────────────────────────────

    /// Pin a refusal exactly: exit code 1, no stdout, and the encoder's stated
    /// diagnostic on stderr. No status-only assertion, so a refusal that starts
    /// failing for a different reason cannot pass.
    function _assertEncoderRefusal(
        string memory label,
        Vm.FfiResult memory result,
        string memory expectedStderr
    ) internal pure {
        if (result.exitCode != 1) {
            revert(
                string.concat(label, ": expected exit code 1, got ", vmToString(result.exitCode))
            );
        }
        if (result.stdout.length != 0) {
            revert(string.concat(label, ": expected empty stdout, got \"", string(result.stdout), "\""));
        }
        if (keccak256(result.stderr) != keccak256(bytes(expectedStderr))) {
            revert(
                string.concat(
                    label,
                    ": stderr mismatch, got \"",
                    string(result.stderr),
                    "\", expected \"",
                    expectedStderr,
                    "\""
                )
            );
        }
    }

    function _encoderCommand() internal view returns (string[] memory command) {
        command = new string[](1);
        command[0] = encoder;
    }

    function _encoderCommand(string memory request) internal view returns (string[] memory command) {
        command = new string[](2);
        command[0] = encoder;
        command[1] = request;
    }

    function _families(string memory a, string memory b, string memory c)
        internal
        pure
        returns (string[3] memory families)
    {
        families[0] = a;
        families[1] = b;
        families[2] = c;
    }

    function testEncoderRejectsMissingRequestArgument() public {
        _assertEncoderRefusal(
            "encoder missing request argument",
            _runEncoder(_encoderCommand()),
            STDERR_MISSING_REQUEST_ARGUMENT
        );
    }

    function testEncoderRejectsMalformedJson() public {
        _assertEncoderRefusal(
            "encoder malformed JSON", _runEncoder(_encoderCommand("{")), STDERR_MALFORMED_JSON
        );
    }

    function testEncoderRejectsHopFamilyItCannotDecode() public {
        string memory request =
            _refusalDocument(_families("V2", "V5", "V2"), V2_FEE);
        _assertEncoderRefusal(
            "encoder unsupported family", _runEncoder(_encoderCommand(request)), STDERR_UNSUPPORTED_FAMILY
        );
    }

    function testEncoderRequiresTickSpacingForV4Hops() public {
        string memory request =
            _refusalDocument(_families("V2", "V2", "V4"), V2_FEE);
        _assertEncoderRefusal(
            "encoder missing V4 tick spacing",
            _runEncoder(_encoderCommand(request)),
            STDERR_MISSING_TICK_SPACING
        );
    }

    function testEncoderRejectsV2FeeAboveU16() public {
        string memory request =
            _refusalDocument(_families("V2", "V2", "V2"), 70_000);
        _assertEncoderRefusal(
            "encoder V2 fee above u16",
            _runEncoder(_encoderCommand(request)),
            STDERR_U16_FEE_OVERFLOW
        );
    }

    // ── The 27 production permutations ───────────────────────────────────
    // One test per row, in the source's itertools.product order.

    function testV2V2V2() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V2, FAMILY_V2));
    }

    function testV2V2V3() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V2, FAMILY_V3));
    }

    function testV2V2V4() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V2, FAMILY_V4));
    }

    function testV2V3V2() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V3, FAMILY_V2));
    }

    function testV2V3V3() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V3, FAMILY_V3));
    }

    function testV2V3V4() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V3, FAMILY_V4));
    }

    function testV2V4V2() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V4, FAMILY_V2));
    }

    function testV2V4V3() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V4, FAMILY_V3));
    }

    function testV2V4V4() public {
        _runPermutation(_combo(FAMILY_V2, FAMILY_V4, FAMILY_V4));
    }

    function testV3V2V2() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V2, FAMILY_V2));
    }

    function testV3V2V3() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V2, FAMILY_V3));
    }

    function testV3V2V4() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V2, FAMILY_V4));
    }

    function testV3V3V2() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V3, FAMILY_V2));
    }

    function testV3V3V3() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V3, FAMILY_V3));
    }

    function testV3V3V4() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V3, FAMILY_V4));
    }

    function testV3V4V2() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V4, FAMILY_V2));
    }

    function testV3V4V3() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V4, FAMILY_V3));
    }

    function testV3V4V4() public {
        _runPermutation(_combo(FAMILY_V3, FAMILY_V4, FAMILY_V4));
    }

    function testV4V2V2() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V2, FAMILY_V2));
    }

    function testV4V2V3() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V2, FAMILY_V3));
    }

    function testV4V2V4() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V2, FAMILY_V4));
    }

    function testV4V3V2() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V3, FAMILY_V2));
    }

    function testV4V3V3() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V3, FAMILY_V3));
    }

    function testV4V3V4() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V3, FAMILY_V4));
    }

    function testV4V4V2() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V4, FAMILY_V2));
    }

    function testV4V4V3() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V4, FAMILY_V3));
    }

    function testV4V4V4() public {
        _runPermutation(_combo(FAMILY_V4, FAMILY_V4, FAMILY_V4));
    }

    // ── Label helpers ────────────────────────────────────────────────────

    function _combo(uint8 a, uint8 b, uint8 c) internal pure returns (uint8[3] memory combo) {
        combo[0] = a;
        combo[1] = b;
        combo[2] = c;
    }

    function _comboLabel(uint8[3] memory combo) internal pure returns (string memory) {
        return string.concat(
            _familyName(combo[0]), "-", _familyName(combo[1]), "-", _familyName(combo[2])
        );
    }

    function _familyName(uint8 family) internal pure returns (string memory) {
        if (family == FAMILY_V2) return "V2";
        if (family == FAMILY_V3) return "V3";
        return "V4";
    }
}
