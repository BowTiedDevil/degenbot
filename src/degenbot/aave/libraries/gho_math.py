"""GHO-specific math calculations.

GHO has unique discount mechanics that are separate from standard Aave V3 math.
These calculations mirror the logic in the GhoDiscountRateStrategy contract.

Contract Reference:
    - GhoDiscountRateStrategy: 0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812
    - See: contract_reference/aave/GhoDiscountRateStrategy/contract.sol
"""

from degenbot.aave.libraries import wad_ray_math
from degenbot.logging import logger


class GhoMath:
    """GHO-specific calculations for discount rate and balance computations.

    GHO uses a discount mechanism where stkAAVE holders can receive a discount
    on their GHO borrowing interest. The discount rate is calculated based on:
    - stkAAVE balance held
    - GHO debt balance

    These calculations are performed by the GhoDiscountRateStrategy contract
    and applied by the GhoVariableDebtToken contract (rev 1-3).
    """

    # Constants from GhoDiscountRateStrategy contract
    GHO_DISCOUNTED_PER_DISCOUNT_TOKEN: int = 100 * 10**18
    """Amount of debt entitled to discount per unit of discount token (100 wad per stkAAVE)."""

    DISCOUNT_RATE_BPS: int = 3000
    """Maximum discount rate in basis points (3000 = 30.00%)."""

    MIN_DISCOUNT_TOKEN_BALANCE: int = 10**15
    """Minimum stkAAVE balance to qualify for discount (0.001 stkAAVE)."""

    MIN_DEBT_TOKEN_BALANCE: int = 10**18
    """Minimum GHO debt balance to qualify for discount (1 GHO)."""

    @staticmethod
    def calculate_discount_rate(
        debt_balance: int,
        discount_token_balance: int,
        gho_discounted_per_discount_token: int = GHO_DISCOUNTED_PER_DISCOUNT_TOKEN,
    ) -> int:
        """Calculate GHO discount rate in basis points.

        Mirrors the logic in GhoDiscountRateStrategy.calculateDiscountRate.

        Args:
            debt_balance: Current GHO debt balance in underlying units
            discount_token_balance: stkAAVE balance held
            gho_discounted_per_discount_token: How much GHO gets discount per stkAAVE
                (default: 100 * 10**18 = 100 GHO per stkAAVE)

        Returns:
            Discount rate in basis points (0-3000, where 10000 = 100%)

        Example:
            >>> GhoMath.calculate_discount_rate(
            ...     debt_balance=5000 * 10**18,  # 5000 GHO
            ...     discount_token_balance=100 * 10**18,  # 100 stkAAVE
            ... )
            3000  # Max 30% discount

        Contract Reference:
            GhoDiscountRateStrategy.calculateDiscountRate
            See: contract_reference/aave/GhoDiscountRateStrategy/contract.sol:40-54
        """
        if (
            discount_token_balance < GhoMath.MIN_DISCOUNT_TOKEN_BALANCE
            or debt_balance < GhoMath.MIN_DEBT_TOKEN_BALANCE
        ):
            logger.debug(
                f"GhoMath.calculate_discount_rate: below minimums "
                f"(discount_token={discount_token_balance}, debt={debt_balance})"
            )
            return 0

        discounted_balance = wad_ray_math.wad_mul(
            discount_token_balance,
            gho_discounted_per_discount_token,
        )

        if discounted_balance >= debt_balance:
            discount_rate = GhoMath.DISCOUNT_RATE_BPS
            logger.debug(
                f"GhoMath.calculate_discount_rate: max discount "
                f"(discounted_balance={discounted_balance} >= debt={debt_balance})"
            )
        else:
            # Proportional discount: (discounted / total) * max_rate
            discount_rate = (discounted_balance * GhoMath.DISCOUNT_RATE_BPS) // debt_balance
            logger.debug(
                f"GhoMath.calculate_discount_rate: proportional discount "
                f"(discounted_balance={discounted_balance}, debt={debt_balance}, "
                f"rate={discount_rate})"
            )

        return discount_rate

    @staticmethod
    def calculate_discounted_balance(
        debt_balance: int,
        discount_token_balance: int,
        gho_discounted_per_discount_token: int = GHO_DISCOUNTED_PER_DISCOUNT_TOKEN,
    ) -> int:
        """Calculate the discounted portion of debt balance.

        This is the amount of debt that qualifies for the discount rate.
        The discounted balance is capped at the total debt balance.

        Args:
            debt_balance: Current GHO debt balance in underlying units
            discount_token_balance: stkAAVE balance held
            gho_discounted_per_discount_token: How much GHO gets discount per stkAAVE

        Returns:
            The amount of debt that receives the discount (0 to debt_balance)

        Example:
            >>> GhoMath.calculate_discounted_balance(
            ...     debt_balance=5000 * 10**18,
            ...     discount_token_balance=100 * 10**18,
            ... )
            5000 * 10**18  # All debt is discounted
        """
        if (
            discount_token_balance < GhoMath.MIN_DISCOUNT_TOKEN_BALANCE
            or debt_balance < GhoMath.MIN_DEBT_TOKEN_BALANCE
        ):
            return 0

        discounted = wad_ray_math.wad_mul(
            discount_token_balance,
            gho_discounted_per_discount_token,
        )

        # Capped at total debt
        return min(discounted, debt_balance)

    @staticmethod
    def calculate_effective_debt_balance(
        debt_balance: int,
        discount_token_balance: int,
        gho_discounted_per_discount_token: int = GHO_DISCOUNTED_PER_DISCOUNT_TOKEN,
    ) -> int:
        """Calculate the effective debt balance after discount.

        The effective balance is what the debt balance would be after
        applying the discount rate to the discounted portion.

        Formula:
            effective = undiscounted + (discounted * (10000 - rate)) / 10000

        Args:
            debt_balance: Current GHO debt balance in underlying units
            discount_token_balance: stkAAVE balance held
            gho_discounted_per_discount_token: How much GHO gets discount per stkAAVE

        Returns:
            Effective debt balance after discount

        Example:
            >>> GhoMath.calculate_effective_debt_balance(
            ...     debt_balance=10000 * 10**18,
            ...     discount_token_balance=100 * 10**18,
            ... )
            7000 * 10**18  # 30% discount on all 10000 GHO
        """
        discounted_balance = GhoMath.calculate_discounted_balance(
            debt_balance,
            discount_token_balance,
            gho_discounted_per_discount_token,
        )

        if discounted_balance == 0:
            return debt_balance

        discount_rate = GhoMath.calculate_discount_rate(
            debt_balance,
            discount_token_balance,
            gho_discounted_per_discount_token,
        )

        undiscounted_balance = debt_balance - discounted_balance

        # Apply discount: discounted * (10000 - rate) / 10000
        discounted_after = (discounted_balance * (10000 - discount_rate)) // 10000

        return undiscounted_balance + discounted_after
