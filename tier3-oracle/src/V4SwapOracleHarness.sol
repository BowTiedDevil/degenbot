// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

import {PoolManager} from "v4-core/src/PoolManager.sol";
import {IPoolManager} from "v4-core/src/interfaces/IPoolManager.sol";
import {IUnlockCallback} from "v4-core/src/interfaces/callback/IUnlockCallback.sol";
import {PoolKey} from "v4-core/src/types/PoolKey.sol";
import {BalanceDelta, toBalanceDelta} from "v4-core/src/types/BalanceDelta.sol";
import {IHooks} from "v4-core/src/interfaces/IHooks.sol";
import {Currency, CurrencyLibrary} from "v4-core/src/types/Currency.sol";

/// Minimal ERC20 with `balances` at storage slot 0 so the unlock/settle dance
/// can transfer real token balances into/out of the PoolManager (`_settle`
/// reads `balanceOfSelf` delta to compute the paid amount).
contract MockERC20V4 {
    mapping(address => uint256) public balanceOf; // slot 0

    function transfer(address to, uint256 amount) external returns (bool) {
        uint256 b = balanceOf[msg.sender];
        require(b >= amount, "mock: insufficient balance");
        balanceOf[msg.sender] = b - amount;
        balanceOf[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 b = balanceOf[from];
        require(b >= amount, "mock: insufficient balance");
        balanceOf[from] = b - amount;
        balanceOf[to] += amount;
        return true;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }
}

/// Tier-3b end-to-end V4 `PoolManager.swap` oracle harness (ergo task 2LTKVO,
/// epic UP5NH6). Deploys the canonical `PoolManager` from v4-core as real
/// bytecode, exposes a `swap` entry that wraps the unlock→swap→settle dance,
/// and exposes the pool key + token references so the Rust `#[test]` seeds the
/// pool's storage slots (see `degenbot_pools::v4_storage_slots`) directly.
/// Settlement satisfies `NonzeroDeltaCount == 0` (else `CurrencyNotSettled`
/// reverts) by settling the negative (input) delta with a real token transfer
/// into the manager and taking the positive (output) delta out.
contract V4SwapOracleHarness is IUnlockCallback {
    IPoolManager public manager;
    MockERC20V4 public currency0;
    MockERC20V4 public currency1;
    uint24 public fee;
    int24 public tickSpacing;

    PoolKey private activeKey;

    event Swap(
        bytes32 indexed id,
        address indexed sender,
        int128 amount0,
        int128 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick,
        uint24 fee
    );

    constructor(uint24 _fee, int24 _tickSpacing) {
        fee = _fee;
        tickSpacing = _tickSpacing;
        currency0 = new MockERC20V4();
        currency1 = new MockERC20V4();
        // Deploy the canonical PoolManager at the singleton-invoked address.
        manager = new PoolManager(address(this));
        // Pre-fund this harness so the negative-delta settle transfer succeeds.
        currency0.mint(address(this), type(uint256).max / 4);
        currency1.mint(address(this), type(uint256).max / 4);
        // Pre-fund the PoolManager so the positive-delta `take` (which transfers
        // the output token OUT of the manager to the recipient) has a balance.
        currency0.mint(address(manager), type(uint256).max / 4);
        currency1.mint(address(manager), type(uint256).max / 4);
    }

    /// The pool key for the SINGLE pool this oracle drives (both currencies,
    /// `_fee`, `_tickSpacing`, no hooks).
    function key() external view returns (PoolKey memory) {
        return PoolKey({
            currency0: Currency.wrap(address(currency0)),
            currency1: Currency.wrap(address(currency1)),
            fee: fee,
            tickSpacing: tickSpacing,
            hooks: IHooks(address(0))
        });
    }

    /// The `_pools` mapping base slot (top-level slot 6) constant, reused by
    /// the Rust seed layer to derive `S_state` from the pool id.
    function poolsMappingSlot() external pure returns (uint256) {
        return 6;
    }

    /// Drives `PoolManager.unlock` -> `unlockCallback` -> `swap` -> settle.
    /// `data` ABI-encodes `(bool zeroForOne, int256 amountSpecified,
    /// uint160 sqrtPriceLimitX96)`. Returns the raw Swap-event-ish result via
    /// the unlock return only on success; the Rust side decodes the emitted
    /// `Swap` event for the authoritative byte-exact values.
    function swap(bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)
        external
        returns (BalanceDelta)
    {
        activeKey = PoolKey({
            currency0: Currency.wrap(address(currency0)),
            currency1: Currency.wrap(address(currency1)),
            fee: fee,
            tickSpacing: tickSpacing,
            hooks: IHooks(address(0))
        });
        bytes memory cbData = abi.encode(
            activeKey,
            IPoolManager.SwapParams({
                zeroForOne: zeroForOne,
                amountSpecified: amountSpecified,
                sqrtPriceLimitX96: sqrtPriceLimitX96
            })
        );
        return abi.decode(manager.unlock(cbData), (BalanceDelta));
    }

    /// PoolManager calls back here (msg.sender == manager) during `unlock`.
    function unlockCallback(bytes calldata rawData) external returns (bytes memory) {
        require(msg.sender == address(manager), "not manager");
        (PoolKey memory key_, IPoolManager.SwapParams memory params) =
            abi.decode(rawData, (PoolKey, IPoolManager.SwapParams));

        BalanceDelta delta = manager.swap(key_, params, "");

        // zeroForOne: input = currency0 (negative delta), output = currency1
        // (positive delta). settle the negative, take the positive.
        if (params.zeroForOne) {
            if (delta.amount0() < 0) {
                _settleIn(currency0, uint256(int256(delta.amount0()) * -1));
            }
            if (delta.amount1() > 0) {
                _takeOut(currency1, uint256(int256(delta.amount1())));
            }
        } else {
            if (delta.amount1() < 0) {
                _settleIn(currency1, uint256(int256(delta.amount1()) * -1));
            }
            if (delta.amount0() > 0) {
                _takeOut(currency0, uint256(int256(delta.amount0())));
            }
        }

        return abi.encode(delta);
    }

    function _settleIn(MockERC20V4 token, uint256 amount) internal {
        // sync() captures reservesBefore=0 FIRST, then the transfer raises the
        // manager's balance, and settle() computes paid = now - before.
        manager.sync(Currency.wrap(address(token)));
        token.transfer(address(manager), amount);
        manager.settle();
    }

    function _takeOut(MockERC20V4 token, uint256 amount) internal {
        manager.take(Currency.wrap(address(token)), address(this), amount);
    }
}
