"""TokenMath library for calculating scaled amounts.

This module implements the Pool contract's TokenMath library which provides
rounding-aware calculations for mint/burn operations. The logic is separated
from token processors since the calculations originate in the Pool contract.
"""

from abc import abstractmethod
from typing import ClassVar, Protocol

from degenbot.aave.libraries import wad_ray_math


class TokenMath(Protocol):
    """Protocol for Pool contract's TokenMath library operations.

    All methods follow the on-chain TokenMath library specification:
    - Mint operations round DOWN for collateral (aTokens), UP for debt (vTokens)
    - Burn operations round UP for collateral, DOWN for debt
    - This ensures protocol safety: never over-mint collateral, never under-account debt
    """

    @abstractmethod
    def get_collateral_mint_scaled_amount(self, amount: int, liquidity_index: int) -> int:
        """Calculate scaled amount for collateral mint (supply).

        Rounds down to ensure minted aTokens <= supplied amount.
        """
        ...

    @abstractmethod
    def get_collateral_burn_scaled_amount(self, amount: int, liquidity_index: int) -> int:
        """Calculate scaled amount for collateral burn (withdraw).

        Rounds up to ensure sufficient balance reduction.
        """
        ...

    @abstractmethod
    def get_collateral_transfer_scaled_amount(self, amount: int, liquidity_index: int) -> int:
        """Calculate scaled amount for collateral transfer.

        Rounds up to ensure recipient receives at least the requested amount.
        """
        ...

    @abstractmethod
    def get_collateral_balance(self, scaled_amount: int, liquidity_index: int) -> int:
        """Calculate actual balance from scaled collateral balance.

        Rounds down to prevent over-accounting.
        """
        ...

    @abstractmethod
    def get_debt_mint_scaled_amount(self, amount: int, borrow_index: int) -> int:
        """Calculate scaled amount for debt mint (borrow).

        Rounds up to ensure protocol never underaccounts user's debt.
        """
        ...

    @abstractmethod
    def get_debt_burn_scaled_amount(self, amount: int, borrow_index: int) -> int:
        """Calculate scaled amount for debt burn (repay).

        Rounds down to prevent over-burning of vTokens.
        """
        ...

    @abstractmethod
    def get_debt_balance(self, scaled_amount: int, borrow_index: int) -> int:
        """Calculate actual balance from scaled debt balance.

        Rounds up to prevent under-accounting user's debt.
        """
        ...


class HalfUpRoundingMath:
    """Standard half-up rounding for pool revisions 1-3.

    Uses traditional ray_div/ray_mul with half-up rounding.
    This was the default before explicit floor/ceil rounding was introduced
    in token revision 4.
    """

    @staticmethod
    def get_collateral_mint_scaled_amount(amount: int, liquidity_index: int) -> int:
        """Standard half-up rounding for collateral mint."""
        return wad_ray_math.ray_div(amount, liquidity_index)

    @staticmethod
    def get_collateral_burn_scaled_amount(amount: int, liquidity_index: int) -> int:
        """Standard half-up rounding for collateral burn."""
        return wad_ray_math.ray_div(amount, liquidity_index)

    @staticmethod
    def get_collateral_transfer_scaled_amount(amount: int, liquidity_index: int) -> int:
        """Standard half-up rounding for collateral transfer."""
        return wad_ray_math.ray_div(amount, liquidity_index)

    @staticmethod
    def get_collateral_balance(scaled_amount: int, liquidity_index: int) -> int:
        """Standard half-up rounding for collateral balance."""
        return wad_ray_math.ray_mul(scaled_amount, liquidity_index)

    @staticmethod
    def get_debt_mint_scaled_amount(amount: int, borrow_index: int) -> int:
        """Standard half-up rounding for debt mint."""
        return wad_ray_math.ray_div(amount, borrow_index)

    @staticmethod
    def get_debt_burn_scaled_amount(amount: int, borrow_index: int) -> int:
        """Standard half-up rounding for debt burn."""
        return wad_ray_math.ray_div(amount, borrow_index)

    @staticmethod
    def get_debt_balance(scaled_amount: int, borrow_index: int) -> int:
        """Standard half-up rounding for debt balance."""
        return wad_ray_math.ray_mul(scaled_amount, borrow_index)


