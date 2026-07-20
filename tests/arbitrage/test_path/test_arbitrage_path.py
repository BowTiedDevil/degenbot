"""Tests for path-building primitives kept after ACDWOC's ArbitragePath retirement.

ACDWOC deleted ``ArbitragePath`` and the f64 Möbius solver stack — the
construction/validation/calculate/close classes that exercised the deleted
``ArbitragePath`` API went with it. The hop-state conversion surface
(``to_hop_state`` / ``extract_fee`` / ``build_swap_amount``) was retired in
the hop/encoding relay retirement (epic `6Y2PBF`) once the Rust engine
became the sole solve/encode surface. What survives here is the pure-integer
``v3_libraries.functions.v3_virtual_reserves`` math coverage.
"""

from degenbot.uniswap.v3_libraries.constants import Q96
from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves as _v3_virtual_reserves


class TestV3VirtualReservesIntegerMath:
    def test_price_one_symmetric(self):

        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=Q96, zero_for_one=True)
        assert x == 10**18 * Q96
        assert y == 10**18 * Q96

    def test_price_one_reversed(self):

        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=Q96, zero_for_one=False)
        assert x == 10**18 * Q96
        assert y == 10**18 * Q96

    def test_price_four(self):

        sqrt_p = 2 * Q96
        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=sqrt_p, zero_for_one=True)
        assert x == 10**18 * Q96 * Q96 // (2 * Q96)
        assert y == 10**18 * 2 * Q96

    def test_direction_swap(self):

        sqrt_p = 2 * Q96
        x_zfo, y_zfo = _v3_virtual_reserves(10**18, sqrt_p, zero_for_one=True)
        x_ofz, y_ofz = _v3_virtual_reserves(10**18, sqrt_p, zero_for_one=False)
        assert x_zfo == y_ofz
        assert y_zfo == x_ofz

    def test_product_equals_liquidity_squared_scaled(self):

        liquidity = 10**18
        sqrt_p = 79228162514264337593543950336
        x, y = _v3_virtual_reserves(liquidity, sqrt_p, zero_for_one=True)
        assert x * y == liquidity * liquidity * Q96 * Q96

    def test_large_liquidity_no_precision_loss(self):

        liquidity = 2**100
        sqrt_p = Q96
        x, y = _v3_virtual_reserves(liquidity, sqrt_p, zero_for_one=True)
        assert x == liquidity * Q96
        assert y == liquidity * Q96

    def test_matches_float_for_typical_values(self):

        liquidity = 10**18
        sqrt_price_x96 = 79228162514264337593543950336

        x_int, y_int = _v3_virtual_reserves(liquidity, sqrt_price_x96, zero_for_one=True)

        sqrt_price = sqrt_price_x96 / Q96
        x_float = round(liquidity / sqrt_price * Q96)
        y_float = round(liquidity * sqrt_price * Q96)

        assert abs(x_int - x_float) <= 1
        assert abs(y_int - y_float) <= 1
