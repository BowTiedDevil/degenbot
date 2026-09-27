"""Test helpers for degenbot.

A fresh interpreter is for claims that only exist at process level: import
cost, exit codes, argv, transport. Resolution claims go through
``resolve_hypothetical``; installed-verdict claims through ``resolved_config``.
"""

from pathlib import Path

test_helpers_dir = Path(__file__).parent
fixtures_dir = test_helpers_dir.parent / "fixtures"
chain_data_dir = fixtures_dir / "chain_data"

__all__ = (
    "chain_data_dir",
    "fixtures_dir",
    "test_helpers_dir",
)
