"""The pricing ladder: the one implementation behind ``PoolPricing``.

Four questions are asked of every priced pool, and every pool answers them the
same way: ``get_absolute_price``/``get_absolute_exchange_rate`` report the
token1-per-token0 relation in raw units, and ``get_nominal_price``/
``get_nominal_exchange_rate`` correct it for the pair's decimal places. Only the
ENCODING of that relation differs per family — a V2/Aerodrome pool encodes it in
reserves, a V3/V4 pool in ``sqrt_price_x96`` — so the ladder owns the verb
family and each family owns one leaf, :meth:`PriceLadder._absolute_rate`.

That split is what makes the four verbs one behaviour instead of four
near-identical copies: the decimal correction, the unknown-token refusal, the
price-is-the-reciprocal-of-the-rate relation, and the override-state precedence
are stated once, here, and cannot drift between families.

``degenbot.types.pool_protocols.PoolPricing`` is the structural declaration of
the surviving verb set; this class is its implementation.

See: contract_reference/uniswap/V2/UniswapV2Factory.sol,
contract_reference/uniswap/V3/UniswapV3Factory.sol,
contract_reference/uniswap/V4/PoolManager.sol
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.exceptions import DegenbotValueError

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token, NativeToken
    from degenbot.types.abstract import AbstractPoolState


class PriceLadder:
    """The price/rate verb family every priced pool answers.

    A family mixin inherits this and supplies :meth:`_absolute_rate`; the
    attribute declarations below are satisfied by the state mixin and the
    concrete pool the family mixin is composed with.
    """

    # Attributes provided by the family's state mixin in the MRO. A pair leg
    # is a token companion of either kind (ADR-097): V4 pairs may hold the
    # conceptual native token where V2/V3 pairs hold only ERC-20s.
    _token0: Erc20Token | NativeToken
    _token1: Erc20Token | NativeToken

    @property
    def tokens(self) -> tuple[Erc20Token | NativeToken, Erc20Token | NativeToken]:
        """The pair this pool prices (concrete pool provides).

        Returns:
            The token pair held by this pool.

        """
        raise NotImplementedError

    @property
    def state(self) -> AbstractPoolState:
        """The pool's current state (concrete pool provides).

        Returns:
            The state the ladder reads when no override is supplied.

        """
        raise NotImplementedError

    def _absolute_rate(self, token: Erc20Token, state: AbstractPoolState) -> Fraction:
        """Return the token1-per-token0 exchange rate, in the family's own encoding.

        Args:
            token: The token whose rate is being read.
            state: The resolved pool state (the concrete state, at runtime).

        Returns:
            The absolute exchange rate as a Fraction.

        """
        raise NotImplementedError

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: AbstractPoolState | None = None,
    ) -> Fraction:
        """Get the absolute price for the given token, expressed in units of the other.

        Returns:
            The absolute price as a Fraction.

        """
        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: AbstractPoolState | None = None,
    ) -> Fraction:
        """Get the absolute exchange rate for the given token.

        Returns:
            The absolute exchange rate as a Fraction.

        Raises:
            DegenbotValueError: If the token is not held by this pool.

        """
        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return self._absolute_rate(token, state)

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: AbstractPoolState | None = None,
    ) -> Fraction:
        """Get the nominal price for the given token, corrected for decimals.

        Returns:
            The nominal price as a Fraction.

        """
        return 1 / self.get_nominal_exchange_rate(token=token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: AbstractPoolState | None = None,
    ) -> Fraction:
        """Get the nominal exchange rate, corrected for decimal place values.

        Returns:
            The nominal exchange rate as a Fraction.

        """
        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            self._decimal_scale(token)
        )

    def _decimal_scale(self, token: Erc20Token) -> Fraction:
        """Return the decimal correction for ``token``: token1 decimals over token0's.

        Args:
            token: The token whose rate is being corrected.

        Returns:
            The correction factor as a Fraction.

        """
        return (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
        )