class ExplicitRoundingMath:
    """Explicit floor/ceil rounding for pool revisions 4+.

    Introduced in token revision 4 to provide protocol-controlled rounding:
    - Floor for collateral mints (prevent over-minting)
    - Ceil for collateral burns (ensure sufficient reduction)
    - Inverse logic for debt (ceil for mints, floor for burns)

    Note: TokenMathV4 and TokenMathV5 had identical implementations,
    so they are consolidated into this single class.
    """

    @staticmethod
    def get_collateral_mint_scaled_amount(amount: int, liquidity_index: int) -> int:
        """Floor rounding: minted aTokens <= supplied amount."""
        return wad_ray_math.ray_div_floor(amount, liquidity_index)

    @staticmethod
    def get_collateral_burn_scaled_amount(amount: int, liquidity_index: int) -> int:
        """Ceil rounding: ensure sufficient balance reduction."""
        return wad_ray_math.ray_div_ceil(amount, liquidity_index)

    @staticmethod
    def get_collateral_transfer_scaled_amount(amount: int, liquidity_index: int) -> int:
        """Ceil rounding: ensure recipient gets at least requested."""
        return wad_ray_math.ray_div_ceil(amount, liquidity_index)

    @staticmethod
    def get_collateral_balance(scaled_amount: int, liquidity_index: int) -> int:
        """Floor rounding: prevent over-accounting."""
        return wad_ray_math.ray_mul_floor(scaled_amount, liquidity_index)

    @staticmethod
    def get_debt_mint_scaled_amount(amount: int, borrow_index: int) -> int:
        """Ceil rounding: never underaccount user's debt."""
        return wad_ray_math.ray_div_ceil(amount, borrow_index)

    @staticmethod
    def get_debt_burn_scaled_amount(amount: int, borrow_index: int) -> int:
        """Floor rounding: prevent over-burning."""
        return wad_ray_math.ray_div_floor(amount, borrow_index)

    @staticmethod
    def get_debt_balance(scaled_amount: int, borrow_index: int) -> int:
        """Ceil rounding: prevent under-accounting."""
        return wad_ray_math.ray_mul_ceil(scaled_amount, borrow_index)


class TokenMathFactory:
    """Factory for creating TokenMath instances by pool version."""

    _TOKEN_MATH: ClassVar[dict[int, type[TokenMath]]] = {
        1: HalfUpRoundingMath,
        2: HalfUpRoundingMath,
        3: HalfUpRoundingMath,
        4: ExplicitRoundingMath,
        5: ExplicitRoundingMath,
        6: ExplicitRoundingMath,
        7: ExplicitRoundingMath,
        8: ExplicitRoundingMath,
        9: ExplicitRoundingMath,
        10: ExplicitRoundingMath,
    }

    @classmethod
    def get_token_math(cls, pool_version: int) -> TokenMath:
        """Get TokenMath instance for the given pool version.

        Args:
            pool_version: The pool revision number (1-5)

        Returns:
            TokenMath instance with appropriate rounding for the version

        Raises:
            ValueError: If pool_version is not supported
        """
        token_math_class = cls._TOKEN_MATH.get(pool_version)
        if token_math_class is None:
            msg = f"No TokenMath implementation for pool version {pool_version}"
            raise ValueError(msg)
        return token_math_class()

    @classmethod
    def get_token_math_for_token_revision(cls, token_revision: int) -> TokenMath:
        """Get TokenMath instance appropriate for a token revision.

        Maps token revisions to rounding implementations:
        - Token rev 1-3 -> HalfUpRoundingMath (standard half-up rounding)
        - Token rev 4+ -> ExplicitRoundingMath (floor/ceil rounding)

        Args:
            token_revision: The token contract revision number

        Returns:
            TokenMath instance with appropriate rounding
        """

        if token_revision <= 3:  # noqa:PLR2004
            return cls.get_token_math(1)
        if token_revision == 4:  # noqa:PLR2004
            return cls.get_token_math(4)
        return cls.get_token_math(5)
