"""Tests for Bot.build_managed_pool()."""

from __future__ import annotations

import dataclasses
import inspect

from degenbot.bot import Bot
from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


class TestBuildManagedPoolSignature:
    """Tests for the build_managed_pool() method signatures."""

    def test_bot_has_build_managed_pool(self):
        """Bot class has a build_managed_pool method."""
        assert hasattr(Bot, "build_managed_pool")

    def test_build_managed_pool_takes_address_and_request(self):
        """build_managed_pool() takes (address, request) positionally after self."""
        sig = inspect.signature(Bot.build_managed_pool)
        params = list(sig.parameters.values())
        assert params[1].name == "address"
        assert params[2].name == "request"
        assert params[2].default is inspect.Parameter.empty

    def test_build_managed_pool_request_requires_pool_id(self):
        """BuildManagedPoolRequest has a required pool_id (no default)."""
        fields = {f.name: f for f in dataclasses.fields(BuildManagedPoolRequest)}
        assert fields["pool_id"].default is dataclasses.MISSING

    def test_build_managed_pool_has_no_deployer_or_init_hash_kwargs(self):
        """build_managed_pool() does not accept deployer_address or init_hash."""
        sig = inspect.signature(Bot.build_managed_pool)
        param_names = set(sig.parameters.keys())
        assert "deployer_address" not in param_names
        assert "init_hash" not in param_names

    def test_build_managed_pool_request_has_v4_fields(self):
        """The request carries the V4-specific identity overrides."""
        field_names = {f.name for f in dataclasses.fields(BuildManagedPoolRequest)}
        assert "state_view_address" in field_names
        assert "tokens" in field_names
        assert "fee" in field_names
        assert "tick_spacing" in field_names
        assert "hook_address" in field_names

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
