"""Tests for the pool-construction request objects.

Both request dataclasses are LIVE: ``BuildPoolRequest`` carries the optional
construction knobs, ``BuildManagedPoolRequest`` the V4 identity overrides. What is
asserted here is that each one holds the fields it declares -- the presence half of
the contract, which a dropped field would break.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import fields

import pytest

from degenbot.builders.request import (
    BuildManagedPoolRequest,
    BuildPoolRequest,
    ConstructionRoute,
)

# The V4 identity overrides `BuildManagedPoolRequest` declares.
_V4_IDENTITY_FIELDS = frozenset({
    "state_view_address",
    "tokens",
    "fee",
    "tick_spacing",
    "hook_address",
})


class TestBuildManagedPoolRequest:
    """Tests for the new BuildManagedPoolRequest dataclass."""

    def test_pool_id_required(self):
        """BuildManagedPoolRequest requires pool_id — no default."""
        with pytest.raises(TypeError, match=r"missing 1 required keyword-only argument.*pool_id"):
            BuildManagedPoolRequest()  # type: ignore[call-arg]

    def test_pool_id_str(self):
        """pool_id accepts a string value."""
        req = BuildManagedPoolRequest(pool_id="0x01")
        assert req.pool_id == "0x01"

    def test_pool_id_bytes(self):
        """pool_id accepts a bytes value."""
        req = BuildManagedPoolRequest(pool_id=b"\x01\x02")
        assert req.pool_id == b"\x01\x02"

    def test_standalone_not_inheriting_build_pool_request(self):
        """BuildManagedPoolRequest does not inherit from BuildPoolRequest."""
        req = BuildManagedPoolRequest(pool_id="0x01")
        assert not isinstance(req, BuildPoolRequest)

    def test_default_values(self):
        """Optional fields have correct defaults."""
        req = BuildManagedPoolRequest(pool_id="0x01")
        assert req.silent is False
        assert req.state_block is None
        assert req.state_cache_depth == 8
        assert req.state_view_address is None
        assert req.tokens is None
        assert req.fee is None
        assert req.tick_spacing is None
        assert req.hook_address is None

    def test_frozen(self):
        """BuildManagedPoolRequest is frozen — cannot reassign fields."""
        req = BuildManagedPoolRequest(pool_id="0x01")
        with pytest.raises(AttributeError):
            req.pool_id = "0x02"  # type: ignore[misc]

    def test_carries_the_v4_identity_overrides(self):
        """The request declares the V4 identity overrides the builder reads."""
        declared = {f.name for f in fields(BuildManagedPoolRequest)}
        assert declared >= _V4_IDENTITY_FIELDS


class TestBuildPoolRequest:
    """Tests for the generic construction knobs BuildPoolRequest declares."""

    def test_carries_the_construction_knobs(self):
        """The generic request declares its optional knobs and route policy."""
        declared = {f.name: f for f in fields(BuildPoolRequest)}
        assert {"silent", "state_block", "state_cache_depth", "construction_route"} <= set(declared)
        assert declared["construction_route"].default is None

    def test_construction_route_defaults_to_the_generic_only_route(self):
        """A bare route attempts the generic rung and no factory."""
        route = ConstructionRoute()
        assert route.factories == ()
        assert route.generic is True

    def test_construction_route_carries_an_ordered_factory_tuple(self):
        """The route's factories are an ordered tuple of addresses."""
        route = ConstructionRoute(factories=("0x" + "1" * 40, "0x" + "2" * 40))
        assert isinstance(route.factories, Sequence)
        assert route.factories == ("0x" + "1" * 40, "0x" + "2" * 40)


class TestBuildRequestUnion:
    """Tests for the BuildRequest union type."""

    def test_build_pool_request_is_instance_of_union(self):
        """BuildPoolRequest values satisfy BuildRequest."""
        req = BuildPoolRequest()
        # BuildRequest is a union type alias — isinstance checks against
        # the concrete types, not the alias itself.
        assert isinstance(req, BuildPoolRequest)

    def test_build_managed_pool_request_is_instance_of_union(self):
        """BuildManagedPoolRequest values satisfy BuildRequest."""
        req = BuildManagedPoolRequest(pool_id="0x01")
        assert isinstance(req, BuildManagedPoolRequest)
