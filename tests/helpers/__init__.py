"""Test helpers for degenbot."""

from pathlib import Path

test_helpers_dir = Path(__file__).parent
fixtures_dir = test_helpers_dir.parent / "fixtures"
chain_data_dir = fixtures_dir / "chain_data"

__all__ = (
    "chain_data_dir",
    "fixtures_dir",
    "test_helpers_dir",
)
