"""Factory for creating token processors by revision."""

from typing import ClassVar

from degenbot.aave.processors.base import (
    CollateralTokenProcessor,
    DebtTokenProcessor,
    GhoTokenProcessor,
)
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor
from degenbot.aave.processors.collateral.v3 import CollateralV3Processor
from degenbot.aave.processors.collateral.v4 import CollateralV4Processor
from degenbot.aave.processors.collateral.v5 import CollateralV5Processor
from degenbot.aave.processors.debt.gho.v1 import GhoV1Processor
from degenbot.aave.processors.debt.gho.v2 import GhoV2Processor
from degenbot.aave.processors.debt.gho.v4 import GhoV4Processor
from degenbot.aave.processors.debt.v1 import DebtV1Processor
from degenbot.aave.processors.debt.v3 import DebtV3Processor
from degenbot.aave.processors.debt.v4 import DebtV4Processor
from degenbot.aave.processors.debt.v5 import DebtV5Processor


class TokenProcessorFactory:
    """Factory for creating token processors by revision number."""

    # AToken revisions: 1, 3, 4, 5 (rev 2 was skipped)
    COLLATERAL_PROCESSORS: ClassVar[dict[int, type[CollateralTokenProcessor]]] = {
        1: CollateralV1Processor,
        3: CollateralV3Processor,
        4: CollateralV4Processor,
        5: CollateralV5Processor,
    }

    # VToken revisions: 1, 3, 4, 5 (rev 2 was skipped)
    DEBT_PROCESSORS: ClassVar[dict[int, type[DebtTokenProcessor]]] = {
        1: DebtV1Processor,
        3: DebtV3Processor,
        4: DebtV4Processor,
        5: DebtV5Processor,
    }

    # GHO VariableDebtToken revisions: 1-6
    # rev 2-3 share implementation (discount support)
    # rev 4+ share implementation (discount deprecated)
    GHO_PROCESSORS: ClassVar[dict[int, type[GhoTokenProcessor]]] = {
        1: GhoV1Processor,
        2: GhoV2Processor,
        3: GhoV2Processor,  # Same as rev 2
        4: GhoV4Processor,
        5: GhoV4Processor,  # Same as rev 4
        6: GhoV4Processor,  # Same as rev 4
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
        return processor_class()

    @classmethod
    def get_debt_processor(cls, revision: int) -> DebtTokenProcessor:
        """Get processor for debt (vToken) by revision.

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
        return processor_class()

    @classmethod
    def get_gho_processor(cls, revision: int) -> GhoTokenProcessor:
        """Get processor for GHO variable debt token by revision.

        Args:
            revision: The GHO vToken revision number

        Returns:
            Processor instance for the revision

        Raises:
            ValueError: If revision is not supported
        """
        processor_class = cls.GHO_PROCESSORS.get(revision)
        if processor_class is None:
            msg = f"No processor for GHO revision {revision}"
            raise ValueError(msg)
        return processor_class()
