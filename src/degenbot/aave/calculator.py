"""Scaled amount calculation using TokenMath."""

from collections.abc import Callable

from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.libraries.token_math import TokenMathFactory
from degenbot.aave.models import EnrichmentError


class ScaledAmountCalculator:
    """
    Calculates scaled amounts using TokenMath.

    Mirrors the Pool contract's calculation exactly. Uses the same
    TokenMath methods and rounding behaviors.
    """

    def __init__(self, pool_revision: int, token_revision: int) -> None:
        self.pool_revision = pool_revision
        self.token_revision = token_revision
        self.token_math = TokenMathFactory.get_token_math_for_token_revision(token_revision)

    def calculate(
        self,
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
    ) -> int:
        """
        Calculate scaled amount for the given event type.

        Args:
            event_type: Type of scaled token event
            raw_amount: Raw amount from Pool event
            index: Current liquidity/borrow index

        Returns:
            Scaled amount calculated using TokenMath

        Raises:
            EnrichmentError: If event type is not supported
        """
        method = self._get_calculation_method(event_type)
        return method(raw_amount, index)

    def _get_calculation_method(
        self, event_type: ScaledTokenEventType
    ) -> Callable[[int, int], int]:
        """Get the appropriate TokenMath method for this event type."""
        method_map: dict[ScaledTokenEventType, Callable[[int, int], int]] = {
            ScaledTokenEventType.COLLATERAL_MINT: self.token_math.get_collateral_mint_scaled_amount,
            ScaledTokenEventType.COLLATERAL_BURN: self.token_math.get_collateral_burn_scaled_amount,
            ScaledTokenEventType.COLLATERAL_TRANSFER: (
                self.token_math.get_collateral_transfer_scaled_amount
            ),
            ScaledTokenEventType.DEBT_MINT: self.token_math.get_debt_mint_scaled_amount,
            ScaledTokenEventType.DEBT_BURN: self.token_math.get_debt_burn_scaled_amount,
            ScaledTokenEventType.GHO_DEBT_MINT: self.token_math.get_debt_mint_scaled_amount,
            ScaledTokenEventType.GHO_DEBT_BURN: self.token_math.get_debt_burn_scaled_amount,
        }

        method = method_map.get(event_type)
        if method is None:
            msg = f"No TokenMath method for event type: {event_type}"
            raise EnrichmentError(msg)
        return method

    def get_method_name(self, event_type: ScaledTokenEventType) -> str:
        """Get the TokenMath method name for debugging."""
        method = self._get_calculation_method(event_type)
        return method.__name__
