"""Thin orchestrator for event enrichment."""

from typing import TYPE_CHECKING

from sqlalchemy.orm import Session

from degenbot.aave.enrichment.context import EnrichmentContext
from degenbot.aave.enrichment.handlers import HANDLER_REGISTRY
from degenbot.aave.models import EnrichedScaledTokenEvent

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.aave.operations import Operation, ScaledTokenEvent


class ScaledEventEnricher:
    """Enriches ScaledTokenEvent with calculated scaled amounts.

    This is the main entry point for event enrichment. It dispatches
    to appropriate handlers based on operation type.

    Each handler:
    1. Extracts raw amounts from Pool events
    2. Calculates scaled amounts using TokenMath
    3. Creates validated Pydantic event objects
    4. Raises immediately on any error
    """

    def __init__(
        self,
        pool_revision: int,
        token_revisions: dict["ChecksumAddress", int],
        session: Session,
    ) -> None:
        """Initialize the instance."""
        self.pool_revision = pool_revision
        self.token_revisions = token_revisions
        self.session = session

    def enrich(
        self,
        scaled_event: "ScaledTokenEvent",
        operation: "Operation",
    ) -> EnrichedScaledTokenEvent:
        """Enrich a single ScaledTokenEvent.

        Args:
            scaled_event: The raw ScaledTokenEvent to enrich
            operation: The Operation containing context

        Returns:
            EnrichedScaledTokenEvent with validated scaled amounts

        Raises:
            EnrichmentError: If extraction or calculation fails

        """
        handler = HANDLER_REGISTRY.get(operation.operation_type)
        if handler is None:
            msg = f"No handler for operation type: {operation.operation_type}"
            raise NotImplementedError(msg)

        context = EnrichmentContext(
            pool_revision=self.pool_revision,
            token_revisions=self.token_revisions,
            session=self.session,
        )

        return handler.handle(scaled_event, operation, context)
