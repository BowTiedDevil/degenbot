"""UNKNOWN operation handler.

UNKNOWN operations are those that cannot be classified into a specific
operation type. Enrichment should fail rather than produce incorrect results.
"""

from typing import TYPE_CHECKING, ClassVar

from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class UnknownHandler:
    """Handle enrichment for UNKNOWN operations."""

    operation_types: ClassVar[set[OperationType]] = {OperationType.UNKNOWN}

    def handle(  # noqa: PLR6301
        self,
        event: "ScaledTokenEvent",  # noqa: ARG002
        operation: "Operation",
        context: "EnrichmentContext",  # noqa: ARG002
    ) -> "EnrichedScaledTokenEvent":
        """
        Raise EnrichmentError for UNKNOWN operations.

        When an operation cannot be classified, enrichment should fail
        rather than produce incorrect results.
        """
        msg = f"Cannot enrich UNKNOWN operation: {operation}"
        raise EnrichmentError(msg)
