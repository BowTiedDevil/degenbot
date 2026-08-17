"""Tests for BuilderContext."""

import dataclasses

import pytest

from degenbot._ffi import Bot
from degenbot.builders.context import BuilderContext
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.registry import PoolRegistry, TokenRegistry


def _make_ctx(**overrides) -> BuilderContext:
    """Create a BuilderContext with fakes for required fields."""
    fake_db = object.__new__(DatabaseSessionManager)
    fake_pools = object.__new__(PoolRegistry)
    fake_tokens = object.__new__(TokenRegistry)
    fake_erc20 = object.__new__(Erc20Builder)

    defaults = {
        "db": fake_db,
        "pools": fake_pools,
        "tokens": fake_tokens,
        "erc20_builder": fake_erc20,
        "py_bot": Bot(),
        "default_chain_id": 1,
    }
    defaults.update(overrides)
    return BuilderContext(**defaults)


class TestBuilderContextConstruction:
    """BuilderContext can be constructed with required and optional fields."""

    def test_required_fields(self) -> None:
        ctx = _make_ctx()
        assert ctx.db is not None
        assert ctx.pools is not None
        assert ctx.tokens is not None
        assert ctx.erc20_builder is not None
        assert isinstance(ctx.py_bot, Bot)
        assert ctx.default_chain_id == 1

    def test_frozen(self) -> None:
        ctx = _make_ctx()
        with pytest.raises(dataclasses.FrozenInstanceError):
            ctx.db = None  # type: ignore[misc]

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
            "db",
            "pools",
            "tokens",
            "erc20_builder",
            "py_bot",
            "default_chain_id",
        }

    def test_default_chain_id_default_is_none(self) -> None:
        """default_chain_id can be None if not yet configured."""
        ctx = _make_ctx(default_chain_id=None)
        assert ctx.default_chain_id is None
