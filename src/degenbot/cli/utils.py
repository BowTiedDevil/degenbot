"""CLI utility helpers (formatting, output)."""

from degenbot.provider.factory import (
    _get_use_alloy_from_env,
    get_provider_from_config,  # re-export (ADR-006 D5): canonical factory lives in the lib layer
)

__all__ = ["_get_use_alloy_from_env", "get_provider_from_config"]
