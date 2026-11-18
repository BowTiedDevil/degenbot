// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {Vm} from "../src/Vm.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {IFakeUniswapV4PoolManager} from "../src/generated/IFakeUniswapV4PoolManager.sol";
import {IUniswapV2Factory} from "../src/generated/IUniswapV2Factory.sol";
import {IUniswapV2Pair} from "../src/generated/IUniswapV2Pair.sol";
import {IUniswapV3Factory} from "../src/generated/IUniswapV3Factory.sol";
import {IUniswapV3Pool} from "../src/generated/IUniswapV3Pool.sol";
import {ModifyLiquidityParams, PoolKey} from "../src/generated/Types.sol";

/// The one surface a shared unlock callback needs: the fake and the canonical
/// PoolManager expose the same ABI for every call made inside the callback, so
/// one callback body drives either manager and the comparison is
/// emission-to-emission rather than declaration-to-declaration.
interface IV4ManagerLike {
    function unlock(bytes calldata data) external returns (bytes memory);

    function modifyLiquidity(
        PoolKey calldata key,
        ModifyLiquidityParams calldata params,
        bytes calldata hookData
    ) external returns (int256, int256);

    function donate(PoolKey calldata key, uint256 amount0, uint256 amount1, bytes calldata hookData)
        external
        returns (int256);

    function mint(address to, uint256 id, uint256 amount) external;

    function sync(address currency) external;

    function settle() external payable returns (uint256);
}

