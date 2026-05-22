"""Base protocol for operation handlers."""

from typing import TYPE_CHECKING, Protocol, runtime_checkable

from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.aave.operations import Operation, ScaledTokenEvent


@runtime_checkable
class OperationHandler(Protocol):
    """
    Handle enrichment for a specific OperationType.

    Each handler is a standalone module that knows how to transform
    a ScaledTokenEvent into the correct enriched model for its
    operation type. Handlers are stateless and thread-safe.
    """

    operation_types: set[OperationType]

    def handle(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent": ...
