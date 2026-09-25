"""Tests for the parameterized pool updater configs."""

from dataclasses import FrozenInstanceError
from pathlib import Path
from types import SimpleNamespace
from typing import get_type_hints

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import ExchangeRow, PoolManagerRow, db_upsert_exchange, db_upsert_pool_manager
from degenbot.updater.pool_updater_configs import (
    PoolUpdateRequest,
    V2PoolUpdateConfig,
    V3PoolUpdateConfig,
    V4PoolUpdateConfig,
    apply_v3_liquidity_updates,
    apply_v4_liquidity_updates,
)


UPDATER_SOURCE = Path(__file__).parents[2] / "src/degenbot/updater/pool_updater_configs.py"


def test_pool_updater_config_module_has_no_sqlalchemy_model_import() -> None:
    assert "degenbot.database.models" not in UPDATER_SOURCE.read_text()


def test_pool_updater_requests_accept_rust_backed_rows(tmp_path: Path) -> None:
    exchange = db_upsert_exchange(
        database_path=str(tmp_path / "updater.db"),
        chain_id=1,
        name="test_exchange",
        factory=get_checksum_address("0x" + "f" * 40),
        deployer=None,
    )
    manager = db_upsert_pool_manager(
        database_path=str(tmp_path / "updater.db"),
        address=get_checksum_address("0x" + "e" * 40),
        chain=1,
        kind="uniswap_v4",
        state_view=None,
        exchange_id=exchange.id,
    )
    request = PoolUpdateRequest(
        provider=SimpleNamespace(chain_id=1),
        start_block=1,
        end_block=2,
        exchange=exchange,
        database_path=str(tmp_path / "updater.db"),
        config=V2PoolUpdateConfig(
            name="test_v2",
            event_hash=b"\x01",
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
        ),
        get_events_fn=lambda **_: [],
    )

    assert request.exchange is exchange
    assert get_type_hints(PoolUpdateRequest)["exchange"] is ExchangeRow
    assert get_type_hints(apply_v3_liquidity_updates)["exchanges_in_scope"] == set[ExchangeRow]
    assert get_type_hints(apply_v4_liquidity_updates)["pool_manager"] is PoolManagerRow

    apply_v3_liquidity_updates(
        provider=SimpleNamespace(chain_id=1),
        pool_address=get_checksum_address("0x" + "1" * 40),
        liquidity_events=[],
        exchanges_in_scope={exchange},
        database_path=str(tmp_path / "updater.db"),
    )
    apply_v4_liquidity_updates(
        pool_id=bytes.fromhex("c" * 64),
        liquidity_events=[],
        pool_manager=manager,
        database_path=str(tmp_path / "updater.db"),
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
