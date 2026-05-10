"""
Main enrichment service for scaled token events.

Dispatches to legacy or new handler-based implementation based on
the DEGENBOT_NEW_AAVE_ENRICHMENT environment variable.
"""

import os
from typing import TYPE_CHECKING

from sqlalchemy.orm import Session

from degenbot.aave.models import EnrichedScaledTokenEvent

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.aave.enrichment._legacy import (
        ScaledEventEnricher as LegacyEnricher,
    )
    from degenbot.aave.enrichment.core import (
        ScaledEventEnricher as NewEnricher,
    )
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent

# Feature flag: use new handler-based enrichment if env var is set
_USE_NEW_ENRICHMENT = os.environ.get("DEGENBOT_NEW_AAVE_ENRICHMENT", "").lower() in {
    "1",
    "true",
    "yes",
}


class ScaledEventEnricher:
    """
    Enriches ScaledTokenEvent with calculated scaled amounts.

    Dispatches to the appropriate implementation based on the
    DEGENBOT_NEW_AAVE_ENRICHMENT environment variable.

    If DEGENBOT_NEW_AAVE_ENRICHMENT is set (1, true, yes), uses the
    new handler-based implementation. Otherwise, uses the legacy
    monolithic implementation.
    """

    def __init__(
        self,
        pool_revision: int,
        token_revisions: dict["ChecksumAddress", int],
        session: Session,
    ) -> None:
        self.pool_revision = pool_revision
        self.token_revisions = token_revisions
        self.session = session

        # Lazy initialization of the actual enricher
        self._enricher: NewEnricher | LegacyEnricher | None = None

    def _get_enricher(self) -> "NewEnricher | LegacyEnricher":
        """Get the appropriate enricher implementation."""
        if self._enricher is None:
            if _USE_NEW_ENRICHMENT:
                # Local import to avoid circular import and allow lazy loading
                from degenbot.aave.enrichment.core import (  # noqa: PLC0415
                    ScaledEventEnricher as NewEnricher,
                )

                self._enricher = NewEnricher(
                    pool_revision=self.pool_revision,
                    token_revisions=self.token_revisions,
                    session=self.session,
                )
            else:
                # Local import to avoid circular import and allow lazy loading
                from degenbot.aave.enrichment._legacy import (  # noqa: PLC0415
                    ScaledEventEnricher as LegacyEnricher,
                )

                self._enricher = LegacyEnricher(
                    pool_revision=self.pool_revision,
                    token_revisions=self.token_revisions,
                    session=self.session,
                )
        return self._enricher

    def enrich(
        self,
        scaled_event: "ScaledTokenEvent",
        operation: "Operation",
    ) -> EnrichedScaledTokenEvent:
        """
        Enrich a single ScaledTokenEvent.

        Args:
            scaled_event: The raw ScaledTokenEvent to enrich
            operation: The Operation containing context

        Returns:
            EnrichedScaledTokenEvent with validated scaled amounts

        Raises:
            EnrichmentError: If extraction or calculation fails
        """
        enricher = self._get_enricher()
        return enricher.enrich(scaled_event, operation)


__all__ = ["ScaledEventEnricher"]
