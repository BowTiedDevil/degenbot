// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./StubToken.sol";

// ── Shared V3/V4 price math (copied verbatim from v3-core; V4 exact-input uses
//    the same SqrtPriceMath family). Duplicated here so the V4 stub is
//    self-contained (the V3 stub keeps its own copy). ──

library FullMath {
    function mulDiv(uint256 a, uint256 b, uint256 denominator)
        internal pure returns (uint256 result)
    {
        unchecked {
        uint256 prod0;
        uint256 prod1;
        assembly {
            let mm := mulmod(a, b, not(0))
            prod0 := mul(a, b)
            prod1 := sub(sub(mm, prod0), lt(mm, prod0))
        }
        if (prod1 == 0) {
            require(denominator > 0);
            assembly { result := div(prod0, denominator) }
            return result;
        }
        require(denominator > prod1);
        uint256 remainder;
        assembly {
            remainder := mulmod(a, b, denominator)
            prod1 := sub(prod1, gt(remainder, prod0))
            prod0 := sub(prod0, remainder)
        }
        uint256 twos = denominator & (~denominator + 1);
        assembly { denominator := div(denominator, twos) }
        assembly { prod0 := div(prod0, twos) }
        assembly { twos := add(div(sub(0, twos), twos), 1) }
        prod0 |= prod1 * twos;
        uint256 inv = (3 * denominator) ^ 2;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        inv *= 2 - denominator * inv;
        result = prod0 * inv;
        return result;
        }
    }

    function mulDivRoundingUp(uint256 a, uint256 b, uint256 denominator)
        internal pure returns (uint256 result)
    {
        result = mulDiv(a, b, denominator);
        if (mulmod(a, b, denominator) > 0) {
            require(result < type(uint256).max);
            result++;
        }
    }
}

library FixedPoint96 { uint8 internal constant RESOLUTION = 96; uint256 internal constant Q96 = 0x1000000000000000000000000; }

library UnsafeMath {
    function divRoundingUp(uint256 x, uint256 y) internal pure returns (uint256 result) {
        assembly { result := add(div(x, y), gt(mod(x, y), 0)) }
    }
}

library SqrtPriceMath {
    function getNextSqrtPriceFromAmount0RoundingUp(
        uint160 sqrtPX96, uint128 liquidity, uint256 amount, bool add
    ) internal pure returns (uint160) {
        if (amount == 0) return sqrtPX96;
        uint256 numerator1 = uint256(liquidity) << FixedPoint96.RESOLUTION;
        if (add) {
            uint256 product;
            if ((product = amount * sqrtPX96) / amount == sqrtPX96) {
                uint256 denominator = numerator1 + product;
                if (denominator >= numerator1)
                    return uint160(FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator));
            }
            return uint160(UnsafeMath.divRoundingUp(numerator1, (numerator1 / sqrtPX96) + amount));
        } else {
            uint256 product;
            require((product = amount * sqrtPX96) / amount == sqrtPX96 && numerator1 > product);
            uint256 denominator = numerator1 - product;
            return uint160(FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator));
        }
    }
    function getNextSqrtPriceFromAmount1RoundingDown(
        uint160 sqrtPX96, uint128 liquidity, uint256 amount, bool add
    ) internal pure returns (uint160) {
        if (add) {
            uint256 quotient = amount <= type(uint160).max
                ? (amount << FixedPoint96.RESOLUTION) / liquidity
                : FullMath.mulDiv(amount, FixedPoint96.Q96, liquidity);
            return uint160(uint256(sqrtPX96) + quotient);
        } else {
            uint256 quotient = amount <= type(uint160).max
                ? UnsafeMath.divRoundingUp(amount << FixedPoint96.RESOLUTION, liquidity)
                : FullMath.mulDivRoundingUp(amount, FixedPoint96.Q96, liquidity);
            require(sqrtPX96 > quotient);
            return uint160(sqrtPX96 - quotient);
        }
    }
    function getNextSqrtPriceFromInput(uint160 sqrtPX96, uint128 liquidity, uint256 amountIn, bool zeroForOne)
        internal pure returns (uint160)
    {
        require(sqrtPX96 > 0); require(liquidity > 0);
        return zeroForOne
            ? getNextSqrtPriceFromAmount0RoundingUp(sqrtPX96, liquidity, amountIn, true)
            : getNextSqrtPriceFromAmount1RoundingDown(sqrtPX96, liquidity, amountIn, true);
    }
    function getAmount0Delta(uint160 sa, uint160 sb, uint128 liquidity, bool roundUp)
        internal pure returns (uint256 amount0)
    {
        if (sa > sb) (sa, sb) = (sb, sa);
        uint256 numerator1 = uint256(liquidity) << FixedPoint96.RESOLUTION;
        uint256 numerator2 = sb - sa;
        require(sa > 0);
        return roundUp
            ? UnsafeMath.divRoundingUp(FullMath.mulDivRoundingUp(numerator1, numerator2, sb), sa)
            : FullMath.mulDiv(numerator1, numerator2, sb) / sa;
    }
    function getAmount1Delta(uint160 sa, uint160 sb, uint128 liquidity, bool roundUp)
        internal pure returns (uint256 amount1)
    {
        if (sa > sb) (sa, sb) = (sb, sa);
        return roundUp
            ? FullMath.mulDivRoundingUp(liquidity, sb - sa, FixedPoint96.Q96)
            : FullMath.mulDiv(liquidity, sb - sa, FixedPoint96.Q96);
    }
}

