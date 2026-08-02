// SPDX-License-Identifier: MIT
pragma solidity =0.8.26;

/// Tier-3 Curve standard-stableswap on-chain accuracy oracle harness (ergo task
/// YXMNWB, epic UP5NH6/GNMN3P — family 2/3 of SH6HAK's Tier-3 cutover).
///
/// Curve's canonical stableswap reference is a VYPER contract (this environment
/// has no vyper toolchain), so this harness is a faithful Solidity 0.8.26 port
/// of the STANDARD `get_dy` path (`swap_style == STANDARD`,
/// `YVariant::STANDARD`, `DVariant::STANDARD` — the `FEE_THEN_RATE`
/// conversion) that `degenbot-curve-math` mirrors. The byte-exactness proven
/// here is *Rust U256 integer math === EVM (Solidity 0.8.26) integer math* for
/// the SAME documented stableswap get_dy algorithm — catching any
/// integer-division ordering, rounding, or A-precision drift between the two
/// consumers of one algorithm. This is a documented toolchain deviation
/// (vyper absent) mirroring the V2/V3 direct-solc deviation: the `get_dy`
/// algorithm (not a separate Vyper→EVM code route) is the on-chain reference.
contract CurveSwapOracleHarness {
    uint256 public constant PRECISION = 1e18;
    uint256 public constant FEE_DENOMINATOR = 1e10;
    uint256 public constant MAX_COINS = 4;

    uint256[MAX_COINS] private _balances;
    uint256[MAX_COINS] private _rates;
    uint256 private _aCoefficient;
    uint256 private _aPrecision;
    uint256 private _fee;
    uint8 private _nCoins;

    /// Seed the pool's stableswap parameters (balances and rate multipliers,
    /// raw A, A_PRECISION, swap fee) — the whole-slot-set the Rust engine
    /// carries in `CurvePoolState`. `nCoins` in [2, MAX_COINS].
    function setup(
        uint256[MAX_COINS] calldata balances,
        uint256[MAX_COINS] calldata rates,
        uint256 aCoefficient,
        uint256 aPrecision,
        uint256 fee,
        uint8 nCoins
    ) external {
        require(nCoins >= 2 && nCoins <= MAX_COINS, "nCoins");
        for (uint8 i = 0; i < nCoins; i++) {
            _balances[i] = balances[i];
            _rates[i] = rates[i];
        }
        _aCoefficient = aCoefficient;
        _aPrecision = aPrecision;
        _fee = fee;
        _nCoins = nCoins;
    }

    /// Standard stableswap `get_dy(coinIn, coinOut, amountIn)` — byte-exact
    /// mirror of the engine's `simulate_curve_stableswap_swap` standard path.
    function getDy(uint256 coinIn, uint256 coinOut, uint256 amountIn)
        external
        view
        returns (uint256)
    {
        require(coinIn != coinOut, "i==j");
        uint8 n = _nCoins;
        require(coinIn < n && coinOut < n, "idx");
        if (amountIn == 0) return 0;

        // Engine amp passed to get_y already includes the a_precision factor
        // (engine: amp = a_coefficient * a_precision).
        uint256 amp = _aCoefficient * _aPrecision;

        // xp[i] = rates[i] * balances[i] // PRECISION
        uint256[MAX_COINS] memory xp;
        for (uint8 i = 0; i < n; i++) {
            xp[i] = (_rates[i] * _balances[i]) / PRECISION;
        }

        // x = xp[coinIn] + amountIn * rates[coinIn] // PRECISION
        uint256 x = xp[coinIn] + (amountIn * _rates[coinIn]) / PRECISION;

        uint256 y = _getY(uint8(coinIn), uint8(coinOut), x, xp, amp, n);

        // dy = xp[coinOut] - y - 1
        uint256 dy = xp[coinOut] - y;
        if (dy > 0) dy -= 1;

        // fee = fee * dy // FEE_DENOMINATOR
        uint256 fee = (_fee * dy) / FEE_DENOMINATOR;
        // out = (dy - fee) * PRECISION // rates[coinOut]
        uint256 out = ((dy - fee) * PRECISION) / _rates[coinOut];
        return out;
    }

    /// Standard D step: `(a_nn*s//a_prec + d_p*n) * d // ((a_nn-a_prec)*d//a_prec + (n+1)*d_p)`.
    function _calcD(uint256 aNn, uint256 s, uint256 d, uint256 dP, uint8 n, uint256 aPrecision)
        private
        pure
        returns (uint256)
    {
        uint256 lhs = (aNn * s) / aPrecision + dP * n;
        uint256 rhs = ((aNn - aPrecision) * d) / aPrecision + (n + 1) * dP;
        return (lhs * d) / rhs;
    }

    /// Standard D' step: `d_p = d_p * d // (x * n)` for each x in xp.
    function _calcDp(uint256 d, uint256 dP, uint256[MAX_COINS] memory xp, uint8 n)
        private
        pure
        returns (uint256)
    {
        for (uint8 i = 0; i < n; i++) {
            dP = (dP * d) / (xp[i] * n);
        }
        return dP;
    }

    /// Solve the stableswap invariant D (Newton, ≤ 255 iterations).
    function _getD(uint256[MAX_COINS] memory xp, uint256 amp, uint8 n, uint256 aPrecision)
        private
        pure
        returns (uint256)
    {
        uint256 s = 0;
        for (uint8 i = 0; i < n; i++) s += xp[i];
        if (s == 0) return 0;
        uint256 d = s;
        uint256 aNn = amp * n;
        for (uint256 iter = 0; iter < 255; iter++) {
            uint256 dPrev = d;
            uint256 dP = _calcDp(d, d, xp, n);
            d = _calcD(aNn, s, d, dP, n, aPrecision);
            if (d >= dPrev) {
                if (d - dPrev <= 1) return d;
            } else if (dPrev - d <= 1) {
                return d;
            }
        }
        revert("not converged");
    }

    /// Compute x[coinOut] if one makes x[coinIn] = x — STANDARD variant.
    /// `s += x_` includes the in-coin (the Rust `stableswap_get_y` does the
    /// same; the out-coin `j` is skipped with `continue`).
    function _getY(
        uint8 i,
        uint8 j,
        uint256 x,
        uint256[MAX_COINS] memory xp,
        uint256 amp,
        uint8 n
    ) private view returns (uint256) {
        uint256 aPrecision = _aPrecision;
        uint256 d = _getD(xp, amp, n, aPrecision);
        uint256 c = d;
        uint256 s = 0;
        for (uint8 coinIndex = 0; coinIndex < n; coinIndex++) {
            if (coinIndex == i) {
                s += x;
                c = (c * d) / (x * n);
            } else if (coinIndex != j) {
                uint256 x_ = xp[coinIndex];
                s += x_;
                c = (c * d) / (x_ * n);
            }
        }
        uint256 aNn = amp * n;
        // STANDARD (YVariant::STANDARD — NOT omits_a_precision):
        // c = (c * d * a_precision) // (a_nn * n); b = s + (d * a_precision) // a_nn
        c = (c * d * aPrecision) / (aNn * n);
        uint256 b = s + (d * aPrecision) / aNn;
        uint256 y = d;
        for (uint256 iter = 0; iter < 255; iter++) {
            uint256 yPrev = y;
            y = (y * y + c) / (2 * y + b - d);
            if (y > yPrev) {
                if (y - yPrev <= 1) return y;
            } else if (yPrev - y <= 1) {
                return y;
            }
        }
        revert("not converged");
    }
}
