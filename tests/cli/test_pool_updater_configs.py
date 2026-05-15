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
            database_type=type("FakeTable", (), {}),
            event_hash=b"\x01",
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
        )
        with pytest.raises(FrozenInstanceError):
            config.name = "other"  # type: ignore[mutable]

    def test_defaults(self):
        config = V2PoolUpdateConfig(
            name="test_v2",
            database_type=type("FakeTable", (), {}),
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
            database_type=type("FakeTable", (), {}),
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
            database_type=type("FakeTable", (), {}),
            event_hash=b"\x01",
            fee_denominator=1_000_000,
        )
        with pytest.raises(FrozenInstanceError):
            config.name = "other"  # type: ignore[mutable]

    def test_defaults(self):
        config = V3PoolUpdateConfig(
            name="test_v3",
            database_type=type("FakeTable", (), {}),
            event_hash=b"\x01",
            fee_denominator=1_000_000,
        )
        assert config.rpc_fee_call is None
        assert config.rpc_fee_return_types == ["uint24"]

    def test_aerodrome_v3_style_config(self):
        """V3 config with RPC fee call (Aerodrome V3 pattern)."""
        config = V3PoolUpdateConfig(
            name="aerodrome_v3",
            database_type=type("FakeTable", (), {}),
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
            database_type=type("FakeTable", (), {}),
            event_hash=b"\x01",
            fee_denominator=1_000_000,
        )
        with pytest.raises(FrozenInstanceError):
            config.name = "other"  # type: ignore[mutable]


class TestPoolUpdaterDispatch:
    """Tests for the pool.py config registration and dispatch."""

    def test_all_v2_configs_present(self):
        """All V2 DEX names are in the V2 configs dict."""
        from degenbot.cli.pool import _V2_CONFIGS

        expected = {"aerodrome_v2", "pancakeswap_v2", "sushiswap_v2", "swapbased_v2", "uniswap_v2"}
        assert set(_V2_CONFIGS.keys()) == expected

    def test_all_v3_configs_present(self):
        """All V3 DEX names are in the V3 configs dict."""
        from degenbot.cli.pool import _V3_CONFIGS

        expected = {"aerodrome_v3", "pancakeswap_v3", "sushiswap_v3", "uniswap_v3"}
        assert set(_V3_CONFIGS.keys()) == expected

    def test_all_v4_configs_present(self):
        """All V4 DEX names are in the V4 configs dict."""
        from degenbot.cli.pool import _V4_CONFIGS

        expected = {"uniswap_v4"}
        assert set(_V4_CONFIGS.keys()) == expected

    def test_pool_updater_dict_uses_single_function(self):
        """POOL_UPDATER entries all use the same dispatch function."""
        from degenbot.cli.pool import POOL_UPDATER, _pool_updater

        functions = set(POOL_UPDATER.values())
        assert functions == {_pool_updater}

    def test_pool_updater_covers_all_chains(self):
        """POOL_UPDATER has entries for both Base and Ethereum."""
        from degenbot.cli.pool import POOL_UPDATER

        chains = {chain_id for chain_id, _ in POOL_UPDATER.keys()}
        # BASE = 8453, ETH (mainnet) = 1
        assert len(chains) == 2
