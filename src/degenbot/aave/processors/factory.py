"""Factory for creating token processors by revision."""

from typing import ClassVar

from degenbot.aave.processors.base import (
    CollateralTokenProcessor,
    DebtTokenProcessor,
    GhoDebtTokenProcessor,
)
from degenbot.aave.processors.processor import (
    UnifiedCollateralProcessor,
    UnifiedDebtProcessor,
    UnifiedGhoProcessor,
)
from degenbot.aave.processors.strategies import (
    COLLATERAL_STRATEGIES,
    DEBT_STRATEGIES,
    GHO_DISCOUNT_STRATEGIES,
    GHO_STRATEGIES,
)
from degenbot.logging import logger


class TokenProcessorFactory:
    """Factory for creating token processors by revision number."""

    # Supported revision ranges
    COLLATERAL_REVISIONS: ClassVar[set[int]] = set(COLLATERAL_STRATEGIES.keys())
    DEBT_REVISIONS: ClassVar[set[int]] = set(DEBT_STRATEGIES.keys())
    GHO_REVISIONS: ClassVar[set[int]] = set(GHO_STRATEGIES.keys())

    @classmethod
    def get_collateral_processor(cls, revision: int) -> CollateralTokenProcessor:
        """Get processor for collateral (aToken) by revision.

        Args:
            revision: The aToken revision number

        Returns:
            Processor instance for the revision

        Raises:
            ValueError: If revision is not supported
        """
        if revision not in COLLATERAL_STRATEGIES:
            msg = f"No processor for collateral revision {revision}"
            raise ValueError(msg)

        strategy = COLLATERAL_STRATEGIES[revision]
        processor = UnifiedCollateralProcessor(
            rounding=strategy,
            revision=revision,
        )
        logger.debug(f"Created UnifiedCollateralProcessor for aToken revision {revision}")
        return processor

    @classmethod
    def get_debt_processor(cls, revision: int) -> DebtTokenProcessor:
        """Get processor for standard debt (vToken) by revision.

        This returns processors for non-GHO variable debt tokens.
        For GHO tokens, use get_gho_debt_processor() instead.

        Args:
            revision: The vToken revision number

        Returns:
            Processor instance for the revision

        Raises:
            ValueError: If revision is not supported
        """
        if revision not in DEBT_STRATEGIES:
            msg = f"No processor for debt revision {revision}"
            raise ValueError(msg)

        strategy = DEBT_STRATEGIES[revision]
        processor = UnifiedDebtProcessor(
            rounding=strategy,
            revision=revision,
        )
        logger.debug(f"Created UnifiedDebtProcessor for vToken revision {revision}")
        return processor

    @classmethod
    def get_gho_debt_processor(cls, revision: int) -> GhoDebtTokenProcessor:
        """Get processor for GHO variable debt token by revision.

        GHO tokens have special discount handling that requires
        a separate processor type from standard vTokens.

        Args:
            revision: The GHO vToken revision number

        Returns:
            Processor instance for the revision

        Raises:
            ValueError: If revision is not supported
        """
        if revision not in GHO_STRATEGIES:
            msg = f"No processor for GHO revision {revision}"
            raise ValueError(msg)

        rounding_strategy = GHO_STRATEGIES[revision]
        discount_strategy = GHO_DISCOUNT_STRATEGIES[revision]
        processor = UnifiedGhoProcessor(
            rounding=rounding_strategy,
            discount=discount_strategy,
            revision=revision,
        )
        logger.debug(f"Created UnifiedGhoProcessor for GHO vToken revision {revision}")
        return processor


__all__ = ["TokenProcessorFactory"]
