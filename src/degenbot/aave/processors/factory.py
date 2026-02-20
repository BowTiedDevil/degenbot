"""Factory for creating token processors by revision."""

from typing import Any, ClassVar, cast

from degenbot.aave.processors.base import (
    CollateralTokenProcessor,
    DebtTokenProcessor,
    GhoDebtTokenProcessor,
)
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor
from degenbot.aave.processors.collateral.v3 import CollateralV3Processor
from degenbot.aave.processors.collateral.v4 import CollateralV4Processor
from degenbot.aave.processors.collateral.v5 import CollateralV5Processor
from degenbot.aave.processors.debt.gho.v1 import GhoV1Processor
from degenbot.aave.processors.debt.gho.v2 import GhoV2Processor
from degenbot.aave.processors.debt.gho.v4 import GhoV4Processor
from degenbot.aave.processors.debt.gho.v5 import GhoV5Processor
from degenbot.aave.processors.debt.v1 import DebtV1Processor
from degenbot.aave.processors.debt.v3 import DebtV3Processor
from degenbot.aave.processors.debt.v4 import DebtV4Processor
from degenbot.aave.processors.debt.v5 import DebtV5Processor
from degenbot.logging import logger


class TokenProcessorFactory:
    """Factory for creating token processors by revision number."""

    # AToken revisions: 1-5
    COLLATERAL_PROCESSORS: ClassVar[dict[int, type[CollateralTokenProcessor]]] = {
        1: CollateralV1Processor,
        2: CollateralV1Processor,  # Same as rev 1
        3: CollateralV3Processor,
        4: CollateralV4Processor,
        5: CollateralV5Processor,
    }

    # VToken revisions: 1-5 (standard vTokens, non-GHO)
    DEBT_PROCESSORS: ClassVar[dict[int, type[DebtTokenProcessor]]] = {
        1: DebtV1Processor,
        2: DebtV1Processor,  # Same as rev 1
        3: DebtV3Processor,
        4: DebtV4Processor,
        5: DebtV5Processor,
    }

    # GHO VariableDebtToken revisions: 1-6
    # GHO is special because it has discount handling
    # rev 2-3 share implementation (discount support)
    # rev 4 uses standard rayDiv (discount deprecated, no floor division)
    # rev 5+ uses explicit floor/ceil division (discount deprecated)
    GHO_DEBT_PROCESSORS: ClassVar[dict[int, type[Any]]] = {
        1: GhoV1Processor,
        2: GhoV2Processor,
        3: GhoV2Processor,  # Same as rev 2
        4: GhoV4Processor,
        5: GhoV5Processor,
        6: GhoV5Processor,  # Same as rev 5
    }

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
        processor_class = cls.COLLATERAL_PROCESSORS.get(revision)
        if processor_class is None:
            msg = f"No processor for collateral revision {revision}"
            raise ValueError(msg)
        processor = processor_class()
        logger.debug(
            f"Created {processor_class.__name__} for aToken revision {revision} "
            f"(math lib: {processor.math_lib_version})"
        )
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
        processor_class = cls.DEBT_PROCESSORS.get(revision)
        if processor_class is None:
            msg = f"No processor for debt revision {revision}"
            raise ValueError(msg)
        processor = processor_class()
        logger.debug(
            f"Created {processor_class.__name__} for vToken revision {revision} "
            f"(math lib: {processor.math_lib_version})"
        )
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
        processor_class = cls.GHO_DEBT_PROCESSORS.get(revision)
        if processor_class is None:
            msg = f"No processor for GHO revision {revision}"
            raise ValueError(msg)
        processor = cast("GhoDebtTokenProcessor", processor_class())
        logger.debug(
            f"Created {processor_class.__name__} for GHO vToken revision {revision} "
            f"(math lib: {processor.math_lib_version})"
        )
        return processor
