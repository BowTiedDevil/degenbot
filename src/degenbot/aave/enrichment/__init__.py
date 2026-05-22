"""Main enrichment service for scaled token events.

Dispatches to handler-based implementation using the OperationHandler
pipeline. Each OperationType has a dedicated handler module.
"""

from degenbot.aave.enrichment.core import ScaledEventEnricher

__all__ = ["ScaledEventEnricher"]
