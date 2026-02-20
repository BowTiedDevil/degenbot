"""Pool processors for calculating scaled amounts.

Pool processors encapsulate the TokenMath calculations that occur in the Pool
contract before token mint/burn operations. Callers use these processors to
calculate scaled amounts before passing them to token processors.
"""

from degenbot.aave.libraries.token_math import TokenMath, TokenMathFactory


class PoolProcessor:
    """Processor for Pool-level operations that calculate scaled amounts.

    This processor mirrors the Pool contract's behavior of calculating scaled
    amounts using TokenMath before calling token operations. The caller is
    responsible for using the appropriate pool version that matches the
    on-chain configuration.
    """

    def __init__(self, token_math: TokenMath) -> None:
        """Initialize with TokenMath implementation.

        Args:
            token_math: TokenMath instance with appropriate rounding
        """
        self.token_math = token_math

    def calculate_collateral_mint_scaled_amount(self, amount: int, liquidity_index: int) -> int:
        """Calculate scaled amount for collateral mint (supply).

        Uses floor rounding to ensure minted aTokens <= supplied amount.

        Args:
            amount: The underlying amount to supply
            liquidity_index: The current liquidity index

        Returns:
            The scaled amount to mint
        """
        return self.token_math.get_collateral_mint_scaled_amount(amount, liquidity_index)

    def calculate_collateral_burn_scaled_amount(self, amount: int, liquidity_index: int) -> int:
        """Calculate scaled amount for collateral burn (withdraw).

        Uses ceil rounding to ensure sufficient balance reduction.

        Args:
            amount: The underlying amount to withdraw
            liquidity_index: The current liquidity index

        Returns:
            The scaled amount to burn
        """
        return self.token_math.get_collateral_burn_scaled_amount(amount, liquidity_index)

    def calculate_debt_mint_scaled_amount(self, amount: int, borrow_index: int) -> int:
        """Calculate scaled amount for debt mint (borrow).

        Uses ceil rounding to ensure protocol never underaccounts user's debt.

        Args:
            amount: The underlying amount to borrow
            borrow_index: The current variable borrow index

        Returns:
            The scaled amount to mint
        """
        return self.token_math.get_debt_mint_scaled_amount(amount, borrow_index)

    def calculate_debt_burn_scaled_amount(self, amount: int, borrow_index: int) -> int:
        """Calculate scaled amount for debt burn (repay).

        Uses floor rounding to prevent over-burning of vTokens.

        Args:
            amount: The underlying amount to repay
            borrow_index: The current variable borrow index

        Returns:
            The scaled amount to burn
        """
        return self.token_math.get_debt_burn_scaled_amount(amount, borrow_index)


class PoolProcessorFactory:
    """Factory for creating PoolProcessor instances by version."""

    @staticmethod
    def get_pool_processor(pool_version: int) -> PoolProcessor:
        """Get PoolProcessor for the given pool version.

        Args:
            pool_version: The pool revision number (1-5)

        Returns:
            PoolProcessor with TokenMath for the version

        Raises:
            ValueError: If pool_version is not supported
        """
        token_math = TokenMathFactory.get_token_math(pool_version)
        return PoolProcessor(token_math)

    @staticmethod
    def get_pool_processor_for_token_revision(token_revision: int) -> PoolProcessor:
        """Get PoolProcessor appropriate for a token revision.

        Maps token revisions to the pool version they were deployed with:
        - Token rev 1-3 -> Pool v3.1-v3.3
        - Token rev 4 -> Pool v3.4
        - Token rev 5+ -> Pool v3.5+

        Args:
            token_revision: The token contract revision number

        Returns:
            PoolProcessor with appropriate TokenMath
        """
        token_math = TokenMathFactory.get_token_math_for_token_revision(token_revision)
        return PoolProcessor(token_math)