/// Event-topic fidelity: the fake Vyper pools log the SAME topic0 the canonical
/// Uniswap contracts emit for the same operation.
///
/// The lane's fakes stand in for Uniswap's own bytecode, and every log-counting
/// assertion in the lane (`_countTransfers`, `_countTopic`, the ERC6909 transfer
/// counts) is keyed on topic0. A fake that declared a field with the wrong width
/// would silently change the topic and make those counts read zero while the
/// suite stayed green — so each topic is checked against a real emission from
/// the canonical artifact (`UniswapV2Pair` solc 0.5.16, `UniswapV3Pool` solc
/// 0.7.6, the v4-core `PoolManager` solc 0.8.26) built by
/// `build_solidity_artifacts.py`.
///
/// Recorded divergences, not asserted-as-equal:
///   - The fake V3 pool emits `Swap` only; canonical's `Mint`/`Burn`/`Collect`/
///     `Flash` surface is not modelled by the fake (the fake's liquidity is a
///     test-programmed number, not positions).
///   - `erc6909TransferOnMint` executes the fake mint for real and pins the
///     canonical DECLARED signature, because the canonical manager's `mint` is
///     lock-gated and needs a settled delta; the shared claims flow below
///     exercises the settlement on the fake only.
contract EventTopicFidelityTest is PoolSetup {
    // ── Canonical signatures (what a topic0 must be) ──────────────────────

    bytes32 internal constant FIDELITY_V2_SYNC = keccak256("Sync(uint112,uint112)");
    bytes32 internal constant FIDELITY_V2_SWAP =
        keccak256("Swap(address,uint256,uint256,uint256,uint256,address)");
    bytes32 internal constant FIDELITY_V3_SWAP =
        keccak256("Swap(address,address,int256,int256,uint160,uint128,int24)");
    bytes32 internal constant FIDELITY_V4_INITIALIZE =
        keccak256("Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)");
    bytes32 internal constant FIDELITY_V4_MODIFY_LIQUIDITY =
        keccak256("ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)");
    bytes32 internal constant FIDELITY_V4_DONATE =
        keccak256("Donate(bytes32,address,uint256,uint256)");
    bytes32 internal constant FIDELITY_V4_SWAP =
        keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)");
    bytes32 internal constant FIDELITY_ERC6909_TRANSFER =
        keccak256("Transfer(address,address,address,uint256,uint256)");
    bytes32 internal constant FIDELITY_ERC6909_APPROVAL =
        keccak256("Approval(address,address,uint256,uint256)");
    bytes32 internal constant FIDELITY_ERC6909_OPERATOR = keccak256("OperatorSet(address,address,bool)");

    // ── Shared unlock callback ────────────────────────────────────────────

    uint8 internal constant MODE_MODIFY_LIQUIDITY = 1;
    uint8 internal constant MODE_DONATE = 2;
    uint8 internal constant MODE_CLAIMS = 3;

    uint256 internal constant CLAIM_AMOUNT = 1e6;

    uint8 internal unlockMode;
    PoolKey internal unlockKey;
    ModifyLiquidityParams internal unlockParams;

    function unlockCallback(bytes calldata) external returns (bytes memory) {
        if (unlockMode == MODE_MODIFY_LIQUIDITY) {
            IV4ManagerLike manager = IV4ManagerLike(msg.sender);
            (int256 callerDelta, int256 feesAccrued) =
                manager.modifyLiquidity(unlockKey, unlockParams, "");
            // The canonical manager accounts the position's principal as a debt
            // and refuses an unsettled delta at unlock end, so pay whatever the
            // returned delta says is owed. The fake accounts pre-configured
            // amounts (zero here), so the same code pays nothing on it.
            _settleDebt(manager, unlockKey.currency0, int128(callerDelta >> 128));
            _settleDebt(manager, unlockKey.currency1, int128(callerDelta));
            return abi.encode(callerDelta, feesAccrued);
        }
        if (unlockMode == MODE_DONATE) {
            int256 delta = IV4ManagerLike(msg.sender).donate(unlockKey, 0, 0, "");
            return abi.encode(delta);
        }
        // MODE_CLAIMS: mint ERC6909 claims against a delta, then pay it. Both
        // managers are lock-gated here and both refuse an unsettled delta at
        // unlock end, so the debt is settled inside the same callback.
        uint256 id = uint256(uint160(address(usdc)));
        IV4ManagerLike manager = IV4ManagerLike(msg.sender);
        manager.mint(address(this), id, CLAIM_AMOUNT);
        manager.sync(address(usdc));
        usdc.transfer(address(manager), CLAIM_AMOUNT);
        manager.settle();
        return "";
    }

    /// Pay a negative (owed) delta for one currency: snapshot the manager's
    /// balance, transfer the owed amount in, then let `settle` measure it.
    function _settleDebt(IV4ManagerLike manager, address currency, int128 amount) internal {
        if (amount >= 0) return;
        manager.sync(currency);
        IFakeErc20(currency).transfer(address(manager), uint256(-int256(amount)));
        manager.settle();
    }

    /// Seed a real in-range position on `manager` through the shared callback,
    /// and leave the manager with no open delta.
    function _seedLiquidity(
        IV4ManagerLike manager,
        PoolKey memory key,
        int24 tickLower,
        int24 tickUpper
    ) internal {
        unlockKey = key;
        unlockParams = ModifyLiquidityParams({
            tick_lower: tickLower,
            tick_upper: tickUpper,
            liquidity_delta: 1e18,
            salt: bytes32(0)
        });
        unlockMode = MODE_MODIFY_LIQUIDITY;
        usdc.mint(address(this), 1e30);
        weth.mint(address(this), 1e30);
        manager.unlock("");
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Assert `emitter` logged `expected` exactly once and return it, so a
    /// declaration drift shows up as "not emitted" instead of a vacuous compare.
    function _emittedCount(
        Vm.Log[] memory logs,
        address emitter,
        bytes32 expected,
        uint256 expectedCount,
        string memory what
    ) internal pure returns (bytes32) {
        uint256 n;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter != emitter) continue;
            if (logs[i].topics[0] != expected) continue;
            ++n;
        }
        assertEq(n, expectedCount, string.concat(what, ": wrong emission count"));
        return expected;
    }

    function _emitted(Vm.Log[] memory logs, address emitter, bytes32 expected, string memory what)
        internal
        pure
        returns (bytes32)
    {
        return _emittedCount(logs, emitter, expected, 1, what);
    }

    function _canonicalV2Pair() internal returns (IUniswapV2Pair pair) {
        IUniswapV2Factory factory = IUniswapV2Factory(_deploy("UniswapV2Factory", abi.encode(address(this))));
        pair = IUniswapV2Pair(factory.createPair(address(usdc), address(weth)));
    }

    function _fundPair(address pair) internal {
        usdc.mint(pair, AMOUNT_USDC * 100);
        weth.mint(pair, AMOUNT_WETH * 100);
    }

    /// Sell `AMOUNT_USDC` for WETH: transfer the input in, then swap with no
    /// callback data (the canonical pair's caller-funded form).
    function _swapPair(address pair, bool token0IsUsdc) internal {
        (uint112 r0, uint112 r1,) = IUniswapV2Pair(pair).getReserves();
        uint256 amountOut = _v2GetAmountOut(
            AMOUNT_USDC, token0IsUsdc ? r0 : r1, token0IsUsdc ? r1 : r0, 30
        );
        usdc.transfer(pair, AMOUNT_USDC);
        if (token0IsUsdc) {
            IUniswapV2Pair(pair).swap(0, amountOut, address(this), "");
        } else {
            IUniswapV2Pair(pair).swap(amountOut, 0, address(this), "");
        }
    }

    function _isqrtPriceForV3(bool token0IsUsdc) internal pure returns (uint160) {
        return uint160(
            _isqrt(
                token0IsUsdc
                    ? (ONE_WETH * Q96 * Q96) / 2_000e6
                    : (2_000e6 * Q96 * Q96) / ONE_WETH
            )
        );
    }

    uint256 internal constant ONE_WETH = 1 ether;

    /// The canonical V3 pool's own mint callback (liquidity provision).
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

    /// Drive a V3 swap through the executor's auto-pay path (empty forward
    /// data), which is the flow both the fake and the canonical pool support.
    function _executorV3Swap(address pool, bool zfo, uint256 amountIn)
        internal
        returns (Vm.Log[] memory logs)
    {
        usdc.mint(address(executor), amountIn);
        Cmd.Table memory at = _emptyTable();
        uint8 poolIdx = Cmd.add(at, pool);
        uint8 executorIdx = Cmd.add(at, address(executor));
        bytes memory commands = Cmd.v3SwapCompact(poolIdx, zfo, amountIn, executorIdx, "");
        vm.recordLogs();
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);
        return vm.getRecordedLogs();
    }

    // ── V2 pair: Sync and Swap ────────────────────────────────────────────

    function testV2SyncTopicMatchesCanonical() public {
        IFakeUniswapV2Pair fake = _deployV2Pair(address(usdc), address(weth), 0, 30);
        IUniswapV2Pair canonical = _canonicalV2Pair();
        _fundPair(address(fake));
        _fundPair(address(canonical));

        vm.recordLogs();
        fake.sync();
        IUniswapV2Pair(address(canonical)).sync();
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(fake), FIDELITY_V2_SYNC, "fake pair Sync");
        _emitted(logs, address(canonical), FIDELITY_V2_SYNC, "canonical pair Sync");
    }

    function testV2SwapTopicMatchesCanonical() public {
        IFakeUniswapV2Pair fake = _deployV2Pair(address(usdc), address(weth), 0, 30);
        IUniswapV2Pair canonical = _canonicalV2Pair();
        _fundPair(address(fake));
        _fundPair(address(canonical));
        fake.sync();
        canonical.sync();

        vm.recordLogs();
        _swapPair(address(fake), fake.token0() == address(usdc));
        _swapPair(address(canonical), canonical.token0() == address(usdc));
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(fake), FIDELITY_V2_SWAP, "fake pair Swap");
        _emitted(logs, address(canonical), FIDELITY_V2_SWAP, "canonical pair Swap");
    }

    // ── V3 pool: Swap ─────────────────────────────────────────────────────

    function testV3SwapTopicMatchesCanonical() public {
        // The fake pool: initialize + programmed liquidity + executor swap.
        IFakeUniswapV3Pool fake =
            _deployV3Pool(address(usdc), address(weth), 0, 3000);
        (bool fakeZfo,) = _setupV3(
            fake, IFakeErc20(address(usdc)), IFakeErc20(address(weth)), AMOUNT_USDC, AMOUNT_WETH
        );
        uint256 amountIn = 200e6;
        Vm.Log[] memory fakeLogs = _executorV3Swap(address(fake), fakeZfo, amountIn);
        _emitted(fakeLogs, address(fake), FIDELITY_V3_SWAP, "fake V3 pool Swap");

        // The canonical pool: real factory, real tick liquidity, real swap.
        IUniswapV3Factory factory = IUniswapV3Factory(_deploy("UniswapV3Factory", ""));
        IUniswapV3Pool canonical =
            IUniswapV3Pool(factory.createPool(address(usdc), address(weth), uint24(3000)));
        bool zfo = canonical.token0() == address(usdc);
        canonical.initialize(_isqrtPriceForV3(zfo));
        (uint160 sp, int24 tick,,,,,) = canonical.slot0();
        assertGt(uint256(sp), 0, "canonical pool priced");
        int24 spacing = canonical.tickSpacing();
        int24 base = (tick / spacing) * spacing;
        usdc.mint(address(this), 1e30);
        weth.mint(address(this), 1e30);
        canonical.mint(address(this), base - 600, base + 600, uint128(1e18), "");

        Vm.Log[] memory canonicalLogs = _executorV3Swap(address(canonical), zfo, amountIn);
        _emitted(canonicalLogs, address(canonical), FIDELITY_V3_SWAP, "canonical V3 pool Swap");
    }

    // ── V4 PoolManager: Initialize, ModifyLiquidity, Donate ───────────────

    PoolKey internal v4Key;

    function _prepareV4Key() internal returns (PoolKey memory) {
        v4Key = _poolKey(address(usdc), address(weth), 3000, 60);
        return v4Key;
    }

    function testV4InitializeTopicMatchesCanonical() public {
        PoolKey memory key = _prepareV4Key();
        IPoolManagerLike canonical = IPoolManagerLike(_deploy("PoolManager", abi.encode(address(this))));

        vm.recordLogs();
        v4Pm.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));
        canonical.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(v4Pm), FIDELITY_V4_INITIALIZE, "fake manager Initialize");
        _emitted(
            logs,
            address(canonical),
            FIDELITY_V4_INITIALIZE,
            "canonical manager Initialize"
        );
    }

    function testV4ModifyLiquidityTopicMatchesCanonical() public {
        PoolKey memory key = _prepareV4Key();
        IPoolManagerLike canonical = IPoolManagerLike(_deploy("PoolManager", abi.encode(address(this))));
        v4Pm.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));
        int24 tick0 = canonical.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));

        // A real in-range position on both managers; the canonical manager
        // refuses a zero-delta update of an empty position
        // (`CannotUpdateEmptyPosition`), so the compared call carries principal
        // and the shared callback settles it. The manager's `initialize` return
        // is the tick it priced at, which is what the range must straddle.
        int24 spacing = key.tick_spacing;
        int24 base = (tick0 / spacing) * spacing;
        (int24 tickLower, int24 tickUpper) = (base - spacing, base + spacing);
        vm.recordLogs();
        _seedLiquidity(IV4ManagerLike(address(v4Pm)), key, tickLower, tickUpper);
        _seedLiquidity(IV4ManagerLike(address(canonical)), key, tickLower, tickUpper);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(v4Pm), FIDELITY_V4_MODIFY_LIQUIDITY, "fake manager ModifyLiquidity");
        _emitted(
            logs,
            address(canonical),
            FIDELITY_V4_MODIFY_LIQUIDITY,
            "canonical manager ModifyLiquidity"
        );
    }

    function testV4DonateTopicMatchesCanonical() public {
        PoolKey memory key = _prepareV4Key();
        IPoolManagerLike canonical = IPoolManagerLike(_deploy("PoolManager", abi.encode(address(this))));
        v4Pm.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));
        int24 tick0 = canonical.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));

        // The canonical manager refuses a donation to a pool with no liquidity
        // (`NoLiquidityToReceiveFees`), so both managers get a real position
        // first. Zero amounts then account a zero delta on both sides.
        int24 spacing = key.tick_spacing;
        int24 base = (tick0 / spacing) * spacing;
        (int24 tickLower, int24 tickUpper) = (base - spacing, base + spacing);
        _seedLiquidity(IV4ManagerLike(address(v4Pm)), key, tickLower, tickUpper);
        _seedLiquidity(IV4ManagerLike(address(canonical)), key, tickLower, tickUpper);
        unlockKey = key;
        unlockMode = MODE_DONATE;

        vm.recordLogs();
        IV4ManagerLike(address(v4Pm)).unlock("");
        IV4ManagerLike(address(canonical)).unlock("");
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(v4Pm), FIDELITY_V4_DONATE, "fake manager Donate");
        _emitted(logs, address(canonical), FIDELITY_V4_DONATE, "canonical manager Donate");
    }

    // ── V4 ERC6909 claims surface ─────────────────────────────────────────

    function testV4Erc6909ApprovalAndOperatorTopicsMatchCanonical() public {
        IPoolManagerLike canonical = IPoolManagerLike(_deploy("PoolManager", abi.encode(address(this))));
        uint256 id = uint256(uint160(address(usdc)));

        vm.recordLogs();
        v4Pm.approve(address(this), id, CLAIM_AMOUNT);
        v4Pm.setOperator(address(this), true);
        canonical.approve(address(this), id, CLAIM_AMOUNT);
        canonical.setOperator(address(this), true);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(v4Pm), FIDELITY_ERC6909_APPROVAL, "fake manager Approval");
        _emitted(logs, address(canonical), FIDELITY_ERC6909_APPROVAL, "canonical manager Approval");
        _emitted(logs, address(v4Pm), FIDELITY_ERC6909_OPERATOR, "fake manager OperatorSet");
        _emitted(logs, address(canonical), FIDELITY_ERC6909_OPERATOR, "canonical manager OperatorSet");
    }

    /// The mint `Transfer` shape: the fake emits it for real (mint against a
    /// delta, settled inside the same unlock), and the emitted topic0 is pinned
    /// to the canonical declaration. The canonical manager's own mint is
    /// lock-and-settlement gated, so this case proves the fake's emission
    /// rather than re-driving the canonical path the fork lane already covers.
    function testV4Erc6909MintTransferTopicMatchesCanonicalDeclaration() public {
        PoolKey memory key = _prepareV4Key();
        v4Pm.initialize(key, _isqrtPriceForV3(key.currency0 == address(usdc)));
        unlockKey = key;
        unlockMode = MODE_CLAIMS;

        vm.recordLogs();
        IV4ManagerLike(address(v4Pm)).unlock("");
        Vm.Log[] memory logs = vm.getRecordedLogs();

        _emitted(logs, address(v4Pm), FIDELITY_ERC6909_TRANSFER, "fake manager mint Transfer");
        assertEq(
            v4Pm.balanceOf(address(this), uint256(uint160(address(usdc)))),
            CLAIM_AMOUNT,
            "the claim landed on the fake manager"
        );
    }

    // ── V4 swap: the fake's emission against the canonical signature ──────

    /// A single-pool export/import round trip whose two swaps net the
    /// PoolManager deltas to zero (the lane's proven V4_SWAP_COMPACT pair), so
    /// the unlock succeeds and the fake manager logs its `Swap`.
    function testFakeV4SwapTopicMatchesCanonicalSignature() public {
        PoolKey memory keyA = _poolKey(address(weth), address(usdc), 3000, 60);
        bool zfoA = keyA.currency0 == address(weth);
        _setupV4Swap(v4Pm, keyA, AMOUNT_WETH, AMOUNT_USDC, zfoA, address(usdc));
        PoolKey memory keyB = _poolKey(address(weth), address(usdc), 3000, 120);
        bool zfoB = keyB.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyB, AMOUNT_USDC, 2 * AMOUNT_WETH, zfoB, address(weth));

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, keyA.currency0),
            Cmd.add(at, keyA.currency1),
            uint16(keyA.fee),
            int16(keyA.tick_spacing),
            zeroIdx,
            zfoA,
            AMOUNT_WETH
        );
        inner = bytes.concat(
            inner,
            Cmd.v4SwapCompact(
                Cmd.add(at, keyB.currency0),
                Cmd.add(at, keyB.currency1),
                uint16(keyB.fee),
                int16(keyB.tick_spacing),
                zeroIdx,
                zfoB,
                AMOUNT_USDC
            )
        );
        inner = bytes.concat(inner, Cmd.v4TakeDelta(wethIdx, executorIdx));

        vm.recordLogs();
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        // Two legs through the same fake manager, so exactly two Swap logs —
        // both carrying the canonical signature.
        _emittedCount(logs, address(v4Pm), FIDELITY_V4_SWAP, 2, "fake manager Swap");
    }
}

/// Minimal canonical-manager surface for the Initialize/claims comparisons:
/// the v4-core `PoolManager` exposes these outside the lock.
interface IPoolManagerLike {
    function initialize(PoolKey calldata key, uint160 sqrtPriceX96) external returns (int24);

    function approve(address spender, uint256 id, uint256 amount) external returns (bool);

    function setOperator(address operator, bool approved) external returns (bool);
}
