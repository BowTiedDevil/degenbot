// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "../src/TestBase.sol";
import {IExttloadComparator} from "../src/generated/IExttloadComparator.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV4PoolManager} from "../src/generated/IFakeUniswapV4PoolManager.sol";
import {IPoolManager} from "../src/generated/IPoolManager.sol";
import {ModifyLiquidityParams, PoolKey, SwapParams} from "../src/generated/Types.sol";
import {Vm} from "../src/Vm.sol";

/// The fake Vyper `PoolManager` against the real one, on a mainnet archive fork.
///
/// This is the Solidity port of `tests/executor/test_fake_pool_manager_parity.py`
/// and it answers one question: where the fake claims to mirror mainnet V4, does
/// the deployed PoolManager actually behave that way? The real manager is pinned
/// at `REAL_PM` on `FORK_BLOCK`; the fake is deployed from the lane's pinned-vyper
/// artifact beside it, and `ExttloadComparator` reads both managers' transient
/// slots from inside their own unlock callbacks, where transient storage is live.
///
/// A revert is compared as an IDENTITY, never as a shared string: the real
/// manager raises custom errors (`ManagerLocked()`, `SwapAmountCannotBeZero()`,
/// `CurrenciesOutOfOrderOrEqual(currency0, currency1)`, ...) while the fake raises
/// `Error(string)` carrying the same name. Asserting "both reverted" would
/// certify nothing about either, so every negative case pins the real manager's
/// exact custom-error payload and the fake's exact string payload.
///
/// The lane is on demand: `DEGENBOT_POOL_MANAGER_PARITY_FORK` is set only by
/// `just test-foundry-executor-pool-manager-parity`, so the offline gate compiles
/// this file and skips the whole contract. `setUp` refuses to run against a fork
/// whose `REAL_PM` account has no code — a parity assertion against an empty
/// account is vacuous, and a fork that cannot serve the pinned block is an error,
/// not a skip.
contract PoolManagerParityTest is TestBase {
    // ── Fork anchors ──────────────────────────────────────────────────────

    /// Uniswap V4 `PoolManager` on Ethereum mainnet (the singleton every real V4
    /// pool settles through).
    address internal constant REAL_PM = 0x000000000004444c5dc75cB358380D2e3dE08A90;

    /// An archive height well past the V4 deployment (21,688,329) where the
    /// manager holds native ETH, so a native `take` is exercisable.
    uint256 internal constant FORK_BLOCK = 22_000_000;

    string internal constant FORK_LANE_ENV = "DEGENBOT_POOL_MANAGER_PARITY_FORK";
    string internal constant RPC_ENV = "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI";

    /// A mainnet token address, used only as a slot-input that must NOT collide
    /// with the native delta slot.
    address internal constant MAINNET_WETH = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;

    // ── v4-core transient/storage slot identities ─────────────────────────
    //
    // `bytes32(uint256(keccak256(label)) - 1)` for Lock,
    // NonzeroDeltaCount, CurrencyReserves' Currency and ReservesOf; the pools
    // mapping index from `StateLibrary`. These are the deployed manager's own
    // slots, which is what makes a read here a statement about the manager.

    bytes32 internal constant IS_UNLOCKED_SLOT =
        0xc090fc4683624cfc3884e9d8de5eca132f2d0ec062aff75d43c0465d5ceeab23;
    bytes32 internal constant NONZERO_DELTA_COUNT_SLOT =
        0x7d4b3164c6e45b97e7d87b7125a44c5828d005af88f9d751cfd78729c5d99a0b;
    bytes32 internal constant SYNCED_CURRENCY_SLOT =
        0x27e098c505d44ec3574004bca052aabf76bd35004c182099d8c575fb238593b9;
    bytes32 internal constant SYNCED_RESERVES_SLOT =
        0x1e0745a7db1623981f0b2a5d4232364c00787266eb75ad546f190e6cebe9bd95;
    bytes32 internal constant POOLS_SLOT = bytes32(uint256(6));

    // ── Pool geometry ─────────────────────────────────────────────────────

    uint160 internal constant SQRT_PRICE_1_1 = uint160(1) << 96;
    uint160 internal constant MIN_SQRT_PRICE_PLUS_ONE = 4295128740;
    uint24 internal constant POOL_FEE = 3000;
    int24 internal constant POOL_TICK_SPACING = 60;
    uint24 internal constant DYNAMIC_FEE_FLAG = 0x800000;
    int256 internal constant LP_LIQUIDITY = 1e18;
    int24 internal constant LP_TICK_RANGE = 60;
    uint256 internal constant ONE_ETH = 1 ether;
    uint256 internal constant SWAP_AMOUNT_IN = 1e15;

    /// A hook address whose low 14 bits are clear: no hook permission is set, so
    /// the manager makes no hook call, and `Hooks.isValidHookAddress` accepts it
    /// for a dynamic-fee pool. It is the `updateDynamicLPFee` caller the fake's
    /// Ape lane could not impersonate.
    address internal constant DYNAMIC_FEE_HOOK = address(uint160(0x4000));

    // ── Fixture ───────────────────────────────────────────────────────────

    IPoolManager internal realPm;
    IFakeUniswapV4PoolManager internal fakePm;
    IExttloadComparator internal comparator;
    IFakeErc20 internal tokenA;
    IFakeErc20 internal tokenB;
    PoolKey internal key;
    PoolKey internal uninitializedKey;
    PoolKey internal dynamicKey;
    DeltaRecipient internal recipient;
    NonCallbackCaller internal nonCallbackCaller;

    // ── Unlock-callback request state ─────────────────────────────────────

    uint8 internal constant MODE_EMPTY = 0;
    uint8 internal constant MODE_TAKE_NATIVE = 1;
    uint8 internal constant MODE_TAKE_WITHOUT_SETTLE = 2;
    uint8 internal constant MODE_NESTED_UNLOCK = 3;
    uint8 internal constant MODE_SETTLE_FOR = 4;
    uint8 internal constant MODE_MINT_CLAIMS = 5;
    uint8 internal constant MODE_BURN_CLAIMS = 6;
    uint8 internal constant MODE_CLEAR_EXACT_DELTA = 7;
    uint8 internal constant MODE_SYNC_ERC20 = 8;
    uint8 internal constant MODE_SWAP_ZERO = 9;
    uint8 internal constant MODE_SWAP_EXACT_IN = 10;
    uint8 internal constant MODE_ADD_LIQUIDITY = 11;
    uint8 internal constant MODE_MODIFY_LIQUIDITY_PROBE = 12;

    int24 internal lpLower;
    int24 internal lpUpper;

    // Observations one unlock leg returns. They live in memory structs so a
    // real/fake comparison never needs a dozen simultaneous stack slots.
    struct ClearObservation {
        int256 credited;
        uint256 countBefore;
        bool mismatchedOk;
        bytes mismatchedData;
        int256 deltaAfter;
        uint256 countAfter;
    }

    struct MintObservation {
        int256 delta;
        uint256 count;
        uint256 paid;
        uint256 claims;
        int256 deltaAfter;
        uint256 countAfter;
    }

    struct BurnObservation {
        int256 credit;
        int256 deltaAfter;
        uint256 count;
        uint256 claims;
    }

    struct TakeObservation {
        int256 delta;
        uint256 lock;
        uint256 count;
        uint256 delivered;
        uint256 paid;
        int256 deltaAfter;
        uint256 countAfter;
    }

    struct ReservesObservation {
        bytes32 currency;
        uint256 reserves;
        uint256 balanceAtSync;
        uint256 paid;
        bytes32 currencyAfter;
        bytes32 reservesAfter;
        int256 deltaAfter;
        uint256 countAfter;
    }

    struct SettleForObservation {
        int256 recipient;
        int256 caller;
        uint256 count;
        uint256 received;
        int256 deltaAfter;
    }

    function setUp() public {
        if (bytes(vm.envOr(FORK_LANE_ENV, "")).length == 0) {
            // The offline gate has no endpoint and must never dial one.
            vm.skip(true);
        }

        string memory rpcUrl = vm.envOr(RPC_ENV, "");
        require(bytes(rpcUrl).length != 0, "fork lane: no archive endpoint declared");

        uint256 forkId = vm.createFork(rpcUrl, FORK_BLOCK);
        vm.selectFork(forkId);
        require(vm.activeFork() == forkId, "fork lane: selectFork did not take");
        require(REAL_PM.code.length != 0, "fork lane: no PoolManager code at the pinned block");

        realPm = IPoolManager(REAL_PM);
        vm.label(REAL_PM, "mainnet PoolManager");

        vm.deal(address(this), 100 ether);

        tokenA = IFakeErc20(
            _deploy("FakeErc20", abi.encode("Parity Token A", "PTA", uint8(18), uint256(100_000_000)))
        );
        tokenB = IFakeErc20(
            _deploy("FakeErc20", abi.encode("Parity Token B", "PTB", uint8(18), uint256(100_000_000)))
        );

        (address currency0, address currency1) = _sorted(address(tokenA), address(tokenB));
        key = PoolKey(currency0, currency1, POOL_FEE, POOL_TICK_SPACING, address(0));
        uninitializedKey = PoolKey(currency0, currency1, 500, 10, address(0));
        dynamicKey = PoolKey(currency0, currency1, DYNAMIC_FEE_FLAG, POOL_TICK_SPACING, DYNAMIC_FEE_HOOK);

        fakePm = IFakeUniswapV4PoolManager(
            _deploy("FakeUniswapV4PoolManager", abi.encode(currency0, currency1))
        );
        comparator = IExttloadComparator(_deploy("ExttloadComparator", abi.encode(REAL_PM, address(fakePm))));
        recipient = new DeltaRecipient();
        nonCallbackCaller = new NonCallbackCaller();

        // Both managers hold the same inventory so a reserves/delta read is
        // comparable figure-for-figure rather than "both nonzero".
        uint256 pmInventory = 1_000_000e18;
        tokenA.mint(REAL_PM, pmInventory);
        tokenA.mint(address(fakePm), pmInventory);
        tokenB.mint(REAL_PM, pmInventory);
        tokenB.mint(address(fakePm), pmInventory);

        // The comparator takes 1 ETH natively from each manager and settles it
        // back inside the same unlock; the fake pays it out of its own balance.
        vm.deal(address(comparator), 5 ether);
        vm.deal(address(fakePm), 5 ether);
    }

    // ══════════════════════════════════════════════════════════════════════
    // Transient storage: Lock, NonzeroDeltaCount, CurrencyDelta
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestExttloadTransientSlots.test_exttload_native_delta_matches`.
    ///
    /// A native `take` of 1 ETH inside each manager's unlock leaves the caller
    /// owing exactly 1 ETH; `ExttloadComparator` reads that delta from both
    /// managers' `CurrencyDelta` slots and only returns true when the two 32-byte
    /// values are identical. The delta is read again after the unlock to show the
    /// slot is back to zero — transient storage is live INSIDE the callback and
    /// cleared by the time the lock releases.
    function testExttloadNativeDeltaMatches() public {
        assertTrue(comparator.compare(), "the comparator's own real-vs-fake assert must hold");

        bytes32 realValue = comparator.real_exttload_value();
        bytes32 fakeValue = comparator.fake_exttload_value();
        assertEq(realValue, fakeValue, "native delta slot parity");
        assertEq(int256(uint256(realValue)), -int256(ONE_ETH), "real manager owes the caller 1 ETH");

        bytes32 deltaSlot = comparator.nativeDeltaSlot();
        assertEq(deltaSlot, _deltaSlot(address(comparator), NATIVE), "delta slot derivation");
        assertEq(realPm.exttload(deltaSlot), bytes32(0), "real delta slot cleared after settlement");
        assertEq(fakePm.exttload(deltaSlot), bytes32(0), "fake delta slot cleared after settlement");
    }

    /// Port of
    /// `TestExttloadTransientSlots.test_exttload_lock_and_count_slots_inside_unlock`.
    ///
    /// Inside the callback the manager is locked (Lock = 1) and carries one
    /// nonzero delta (NonzeroDeltaCount = 1) — on the real manager and on the
    /// fake. The same slots read zero once the unlock returns.
    function testExttloadLockAndCountSlotsInsideUnlock() public {
        comparator.readSlotsInsideCallback();

        bytes32 realLock = comparator.s_real_lock();
        bytes32 fakeLock = comparator.s_fake_lock();
        bytes32 realCount = comparator.s_real_count();
        bytes32 fakeCount = comparator.s_fake_count();
        bytes32 realDelta = comparator.s_real_delta();
        bytes32 fakeDelta = comparator.s_fake_delta();

        assertEq(realLock, fakeLock, "Lock slot parity");
        assertEq(uint256(realLock), 1, "real manager is locked inside the callback");
        assertEq(uint256(fakeLock), 1, "fake manager is locked inside the callback");

        assertEq(realCount, fakeCount, "NonzeroDeltaCount parity");
        assertEq(uint256(realCount), 1, "real manager carries one nonzero delta");
        assertEq(uint256(fakeCount), 1, "fake manager carries one nonzero delta");

        assertEq(realDelta, fakeDelta, "CurrencyDelta parity");
        assertEq(int256(uint256(realDelta)), -int256(ONE_ETH), "real delta is the taken 1 ETH");

        // Outside the unlock both managers report released/zero state.
        assertEq(realPm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "real lock released");
        assertEq(fakePm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "fake lock released");
        assertEq(_nonzeroDeltaCount(address(realPm)), 0, "real nonzero delta count cleared");
        assertEq(_nonzeroDeltaCount(address(fakePm)), 0, "fake nonzero delta count cleared");
        assertEq(realPm.exttload(comparator.nativeDeltaSlot()), bytes32(0), "real delta cleared");
        assertEq(fakePm.exttload(comparator.nativeDeltaSlot()), bytes32(0), "fake delta cleared");
    }

    /// Port of `TestExttloadTransientSlots.test_exttload_cleared_outside_unlock`.
    ///
    /// Outside any unlock the lock, count, delta and reserves slots read zero on
    /// both managers. This is the case that used to fail against a codeless
    /// account: a typed `exttload` staticcall to an address with no code reverts
    /// on the empty return, so a fork without real bytecode fails here instead of
    /// reporting zero.
    function testExttloadClearedOutsideUnlock() public {
        (bytes32 realLock, bytes32 fakeLock) = comparator.readArbitrarySlot(IS_UNLOCKED_SLOT);
        assertEq(realLock, bytes32(0), "real lock slot zero outside unlock");
        assertEq(fakeLock, bytes32(0), "fake lock slot zero outside unlock");

        (bytes32 realCount, bytes32 fakeCount) = comparator.readArbitrarySlot(NONZERO_DELTA_COUNT_SLOT);
        assertEq(realCount, bytes32(0), "real nonzero-delta count zero outside unlock");
        assertEq(fakeCount, bytes32(0), "fake nonzero-delta count zero outside unlock");

        (bytes32 realDelta, bytes32 fakeDelta) =
            comparator.readArbitrarySlot(comparator.nativeDeltaSlot());
        assertEq(realDelta, bytes32(0), "real native delta zero outside unlock");
        assertEq(fakeDelta, bytes32(0), "fake native delta zero outside unlock");

        (bytes32 realReserves, bytes32 fakeReserves) =
            comparator.readArbitrarySlot(SYNCED_RESERVES_SLOT);
        assertEq(realReserves, bytes32(0), "real synced reserves zero outside unlock");
        assertEq(fakeReserves, bytes32(0), "fake synced reserves zero outside unlock");
    }

    /// Port of
    /// `TestExttloadTransientSlots.test_exttload_slot_computation_consistency`.
    ///
    /// `CurrencyDelta._computeSlot(target, currency)` is
    /// `keccak256(abi.encode(target, currency))`: the comparator's own native
    /// slot agrees with that derivation, and a non-native currency hashes to a
    /// different slot.
    function testExttloadSlotComputationConsistency() public {
        bytes32 nativeSlot = comparator.nativeDeltaSlot();
        assertEq(
            nativeSlot,
            comparator.computeSlot(address(comparator), NATIVE),
            "comparator native slot derivation"
        );
        assertEq(
            nativeSlot,
            _deltaSlot(address(comparator), NATIVE),
            "native slot matches the manager's CurrencyDelta derivation"
        );
        assertTrue(
            comparator.computeSlot(address(comparator), MAINNET_WETH) != nativeSlot,
            "a token currency does not collide with the native slot"
        );
        assertTrue(
            comparator.computeSlot(address(recipient), NATIVE) != nativeSlot,
            "a different target does not collide with the comparator's slot"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // Lock and unlock guard
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestUnlock.test_double_unlock_reverts` (the collected definition).
    ///
    /// `unlock` requires the caller to receive the callback: a contract without
    /// `unlockCallback` is refused by both managers, while the harness — which
    /// implements it — round-trips empty data and leaves the lock released.
    function testDoubleUnlockReverts() public {
        (bool realOk, bytes memory realData) = nonCallbackCaller.callUnlock(address(realPm), "");
        assertFalse(realOk, "real manager refuses a caller that cannot receive the callback");
        assertEq(realData.length, 0, "real refusal carries no payload (the callback call failed)");

        (bool fakeOk, bytes memory fakeData) = nonCallbackCaller.callUnlock(address(fakePm), "");
        assertFalse(fakeOk, "fake manager refuses a caller that cannot receive the callback");
        assertEq(fakeData.length, 0, "fake refusal carries no payload (the callback call failed)");

        // Positive control: the same call from a contract that DOES implement the
        // callback is accepted by both managers.
        assertEq(_unlock(address(realPm), MODE_EMPTY, 0, address(0)), bytes(""), "real unlock echo");
        assertEq(_unlock(address(fakePm), MODE_EMPTY, 0, address(0)), bytes(""), "fake unlock echo");
        assertEq(realPm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "real lock released");
        assertEq(fakePm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "fake lock released");
    }

    /// Port of `TestUnlock.test_operations_locked_outside_unlock`.
    ///
    /// Every delta-accounting entry point is refused while the manager is
    /// unlocked=false, with the manager's own identity on each side.
    function testOperationsLockedOutsideUnlock() public {
        _assertLockedBoth(
            abi.encodeCall(IPoolManager.take, (NATIVE, address(this), 1)), "take"
        );
        _assertLockedBoth(abi.encodeCall(IPoolManager.settle, ()), "settle");
        _assertLockedBoth(abi.encodeCall(IPoolManager.settleFor, (address(this))), "settleFor");
        _assertLockedBoth(abi.encodeCall(IPoolManager.mint, (address(this), uint256(0), uint256(1))), "mint");
        _assertLockedBoth(abi.encodeCall(IPoolManager.burn, (address(this), uint256(0), uint256(1))), "burn");
        _assertLockedBoth(abi.encodeCall(IPoolManager.clear, (NATIVE, uint256(1))), "clear");
        _assertLockedBoth(
            abi.encodeCall(
                IPoolManager.swap, (key, SwapParams(true, -int256(1), MIN_SQRT_PRICE_PLUS_ONE), bytes(""))
            ),
            "swap"
        );
        _assertLockedBoth(
            abi.encodeCall(
                IPoolManager.modifyLiquidity,
                (key, ModifyLiquidityParams(-LP_TICK_RANGE, LP_TICK_RANGE, LP_LIQUIDITY, bytes32(0)), bytes(""))
            ),
            "modifyLiquidity"
        );
        _assertLockedBoth(abi.encodeCall(IPoolManager.donate, (key, uint256(1), uint256(2), bytes(""))), "donate");
    }

    /// The shadowed source definition's intention: a real second `unlock` from
    /// inside the first callback is the manager's own refusal, and the fake
    /// refuses it too.
    function testDoubleUnlockRevertsInsideCallback() public {
        (bool realOk, bytes memory realData) =
            _callExpectRevert(REAL_PM, abi.encodeCall(IPoolManager.unlock, (_request(REAL_PM, MODE_NESTED_UNLOCK, 0, address(0)))));
        assertTrue(realOk, "the outer real unlock completes: the nested refusal is caught");
        (bool realNestedOk, bytes memory realNestedData) = abi.decode(_payload(realData), (bool, bytes));
        assertFalse(realNestedOk, "nested real unlock reverts");
        assertEq(realNestedData, _customError("AlreadyUnlocked()"), "nested real unlock reverts AlreadyUnlocked()");

        (bool fakeOk, bytes memory fakeData) =
            _callExpectRevert(address(fakePm), abi.encodeCall(IFakeUniswapV4PoolManager.unlock, (_request(address(fakePm), MODE_NESTED_UNLOCK, 0, address(0)))));
        assertTrue(fakeOk, "the outer fake unlock completes: the nested refusal is caught");
        (bool fakeNestedOk, bytes memory fakeNestedData) = abi.decode(_payload(fakeData), (bool, bytes));
        assertFalse(fakeNestedOk, "nested fake unlock reverts");
        assertEq(fakeNestedData, _fakeError("AlreadyUnlocked"), "nested fake unlock reverts AlreadyUnlocked");

        assertEq(realPm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "real lock released");
        assertEq(fakePm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "fake lock released");
    }

    // ══════════════════════════════════════════════════════════════════════
    // sync / settle and CurrencyReserves mirroring
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestSyncSettle.test_sync_native_outside_unlock`, hardened with
    /// the ERC-20 reserves leg the source's class docstring claims.
    ///
    /// `sync` is callable outside unlock on both managers (it is not
    /// delta-accounting). A native sync only resets the synced-currency slot; an
    /// ERC-20 sync snapshots the manager's own balance into the transient
    /// `ReservesOf` slot, and the following `settle` measures exactly the
    /// transfer that arrived since.
    function testSyncNativeOutsideUnlock() public {
        realPm.sync(NATIVE);
        fakePm.sync(NATIVE);
        assertEq(realPm.exttload(SYNCED_CURRENCY_SLOT), bytes32(0), "real native sync resets the currency slot");
        assertEq(fakePm.exttload(SYNCED_CURRENCY_SLOT), bytes32(0), "fake native sync resets the currency slot");

        uint256 amount = 1_234e18;
        tokenA.mint(address(this), amount);

        _assertReservesParity(
            _reservesLeg(address(realPm), amount), _reservesLeg(address(fakePm), amount), address(tokenA), amount
        );
    }

    /// Port of `TestSyncSettle.test_settle_native_inside_unlock`, hardened with
    /// the settlement-enforcement refusal.
    ///
    /// The comparator's take → sync → settle round trip must succeed on both
    /// managers (the settled delta is what makes the unlock return), the two
    /// managers' native balances must be unchanged by it, and an unlock that
    /// leaves a delta open must be refused.
    function testSettleNativeInsideUnlock() public {
        uint256 realBefore = REAL_PM.balance;
        uint256 fakeBefore = address(fakePm).balance;

        assertTrue(comparator.compare(), "the take + sync + settle round trip succeeds");
        assertEq(REAL_PM.balance, realBefore, "real manager's native balance is restored by settle");
        assertEq(address(fakePm).balance, fakeBefore, "fake manager's native balance is restored by settle");
        assertEq(comparator.s_real_count(), bytes32(uint256(1)), "the take left one nonzero delta");
        assertEq(comparator.s_fake_count(), bytes32(uint256(1)), "the fake's take left one nonzero delta");

        // Settlement enforcement: the same take without the settle is refused.
        bytes memory realUnpaid =
            abi.encodeCall(IPoolManager.unlock, (_request(REAL_PM, MODE_TAKE_WITHOUT_SETTLE, 1, address(this))));
        (bool realOk, bytes memory realData) = _callExpectRevert(REAL_PM, realUnpaid);
        assertFalse(realOk, "real manager refuses an unlock that leaves a delta open");
        assertEq(realData, _customError("CurrencyNotSettled()"), "real refusal is CurrencyNotSettled()");

        bytes memory fakeUnpaid = abi.encodeCall(
            IFakeUniswapV4PoolManager.unlock,
            (_request(address(fakePm), MODE_TAKE_WITHOUT_SETTLE, 1, address(this)))
        );
        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(address(fakePm), fakeUnpaid);
        assertFalse(fakeOk, "fake manager refuses an unlock that leaves a delta open");
        assertEq(
            fakeData,
            abi.encodeWithSignature(
                "Error(string)",
                string.concat(
                    "CurrencyNotSettled:", vm.toString(NATIVE), ":-", vm.toString(uint256(1))
                )
            ),
            "fake refusal names the unsettled currency and amount"
        );
    }

    /// Port of `TestSyncSettle.test_settle_for_delegates_delta`.
    ///
    /// Outside unlock `settleFor` is refused like every other settlement entry
    /// point; inside unlock it credits the named account, not `msg.sender` — the
    /// delegate then clears its own credit with a `take`.
    function testSettleForDelegatesDelta() public {
        _assertLockedBoth(abi.encodeCall(IPoolManager.settleFor, (address(recipient))), "settleFor");

        _assertSettleForParity(_settleForLeg(address(realPm)), _settleForLeg(address(fakePm)), ONE_ETH);
    }

    // ══════════════════════════════════════════════════════════════════════
    // initialize
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestInitialize.test_initialize_basic`.
    ///
    /// The same key at the same price initializes to tick 0 on both managers, and
    /// both record the price: the fake in its `pools` mapping, the real manager
    /// in the pool state its `extsload` exposes. A second real `initialize` of
    /// that pool is refused with `PoolAlreadyInitialized()`.
    function testInitializeBasic() public {
        int24 realTick = realPm.initialize(key, SQRT_PRICE_1_1);
        int24 fakeTick = fakePm.initialize(key, SQRT_PRICE_1_1);

        assertEq(realTick, int24(0), "real manager reports tick 0 at price 1");
        assertEq(fakeTick, int24(0), "fake manager reports tick 0");

        bytes32 poolId = _poolId(key);
        assertEq(uint256(fakePm.pools(poolId)), uint256(SQRT_PRICE_1_1), "fake stored the pool price");
        assertEq(
            uint256(_sqrtPriceOf(realPm.extsload(_poolStateSlot(poolId)))),
            uint256(SQRT_PRICE_1_1),
            "real stored the pool price in its slot0"
        );

        (bool ok, bytes memory data) = _callExpectRevert(REAL_PM, abi.encodeCall(IPoolManager.initialize, (key, SQRT_PRICE_1_1)));
        assertFalse(ok, "real manager refuses a second initialize");
        assertEq(data, _customError("PoolAlreadyInitialized()"), "real refusal is PoolAlreadyInitialized()");
    }

    /// Port of `TestInitialize.test_initialize_currency_order_violation`.
    function testInitializeCurrencyOrderViolation() public {
        PoolKey memory unordered =
            PoolKey(key.currency1, key.currency0, POOL_FEE, POOL_TICK_SPACING, address(0));

        (bool realOk, bytes memory realData) =
            _callExpectRevert(REAL_PM, abi.encodeCall(IPoolManager.initialize, (unordered, SQRT_PRICE_1_1)));
        assertFalse(realOk, "real manager refuses unordered currencies");
        assertEq(
            realData,
            _customErrorWithAddresses(
                "CurrenciesOutOfOrderOrEqual(address,address)", unordered.currency0, unordered.currency1
            ),
            "real refusal names both currencies"
        );

        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(
            address(fakePm),
            abi.encodeCall(IFakeUniswapV4PoolManager.initialize, (unordered, SQRT_PRICE_1_1))
        );
        assertFalse(fakeOk, "fake manager refuses unordered currencies");
        assertEq(fakeData, _fakeError("CurrenciesOutOfOrderOrEqual"), "fake refusal identity");
    }

    /// Port of `TestInitialize.test_initialize_same_currencies`.
    function testInitializeSameCurrencies() public {
        PoolKey memory equal =
            PoolKey(key.currency0, key.currency0, POOL_FEE, POOL_TICK_SPACING, address(0));

        (bool realOk, bytes memory realData) =
            _callExpectRevert(REAL_PM, abi.encodeCall(IPoolManager.initialize, (equal, SQRT_PRICE_1_1)));
        assertFalse(realOk, "real manager refuses equal currencies");
        assertEq(
            realData,
            _customErrorWithAddresses(
                "CurrenciesOutOfOrderOrEqual(address,address)", equal.currency0, equal.currency1
            ),
            "real refusal is CurrenciesOutOfOrderOrEqual"
        );

        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(
            address(fakePm),
            abi.encodeCall(IFakeUniswapV4PoolManager.initialize, (equal, SQRT_PRICE_1_1))
        );
        assertFalse(fakeOk, "fake manager refuses equal currencies");
        assertEq(fakeData, _fakeError("CurrenciesOutOfOrderOrEqual"), "fake refusal identity");
    }

    /// Port of `TestInitialize.test_initialize_tick_spacing_too_large`.
    ///
    /// Both managers cap the tick spacing, but not at the same bound: the
    /// deployed manager's cap is `type(int16).max` (32,767) while the fake keeps
    /// the older 16,384 bound, so the shared-refusal case uses a spacing above
    /// both caps. The divergence at 16,385 is asserted rather than skipped: the
    /// deployed manager records that pool and the fake refuses it.
    function testInitializeTickSpacingTooLarge() public {
        (bool realOk, bytes memory realData) = _callExpectRevert(
            REAL_PM,
            abi.encodeCall(
                IPoolManager.initialize, (PoolKey(key.currency0, key.currency1, POOL_FEE, 32768, address(0)), SQRT_PRICE_1_1)
            )
        );
        assertFalse(realOk, "real manager refuses a spacing above its cap");
        assertEq(
            realData,
            _customErrorWithInt24("TickSpacingTooLarge(int24)", 32768),
            "real refusal carries the spacing"
        );

        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(
            address(fakePm),
            abi.encodeCall(
                IFakeUniswapV4PoolManager.initialize,
                (PoolKey(key.currency0, key.currency1, POOL_FEE, 32768, address(0)), SQRT_PRICE_1_1)
            )
        );
        assertFalse(fakeOk, "fake manager refuses a spacing above its cap");
        assertEq(fakeData, _fakeError("TickSpacingTooLarge"), "fake refusal identity");

        PoolKey memory legacyCap = PoolKey(key.currency0, key.currency1, POOL_FEE, 16385, address(0));
        assertEq(realPm.initialize(legacyCap, SQRT_PRICE_1_1), int24(0), "the deployed manager accepts 16,385");
        (bool legacyOk, bytes memory legacyData) = _callExpectRevert(
            address(fakePm),
            abi.encodeCall(IFakeUniswapV4PoolManager.initialize, (legacyCap, SQRT_PRICE_1_1))
        );
        assertFalse(legacyOk, "the fake keeps the older 16,384 cap");
        assertEq(legacyData, _fakeError("TickSpacingTooLarge"), "fake 16,385 refusal identity");
    }

    /// Port of `TestInitialize.test_initialize_works_outside_unlock`.
    ///
    /// `initialize` is not a delta-accounting action: both managers accept it
    /// with the lock released, and both record the pool.
    function testInitializeWorksOutsideUnlock() public {
        assertEq(realPm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "the real manager is not unlocked");
        assertEq(fakePm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "the fake manager is not unlocked");

        realPm.initialize(uninitializedKey, SQRT_PRICE_1_1);
        fakePm.initialize(uninitializedKey, SQRT_PRICE_1_1);

        bytes32 poolId = _poolId(uninitializedKey);
        assertTrue(fakePm.pools(poolId) != 0, "fake recorded the pool");
        assertEq(
            uint256(_sqrtPriceOf(realPm.extsload(_poolStateSlot(poolId)))),
            uint256(SQRT_PRICE_1_1),
            "real recorded the pool"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // clear
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestClear.test_clear_exact_positive_delta`.
    ///
    /// `clear` is a delta-accounting action, so the lock gate is what the source
    /// case asserts; both managers refuse it outside unlock.
    function testClearExactPositiveDelta() public {
        _assertLockedBoth(abi.encodeCall(IPoolManager.clear, (NATIVE, ONE_ETH)), "clear");
    }

    /// The source case's stated intention, executed: `clear` subtracts an exact
    /// positive delta to zero and releases the nonzero-delta count, and a
    /// non-matching amount is refused instead of silently clearing.
    function testClearExactPositiveDeltaInsideUnlock() public {
        _assertClearParity(_clearLeg(address(realPm)), _clearLeg(address(fakePm)));
    }

    // ══════════════════════════════════════════════════════════════════════
    // ERC6909 claims
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestERC6909.test_mint_credits_erc6909_and_debits_delta`.
    ///
    /// `mint` is lock-gated on both managers; inside unlock it debits the
    /// caller's delta by exactly the minted amount and credits the claim.
    function testMintCreditsErc6909AndDebitsDelta() public {
        _assertLockedBoth(abi.encodeCall(IPoolManager.mint, (address(this), uint256(0), uint256(100))), "mint");

        _assertMintParity(_mintLeg(address(realPm), 100), _mintLeg(address(fakePm), 100), 100);
    }

    /// Port of `TestERC6909.test_burn_debits_erc6909_and_credits_delta`.
    ///
    /// `burn` is lock-gated on both managers; inside unlock it credits the
    /// caller's delta and destroys the claim, and a `take` spends the credit.
    function testBurnDebitsErc6909AndCreditsDelta() public {
        _assertLockedBoth(abi.encodeCall(IPoolManager.burn, (address(this), uint256(0), uint256(100))), "burn");

        uint256 amount = 100;
        _unlock(address(realPm), MODE_MINT_CLAIMS, amount, address(this));
        _unlock(address(fakePm), MODE_MINT_CLAIMS, amount, address(this));

        _assertBurnParity(_burnLeg(address(realPm), amount), _burnLeg(address(fakePm), amount), amount);
    }

    /// Port of `TestERC6909.test_erc6909_transfer`.
    ///
    /// Claims are transferable outside unlock on both managers, and the balances
    /// move by exactly the transferred amount.
    function testErc6909Transfer() public {
        address receiver = address(uint160(0xbeef));
        uint256 id = 0;

        assertEq(realPm.balanceOf(address(this), id), 0, "real claims start at zero");
        assertEq(fakePm.balanceOf(address(this), id), 0, "fake claims start at zero");

        _unlock(address(realPm), MODE_MINT_CLAIMS, 1000, address(this));
        _unlock(address(fakePm), MODE_MINT_CLAIMS, 1000, address(this));

        assertTrue(realPm.transfer(receiver, id, 400), "real transfer succeeds");
        assertTrue(fakePm.transfer(receiver, id, 400), "fake transfer succeeds");

        assertEq(realPm.balanceOf(address(this), id), 600, "real sender balance");
        assertEq(fakePm.balanceOf(address(this), id), 600, "fake sender balance");
        assertEq(realPm.balanceOf(receiver, id), 400, "real receiver balance");
        assertEq(fakePm.balanceOf(receiver, id), 400, "fake receiver balance");
    }

    /// Port of `TestERC6909.test_erc6909_approve_and_allowance`.
    function testErc6909ApproveAndAllowance() public {
        address spender = address(uint160(0xbee1));
        uint256 id = 0;

        assertTrue(realPm.approve(spender, id, 500), "real approve succeeds");
        assertTrue(fakePm.approve(spender, id, 500), "fake approve succeeds");
        assertEq(realPm.allowance(address(this), spender, id), 500, "real allowance");
        assertEq(fakePm.allowance(address(this), spender, id), 500, "fake allowance");
    }

    /// Port of `TestERC6909.test_erc6909_set_operator`.
    function testErc6909SetOperator() public {
        address operator = address(uint160(0xbee2));

        assertFalse(realPm.isOperator(address(this), operator), "real operator starts unset");
        assertFalse(fakePm.isOperator(address(this), operator), "fake operator starts unset");

        realPm.setOperator(operator, true);
        fakePm.setOperator(operator, true);
        assertTrue(realPm.isOperator(address(this), operator), "real operator granted");
        assertTrue(fakePm.isOperator(address(this), operator), "fake operator granted");

        realPm.setOperator(operator, false);
        fakePm.setOperator(operator, false);
        assertFalse(realPm.isOperator(address(this), operator), "real operator revoked");
        assertFalse(fakePm.isOperator(address(this), operator), "fake operator revoked");
    }

    // ══════════════════════════════════════════════════════════════════════
    // swap
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestSwap.test_swap_exact_input_sign_convention`.
    ///
    /// Exact-input (`amountSpecified < 0`) means the SOLD currency's delta is
    /// exactly `amountSpecified` and the bought currency's delta is positive, on
    /// a real pool that computes its own output and on the fake with a
    /// pre-configured one. The manager's own `Swap` event carries the same
    /// amounts it returned.
    function testSwapExactInputSignConvention() public {
        (int256 realSold, int256 realBought) = _realSwapLeg();
        (int256 fakeSold, int256 fakeBought) = _fakeSwapLeg();

        assertEq(realSold, -int256(SWAP_AMOUNT_IN), "real exact-input sold delta");
        assertEq(fakeSold, -int256(SWAP_AMOUNT_IN), "fake exact-input sold delta");
        assertGt(uint256(realBought), 0, "real manager quoted a nonzero output");
        assertEq(fakeBought, int256(SWAP_AMOUNT_IN / 2), "fake paid its configured output");
    }

    /// Port of `TestSwap.test_swap_zero_amount_reverts`.
    ///
    /// Outside unlock the lock gate answers first: a zero-amount swap and a
    /// non-zero one produce the SAME lock error on both managers, so the
    /// amount check the source case described is unreachable in that state.
    /// The executed zero-amount refusal lives in
    /// `testSwapZeroAmountRevertsInsideUnlock`.
    function testSwapZeroAmountReverts() public {
        bytes memory zeroSwap = abi.encodeCall(
            IPoolManager.swap, (key, SwapParams(true, int256(0), MIN_SQRT_PRICE_PLUS_ONE), bytes(""))
        );
        bytes memory liveSwap = abi.encodeCall(
            IPoolManager.swap, (key, SwapParams(true, -int256(1), MIN_SQRT_PRICE_PLUS_ONE), bytes(""))
        );

        (bool realZeroOk, bytes memory realZeroData) = _callExpectRevert(REAL_PM, zeroSwap);
        (bool realLiveOk, bytes memory realLiveData) = _callExpectRevert(REAL_PM, liveSwap);
        assertFalse(realZeroOk, "real manager refuses a zero-amount swap outside unlock");
        assertEq(realZeroData, _customError("ManagerLocked()"), "real zero-amount refusal is the lock gate");
        assertEq(realZeroData, realLiveData, "the amount is never inspected while locked");

        (bool fakeZeroOk, bytes memory fakeZeroData) = _callExpectRevert(address(fakePm), zeroSwap);
        (bool fakeLiveOk, bytes memory fakeLiveData) = _callExpectRevert(address(fakePm), liveSwap);
        assertFalse(fakeZeroOk, "fake manager refuses a zero-amount swap outside unlock");
        assertEq(fakeZeroData, _fakeError("ManagerLocked"), "fake zero-amount refusal is the lock gate");
        assertEq(fakeZeroData, fakeLiveData, "the fake never inspects the amount while locked");
        assertFalse(realLiveOk, "the non-zero swap is refused while locked");
        assertFalse(fakeLiveOk, "the fake refuses the non-zero swap while locked");
    }

    /// The source case's stated intention, executed: with the lock held, a
    /// zero-amount swap is refused by the amount guard on both managers.
    function testSwapZeroAmountRevertsInsideUnlock() public {
        (bytes memory realOut) =
            _call(REAL_PM, abi.encodeCall(IPoolManager.unlock, (_request(REAL_PM, MODE_SWAP_ZERO, 0, address(0)))));
        (bytes memory fakeOut) = _call(
            address(fakePm),
            abi.encodeCall(
                IFakeUniswapV4PoolManager.unlock, (_request(address(fakePm), MODE_SWAP_ZERO, 0, address(0)))
            )
        );

        (bool realOk, bytes memory realData) = abi.decode(_payload(realOut), (bool, bytes));
        (bool fakeOk, bytes memory fakeData) = abi.decode(_payload(fakeOut), (bool, bytes));

        assertFalse(realOk, "real swap refuses a zero amount");
        assertEq(realData, _customError("SwapAmountCannotBeZero()"), "real refusal identity");
        assertFalse(fakeOk, "fake swap refuses a zero amount");
        assertEq(fakeData, _fakeError("SwapAmountCannotBeZero"), "fake refusal identity");
    }

    // ══════════════════════════════════════════════════════════════════════
    // modifyLiquidity
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestModifyLiquidity.test_modify_liquidity_requires_unlock`.
    function testModifyLiquidityRequiresUnlock() public {
        realPm.initialize(key, SQRT_PRICE_1_1);
        fakePm.initialize(key, SQRT_PRICE_1_1);

        _assertLockedBoth(
            abi.encodeCall(
                IPoolManager.modifyLiquidity,
                (key, ModifyLiquidityParams(-LP_TICK_RANGE, LP_TICK_RANGE, LP_LIQUIDITY, bytes32(0)), bytes(""))
            ),
            "modifyLiquidity on an initialized pool"
        );
    }

    /// Port of `TestModifyLiquidity.test_modify_liquidity_requires_initialized_pool`.
    ///
    /// The source could only reach the lock gate; holding the lock makes the real
    /// refusal reachable on both managers: an uninitialized pool is
    /// `PoolNotInitialized`.
    function testModifyLiquidityRequiresInitializedPool() public {
        ModifyLiquidityParams memory params =
            ModifyLiquidityParams(-LP_TICK_RANGE, LP_TICK_RANGE, LP_LIQUIDITY, bytes32(0));

        _assertLockedBoth(
            abi.encodeCall(IPoolManager.modifyLiquidity, (uninitializedKey, params, bytes(""))),
            "modifyLiquidity on an uninitialized pool outside unlock"
        );

        lpLower = params.tick_lower;
        lpUpper = params.tick_upper;

        (bytes memory realOut) = _call(
            REAL_PM,
            abi.encodeCall(IPoolManager.unlock, (_request(REAL_PM, MODE_MODIFY_LIQUIDITY_PROBE, 0, address(0))))
        );
        (bytes memory fakeOut) = _call(
            address(fakePm),
            abi.encodeCall(
                IFakeUniswapV4PoolManager.unlock,
                (_request(address(fakePm), MODE_MODIFY_LIQUIDITY_PROBE, 0, address(0)))
            )
        );

        (bool realOk, bytes memory realData) = abi.decode(_payload(realOut), (bool, bytes));
        (bool fakeOk, bytes memory fakeData) = abi.decode(_payload(fakeOut), (bool, bytes));
        assertFalse(realOk, "real modifyLiquidity refuses an uninitialized pool");
        assertEq(realData, _customError("PoolNotInitialized()"), "real refusal identity");
        assertFalse(fakeOk, "fake modifyLiquidity refuses an uninitialized pool");
        assertEq(fakeData, _fakeError("PoolNotInitialized"), "fake refusal identity");
    }

    // ══════════════════════════════════════════════════════════════════════
    // donate
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestDonate.test_donate_requires_unlock`.
    function testDonateRequiresUnlock() public {
        _assertLockedBoth(
            abi.encodeCall(IPoolManager.donate, (key, uint256(100), uint256(200), bytes(""))), "donate"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // updateDynamicLPFee
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestUpdateDynamicLPFee.test_update_dynamic_lp_fee_only_hook`.
    ///
    /// A static-fee pool has no dynamic fee to update: both managers refuse the
    /// call for any caller.
    function testUpdateDynamicLpFeeOnlyHook() public {
        realPm.initialize(key, SQRT_PRICE_1_1);
        fakePm.initialize(key, SQRT_PRICE_1_1);

        (bool realOk, bytes memory realData) =
            _callExpectRevert(REAL_PM, abi.encodeCall(IPoolManager.updateDynamicLPFee, (key, uint24(500))));
        assertFalse(realOk, "real manager refuses a static-fee pool");
        assertEq(realData, _customError("UnauthorizedDynamicLPFeeUpdate()"), "real refusal identity");

        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(
            address(fakePm),
            abi.encodeCall(IFakeUniswapV4PoolManager.updateDynamicLPFee, (key, uint24(500)))
        );
        assertFalse(fakeOk, "fake manager refuses a static-fee pool");
        assertEq(fakeData, _fakeError("UnauthorizedDynamicLPFeeUpdate"), "fake refusal identity");
    }

    /// Port of `TestUpdateDynamicLPFee.test_update_dynamic_lp_fee_works_outside_unlock`.
    ///
    /// The dynamic-fee update is not lock-gated: outside unlock, a caller that
    /// is not the pool's hook is refused by the HOOK gate (not by the lock), on
    /// both managers. The hook caller's accepted update is
    /// `testUpdateDynamicLpFeeHookCallerUpdatesFee`.
    function testUpdateDynamicLpFeeWorksOutsideUnlock() public {
        realPm.initialize(dynamicKey, SQRT_PRICE_1_1);
        fakePm.initialize(dynamicKey, SQRT_PRICE_1_1);
        assertEq(realPm.exttload(IS_UNLOCKED_SLOT), bytes32(0), "the real manager is not unlocked");

        address impostor = address(uint160(0xdead));
        vm.prank(impostor);
        (bool realOk, bytes memory realData) =
            _callExpectRevert(REAL_PM, abi.encodeCall(IPoolManager.updateDynamicLPFee, (dynamicKey, uint24(500))));
        assertFalse(realOk, "real manager refuses a non-hook caller");
        assertEq(realData, _customError("UnauthorizedDynamicLPFeeUpdate()"), "real refusal is the hook gate, not the lock");

        vm.prank(impostor);
        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(
            address(fakePm),
            abi.encodeCall(IFakeUniswapV4PoolManager.updateDynamicLPFee, (dynamicKey, uint24(500)))
        );
        assertFalse(fakeOk, "fake manager refuses a non-hook caller");
        assertEq(fakeData, _fakeError("UnauthorizedDynamicLPFeeUpdate"), "fake refusal is the hook gate, not the lock");
    }

    /// The intention the Ape case could not execute (it could not impersonate
    /// the hook): the hook caller updates a dynamic pool's LP fee outside unlock
    /// on both managers, and the stored fee is the new one — the real manager's
    /// slot0 and the fake's dynamic-fee mapping agree.
    function testUpdateDynamicLpFeeHookCallerUpdatesFee() public {
        realPm.initialize(dynamicKey, SQRT_PRICE_1_1);
        fakePm.initialize(dynamicKey, SQRT_PRICE_1_1);

        bytes32 poolId = _poolId(dynamicKey);
        assertEq(_lpFeeOf(realPm.extsload(_poolStateSlot(poolId))), 0, "a dynamic pool starts at LP fee 0");
        assertEq(uint256(fakePm.dynamic_lp_fees(poolId)), 0, "the fake starts a dynamic pool at LP fee 0");

        uint24 newFee = 500;
        vm.prank(DYNAMIC_FEE_HOOK);
        realPm.updateDynamicLPFee(dynamicKey, newFee);
        vm.prank(DYNAMIC_FEE_HOOK);
        fakePm.updateDynamicLPFee(dynamicKey, newFee);

        assertEq(_lpFeeOf(realPm.extsload(_poolStateSlot(poolId))), uint256(newFee), "real slot0 carries the new LP fee");
        assertEq(uint256(fakePm.dynamic_lp_fees(poolId)), uint256(newFee), "fake stored the new LP fee");
    }

    // ══════════════════════════════════════════════════════════════════════
    // take
    // ══════════════════════════════════════════════════════════════════════

    /// Port of `TestTake.test_take_native_transfers_eth`.
    ///
    /// A native `take` delivers ETH: the comparator's round trip proves the
    /// delta, and the direct leg pins the delivered balance mid-callback — the
    /// recipient's balance rises by exactly the amount before the settle returns
    /// it.
    function testTakeNativeTransfersEth() public {
        assertGe(REAL_PM.balance, ONE_ETH, "the real manager holds native ETH at the pinned block");
        compareRoundTrip();

        _assertTakeParity(_takeLeg(address(realPm)), _takeLeg(address(fakePm)), ONE_ETH);
    }

    /// Port of `TestTake.test_take_requires_unlock`.
    function testTakeRequiresUnlock() public {
        _assertLockedBoth(abi.encodeCall(IPoolManager.take, (NATIVE, address(this), uint256(1))), "take");
    }

    // ══════════════════════════════════════════════════════════════════════
    // Unlock callback: the harness plays the router role
    // ══════════════════════════════════════════════════════════════════════

    function unlockCallback(bytes calldata data) external returns (bytes memory) {
        (address pm, uint8 mode, uint256 amount, address account) =
            abi.decode(data, (address, uint8, uint256, address));
        require(msg.sender == pm, "only the manager under test may call back");

        if (mode == MODE_EMPTY) return "";

        if (mode == MODE_TAKE_NATIVE) {
            uint256 before = account.balance;
            IPoolManager(pm).take(NATIVE, account, amount);
            int256 delta = _delta(pm, account, NATIVE);
            uint256 lock = uint256(IPoolManager(pm).exttload(IS_UNLOCKED_SLOT));
            uint256 count = _nonzeroDeltaCount(pm);
            uint256 delivered = account.balance - before;
            uint256 paid = IPoolManager(pm).settle{value: amount}();
            return abi.encode(delta, lock, count, delivered, paid, _delta(pm, account, NATIVE), _nonzeroDeltaCount(pm));
        }

        if (mode == MODE_TAKE_WITHOUT_SETTLE) {
            IPoolManager(pm).take(NATIVE, account, amount);
            return "";
        }

        if (mode == MODE_NESTED_UNLOCK) {
            (bool ok, bytes memory revertData) =
                pm.call(abi.encodeCall(IPoolManager.unlock, (bytes(""))));
            return abi.encode(ok, revertData);
        }

        if (mode == MODE_SETTLE_FOR) {
            uint256 before = account.balance;
            IPoolManager(pm).settleFor{value: amount}(account);
            int256 recipientDelta = _delta(pm, account, NATIVE);
            int256 callerDelta = _delta(pm, address(this), NATIVE);
            DeltaRecipient(payable(account)).takeNative(pm, amount);
            return abi.encode(
                recipientDelta, callerDelta, _nonzeroDeltaCount(pm), account.balance - before, _delta(pm, account, NATIVE)
            );
        }

        if (mode == MODE_MINT_CLAIMS) {
            IPoolManager(pm).mint(account, uint256(uint160(NATIVE)), amount);
            int256 delta = _delta(pm, address(this), NATIVE);
            uint256 count = _nonzeroDeltaCount(pm);
            uint256 paid = IPoolManager(pm).settle{value: amount}();
            return abi.encode(
                delta, count, paid, IPoolManager(pm).balanceOf(account, uint256(uint160(NATIVE))),
                _delta(pm, address(this), NATIVE), _nonzeroDeltaCount(pm)
            );
        }

        if (mode == MODE_BURN_CLAIMS) {
            IPoolManager(pm).burn(account, uint256(uint160(NATIVE)), amount);
            int256 credit = _delta(pm, address(this), NATIVE);
            IPoolManager(pm).take(NATIVE, account, amount);
            return abi.encode(
                credit, _delta(pm, address(this), NATIVE), _nonzeroDeltaCount(pm),
                IPoolManager(pm).balanceOf(account, uint256(uint160(NATIVE)))
            );
        }

        if (mode == MODE_CLEAR_EXACT_DELTA) {
            IPoolManager(pm).settle{value: amount}();
            int256 before = _delta(pm, address(this), NATIVE);
            uint256 countBefore = _nonzeroDeltaCount(pm);
            (bool wrongOk, bytes memory wrongRevert) =
                pm.call(abi.encodeCall(IPoolManager.clear, (NATIVE, amount - 1)));
            IPoolManager(pm).clear(NATIVE, amount);
            return abi.encode(
                before, countBefore, wrongOk, wrongRevert, _delta(pm, address(this), NATIVE), _nonzeroDeltaCount(pm)
            );
        }

        if (mode == MODE_SYNC_ERC20) {
            IPoolManager(pm).sync(account);
            bytes32 syncedCurrency = IPoolManager(pm).exttload(SYNCED_CURRENCY_SLOT);
            uint256 syncedReserves = uint256(IPoolManager(pm).exttload(SYNCED_RESERVES_SLOT));
            uint256 balanceAtSync = IFakeErc20(account).balanceOf(pm);
            IFakeErc20(account).transfer(pm, amount);
            uint256 paid = IPoolManager(pm).settle();
            bytes32 currencyAfter = IPoolManager(pm).exttload(SYNCED_CURRENCY_SLOT);
            bytes32 reservesAfter = IPoolManager(pm).exttload(SYNCED_RESERVES_SLOT);
            // settle credits the measured transfer, so the credit is spent by
            // taking it back out; a settle alone leaves a delta open.
            IPoolManager(pm).take(account, address(this), paid);
            return abi.encode(
                syncedCurrency, syncedReserves, balanceAtSync, paid, currencyAfter, reservesAfter,
                _delta(pm, address(this), account), _nonzeroDeltaCount(pm)
            );
        }

        if (mode == MODE_SWAP_ZERO) {
            (bool ok, bytes memory revertData) = pm.call(
                abi.encodeCall(
                    IPoolManager.swap, (key, SwapParams(true, int256(0), MIN_SQRT_PRICE_PLUS_ONE), bytes(""))
                )
            );
            return abi.encode(ok, revertData);
        }

        if (mode == MODE_SWAP_EXACT_IN) {
            int256 swapDelta =
                IPoolManager(pm).swap(key, SwapParams(true, -int256(amount), MIN_SQRT_PRICE_PLUS_ONE), bytes(""));
            int256 sold = _amount0(swapDelta);
            int256 bought = _amount1(swapDelta);
            _payDebit(pm, key.currency0, sold);
            IPoolManager(pm).take(key.currency1, address(this), uint256(bought));
            return abi.encode(sold, bought, _nonzeroDeltaCount(pm));
        }

        if (mode == MODE_ADD_LIQUIDITY) {
            (int256 callerDelta,) = IPoolManager(pm).modifyLiquidity(
                key, ModifyLiquidityParams(lpLower, lpUpper, LP_LIQUIDITY, bytes32(0)), bytes("")
            );
            _payDebit(pm, key.currency0, _amount0(callerDelta));
            _payDebit(pm, key.currency1, _amount1(callerDelta));
            return abi.encode(callerDelta, _nonzeroDeltaCount(pm));
        }

        if (mode == MODE_MODIFY_LIQUIDITY_PROBE) {
            (bool ok, bytes memory revertData) = pm.call(
                abi.encodeCall(
                    IPoolManager.modifyLiquidity,
                    (
                        uninitializedKey,
                        ModifyLiquidityParams(lpLower, lpUpper, LP_LIQUIDITY, bytes32(0)),
                        bytes("")
                    )
                )
            );
            return abi.encode(ok, revertData);
        }

        revert("unexpected callback mode");
    }

    // ══════════════════════════════════════════════════════════════════════
    // Helpers
    // ══════════════════════════════════════════════════════════════════════

    function compareRoundTrip() internal {
        assertTrue(comparator.compare(), "comparator take + sync + settle");
        assertEq(comparator.real_exttload_value(), comparator.fake_exttload_value(), "take delta parity");
        assertEq(comparator.s_real_delta(), comparator.s_fake_delta(), "recorded delta parity");
        assertEq(int256(uint256(comparator.s_real_delta())), -int256(ONE_ETH), "real take delta is -1 ETH");
    }

    function _clearLeg(address pm) internal returns (ClearObservation memory o) {
        bytes memory out = _payload(_call(
            pm, abi.encodeCall(IPoolManager.unlock, (_request(pm, MODE_CLEAR_EXACT_DELTA, ONE_ETH, address(0))))
        ));
        (o.credited, o.countBefore, o.mismatchedOk, o.mismatchedData, o.deltaAfter, o.countAfter) =
            abi.decode(out, (int256, uint256, bool, bytes, int256, uint256));
    }

    function _assertClearParity(ClearObservation memory real, ClearObservation memory fake) internal pure {
        assertEq(real.credited, int256(ONE_ETH), "real settled credit is exactly +1 ETH");
        assertEq(fake.credited, int256(ONE_ETH), "fake settled credit is exactly +1 ETH");
        assertEq(real.countBefore, 1, "real count sees the credit");
        assertEq(fake.countBefore, 1, "fake count sees the credit");
        assertFalse(real.mismatchedOk, "real clear refuses a non-matching amount");
        assertEq(real.mismatchedData, _customError("MustClearExactPositiveDelta()"), "real refusal identity");
        assertFalse(fake.mismatchedOk, "fake clear refuses a non-matching amount");
        assertEq(fake.mismatchedData, _fakeError("MustClearExactPositiveDelta"), "fake refusal identity");
        assertEq(real.deltaAfter, int256(0), "real exact clear zeroes the delta");
        assertEq(fake.deltaAfter, int256(0), "fake exact clear zeroes the delta");
        assertEq(real.countAfter, 0, "real count cleared");
        assertEq(fake.countAfter, 0, "fake count cleared");
    }

    function _mintLeg(address pm, uint256 amount) internal returns (MintObservation memory o) {
        bytes memory out = _payload(_call(
            pm, abi.encodeCall(IPoolManager.unlock, (_request(pm, MODE_MINT_CLAIMS, amount, address(this))))
        ));
        (o.delta, o.count, o.paid, o.claims, o.deltaAfter, o.countAfter) =
            abi.decode(out, (int256, uint256, uint256, uint256, int256, uint256));
    }

    function _assertMintParity(MintObservation memory real, MintObservation memory fake, uint256 amount)
        internal
        pure
    {
        assertEq(real.delta, -int256(amount), "real mint debits the caller's delta");
        assertEq(fake.delta, -int256(amount), "fake mint debits the caller's delta");
        assertEq(real.count, 1, "real mint leaves one nonzero delta");
        assertEq(fake.count, 1, "fake mint leaves one nonzero delta");
        assertEq(real.paid, amount, "real settle measured the debit");
        assertEq(fake.paid, amount, "fake settle measured the debit");
        assertEq(real.claims, amount, "real claims credited");
        assertEq(fake.claims, amount, "fake claims credited");
        assertEq(real.deltaAfter, int256(0), "real delta settled");
        assertEq(fake.deltaAfter, int256(0), "fake delta settled");
        assertEq(real.countAfter, 0, "real count cleared");
        assertEq(fake.countAfter, 0, "fake count cleared");
    }

    function _burnLeg(address pm, uint256 amount) internal returns (BurnObservation memory o) {
        bytes memory out = _payload(_call(
            pm, abi.encodeCall(IPoolManager.unlock, (_request(pm, MODE_BURN_CLAIMS, amount, address(this))))
        ));
        (o.credit, o.deltaAfter, o.count, o.claims) = abi.decode(out, (int256, int256, uint256, uint256));
    }

    function _assertBurnParity(BurnObservation memory real, BurnObservation memory fake, uint256 amount)
        internal
        pure
    {
        assertEq(real.credit, int256(amount), "real burn credits the caller's delta");
        assertEq(fake.credit, int256(amount), "fake burn credits the caller's delta");
        assertEq(real.deltaAfter, int256(0), "real take spent the credit");
        assertEq(fake.deltaAfter, int256(0), "fake take spent the credit");
        assertEq(real.count, 0, "real count cleared");
        assertEq(fake.count, 0, "fake count cleared");
        assertEq(real.claims, 0, "real claims destroyed");
        assertEq(fake.claims, 0, "fake claims destroyed");
    }

    function _takeLeg(address pm) internal returns (TakeObservation memory o) {
        bytes memory out = _payload(_call(
            pm, abi.encodeCall(IPoolManager.unlock, (_request(pm, MODE_TAKE_NATIVE, ONE_ETH, address(this))))
        ));
        (o.delta, o.lock, o.count, o.delivered, o.paid, o.deltaAfter, o.countAfter) =
            abi.decode(out, (int256, uint256, uint256, uint256, uint256, int256, uint256));
    }

    function _assertTakeParity(TakeObservation memory real, TakeObservation memory fake, uint256 amount)
        internal
        pure
    {
        assertEq(real.delta, -int256(amount), "real take debits the caller by the delivered amount");
        assertEq(fake.delta, -int256(amount), "fake take debits the caller by the delivered amount");
        assertEq(real.lock, 1, "real manager locked inside the callback");
        assertEq(fake.lock, 1, "fake manager locked inside the callback");
        assertEq(real.count, 1, "real nonzero delta count after take");
        assertEq(fake.count, 1, "fake nonzero delta count after take");
        assertEq(real.delivered, amount, "real take delivered ETH to the recipient");
        assertEq(fake.delivered, amount, "fake take delivered ETH to the recipient");
        assertEq(real.paid, amount, "real settle measured the returned ETH");
        assertEq(fake.paid, amount, "fake settle measured the returned ETH");
        assertEq(real.deltaAfter, int256(0), "real delta settled");
        assertEq(fake.deltaAfter, int256(0), "fake delta settled");
        assertEq(real.countAfter, 0, "real count cleared");
        assertEq(fake.countAfter, 0, "fake count cleared");
    }

    function _reservesLeg(address pm, uint256 amount) internal returns (ReservesObservation memory o) {
        bytes memory out = _payload(_call(
            pm, abi.encodeCall(IPoolManager.unlock, (_request(pm, MODE_SYNC_ERC20, amount, address(tokenA))))
        ));
        (o.currency, o.reserves, o.balanceAtSync, o.paid, o.currencyAfter, o.reservesAfter, o.deltaAfter, o.countAfter)
        = abi.decode(out, (bytes32, uint256, uint256, uint256, bytes32, bytes32, int256, uint256));
    }

    function _assertReservesParity(
        ReservesObservation memory real,
        ReservesObservation memory fake,
        address currency,
        uint256 amount
    ) internal pure {
        assertEq(real.currency, bytes32(uint256(uint160(currency))), "real sync stored the currency");
        assertEq(fake.currency, bytes32(uint256(uint160(currency))), "fake sync stored the currency");
        assertEq(real.reserves, real.balanceAtSync, "real sync snapshot is the manager's balance");
        assertEq(fake.reserves, fake.balanceAtSync, "fake sync snapshot is the manager's balance");
        assertEq(real.balanceAtSync, fake.balanceAtSync, "both managers held the same inventory at sync");
        assertEq(real.paid, amount, "real settle measured exactly the transfer");
        assertEq(fake.paid, amount, "fake settle measured exactly the transfer");
        assertEq(real.currencyAfter, bytes32(0), "real settle reset the currency slot");
        assertEq(fake.currencyAfter, bytes32(0), "fake settle reset the currency slot");
        // resetCurrency clears the synced currency only; the reserves slot keeps
        // the pre-transfer snapshot on both managers.
        assertEq(real.reservesAfter, bytes32(real.balanceAtSync), "real reserves slot keeps the snapshot");
        assertEq(fake.reservesAfter, bytes32(fake.balanceAtSync), "fake reserves slot keeps the snapshot");
        assertEq(real.deltaAfter, int256(0), "real delta settled");
        assertEq(fake.deltaAfter, int256(0), "fake delta settled");
        assertEq(real.countAfter, 0, "real count cleared");
        assertEq(fake.countAfter, 0, "fake count cleared");
    }

    function _settleForLeg(address pm) internal returns (SettleForObservation memory o) {
        bytes memory out = _payload(_call(
            pm, abi.encodeCall(IPoolManager.unlock, (_request(pm, MODE_SETTLE_FOR, ONE_ETH, address(recipient))))
        ));
        (o.recipient, o.caller, o.count, o.received, o.deltaAfter) =
            abi.decode(out, (int256, int256, uint256, uint256, int256));
    }

    function _assertSettleForParity(SettleForObservation memory real, SettleForObservation memory fake, uint256 amount)
        internal
        pure
    {
        assertEq(real.recipient, int256(amount), "real settleFor credits the named account");
        assertEq(fake.recipient, int256(amount), "fake settleFor credits the named account");
        assertEq(real.caller, int256(0), "real settleFor leaves the caller uncredited");
        assertEq(fake.caller, int256(0), "fake settleFor leaves the caller uncredited");
        assertEq(real.received, amount, "real delegate received the settled ETH");
        assertEq(fake.received, amount, "fake delegate received the settled ETH");
        assertEq(real.deltaAfter, int256(0), "real delegate's delta cleared by its own take");
        assertEq(fake.deltaAfter, int256(0), "fake delegate's delta cleared by its own take");
        assertEq(real.count, 0, "real count cleared");
        assertEq(fake.count, 0, "fake count cleared");
    }

    /// Real swap leg: initialize, fund a position, swap exact-input against real
    /// math, and require the manager's own `Swap` event to agree with the delta
    /// it returned.
    function _realSwapLeg() internal returns (int256 sold, int256 bought) {
        realPm.initialize(key, SQRT_PRICE_1_1);
        lpLower = -LP_TICK_RANGE;
        lpUpper = LP_TICK_RANGE;
        _unlock(address(realPm), MODE_ADD_LIQUIDITY, 0, address(0));

        tokenA.mint(address(this), SWAP_AMOUNT_IN);
        tokenB.mint(address(this), SWAP_AMOUNT_IN);

        vm.recordLogs();
        bytes memory out = _payload(_call(
            REAL_PM,
            abi.encodeCall(IPoolManager.unlock, (_request(REAL_PM, MODE_SWAP_EXACT_IN, SWAP_AMOUNT_IN, address(0))))
        ));
        Vm.Log[] memory logs = vm.getRecordedLogs();

        uint256 count;
        (sold, bought, count) = abi.decode(out, (int256, int256, uint256));
        assertEq(count, 0, "real deltas settled");

        (int128 event0, int128 event1) = _swapEvent(logs, REAL_PM);
        assertEq(int256(event0), sold, "real Swap event amount0 matches the return");
        assertEq(int256(event1), bought, "real Swap event amount1 matches the return");
    }

    /// Fake swap leg: the same call shape against a pre-configured swap.
    function _fakeSwapLeg() internal returns (int256 sold, int256 bought) {
        fakePm.initialize(key, SQRT_PRICE_1_1);
        fakePm.set_next_swap(key, SWAP_AMOUNT_IN, SWAP_AMOUNT_IN / 2, true, "");

        vm.recordLogs();
        bytes memory out = _payload(_call(
            address(fakePm),
            abi.encodeCall(
                IFakeUniswapV4PoolManager.unlock,
                (_request(address(fakePm), MODE_SWAP_EXACT_IN, SWAP_AMOUNT_IN, address(0)))
            )
        ));
        Vm.Log[] memory logs = vm.getRecordedLogs();

        uint256 count;
        (sold, bought, count) = abi.decode(out, (int256, int256, uint256));
        assertEq(count, 0, "fake deltas settled");

        (int128 event0, int128 event1) = _swapEvent(logs, address(fakePm));
        assertEq(int256(event0), sold, "fake Swap event amount0 matches the return");
        assertEq(int256(event1), bought, "fake Swap event amount1 matches the return");
    }

    /// Pay a negative delta the integrator way — sync, transfer, settle — and
    /// require the amount `settle()` measured to be the amount owed.
    function _payDebit(address pm, address currency, int256 delta) internal {
        require(delta <= 0, "expected a debit to pay");
        if (delta == 0) return;
        uint256 owed = uint256(-delta);
        IPoolManager(pm).sync(currency);
        IFakeErc20(currency).transfer(pm, owed);
        uint256 paid = IPoolManager(pm).settle();
        require(paid == owed, "settle() did not measure the transfer");
    }

    function _request(address pm, uint8 mode, uint256 amount, address account)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(pm, mode, amount, account);
    }

    /// `unlock` returns `bytes`, so a low-level call's raw returndata wraps the
    /// callback's payload in one more ABI layer.
    function _payload(bytes memory raw) internal pure returns (bytes memory) {
        return abi.decode(raw, (bytes));
    }

    function _unlock(address pm, uint8 mode, uint256 amount, address account) internal returns (bytes memory) {
        return IPoolManager(pm).unlock(_request(pm, mode, amount, account));
    }

    /// The same calldata against both managers: each refuses with its own
    /// ManagerLocked identity.
    function _assertLockedBoth(bytes memory callData, string memory label) internal {
        (bool realOk, bytes memory realData) = _callExpectRevert(REAL_PM, callData);
        assertFalse(realOk, string.concat(label, ": real manager accepted a locked action"));
        assertEq(realData, _customError("ManagerLocked()"), string.concat(label, ": real ManagerLocked identity"));

        (bool fakeOk, bytes memory fakeData) = _callExpectRevert(address(fakePm), callData);
        assertFalse(fakeOk, string.concat(label, ": fake manager accepted a locked action"));
        assertEq(fakeData, _fakeError("ManagerLocked"), string.concat(label, ": fake ManagerLocked identity"));
    }

    /// CurrencyDelta._computeSlot(target, currency): keccak256 of the two
    /// right-aligned addresses.
    function _deltaSlot(address account, address currency) internal pure returns (bytes32) {
        return keccak256(abi.encode(account, currency));
    }

    function _delta(address pm, address account, address currency) internal view returns (int256) {
        return int256(uint256(IPoolManager(pm).exttload(_deltaSlot(account, currency))));
    }

    function _nonzeroDeltaCount(address pm) internal view returns (uint256) {
        return uint256(IPoolManager(pm).exttload(NONZERO_DELTA_COUNT_SLOT));
    }

    function _poolId(PoolKey memory poolKey) internal pure returns (bytes32) {
        return keccak256(abi.encode(poolKey));
    }

    /// StateLibrary._getPoolStateSlot: keccak256(poolId ++ POOLS_SLOT).
    function _poolStateSlot(bytes32 poolId) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(poolId, POOLS_SLOT));
    }

    /// Slot0 packs the price in the low 160 bits and the LP fee at bit 208.
    function _sqrtPriceOf(bytes32 slot0) internal pure returns (uint160) {
        return uint160(uint256(slot0));
    }

    function _lpFeeOf(bytes32 slot0) internal pure returns (uint256) {
        return uint256(uint24(uint256(slot0) >> 208));
    }

    /// BalanceDelta packing: amount0 in the upper 128 bits, amount1 in the lower
    /// 128, both signed.
    function _amount0(int256 delta) internal pure returns (int256) {
        return delta >> 128;
    }

    function _amount1(int256 delta) internal pure returns (int256) {
        return int256(int128(uint128(uint256(delta))));
    }

    function _sorted(address a, address b) internal pure returns (address, address) {
        return uint160(a) < uint160(b) ? (a, b) : (b, a);
    }

    function _swapEvent(Vm.Log[] memory logs, address pm)
        internal
        pure
        returns (int128 amount0, int128 amount1)
    {
        uint256 hits;
        bytes32 topic = keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)");
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != pm || logs[i].topics.length != 3 || logs[i].topics[0] != topic) {
                continue;
            }
            (amount0, amount1) = abi.decode(logs[i].data, (int128, int128));
            hits += 1;
        }
        require(hits == 1, "exactly one manager Swap event");
    }

    // ── Exact revert identities ──────────────────────────────────────────

    function _customError(string memory signature) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(bytes4(keccak256(bytes(signature))));
    }

    function _customErrorWithAddresses(string memory signature, address a, address b)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodeWithSelector(bytes4(keccak256(bytes(signature))), a, b);
    }

    function _customErrorWithInt24(string memory signature, int24 value)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodeWithSelector(bytes4(keccak256(bytes(signature))), value);
    }

    function _fakeError(string memory message) internal pure returns (bytes memory) {
        return abi.encodeWithSignature("Error(string)", message);
    }
}

/// A recipient that clears a `settleFor` credit by taking it, the way a second
/// integrator contract would. It is the only actor that can zero its own delta.
contract DeltaRecipient {
    receive() external payable {}

    function takeNative(address pm, uint256 amount) external {
        IPoolManager(pm).take(address(0), address(this), amount);
    }
}

/// A caller with no `unlockCallback`: both managers refuse to open an unlock for
/// a contract that cannot receive the callback.
contract NonCallbackCaller {
    function callUnlock(address pm, bytes memory data) external returns (bool ok, bytes memory revertData) {
        (ok, revertData) = pm.call(abi.encodeCall(IPoolManager.unlock, (data)));
    }
}
