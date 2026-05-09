"""Tests verifying Erc20TokenManager has been removed.

The deprecated Erc20TokenManager class has been replaced by
Bot.build_erc20token() / Bot.get_token(). These tests ensure
the old class is no longer importable.
"""

import importlib

import degenbot


def test_erc20_token_manager_not_in_top_level_exports() -> None:
    """Erc20TokenManager should not be in degenbot.__all__."""
    assert "Erc20TokenManager" not in degenbot.__all__


def test_erc20_token_manager_not_importable_from_erc20() -> None:
    """Erc20TokenManager should not be importable from degenbot.erc20."""
    erc20_module = importlib.import_module("degenbot.erc20")
    assert not hasattr(erc20_module, "Erc20TokenManager")


def test_erc20_token_manager_not_importable_from_top_level() -> None:
    """Erc20TokenManager should not be importable from degenbot top-level."""
    assert not hasattr(degenbot, "Erc20TokenManager")
