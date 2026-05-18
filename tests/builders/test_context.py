"""Tests for BuilderContext."""

import dataclasses

import pytest

from degenbot.builders.context import BuilderContext
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry


def _make_ctx(**overrides) -> BuilderContext:
    """Create a BuilderContext with fakes for required fields."""
    fake_connections = object.__new__(ConnectionManager)
    fake_db = object.__new__(DatabaseSessionManager)
    fake_pools = object.__new__(PoolRegistry)
    fake_tokens = object.__new__(TokenRegistry)
    fake_erc20 = object.__new__(Erc20Builder)

    defaults = {
        "connections": fake_connections,
        "db": fake_db,
        "pools": fake_pools,
        "tokens": fake_tokens,
        "erc20_builder": fake_erc20,
        "managed_pools": None,
    }
    defaults.update(overrides)
    return BuilderContext(**defaults)


class TestBuilderContextConstruction:
    """BuilderContext can be constructed with required and optional fields."""

    def test_required_fields(self) -> None:
        ctx = _make_ctx()
        assert ctx.connections is not None
        assert ctx.db is not None
        assert ctx.pools is not None
        assert ctx.tokens is not None
        assert ctx.erc20_builder is not None
        assert ctx.managed_pools is None

    def test_managed_pools_optional(self) -> None:
        fake_managed = object.__new__(ManagedPoolRegistry)
        ctx = _make_ctx(managed_pools=fake_managed)
        assert ctx.managed_pools is not None

    def test_frozen(self) -> None:
        ctx = _make_ctx()
        with pytest.raises(dataclasses.FrozenInstanceError):
            ctx.connections = None  # type: ignore[misc]

    def test_slots_frozen_blocks_new_attrs(self) -> None:
        ctx = _make_ctx()
        # Frozen dataclass blocks all attribute assignment
        with pytest.raises((dataclasses.FrozenInstanceError, TypeError)):
            ctx.nonexistent = 42  # type: ignore[attr-defined]

    def test_field_count(self) -> None:
        fields = dataclasses.fields(BuilderContext)
        assert len(fields) == 6
        field_names = {f.name for f in fields}
        assert field_names == {
            "connections",
            "db",
            "pools",
            "tokens",
            "erc20_builder",
            "managed_pools",
        }

    def test_managed_pools_default_is_none(self) -> None:
        ctx = _make_ctx()
        assert ctx.managed_pools is None
