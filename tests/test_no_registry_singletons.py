"""
Test that the registry module does not export module-level singletons.

Users should construct their own registry instances or use Bot-owned registries.
Importing pool_registry, token_registry, or managed_pool_registry from degenbot.registry
should raise AttributeError.
"""

import importlib

import pytest


def test_registry_module_has_no_pool_registry_singleton() -> None:
    import degenbot.registry

    with pytest.raises(AttributeError):
        degenbot.registry.pool_registry  # type: ignore[attr-defined]


def test_registry_module_has_no_token_registry_singleton() -> None:
    import degenbot.registry

    with pytest.raises(AttributeError):
        degenbot.registry.token_registry  # type: ignore[attr-defined]


def test_registry_module_has_no_managed_pool_registry_singleton() -> None:
    import degenbot.registry

    with pytest.raises(AttributeError):
        degenbot.registry.managed_pool_registry  # type: ignore[attr-defined]


def test_registry_module_still_exports_classes() -> None:
    """The registry classes (PoolRegistry, TokenRegistry, ManagedPoolRegistry) remain importable."""
    from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry

    assert PoolRegistry is not None
    assert TokenRegistry is not None
    assert ManagedPoolRegistry is not None


def test_top_level_init_has_no_registry_singletons() -> None:
    """from degenbot import pool_registry etc. should fail."""
    import degenbot

    with pytest.raises(AttributeError):
        degenbot.pool_registry  # type: ignore[attr-defined]

    with pytest.raises(AttributeError):
        degenbot.token_registry  # type: ignore[attr-defined]

    with pytest.raises(AttributeError):
        degenbot.managed_pool_registry  # type: ignore[attr-defined]
