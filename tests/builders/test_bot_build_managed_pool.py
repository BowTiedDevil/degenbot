"""Tests for Bot.build_managed_pool() and AsyncBot.build_managed_pool()."""

from __future__ import annotations

import inspect

from degenbot.async_bot import AsyncBot
from degenbot.bot import Bot
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


class TestBuildManagedPoolSignature:
    """Tests for the build_managed_pool() method signatures."""

    def test_bot_has_build_managed_pool(self):
        """Bot class has a build_managed_pool method."""
        assert hasattr(Bot, "build_managed_pool")

    def test_async_bot_has_build_managed_pool(self):
        """AsyncBot class has a build_managed_pool method."""
        assert hasattr(AsyncBot, "build_managed_pool")

    def test_build_managed_pool_pool_id_is_required_positional(self):
        """build_managed_pool() requires pool_id as the second positional arg."""
        sig = inspect.signature(Bot.build_managed_pool)
        params = list(sig.parameters.values())
        # self (positional), address (positional), pool_id (positional-or-keyword)
        assert params[0].name == "self"
        assert params[1].name == "address"
        assert params[2].name == "pool_id"
        # pool_id has no default — it's required
        assert params[2].default is inspect.Parameter.empty

    def test_build_managed_pool_has_no_deployer_or_init_hash_kwargs(self):
        """build_managed_pool() does not accept deployer_address or init_hash."""
        sig = inspect.signature(Bot.build_managed_pool)
        param_names = set(sig.parameters.keys())
        assert "deployer_address" not in param_names
        assert "init_hash" not in param_names

    def test_build_managed_pool_has_v4_kwargs(self):
        """build_managed_pool() has V4-specific keyword arguments."""
        sig = inspect.signature(Bot.build_managed_pool)
        param_names = set(sig.parameters.keys())
        assert "state_view_address" in param_names
        assert "tokens" in param_names
        assert "fee" in param_names
        assert "tick_spacing" in param_names
        assert "hook_address" in param_names

    def test_build_managed_pool_returns_uniswap_v4_pool_type(self):
        """build_managed_pool() return annotation is UniswapV4Pool."""
        sig = inspect.signature(Bot.build_managed_pool)
        # The return annotation is a string due to from __future__ import annotations
        # Check the source directly

        # We can check the annotation string
        return_annotation = sig.return_annotation
        if isinstance(return_annotation, str):
            assert "UniswapV4Pool" in return_annotation
        else:
            assert return_annotation is UniswapV4Pool

    def test_build_pool_still_exists(self):
        """Bot.build_pool() still exists (not removed in Slice 1)."""
        assert hasattr(Bot, "build_pool")

    def test_build_pool_no_longer_accepts_pool_id(self):
        """build_pool() no longer accepts pool_id kwarg after Slice 3."""
        sig = inspect.signature(Bot.build_pool)
        param_names = set(sig.parameters.keys())
        assert "pool_id" not in param_names

    def test_build_pool_no_longer_accepts_deployer_or_init_hash(self):
        """build_pool() no longer accepts deployer_address or init_hash kwargs."""
        sig = inspect.signature(Bot.build_pool)
        param_names = set(sig.parameters.keys())
        assert "deployer_address" not in param_names
        assert "init_hash" not in param_names
