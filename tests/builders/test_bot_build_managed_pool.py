"""Tests for Bot.build_managed_pool() and the sibling build_pool() surface.

Both methods are LIVE and their signatures are the contract this module pins: a
caller builds through ``build_managed_pool(address, request)`` and through
``build_pool(address, request)``, so the parameter order, the required request
argument, and the declared request fields are present-code facts.
"""

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


class TestBuildPoolSignature:
    """The generic build_pool() entry point takes the address plus keyword knobs."""

    def test_build_pool_exists(self):
        """Bot.build_pool() is the generic construction door."""
        assert hasattr(Bot, "build_pool")

    def test_build_pool_takes_a_positional_address_and_keyword_knobs(self):
        """build_pool() takes the address positionally and its knobs keyword-only."""
        sig = inspect.signature(Bot.build_pool)
        params = list(sig.parameters.values())
        assert params[1].name == "address"
        assert params[1].default is inspect.Parameter.empty
        knobs = {p.name: p for p in params[2:]}
        assert {"state_block", "silent", "construction_route"} <= set(knobs)
        assert all(p.kind is inspect.Parameter.KEYWORD_ONLY for p in knobs.values())
