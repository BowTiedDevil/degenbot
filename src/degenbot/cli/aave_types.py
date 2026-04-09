"""
Type definitions for Aave V3 CLI processing.

This module re-exports from the aave package for backwards compatibility.
New code should import directly from degenbot.cli.aave.types.
"""

# Re-export from the canonical source
from degenbot.cli.aave.types import (
    TokenType,
    TransactionContext,
)

__all__ = [
    "TokenType",
    "TransactionContext",
]
