// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {IEchoCallee} from "../src/generated/IEchoCallee.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {IFakeUniswapV4PoolManager} from "../src/generated/IFakeUniswapV4PoolManager.sol";
import {ModifyLiquidityParams, PoolKey, SwapParams} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_fake_abi_fidelity.py`.
///
/// The fakes mirror real V2/V3/V4 entry points whose Solidity signatures take
/// unbounded `bytes`, so this suite pins the fakes to that permissiveness: a
/// payload larger than any internal cap must be accepted and reach the callee
/// byte-for-byte. Real counterparts, all unbounded `bytes`:
///
/// - `UniswapV2Pair.swap(uint,uint,address,bytes)` plus the `uniswapV2Call` /
///   `hook` / `pancakeCall` callback variants;
/// - `UniswapV3Pool.swap(...,bytes)` plus the `uniswapV3SwapCallback` /
///   `pancakeV3SwapCallback` variants;
/// - `PoolManager.unlock(bytes)` and `swap`/`modifyLiquidity`/`donate`'s
///   `bytes hookData`.
///
/// The source parametrizes two functions into five collected cases (three V2
/// callback variants, two V3 variants); here each variant is its own named test,
/// so the suite carries the same nine cases across nine tests.
///
/// `ECHO_VERIFIED` is NOT an executor or pool failure: `EchoCallee` raises it on
/// purpose AFTER its length and digest checks pass, because a revert's data is
/// the only thing that survives the callback frame's rollback. Every callback
/// case below therefore asserts that exact revert payload, and every one also
/// drives a corrupted-digest control that must fail the earlier check instead
/// (`ECHO_BAD_DIGEST`) — which is what makes the marker evidence of a verified
/// payload rather than of a swallowed error.
contract FakeABIFidelityTest is PoolSetup {
    /// `_make_pool_key`'s SALT (`b"\x11" * 32`).
    bytes32 internal constant SALT =
        0x1111111111111111111111111111111111111111111111111111111111111111;

    uint256 internal constant V2_LIQUIDITY = 10 ** 12;
    uint256 internal constant V3_AMOUNT_IN = 10 ** 9;

    /// The fake PoolManager's own `set_next_swap` data cap. The unbounded
    /// `swap`/`modifyLiquidity`/`donate` entry points must not inherit it.
    uint256 internal constant FAKE_SWAP_DATA_CAP = 32;

    IEchoCallee internal echo;

    function setUp() public override {
        super.setUp();
        echo = IEchoCallee(_deploy("EchoCallee", ""));
    }

    // ── Payload helpers ───────────────────────────────────────────────────

    /// Word 0 = total length, word 1 = keccak256 of everything except word 1.
    function _selfVerifyingPayload(uint256 n) internal pure returns (bytes memory) {
        bytes memory filler = new bytes(n - 64);
        for (uint256 i = 0; i < filler.length; i++) {
            filler[i] = 0xab;
        }
        return abi.encodePacked(bytes32(n), keccak256(abi.encodePacked(bytes32(n), filler)), filler);
    }

    /// The digest's own pre-image: the length word plus the filler, with the
    /// digest word removed.
    function _covered(bytes memory payload) internal pure returns (bytes memory out) {
        bytes32 lengthWord;
        assembly {
            lengthWord := mload(add(payload, 0x20))
        }
        out = new bytes(payload.length - 64);
        for (uint256 i = 0; i < out.length; i++) {
            out[i] = payload[i + 64];
        }
        return abi.encodePacked(lengthWord, out);
    }

    /// Flip one filler byte, leaving the declared length word intact: the
    /// digest check must fail while the length check still passes.
    function _corruptDigest(bytes memory payload) internal pure returns (bytes memory out) {
        out = payload;
        out[out.length - 1] = bytes1(uint8(out[out.length - 1]) ^ 0xff);
    }

    function _errorString(string memory message) internal pure returns (bytes memory) {
        return abi.encodeWithSignature("Error(string)", message);
    }

    /// Assert an exact `Error(string)` revert payload, selector and argument
    /// both — no bare status check that a setup error could also satisfy.
    function _assertErrorRevert(bool ok, bytes memory revertData, string memory expected, string memory label)
        internal
        pure
    {
        assertFalse(ok, string.concat(label, ": expected revert, call succeeded"));
        if (keccak256(revertData) != keccak256(_errorString(expected))) {
            revert(
                string.concat(
                    label,
                    ": revert mismatch, got ",
                    _hexPreview(revertData),
                    " (",
                    _revertName(revertData),
                    "), expected Error(\"",
                    expected,
                    "\")"
                )
            );
        }
    }

    /// `_make_pool_key(v4_pm.address, echo.address)`: sorted currencies, fee 0,
    /// tick spacing 60, no hooks.
    function _fakePoolKey() internal view returns (PoolKey memory) {
        return _poolKey(address(v4Pm), address(echo), 0, 60);
    }

    // ── V4 PoolManager: unlock round-trip ─────────────────────────────────

    /// `TestFakeV4PoolManagerAbi.test_unlock_round_trips_oversized_data`: the
    /// unlock callback receives a 1200-byte payload intact, verifies it, and
    /// the manager returns exactly what the callback returned.
    function testUnlockRoundTripsOversizedData() public {
        bytes memory payload = _selfVerifyingPayload(1200);

        bytes memory out = echo.do_unlock(address(v4Pm), payload);

        assertEq(out, payload, "unlock returned the payload byte-for-byte");
        assertEq(echo.last_len(), 1200, "callback observed the declared length");
        assertEq(echo.last_digest(), keccak256(_covered(payload)), "callback observed the payload digest");
    }

    // ── V4 PoolManager: oversized hook data reaches the lock gate ─────────

    /// `test_swap_accepts_oversized_hook_data`: 600 bytes of hook data pass the
    /// ABI boundary and reach `swap`'s body, whose first statement is the lock
    /// gate — so `ManagerLocked` (not a length rejection) is the observed
    /// revert.
    function testSwapAcceptsOversizedHookData() public {
        bytes memory hookData = _selfVerifyingPayload(600);
        assertGt(hookData.length, FAKE_SWAP_DATA_CAP, "payload exceeds the next-swap data cap");

        (bool ok, bytes memory revertData) = _callExpectRevert(
            address(v4Pm),
            abi.encodeCall(
                IFakeUniswapV4PoolManager.swap,
                (_fakePoolKey(), SwapParams(true, 1, MIN_SQRT_PRICE_X96 + 1), hookData)
            )
        );
        _assertErrorRevert(ok, revertData, "ManagerLocked", "swap oversized hookData");
    }

    /// `test_modify_liquidity_accepts_oversized_hook_data`.
    function testModifyLiquidityAcceptsOversizedHookData() public {
        bytes memory hookData = _selfVerifyingPayload(100);

        (bool ok, bytes memory revertData) = _callExpectRevert(
            address(v4Pm),
            abi.encodeCall(
                IFakeUniswapV4PoolManager.modifyLiquidity,
                (_fakePoolKey(), ModifyLiquidityParams(-60, 60, 0, SALT), hookData)
            )
        );
        _assertErrorRevert(ok, revertData, "ManagerLocked", "modifyLiquidity oversized hookData");
    }

    /// `test_donate_accepts_oversized_hook_data`.
    function testDonateAcceptsOversizedHookData() public {
        bytes memory hookData = _selfVerifyingPayload(100);

        (bool ok, bytes memory revertData) = _callExpectRevert(
            address(v4Pm),
            abi.encodeCall(IFakeUniswapV4PoolManager.donate, (_fakePoolKey(), 1, 1, hookData))
        );
        _assertErrorRevert(ok, revertData, "ManagerLocked", "donate oversized hookData");
    }

    // ── V2 pair: oversized callback data, one case per variant ────────────

    /// `TestFakeV2PairAbi.test_swap_delivers_oversized_callback_data` (0):
    /// `uniswapV2Call`.
    function testUniswapV2CallDeliversOversizedCallbackData() public {
        _v2OversizedCallback(0);
    }

    /// Variant 1: the `hook` callback.
    function testHookDeliversOversizedCallbackData() public {
        _v2OversizedCallback(1);
    }

    /// Variant 2: `pancakeCall`.
    function testPancakeCallDeliversOversizedCallbackData() public {
        _v2OversizedCallback(2);
    }

    function _v2OversizedCallback(uint256 variant) internal {
        IFakeUniswapV2Pair pair = _deployV2Pair(address(usdc), address(wbtc), variant, 3000);
        usdc.mint(address(pair), V2_LIQUIDITY);
        wbtc.mint(address(pair), V2_LIQUIDITY);
        pair.sync();

        bytes memory payload = _selfVerifyingPayload(600);
        (bool ok, bytes memory revertData) = _callExpectRevert(
            address(pair),
            abi.encodeCall(IFakeUniswapV2Pair.swap, (uint256(0), uint256(1), address(echo), payload))
        );
        _assertErrorRevert(ok, revertData, "ECHO_VERIFIED", "V2 oversized callback data");

        // Negative control: the same callback path with a corrupted digest must
        // fail the digest check, never reach the marker.
        (bool okCorrupt, bytes memory corruptData) = _callExpectRevert(
            address(pair),
            abi.encodeCall(
                IFakeUniswapV2Pair.swap, (uint256(0), uint256(1), address(echo), _corruptDigest(payload))
            )
        );
        _assertErrorRevert(okCorrupt, corruptData, "ECHO_BAD_DIGEST", "V2 corrupted callback digest");
    }

    // ── V3 pool: oversized callback data, one case per variant ────────────

    /// `TestFakeV3PoolAbi.test_swap_delivers_oversized_callback_data` (0):
    /// `uniswapV3SwapCallback`.
    function testUniswapV3SwapCallbackDeliversOversizedCallbackData() public {
        _v3OversizedCallback(0);
    }

    /// Variant 1: `pancakeV3SwapCallback`.
    function testPancakeV3SwapCallbackDeliversOversizedCallbackData() public {
        _v3OversizedCallback(1);
    }

    function _v3OversizedCallback(uint256 variant) internal {
        IFakeUniswapV3Pool pool = _deployV3Pool(address(usdc), address(wbtc), variant, 3000);
        (bool zfo,) = _setupV3(pool, usdc, wbtc, V3_AMOUNT_IN, V3_AMOUNT_IN);
        uint160 limit = zfo ? MIN_SQRT_PRICE_X96 + 1 : MAX_SQRT_PRICE_X96 - 1;

        bytes memory payload = _selfVerifyingPayload(600);
        (bool ok, bytes memory revertData) = _callExpectRevert(
            address(echo),
            abi.encodeCall(
                IEchoCallee.do_swap_v3,
                (address(pool), address(echo), zfo, -int256(V3_AMOUNT_IN), limit, payload)
            )
        );
        _assertErrorRevert(ok, revertData, "ECHO_VERIFIED", "V3 oversized callback data");

        (bool okCorrupt, bytes memory corruptData) = _callExpectRevert(
            address(echo),
            abi.encodeCall(
                IEchoCallee.do_swap_v3,
                (
                    address(pool),
                    address(echo),
                    zfo,
                    -int256(V3_AMOUNT_IN),
                    limit,
                    _corruptDigest(payload)
                )
            )
        );
        _assertErrorRevert(okCorrupt, corruptData, "ECHO_BAD_DIGEST", "V3 corrupted callback digest");
    }
}
