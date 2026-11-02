"""Tests for Bot.build_managed_pool() and the sibling build_pool() surface.

Both methods are LIVE and their signatures are the contract this module pins: a
caller builds through ``build_managed_pool(address, request)`` and through
``build_pool(address, request)``, so the parameter order, the required request
argument, and the declared request fields are present-code facts.
"""

from __future__ import annotations

import dataclasses
import inspect
from types import SimpleNamespace

import pytest

from degenbot.bot import Bot
from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.exceptions import HighFeePoolRejectedError
from degenbot.exceptions.base import DegenbotValueError
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


class _RaisingConstruct:
    """The FFI construction door, raising one fixed refusal."""

    def __init__(self, exc: BaseException) -> None:
        self.exc = exc
        self.calls = 0

    def construct_pool(self, request: object) -> object:
        self.calls += 1
        raise self.exc


class _EmptyManagedPools:
    def get(self, **_kwargs: object) -> None:
        return None


class _HeadOnlyIo:
    def get_block_number(self) -> int:
        return 100


def _managed_pool_shell(exc: BaseException) -> object:
    """A `Bot` stand-in exposing only what `build_managed_pool` reads pre-build."""
    return SimpleNamespace(
        chain_id=1,
        managed_pools=_EmptyManagedPools(),
        _io=_HeadOnlyIo(),
        _py_bot=_RaisingConstruct(exc),
    )


POOL_MANAGER_ADDRESS = "0x" + "11" * 20


def _build_managed(shell: object) -> object:
    return Bot.build_managed_pool(
        shell,  # type: ignore[arg-type]  # the door reads five attributes pre-build
        POOL_MANAGER_ADDRESS,
        BuildManagedPoolRequest(pool_id=bytes([0xAB]) * 32),
    )


class TestBuildManagedPoolRefusalIdentity:
    """The typed registration refusals cross this door UNCHANGED.

    The crawl classifies a build refusal by type (stable pool fact vs transient
    blip) and memoizes the stable ones; a wrap into `DegenbotValueError` here
    would erase the admission facts and make every re-sighting re-pay the build.
    """

    def test_typed_admission_refusal_is_not_flattened(self) -> None:
        exc = HighFeePoolRejectedError(
            "V4 pool fee (fee=320000) exceeds the cmd_executor's 2-byte encoding "
            "limit (65535) — excluded from arbitrage"
        )
        with pytest.raises(HighFeePoolRejectedError) as raised:
            _build_managed(_managed_pool_shell(exc))
        assert raised.value is exc, "the typed instance itself crosses, never a re-wrap"

    def test_untyped_identity_value_error_still_wraps(self) -> None:
        """A plain ValueError is the identity-resolution arm: it still surfaces as
        the documented `DegenbotValueError`."""
        with pytest.raises(DegenbotValueError):
            _build_managed(_managed_pool_shell(ValueError("V4 identity incomplete: no DB row")))


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
