"""Collateral token processor for revision 3."""

from typing import TYPE_CHECKING

import degenbot.aave.libraries.v3_3 as aave_library_v3_3
from degenbot.aave.processors.base import (
    CollateralTokenProcessor,
    CollateralBurnEvent,
    CollateralMintEvent,
    MathLibraries,
)
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor

if TYPE_CHECKING:
    from degenbot.database.models.aave import AaveV3CollateralPositionsTable


class CollateralV3Processor(CollateralV1Processor):
    """Processor for AToken revision 3."""

    revision = 3

    def __init__(self) -> None:
        """Initialize with v3.3 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_3.wad_ray_math,
            percentage=aave_library_v3_3.percentage_math,
        )

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        position: "AaveV3CollateralPositionsTable",
        scaled_delta: int | None = None,
    ) -> tuple[int, bool]:
        """Process a collateral mint event using v3.3 math."""
        # Revisions 1 and 3 have identical mint logic, only math library differs
        return super().process_mint_event(event_data, position, scaled_delta)

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        position: "AaveV3CollateralPositionsTable",
    ) -> int:
        """Process a collateral burn event using v3.3 math."""
        # Revisions 1 and 3 have identical burn logic, only math library differs
        return super().process_burn_event(event_data, position)
