// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ICmdExecutor} from "../src/generated/ICmdExecutor.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IPoolManager} from "../src/generated/IPoolManager.sol";
import {IUniswapV2Factory} from "../src/generated/IUniswapV2Factory.sol";
import {IUniswapV2Pair} from "../src/generated/IUniswapV2Pair.sol";
import {IUniswapV3Factory} from "../src/generated/IUniswapV3Factory.sol";
import {IUniswapV3Pool} from "../src/generated/IUniswapV3Pool.sol";
import {ModifyLiquidityParams, PoolKey} from "../src/generated/Types.sol";
import {Vm} from "../src/Vm.sol";

/// The executor against CANONICAL Uniswap bytecode: `UniswapV2Pair` (solc
/// 0.5.16), `UniswapV3Pool` (solc 0.7.6) and the v4-core `PoolManager` (solc
/// 0.8.26, EVM `cancun`), each compiled one-off by
/// `build_solidity_artifacts.py` from the repository's vendored sources with
/// the pinned binaries and deployed here from their artifacts. No forge solc
/// manager, no proxy contract in the path, and no second PoolManager
/// implementation: the V4 tier is the real `tier3-oracle/lib/v4-core` source.
///
/// The fakes stay the default fixture for canned mechanics; this file is the
/// fidelity tier where a pool returns its own math.
contract RealLibrariesTest is PoolSetup {
    uint256 internal constant ONE_WETH = 1 ether;

    // ── V2: canonical UniswapV2Pair ──────────────────────────────────────

    function testExecutorDrivesCanonicalV2Pair() public {
        // The canonical pair asks its factory for `feeTo()` (protocol-fee
        // accounting), so it needs the real factory behind it — deploying the
        // pair directly would make this test contract the factory and the
        // `mint` path would revert on the missing `feeTo()` selector.
        IUniswapV2Factory factory = IUniswapV2Factory(
            _deploy("UniswapV2Factory", abi.encode(address(this)))
        );
        IUniswapV2Pair pair =
            IUniswapV2Pair(factory.createPair(address(usdc), address(weth)));

        usdc.mint(address(pair), 200_000e6);
        weth.mint(address(pair), 100 ether);
        pair.mint(address(this));

        uint256 amountIn = 2_000e6;
        bool zfo = pair.token0() == address(usdc);
        (uint112 r0, uint112 r1,) = pair.getReserves();
        uint256 amountOut = _v2GetAmountOut(amountIn, zfo ? r0 : r1, zfo ? r1 : r0, 30);
        assertGt(amountOut, 0, "canonical pair quotes a nonzero output");

        usdc.mint(address(executor), amountIn);

        Cmd.Table memory at = _emptyTable();
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands = Cmd.erc20Transfer(usdcIdx, pairIdx, amountIn);
        commands = bytes.concat(
            commands, Cmd.v2SwapCompact(pairIdx, zfo, amountOut, executorIdx, 30, "")
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);
        assertEq(weth.balanceOf(address(executor)), amountOut, "canonical pair paid the executor");
    }

    // ── V3: canonical UniswapV3Factory + UniswapV3Pool ───────────────────

    function uniswapV3MintCallback(uint256 amount0Owed, uint256 amount1Owed, bytes calldata)
        external
    {
        if (amount0Owed > 0) {
            IFakeErc20(IUniswapV3Pool(msg.sender).token0()).transfer(msg.sender, amount0Owed);
        }
        if (amount1Owed > 0) {
            IFakeErc20(IUniswapV3Pool(msg.sender).token1()).transfer(msg.sender, amount1Owed);
        }
    }

    function testExecutorDrivesCanonicalV3Pool() public {
        IUniswapV3Factory factory = IUniswapV3Factory(_deploy("UniswapV3Factory", ""));
        address poolAddr = factory.createPool(address(usdc), address(weth), uint24(3000));
        IUniswapV3Pool pool = IUniswapV3Pool(poolAddr);

        bool zfo = pool.token0() == address(usdc);
        uint256 sqrtP = zfo
            ? _isqrt((ONE_WETH * Q96 * Q96) / 2_000e6)
            : _isqrt((2_000e6 * Q96 * Q96) / ONE_WETH);
        pool.initialize(uint160(sqrtP));

        (uint160 sp, int24 tick,,,,,) = pool.slot0();
        assertGt(uint256(sp), 0, "pool priced");
        int24 spacing = pool.tickSpacing();
        int24 base = (tick / spacing) * spacing;
        usdc.mint(address(this), 1e30);
        weth.mint(address(this), 1e30);
        pool.mint(address(this), base - 600, base + 600, uint128(1e18), "");
        assertGt(pool.liquidity(), 0, "canonical pool has liquidity");

        uint256 amountIn = 2_000e6 / 10;
        usdc.mint(address(executor), amountIn);
        uint256 wethBefore = weth.balanceOf(address(executor));

        Cmd.Table memory at = _emptyTable();
        uint8 poolIdx = Cmd.add(at, address(pool));
        uint8 executorIdx = Cmd.add(at, address(executor));

        // Empty forward_data = the executor's V3 auto-pay path: the pool's
        // callback delta is paid from the executor's own balance.
        bytes memory commands = Cmd.v3SwapCompact(poolIdx, zfo, amountIn, executorIdx, "");
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);

        assertGt(weth.balanceOf(address(executor)), wethBefore, "canonical pool paid WETH out");
        assertEq(usdc.balanceOf(address(executor)), 0, "executor paid its USDC input");
    }
}

