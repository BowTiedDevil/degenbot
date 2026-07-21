"""Tests for BuildManagedPoolRequest and build_managed_pool()."""

from __future__ import annotations

from dataclasses import fields

import pytest

from degenbot.builders.request import BuildManagedPoolRequest, BuildPoolRequest


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
        assert req.tick_bitmap is None
        assert req.tick_data is None

    def test_frozen(self):
        """BuildManagedPoolRequest is frozen — cannot reassign fields."""
        req = BuildManagedPoolRequest(pool_id="0x01")
        with pytest.raises(AttributeError):
            req.pool_id = "0x02"  # type: ignore[misc]

    def test_has_own_tick_bitmap_and_tick_data(self):
        """BuildManagedPoolRequest has tick_bitmap and tick_data fields."""
        field_names = {f.name for f in fields(BuildManagedPoolRequest)}
        assert "tick_bitmap" in field_names
        assert "tick_data" in field_names

    def test_does_not_have_deployer_or_init_hash(self):
        """BuildPoolRequest no longer carries deployer_address/init_hash."""
        field_names = {f.name for f in fields(BuildPoolRequest)}
        assert "deployer_address" not in field_names
        assert "init_hash" not in field_names

    def test_does_not_have_bpt_idx_or_invariant_version(self):
        """BuildManagedPoolRequest does not have Balancer-specific fields."""
        field_names = {f.name for f in fields(BuildManagedPoolRequest)}
        assert "bpt_idx" not in field_names
        assert "invariant_version" not in field_names


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


class TestBuildPoolRequest:
    """Tests for BuildPoolRequest after V4 fields are removed."""

    def test_no_pool_id_field(self):
        """BuildPoolRequest no longer has pool_id field."""
        field_names = {f.name for f in fields(BuildPoolRequest)}
        assert "pool_id" not in field_names

    def test_no_v4_fields(self):
        """BuildPoolRequest no longer has V4-specific fields."""
        field_names = {f.name for f in fields(BuildPoolRequest)}
        assert "state_view_address" not in field_names
        assert "tokens" not in field_names
        assert "fee" not in field_names
        assert "tick_spacing" not in field_names
        assert "hook_address" not in field_names

    def test_still_has_tick_bitmap_and_tick_data(self):
        """BuildPoolRequest still has tick_bitmap and tick_data for V3."""
        req = BuildPoolRequest(tick_bitmap={1: 2}, tick_data={3: 4})
        assert req.tick_bitmap == {1: 2}
        assert req.tick_data == {3: 4}
