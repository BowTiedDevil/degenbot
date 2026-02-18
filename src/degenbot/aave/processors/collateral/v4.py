"""Collateral token processor for revision 4."""

from typing import TYPE_CHECKING

import degenbot.aave.libraries.v3_4 as aave_library_v3_4
from degenbot.aave.processors.base import (
    CollateralTokenProcessor,
    CollateralBurnEvent,
    CollateralMintEvent,
    MathLibraries,
)
from degenbot.aave.processors.collateral.v1 import CollateralV1Processor

if TYPE_CHECKING:
    from degenbot.database.models.aave import AaveV3CollateralPositionsTable


class CollateralV4Processor(CollateralV1Processor):
    """Processor for AToken revision 4."""

    revision = 4

    def __init__(self) -> None:
        """Initialize with v3.4 math libraries."""
        self._math_libs = MathLibraries(
            wad_ray=aave_library_v3_4.wad_ray_math,
            percentage=aave_library_v3_4.percentage_math,
        )

    def process_mint_event(
        self,
        event_data: CollateralMintEvent,
        position: "AaveV3CollateralPositionsTable",
        scaled_delta: int | None = None,
    ) -> tuple[int, bool]:
        """Process a collateral mint event using v3.4 math."""
        return super().process_mint_event(event_data, position, scaled_delta)

    def process_burn_event(
        self,
        event_data: CollateralBurnEvent,
        position: "AaveV3CollateralPositionsTable",
    ) -> int:
        """Process a collateral burn event using v3.4 math."""
        return super().process_burn_event(event_data, position)