// ── Minimal V4 PoolManager, executor-faithful (UQOAHA) ──
//
// Implements the narrow surface the `cmd_executor` actually drives
// (`_cmd_v4_swap_compact`/`take`/`settle`/`sync`/`unlock` + `exttload` for
// `_read_pm_delta`/`_auto_settle_touched`), with the v4-core transient-delta
// slot layout `keccak256(abi.encodePacked(target, currency))` so the executor's
// precomputed slots line up. Single-tick-range exact-input swap math shared
// with V3. `take`/`settle` transfer the executor's PM deltas to/from the PM's
// own token balances (seed-funded like the V2/V3 pools).

contract PoolManager {
    address public constant NATIVE = address(0);
    uint24 public constant MAX_SWAP_FEE = 1_000_000; // 100%

    // Per-pool state keyed by keccak256(poolKey). We keep one liquidity + one
    // sqrt price per (c0,c1,fee,ts) pool; initialize() seeds it.
    uint160 public constant MIN_SQRT = 4295128739;

    struct PoolState { uint160 sqrtPriceX96; uint128 liquidity; bool init; }

    // currency -> balance held by the PM (delivered on take).
    mapping(address => uint256) public tokenBalance;

    // Transient delta slots (tstore/tload) keyed like v4-core CurrencyDelta.
    bool private _unlocked;
    address private _pendingSettle;

    constructor() {}

    // ── pool key → storage ──
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }
    struct SwapParams { bool zeroForOne; int256 amountSpecified; uint160 sqrtPriceLimitX96; }

    mapping(bytes32 => PoolState) private _pools;

    function _poolKeyHash(PoolKey memory k) private pure returns (bytes32) {
        return keccak256(abi.encode(k.currency0, k.currency1, k.fee, k.tickSpacing, k.hooks));
    }

    event PoolInitialized(bytes32 indexed key, uint160 sqrtPriceX96, uint128 liquidity);
    event V4Swap(bytes32 indexed key, address inputCurrency, address outputCurrency, uint256 amountIn, uint256 amountOut);
    function initialize(address c0, address c1, uint24 fee, int24 ts, uint160 sqrtPriceX96, uint128 liquidity) external {
        PoolKey memory k = PoolKey(c0, c1, fee, ts, address(0));
        bytes32 h = _poolKeyHash(k);
        _pools[h] = PoolState(sqrtPriceX96, liquidity, true);
        // fund PM with both currencies for future take()
        emit PoolInitialized(h, sqrtPriceX96, liquidity);
    }
    function setPrice(address c0, address c1, uint24 fee, int24 ts, uint160 sqrtPriceX96) external {
        bytes32 h = _poolKeyHash(PoolKey(c0, c1, fee, ts, address(0)));
        PoolState storage s = _pools[h];
        s.sqrtPriceX96 = sqrtPriceX96; s.init = true;
    }
    function setLiquidity(address c0, address c1, uint24 fee, int24 ts, uint128 liquidity) external {
        bytes32 h = _poolKeyHash(PoolKey(c0, c1, fee, ts, address(0)));
        PoolState storage s = _pools[h];
        s.liquidity = liquidity; s.init = true;
    }

    function _fund(address currency, uint256 amt) external {
        if (currency == NATIVE) {
            tokenBalance[currency] += amt; // nb: native funding via balanceOf is not real; value funded by caller
        } else {
            Token(payable(currency)).mint(address(this), amt);
            tokenBalance[currency] += amt;
        }
    }

    // ── delta bookkeeping (v4-core CurrencyDelta slot) ──
    function _slot(address target, address currency) private pure returns (bytes32) {
        // v4-core CurrencyDelta slot: keccak256(abi.encodePacked(target, currency)) with
        // BOTH left-padded to 32 bytes (matching the executor's Vyper
        // keccak256(concat(convert(self,bytes32), convert(currency,bytes32)))).
        return keccak256(
            abi.encodePacked(uint256(uint160(target)), uint256(uint160(currency)))
        );
    }
    function _accountDelta(address target, address currency, int256 delta) private {
        bytes32 s = _slot(target, currency);
        int256 cur;
        assembly { cur := tload(s) }
        int256 next = cur + delta;
        assembly { tstore(s, next) }
    }
    function exttload(bytes32 s) external view returns (bytes32 v) {
        assembly { v := tload(s) }
    }
    function getDelta(address target, address currency) external view returns (int256) {
        bytes32 s = _slot(target, currency);
        bytes32 v; assembly { v := tload(s) }
        return int256(uint256(v));
    }

    // ── the executor's command surface ──
    function sync(address currency) external {
        // Executor always calls sync() immediately before an ERC20 settle();
        // record which currency so the next settle() knows what to zero.
        _pendingSettle = currency;
    }

    // Matches v4-core main: `function settle() external payable returns (uint256 paid)`.
    // The cmd_executor extcall frame was recompiled for the uint256 return
    // (b9cb64e50), so the stub must return the settled amount or the frame
    // reverts on the expected-return-size assert.
    function settle() external payable returns (uint256 paid) {
        if (msg.value > 0) {
            // Native settle: the caller has physically transferred `msg.value`
            // into the PM; CREDIT the caller's native delta by +value (reducing
            // a negative debt to zero). Matches v4-core `_settle` / the
            // executor's proven fake PM (`_account_delta(account, NATIVE, +paid)`).
            _accountDelta(msg.sender, NATIVE, int256(uint256(msg.value)));
            paid = msg.value;
        } else {
            address c = _pendingSettle;
            bytes32 s = _slot(msg.sender, c);
            int256 cur; assembly { cur := tload(s) }
            if (cur < 0) {
                paid = uint256(-cur);
                assembly { tstore(s, 0) }
            }
            _pendingSettle = address(0);
        }
    }

    function take(address currency, address recipient, uint256 amount) external {
        bytes32 s = _slot(msg.sender, currency);
        int256 cur; assembly { cur := tload(s) }
        require(cur > 0, "D0"); // can only take an OWED (positive) delta
        uint256 takeAmt = amount < uint256(cur) ? amount : uint256(cur);
        require(tokenBalance[currency] >= takeAmt, "PM:B"); // seed-funded
        tokenBalance[currency] -= takeAmt;
        if (currency == NATIVE) {
            (bool ok, ) = recipient.call{value: takeAmt}("");
            require(ok, "PM:ETH");
        } else {
            Token(payable(currency)).transfer(recipient, takeAmt);
        }
        if (takeAmt == uint256(cur)) { assembly { tstore(s, 0) } }
        else {
            int256 newCur = cur - int256(uint256(takeAmt));
            assembly { tstore(s, newCur) }
        }
    }

    function unlock(bytes calldata data) external returns (bytes memory) {
        require(!_unlocked, "ULOCK");
        _unlocked = true;
        (bool ok, bytes memory ret) = msg.sender.call(abi.encodeWithSignature("unlockCallback(bytes)", data));
        if (!ok) {
            _unlocked = false;
            assembly { revert(add(ret, 32), mload(ret)) }
        }
        // v4-core `_checkDelta`: no nonzero deltas may remain for the executor.
        // (settle_all inside zeroed them; if one remains, fail loudly.)
        address exec = msg.sender;
        address[] memory touched = _touched[exec];
        for (uint256 i = 0; i < touched.length; i++) {
            address c = touched[i];
            bytes32 s = _slot(exec, c);
            int256 d; assembly { d := tload(s) }
            require(d == 0, "DELTA");
        }
        delete _touched[exec];
        _touchedLen[exec] = 0;
        _unlocked = false;
        return ret;
    }

    // registry of currencies a caller accrued deltas on (for the post-unlock check)
    mapping(address => address[]) internal _touched;
    function _noteDelta(address exec, address currency) private {
        if (_touchedLen[exec] == 0 || _touched[exec][_touchedLen[exec] - 1] != currency) {
            _touched[exec].push(currency);
            _touchedLen[exec]++;
        }
    }
    mapping(address => uint256) internal _touchedLen;

    // ── ERC6909 internal balances (V4_MINT_COMPACT / V4_BURN_COMPACT / check_mode=2) ──
    // v4-core ERC6909: `id = uint160(currency)`. `balanceOf[owner][id]` is the
    // PM-held claim; minting converts a positive caller delta into a claim,
    // burning converts a claim back into a payable caller delta. Enables the
    // executor's 0x58/0x59 commands and `check_mode=2` profit measurement for
    // the gas-saving pure-V4 paths (EYUWFG).
    mapping(address => mapping(uint256 => uint256)) public balanceOf;

    /// Convert a positive caller PM delta into an ERC6909 claim for `to`.
    /// Mirrors v4-core `mint`: requires the caller to be owed `currency`
    /// (D0 credit-before-debit), decrements the caller's delta, and credits
    /// `to`'s ERC6909 balance. No physical transfer — the asset stays inside
    /// the PM as an accounting entry.
    function mint(address to, uint256 id, uint256 amount) external {
        address currency = address(uint160(id));
        bytes32 s = _slot(msg.sender, currency);
        int256 cur; assembly { cur := tload(s) }
        require(cur > 0, "D0"); // credit-before-debit
        uint256 taken = amount < uint256(cur) ? amount : uint256(cur);
        int256 newCur = cur - int256(uint256(taken));
        assembly { tstore(s, newCur) }
        balanceOf[to][id] += taken;
    }

    /// Convert `from`'s ERC6909 claim into a payable PM delta for the caller.
    /// Mirrors v4-core `burn` (the executor always burns its OWN claim, so
    /// `from` == `msg.sender` in practice): decrements the claim balance and
    /// credits the caller's delta, retrievable via a later `take`.
    function burn(address from, uint256 id, uint256 amount) external {
        address currency = address(uint160(id));
        require(balanceOf[from][id] >= amount, "BAL");
        balanceOf[from][id] -= amount;
        _accountDelta(msg.sender, currency, int256(uint256(amount)));
        _noteDelta(msg.sender, currency);
    }

    /// Exact-input swap. Returns (int256 delta0, int256 delta1).
    function swap(PoolKey calldata key, SwapParams calldata params, bytes calldata /*data*/)
        external returns (int256, int256)
    {
        bytes32 h = _poolKeyHash(key);
        PoolState storage st = _pools[h];
        require(st.init, "NP");
        require(params.amountSpecified < 0, "EXOUT"); // harness drives exact-input

        uint160 current = st.sqrtPriceX96;
        uint128 liq = st.liquidity;
        uint256 amountIn = uint256(-params.amountSpecified);
        uint24 fee = key.fee;
        uint256 amountLessFee = FullMath.mulDiv(amountIn, 1_000_000 - fee, 1_000_000);
        bool zfo = params.zeroForOne;

        // input / output currencies for this direction
        address inputCurrency = zfo ? key.currency0 : key.currency1;
        address outputCurrency = zfo ? key.currency1 : key.currency0;

        uint160 next;
        uint256 inUsed;
        uint256 outAmt;
        if (zfo) {
            next = SqrtPriceMath.getNextSqrtPriceFromInput(current, liq, amountLessFee, true);
            inUsed = SqrtPriceMath.getAmount0Delta(next, current, liq, true);
            outAmt = SqrtPriceMath.getAmount1Delta(next, current, liq, false);
        } else {
            next = SqrtPriceMath.getNextSqrtPriceFromInput(current, liq, amountLessFee, false);
            inUsed = SqrtPriceMath.getAmount1Delta(current, next, liq, true);
            outAmt = SqrtPriceMath.getAmount0Delta(current, next, liq, false);
        }
        st.sqrtPriceX96 = next;

        // caller's deltas: owes input (-), owed output (+). No physical transfer
        // here — V4 pays the executor's positive delta via a later take().
        _accountDelta(msg.sender, inputCurrency, -int256(uint256(amountIn)));
        _accountDelta(msg.sender, outputCurrency, int256(uint256(outAmt)));
        _noteDelta(msg.sender, inputCurrency);
        _noteDelta(msg.sender, outputCurrency);

        int256 delta0 = zfo ? -int256(int256(amountIn)) : int256(uint256(outAmt));
        int256 delta1 = zfo ? int256(uint256(outAmt)) : -int256(int256(amountIn));
        emit V4Swap(h, inputCurrency, outputCurrency, amountIn, outAmt);
        return (delta0, delta1);
    }
}
