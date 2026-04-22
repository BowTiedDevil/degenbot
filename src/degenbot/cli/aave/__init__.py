"""Aave V3 CLI subpackage for market management and position tracking."""

# Re-export types
# Import commands module to register CLI commands
# This must happen after other imports to avoid circular dependencies
from degenbot.cli.aave import commands

# Re-export the aave CLI group
from degenbot.cli.aave.commands import aave

# Re-export constants
from degenbot.cli.aave.constants import (
    AAVE_EVENT_TOPIC_TO_CATEGORY,
    GHO_DISCOUNT_DEPRECATION_REVISION,
    POSITION_RISK_DISPLAY_LIMIT,
    SCALED_AMOUNT_POOL_REVISION,
    UserOperation,
    WadRayMathLibrary,
)

# Re-export event fetchers
from degenbot.cli.aave.event_fetchers import (
    fetch_address_provider_events,
    fetch_discount_config_events,
    fetch_oracle_events,
    fetch_pool_events,
    fetch_reserve_initialization_events,
    fetch_scaled_token_events,
    fetch_stk_aave_events,
)
from degenbot.cli.aave.types import TokenType, TransactionContext

__all__ = [
    "AAVE_EVENT_TOPIC_TO_CATEGORY",
    "GHO_DISCOUNT_DEPRECATION_REVISION",
    "POSITION_RISK_DISPLAY_LIMIT",
    "SCALED_AMOUNT_POOL_REVISION",
    "TokenType",
    "TransactionContext",
    "UserOperation",
    "WadRayMathLibrary",
    "aave",
    "commands",
    "fetch_address_provider_events",
    "fetch_discount_config_events",
    "fetch_oracle_events",
    "fetch_pool_events",
    "fetch_reserve_initialization_events",
    "fetch_scaled_token_events",
    "fetch_stk_aave_events",
]
