"""Pool-level math calculations that depend on Pool revision.

Pool and Token revisions can change independently. Pool-level operations
(such as MINT_TO_TREASURY) depend on Pool revision, not Token revision.

This module provides PoolMath, which delegates to the appropriate rounding
library (wad_ray_math) based on Pool revision. This ensures the Python
implementation matches the on-chain PoolLogic calculations exactly.

Key differences:
- Rev 1-8: Uses inline math, half-up rounding
- Rev 9+: Uses TokenMath library, explicit floor/ceil rounding

Contract References:
    - Rev 8: PoolLogic.executeMintToTreasury uses rayMul
      See: contract_reference/aave/Pool/rev_8.sol
    - Rev 9+: PoolLogic.executeMintToTreasury uses getATokenBalance (rayMulFloor)
      See: contract_reference/aave/Pool/rev_9.sol
"""

from degenbot.aave.libraries import wad_ray_math
from degenbot.logging import logger


class PoolMath:
    """Pool-level calculations for Aave V3 Pool contract.

    These calculations are performed by the Pool contract (specifically
    PoolLogic library) and depend on the Pool revision, not the Token revision.

    All methods delegate to wad_ray_math functions with the appropriate
    rounding mode for the given Pool revision.
    """

    @staticmethod
    def get_treasury_mint_amount(
        accrued_to_treasury: int,
        index: int,
        pool_revision: int,
    ) -> int:
        """Calculate underlying amount to mint to treasury.

        This is the forward calculation performed by PoolLogic.executeMintToTreasury.

        Args:
            accrued_to_treasury: Scaled amount of accrued fees
            index: Current liquidity index
            pool_revision: Pool contract revision

        Returns:
            Underlying amount to mint

        Contract References:
            - Rev 1-8: PoolLogic.executeMintToTreasury uses rayMul
              (see contract_reference/aave/Pool/rev_8.sol)
            - Rev 9+: PoolLogic.executeMintToTreasury uses getATokenBalance
              which calls rayMulFloor
              (see contract_reference/aave/Pool/rev_9.sol)
        """
        if pool_revision >= 9:  # noqa: PLR2004
            # TokenMath library: floor rounding
            amount = wad_ray_math.ray_mul_floor(accrued_to_treasury, index)
            logger.debug(
                f"PoolMath.get_treasury_mint_amount(rev {pool_revision}): "
                f"ray_mul_floor({accrued_to_treasury}, {index}) = {amount}"
            )
        else:
            # Legacy: half-up rounding
            amount = wad_ray_math.ray_mul(accrued_to_treasury, index)
            logger.debug(
                f"PoolMath.get_treasury_mint_amount(rev {pool_revision}): "
                f"ray_mul({accrued_to_treasury}, {index}) = {amount}"
            )
        return amount

    @staticmethod
    def underlying_to_scaled_collateral(
        underlying_amount: int,
        liquidity_index: int,
        pool_revision: int,
    ) -> int:
        """Convert underlying amount to scaled collateral amount.

        This is the INVERSE of calculating collateral balance from scaled amount.
        Used by MINT_TO_TREASURY operations to determine the actual scaled
        amount minted given the MintedToTreasury event amount.

        Args:
            underlying_amount: Underlying amount (e.g., from MintedToTreasury event)
            liquidity_index: Current liquidity index
            pool_revision: Pool contract revision

        Returns:
            Scaled collateral amount

        Contract References:
            - Rev 1-8: Reverse of ray_mul (half-up) = ray_div (half-up)
            - Rev 9+: Reverse of getATokenBalance (ray_mul_floor) = ray_div_ceil

        See Also:
            debug/aave/0034: Pool Rev 9 MINT_TO_TREASURY rounding
            debug/aave/0036: Pool Rev 8 MINT_TO_TREASURY rounding
        """
        if pool_revision >= 9:  # noqa: PLR2004
            # Reverse of ray_mul_floor = ray_div_ceil
            scaled = wad_ray_math.ray_div_ceil(underlying_amount, liquidity_index)
            logger.debug(
                f"PoolMath.underlying_to_scaled_collateral(rev {pool_revision}): "
                f"ray_div_ceil({underlying_amount}, {liquidity_index}) = {scaled}"
            )
        else:
            # Reverse of ray_mul (half-up) = ray_div (half-up)
            scaled = wad_ray_math.ray_div(underlying_amount, liquidity_index)
            logger.debug(
                f"PoolMath.underlying_to_scaled_collateral(rev {pool_revision}): "
                f"ray_div({underlying_amount}, {liquidity_index}) = {scaled}"
            )
        return scaled

    @staticmethod
    def underlying_to_scaled_debt(
        underlying_amount: int,
        borrow_index: int,
        pool_revision: int,
    ) -> int:
        """Convert underlying amount to scaled debt amount.

        This is the INVERSE of calculating debt balance from scaled amount.

        Args:
            underlying_amount: Underlying debt amount
            borrow_index: Current borrow index
            pool_revision: Pool contract revision

        Returns:
            Scaled debt amount

        Note:
            Debt uses inverse rounding compared to collateral:
            - get_debt_balance uses ray_mul_ceil (prevent under-accounting)
            - So reverse uses ray_div_floor (prevent over-scaling)
        """
        if pool_revision >= 9:  # noqa: PLR2004
            # Reverse of ray_mul_ceil = ray_div_floor
            scaled = wad_ray_math.ray_div_floor(underlying_amount, borrow_index)
            logger.debug(
                f"PoolMath.underlying_to_scaled_debt(rev {pool_revision}): "
                f"ray_div_floor({underlying_amount}, {borrow_index}) = {scaled}"
            )
        else:
            # Reverse of ray_mul (half-up) = ray_div (half-up)
            scaled = wad_ray_math.ray_div(underlying_amount, borrow_index)
            logger.debug(
                f"PoolMath.underlying_to_scaled_debt(rev {pool_revision}): "
                f"ray_div({underlying_amount}, {borrow_index}) = {scaled}"
            )
        return scaled