/// The canonical v4-core `PoolManager`, deployed from
/// `build_solidity_artifacts.py`'s solc-0.8.26 / EVM-`cancun` compile of
/// `tier3-oracle/lib/v4-core/src/PoolManager.sol`.
///
/// The harness is its own unlock-callback implementer — the role a v4 router
/// plays — so every PoolManager action below is a real `unlock` →
/// callback → `swap`/`modifyLiquidity`/`take`/`settle`/`mint`/`burn` dance with
/// the manager's own transient-storage delta accounting, and the executor leg
/// drives that same PoolManager through the real command stream. The
/// `exttload`/`extsload` slot constants are v4-core's
/// `bytes32(uint256(keccak256(label)) - 1)` identities, which is what makes the
/// delta and reserve assertions statements about the manager and not about a
/// parallel model of it.
contract RealLibrariesV4Test is PoolSetup {
    uint256 internal constant ONE_WETH = 1 ether;
    uint24 internal constant POOL_FEE = 3000;
    int24 internal constant POOL_TICK_SPACING = 60;
    int256 internal constant LP_LIQUIDITY = 1e18;
    int24 internal constant LP_TICK_RANGE = 600;
    uint256 internal constant SWAP_AMOUNT_IN = 200e6;

    // v4-core transient-slot identities: Lock, NonzeroDeltaCount,
    // CurrencyReserves' synced currency and synced reserves.
    bytes32 internal constant IS_UNLOCKED_SLOT =
        0xc090fc4683624cfc3884e9d8de5eca132f2d0ec062aff75d43c0465d5ceeab23;
    bytes32 internal constant NONZERO_DELTA_COUNT_SLOT =
        0x7d4b3164c6e45b97e7d87b7125a44c5828d005af88f9d751cfd78729c5d99a0b;
    bytes32 internal constant SYNCED_CURRENCY_SLOT =
        0x27e098c505d44ec3574004bca052aabf76bd35004c182099d8c575fb238593b9;
    bytes32 internal constant SYNCED_RESERVES_SLOT =
        0x1e0745a7db1623981f0b2a5d4232364c00787266eb75ad546f190e6cebe9bd95;

    // Callback actions. The manager only permits these calls inside `unlock`,
    // so the callback carries the whole action, exactly like a router.
    uint8 internal constant MODE_ECHO = 1;
    uint8 internal constant MODE_NESTED_UNLOCK = 2;
    uint8 internal constant MODE_ADD_LIQUIDITY = 3;
    uint8 internal constant MODE_TAKE_AND_SETTLE = 4;
    uint8 internal constant MODE_MINT_CLAIMS = 5;
    uint8 internal constant MODE_BURN_CLAIMS = 6;

    IPoolManager internal pm;
    PoolKey internal key;
    uint160 internal sqrtPrice0;
    int24 internal tick0;

    uint8 internal callbackMode;
    bytes internal echoPayload;
    int24 internal lpLower;
    int24 internal lpUpper;
    int256 internal lpDelta;
    bytes32 internal lpSalt;
    uint256 internal callbackAmount;

    function setUp() public override {
        super.setUp();

        pm = IPoolManager(_deploy("PoolManager", abi.encode(address(this))));
        key = _poolKey(address(usdc), address(weth), POOL_FEE, POOL_TICK_SPACING);

        // 2000 USDC per WETH in raw units; whichever of the two the sorted
        // currency order puts second is the quote currency.
        sqrtPrice0 = uint160(
            _isqrt(
                key.currency0 == address(usdc)
                    ? (ONE_WETH * Q96 * Q96) / 2_000e6
                    : (2_000e6 * Q96 * Q96) / ONE_WETH
            )
        );
        tick0 = pm.initialize(key, sqrtPrice0);

        usdc.mint(address(this), 1e30);
        weth.mint(address(this), 1e30);
        _provideLiquidity(bytes32(0));
    }

    // ── PoolManager lock: unlock, and the lock's refusal to re-enter ──────

    function testCanonicalV4UnlockRoundTripsCallbackData() public {
        bytes memory payload = abi.encode(uint256(0xc0ffee), address(this));
        echoPayload = payload;
        callbackMode = MODE_ECHO;

        bytes memory out = pm.unlock("");

        assertEq(out, payload, "unlock returned the callback's bytes");
        assertEq(pm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "lock released after unlock");

        // A second unlock from inside the callback is the manager's own refusal.
        callbackMode = MODE_NESTED_UNLOCK;
        (bool ok, bytes memory revertData) =
            _callExpectRevert(address(pm), abi.encodeCall(IPoolManager.unlock, (bytes(""))));
        assertFalse(ok, "nested unlock must revert");
        assertEq(
            revertData,
            abi.encodeWithSelector(bytes4(keccak256("AlreadyUnlocked()"))),
            "nested unlock reverts AlreadyUnlocked()"
        );
    }

    // ── modifyLiquidity: the manager's own delta accounting ───────────────

    function testCanonicalV4ModifyLiquidityAccountsDeltas() public {
        address c0 = key.currency0;
        address c1 = key.currency1;
        uint256 pm0Before = IFakeErc20(c0).balanceOf(address(pm));
        uint256 pm1Before = IFakeErc20(c1).balanceOf(address(pm));

        bytes memory out = _provideLiquidity(bytes32(uint256(1)));
        (
            int256 callerDelta,
            int256 observed0,
            int256 observed1,
            uint256 observedCount,
            int256 settled0,
            int256 settled1,
            uint256 settledCount
        ) = abi.decode(out, (int256, int256, int256, uint256, int256, int256, uint256));

        int256 amount0 = _amount0(callerDelta);
        int256 amount1 = _amount1(callerDelta);
        assertTrue(amount0 < 0, "an in-range position owes currency0");
        assertTrue(amount1 < 0, "an in-range position owes currency1");

        assertEq(observed0, amount0, "currency0 delta slot carries the principal");
        assertEq(observed1, amount1, "currency1 delta slot carries the principal");
        assertEq(observedCount, 2, "two nonzero deltas while the position is unpaid");
        assertEq(settled0, 0, "settle zeroed the currency0 delta");
        assertEq(settled1, 0, "settle zeroed the currency1 delta");
        assertEq(settledCount, 0, "no nonzero deltas once paid");

        assertEq(
            IFakeErc20(c0).balanceOf(address(pm)),
            pm0Before + uint256(-amount0),
            "PoolManager holds the currency0 principal"
        );
        assertEq(
            IFakeErc20(c1).balanceOf(address(pm)),
            pm1Before + uint256(-amount1),
            "PoolManager holds the currency1 principal"
        );
    }

    // ── take + sync/settle ───────────────────────────────────────────────

    function testCanonicalV4TakeThenSyncSettle() public {
        address currency = key.currency0;
        uint256 amount = 1_000e6;
        callbackAmount = amount;

        uint256 pmBefore = IFakeErc20(currency).balanceOf(address(pm));
        uint256 ownerBefore = IFakeErc20(currency).balanceOf(address(this));

        callbackMode = MODE_TAKE_AND_SETTLE;
        vm.recordLogs();
        bytes memory out = pm.unlock("");
        Vm.Log[] memory logs = vm.getRecordedLogs();

        (
            int256 taken,
            uint256 countAfterTake,
            bytes32 syncedCurrency,
            uint256 syncedReserves,
            uint256 paid,
            int256 afterDelta,
            uint256 afterCount
        ) = abi.decode(out, (int256, uint256, bytes32, uint256, uint256, int256, uint256));

        assertEq(taken, -int256(amount), "take debited exactly what it delivered");
        assertEq(countAfterTake, 1, "one nonzero delta after take");
        assertEq(syncedCurrency, bytes32(uint256(uint160(currency))), "sync recorded the currency");
        // The take already left the PoolManager, so the sync snapshot is the
        // balance net of it — and the settle that follows measures exactly the
        // transfer that refills it.
        assertEq(syncedReserves, pmBefore - amount, "sync captured the pre-transfer balance");
        assertEq(paid, amount, "settle measured the transfer, not a caller-supplied amount");
        assertEq(afterDelta, 0, "settle zeroed the taken delta");
        assertEq(afterCount, 0, "no nonzero deltas left");
        assertEq(pm.exttload(SYNCED_CURRENCY_SLOT), bytes32(0), "synced currency slot reset by settle");
        assertEq(IFakeErc20(currency).balanceOf(address(pm)), pmBefore, "PoolManager balance restored");
        assertEq(IFakeErc20(currency).balanceOf(address(this)), ownerBefore, "the round trip nets to zero");

        assertEq(
            _countTokenTransfers(logs, currency, address(pm), address(this), amount),
            1,
            "take moved the currency out of the PoolManager exactly once"
        );
        assertEq(
            _countTokenTransfers(logs, currency, address(this), address(pm), amount),
            1,
            "settle moved the same amount back exactly once"
        );
    }

    // ── ERC6909 claims ───────────────────────────────────────────────────

    function testCanonicalV4Erc6909MintClaims() public {
        address currency = key.currency0;
        uint256 id = uint256(uint160(currency));
        uint256 amount = 1_000e6;
        callbackAmount = amount;

        uint256 pmBefore = IFakeErc20(currency).balanceOf(address(pm));
        assertEq(pm.balanceOf(address(this), id), 0, "no claims before the mint");

        callbackMode = MODE_MINT_CLAIMS;
        bytes memory out = pm.unlock("");
        (int256 minted, uint256 countAfterMint, uint256 paid, int256 afterDelta, uint256 afterCount) =
            abi.decode(out, (int256, uint256, uint256, int256, uint256));

        assertEq(minted, -int256(amount), "mint debits the caller's delta");
        assertEq(countAfterMint, 1, "the mint leaves one nonzero delta");
        assertEq(paid, amount, "the debit settles by transfer");
        assertEq(afterDelta, 0, "settled delta");
        assertEq(afterCount, 0, "no nonzero deltas left");
        assertEq(pm.balanceOf(address(this), id), amount, "claims credited to the minter");
        assertEq(
            IFakeErc20(currency).balanceOf(address(pm)),
            pmBefore + amount,
            "PoolManager holds the mint's collateral"
        );
    }

    function testCanonicalV4Erc6909BurnClaims() public {
        address currency = key.currency0;
        uint256 id = uint256(uint160(currency));
        uint256 amount = 1_000e6;

        _mintClaims(amount);
        assertEq(pm.balanceOf(address(this), id), amount, "claims minted by the funding leg");

        uint256 ownerBefore = IFakeErc20(currency).balanceOf(address(this));
        uint256 pmBefore = IFakeErc20(currency).balanceOf(address(pm));

        callbackAmount = amount;
        callbackMode = MODE_BURN_CLAIMS;
        bytes memory out = pm.unlock("");
        (int256 burned, int256 afterDelta, uint256 afterCount) =
            abi.decode(out, (int256, int256, uint256));

        assertEq(burned, int256(amount), "burn credits the caller's delta");
        assertEq(afterDelta, 0, "the take consumed the burn's credit");
        assertEq(afterCount, 0, "no nonzero deltas left");
        assertEq(pm.balanceOf(address(this), id), 0, "claims destroyed");
        assertEq(
            IFakeErc20(currency).balanceOf(address(this)),
            ownerBefore + amount,
            "the burned claim's collateral was delivered"
        );
        assertEq(
            IFakeErc20(currency).balanceOf(address(pm)),
            pmBefore - amount,
            "PoolManager paid the collateral out"
        );

        // Burning with no claims left is ERC6909's checked subtraction —
        // Panic(0x11) — not a PoolManager error.
        callbackAmount = 1;
        callbackMode = MODE_BURN_CLAIMS;
        (bool ok, bytes memory revertData) =
            _callExpectRevert(address(pm), abi.encodeCall(IPoolManager.unlock, (bytes(""))));
        assertFalse(ok, "over-burn must revert");
        assertEq(
            revertData,
            abi.encodeWithSelector(bytes4(keccak256("Panic(uint256)")), uint256(0x11)),
            "over-burn is a checked-arithmetic panic"
        );
    }

    // ── executor-driven swap against the canonical PoolManager ────────────

    function testExecutorDrivesCanonicalV4Swap() public {
        ICmdExecutor canonicalExecutor = ICmdExecutor(
            _deploy("CmdExecutor", abi.encode(address(weth), address(pm), address(usdc), address(wbtc)))
        );
        vm.deal(address(this), 10_000 ether);
        canonicalExecutor.initialize{value: 1}();
        usdc.mint(address(canonicalExecutor), SWAP_AMOUNT_IN);

        bool zfo = key.currency0 == address(usdc);

        Cmd.Table memory at = Cmd.tokenTable(address(usdc), address(wbtc));
        at.weth = address(weth);
        at.executor = address(canonicalExecutor);
        at.pm = address(pm);
        uint8 c0Idx = Cmd.add(at, key.currency0);
        uint8 c1Idx = Cmd.add(at, key.currency1);
        uint8 hooksIdx = Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            c0Idx, c1Idx, uint16(key.fee), int16(key.tick_spacing), hooksIdx, zfo, SWAP_AMOUNT_IN
        );
        inner = bytes.concat(inner, Cmd.v4SettleAll());
        bytes memory stream = bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner));

        uint256 pmUsdcBefore = usdc.balanceOf(address(pm));
        uint256 pmWethBefore = weth.balanceOf(address(pm));

        vm.recordLogs();
        _execute(canonicalExecutor, stream, 1);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        (
            int128 amount0,
            int128 amount1,
            uint160 sqrtPriceAfter,
            uint128 liquidityAfter,
            uint24 feeAfter
        ) = _canonicalSwapEvent(logs, canonicalExecutor);

        int256 soldDelta = zfo ? int256(amount0) : int256(amount1);
        int256 boughtDelta = zfo ? int256(amount1) : int256(amount0);

        assertEq(soldDelta, -int256(SWAP_AMOUNT_IN), "exact-input debit of the sold currency");
        assertTrue(boughtDelta > 0, "canonical PoolManager math quoted a nonzero output");
        assertEq(
            weth.balanceOf(address(canonicalExecutor)),
            uint256(boughtDelta),
            "executor custody of the canonical output"
        );
        assertEq(usdc.balanceOf(address(canonicalExecutor)), 0, "executor paid its whole input");
        assertEq(usdc.balanceOf(address(pm)), pmUsdcBefore + SWAP_AMOUNT_IN, "PoolManager took the input");
        assertEq(weth.balanceOf(address(pm)), pmWethBefore - uint256(boughtDelta), "PoolManager paid the output");
        assertEq(liquidityAfter, uint128(uint256(LP_LIQUIDITY)), "position liquidity unchanged by the swap");
        assertEq(uint256(feeAfter), uint256(POOL_FEE), "the swap ran at the pool's fee");

        if (zfo) {
            assertLt(uint256(sqrtPriceAfter), uint256(sqrtPrice0), "zero-for-one moves the price down");
        } else {
            assertGt(uint256(sqrtPriceAfter), uint256(sqrtPrice0), "one-for-zero moves the price up");
        }
    }

    // ── Callback ─────────────────────────────────────────────────────────

    function unlockCallback(bytes calldata data) external returns (bytes memory) {
        require(msg.sender == address(pm), "canonical PoolManager only");

        if (callbackMode == MODE_ECHO) {
            return echoPayload;
        }
        if (callbackMode == MODE_NESTED_UNLOCK) {
            pm.unlock(data);
            return "";
        }
        if (callbackMode == MODE_ADD_LIQUIDITY) {
            (int256 callerDelta,) =
                pm.modifyLiquidity(key, ModifyLiquidityParams(lpLower, lpUpper, lpDelta, lpSalt), "");
            int256 observed0 = _pmDelta(key.currency0);
            int256 observed1 = _pmDelta(key.currency1);
            uint256 observedCount = _nonzeroDeltaCount();
            _settleDebit(key.currency0, _amount0(callerDelta));
            _settleDebit(key.currency1, _amount1(callerDelta));
            return abi.encode(
                callerDelta,
                observed0,
                observed1,
                observedCount,
                _pmDelta(key.currency0),
                _pmDelta(key.currency1),
                _nonzeroDeltaCount()
            );
        }
        if (callbackMode == MODE_TAKE_AND_SETTLE) {
            address currency = key.currency0;
            pm.take(currency, address(this), callbackAmount);
            int256 taken = _pmDelta(currency);
            uint256 countAfterTake = _nonzeroDeltaCount();
            pm.sync(currency);
            bytes32 syncedCurrency = pm.exttload(SYNCED_CURRENCY_SLOT);
            uint256 syncedReserves = uint256(pm.exttload(SYNCED_RESERVES_SLOT));
            IFakeErc20(currency).transfer(address(pm), callbackAmount);
            uint256 paid = pm.settle();
            return abi.encode(
                taken,
                countAfterTake,
                syncedCurrency,
                syncedReserves,
                paid,
                _pmDelta(currency),
                _nonzeroDeltaCount()
            );
        }
        if (callbackMode == MODE_MINT_CLAIMS) {
            address currency = key.currency0;
            pm.mint(address(this), uint256(uint160(currency)), callbackAmount);
            int256 minted = _pmDelta(currency);
            uint256 countAfterMint = _nonzeroDeltaCount();
            uint256 paid = _settleDebit(currency, minted);
            return abi.encode(minted, countAfterMint, paid, _pmDelta(currency), _nonzeroDeltaCount());
        }
        if (callbackMode == MODE_BURN_CLAIMS) {
            address currency = key.currency0;
            pm.burn(address(this), uint256(uint160(currency)), callbackAmount);
            int256 burned = _pmDelta(currency);
            pm.take(currency, address(this), callbackAmount);
            return abi.encode(burned, _pmDelta(currency), _nonzeroDeltaCount());
        }
        revert("unexpected callback mode");
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Add an in-range position and pay for it, the way a v4 LP must.
    function _provideLiquidity(bytes32 salt) internal returns (bytes memory) {
        int24 base = (tick0 / POOL_TICK_SPACING) * POOL_TICK_SPACING;
        lpLower = base - LP_TICK_RANGE;
        lpUpper = base + LP_TICK_RANGE;
        lpDelta = LP_LIQUIDITY;
        lpSalt = salt;
        callbackMode = MODE_ADD_LIQUIDITY;
        return pm.unlock("");
    }

    function _mintClaims(uint256 amount) internal {
        callbackAmount = amount;
        callbackMode = MODE_MINT_CLAIMS;
        pm.unlock("");
    }

    /// CurrencyDelta._computeSlot(target, currency): keccak256 of the two
    /// right-aligned addresses.
    function _deltaSlot(address account, address currency) internal pure returns (bytes32) {
        return keccak256(abi.encode(account, currency));
    }

    function _pmDelta(address currency) internal view returns (int256) {
        return int256(uint256(pm.exttload(_deltaSlot(address(this), currency))));
    }

    function _nonzeroDeltaCount() internal view returns (uint256) {
        return uint256(pm.exttload(NONZERO_DELTA_COUNT_SLOT));
    }

    /// BalanceDelta packing: amount0 in the upper 128 bits, amount1 in the
    /// lower 128, both signed.
    function _amount0(int256 delta) internal pure returns (int256) {
        return delta >> 128;
    }

    function _amount1(int256 delta) internal pure returns (int256) {
        return int256(int128(uint128(uint256(delta))));
    }

    /// Pay a debit the integrator way — sync, transfer, settle — and require the
    /// amount `settle()` measured is the amount owed.
    function _settleDebit(address currency, int256 delta) internal returns (uint256 paid) {
        require(delta <= 0, "expected a debit");
        if (delta == 0) return 0;
        uint256 owed = uint256(-delta);
        pm.sync(currency);
        IFakeErc20(currency).transfer(address(pm), owed);
        paid = pm.settle();
        require(paid == owed, "settle() did not measure the transfer");
    }

    /// Count the token's `Transfer(from → to, value)` events exactly.
    function _countTokenTransfers(
        Vm.Log[] memory logs,
        address token,
        address from,
        address to,
        uint256 value
    ) internal pure returns (uint256 n) {
        bytes32 topic = keccak256("Transfer(address,address,uint256)");
        for (uint256 i = 0; i < logs.length; i++) {
            Vm.Log memory entry = logs[i];
            if (entry.emitter != token || entry.topics.length != 3 || entry.topics[0] != topic) continue;
            if (address(uint160(uint256(entry.topics[1]))) != from) continue;
            if (address(uint160(uint256(entry.topics[2]))) != to) continue;
            if (abi.decode(entry.data, (uint256)) != value) continue;
            n += 1;
        }
    }

    /// The canonical PoolManager's own statement of what its swap produced: the
    /// single `Swap` event it emitted for the executor's call.
    function _canonicalSwapEvent(Vm.Log[] memory logs, ICmdExecutor ex)
        internal
        view
        returns (int128 amount0, int128 amount1, uint160 sqrtPriceX96, uint128 liquidity, uint24 fee)
    {
        bytes32 topic = IPoolManager.Swap.selector;
        uint256 hits;
        for (uint256 i = 0; i < logs.length; i++) {
            Vm.Log memory entry = logs[i];
            if (entry.emitter != address(pm) || entry.topics.length != 3 || entry.topics[0] != topic) {
                continue;
            }
            require(
                address(uint160(uint256(entry.topics[2]))) == address(ex),
                "Swap sender is the executor"
            );
            (amount0, amount1, sqrtPriceX96, liquidity,, fee) =
                abi.decode(entry.data, (int128, int128, uint160, uint128, int24, uint24));
            hits += 1;
        }
        require(hits == 1, "exactly one canonical Swap event");
    }
}
