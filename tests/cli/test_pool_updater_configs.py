"""Tests for the parameterized pool updater configs."""

from dataclasses import FrozenInstanceError

import pytest

from degenbot.cli.pool_updater_configs import (
    V2PoolUpdateConfig,
    V3PoolUpdateConfig,
    V4PoolUpdateConfig,
)


class TestV2PoolUpdateConfig:
    """Tests for V2PoolUpdateConfig frozen dataclass."""

    def test_frozen(self):
        config = V2PoolUpdateConfig(
            name="test_v2",
            event_hash=b"\x01",
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
        )
        with pytest.raises(FrozenInstanceError):
            config.name = "other"

    def test_defaults(self):
        config = V2PoolUpdateConfig(
            name="test_v2",
            event_hash=b"\x01",
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
        )
        assert config.has_stable_flag is False
        assert config.rpc_fee_call is None
        assert config.rpc_fee_return_types == ["uint256"]
        assert config.rpc_fee_includes_stable is False

    def test_aerodrome_style_config(self):
        """V2 config with stable flag and RPC fee call (Aerodrome pattern)."""
        config = V2PoolUpdateConfig(
            name="aerodrome_v2",
            event_hash=b"\x01",
            fee_token0=0,
            fee_token1=0,
            fee_denominator=10_000,
            has_stable_flag=True,
            rpc_fee_call="getFee(address,bool)",
            rpc_fee_return_types=["uint256"],
            rpc_fee_includes_stable=True,
        )
        assert config.has_stable_flag is True
        assert config.rpc_fee_call == "getFee(address,bool)"
        assert config.rpc_fee_includes_stable is True


class TestV3PoolUpdateConfig:
    """Tests for V3PoolUpdateConfig frozen dataclass."""

    def test_frozen(self):
        config = V3PoolUpdateConfig(
            name="test_v3",
            event_hash=b"\x01",
            fee_denominator=1_000_000,
        )
        with pytest.raises(FrozenInstanceError):
            config.name = "other"

    def test_defaults(self):
        config = V3PoolUpdateConfig(
            name="test_v3",
            event_hash=b"\x01",
            fee_denominator=1_000_000,
        )
        assert config.rpc_fee_call is None
        assert config.rpc_fee_return_types == ["uint24"]

    def test_aerodrome_v3_style_config(self):
        """V3 config with RPC fee call (Aerodrome V3 pattern)."""
        config = V3PoolUpdateConfig(
            name="aerodrome_v3",
            event_hash=b"\x01",
            fee_denominator=1_000_000,
            rpc_fee_call="getSwapFee(address)",
            rpc_fee_return_types=["uint24"],
        )
        assert config.rpc_fee_call == "getSwapFee(address)"


class TestV4PoolUpdateConfig:
    """Tests for V4PoolUpdateConfig frozen dataclass."""

    def test_frozen(self):
        config = V4PoolUpdateConfig(
            name="test_v4",
            event_hash=b"\x01",
            fee_denominator=1_000_000,
        )
        with pytest.raises(FrozenInstanceError):
            config.name = "other"
