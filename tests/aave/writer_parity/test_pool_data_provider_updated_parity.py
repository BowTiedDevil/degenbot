"""§4.2 live-vs-live parity: ``PoolDataProviderUpdated``.

`PoolDataProviderUpdated(address indexed oldAddress, address indexed
newAddress)` — emitted by the PoolAddressesProvider. Two apply branches:
INSERT (when `oldAddress == ZERO_ADDRESS`) or UPDATE-by-old-address (the
existing `aave_v3_contracts` row whose `address` == oldAddress gets its address
bumped to newAddress). The Python asserts the update target exists; the Rust
returns `MissingRow` otherwise (an identity-test precondition held).

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from sqlalchemy import select, text

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    POOL_DATA_PROVIDER_ADDRESS,
    dump_contract_rows,
    make_pool_data_provider_updated_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _seed_pool_data_provider(session: Session, address: str) -> None:
    """Pre-seed a `POOL_DATA_PROVIDER` contract row (for the UPDATE-path test)."""
    session.execute(
        text(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
            "VALUES (1, 'POOL_DATA_PROVIDER', :addr, NULL)"
        ),
        {"addr": get_checksum_address(address)},
    )
    session.flush()


def test_pool_data_provider_updated_insert_rust_matches_python_oracle(tmp_path: Path) -> None:
    """INSERT path (oldAddress == ZERO_ADDRESS): both paths register the contract."""
    new_addr = "0x" + "71" * 20
    fixture_log = make_pool_data_provider_updated_log(new_address=new_addr)

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        provider = ProviderAdapter.from_alloy(AlloyProvider(rpc_url, 0))
        py_market = _load_market(py_session)

        handle = CancelHandle()
        run_aave_update(
            database_path=str(rust_path),
            chain_id=1,
            market_id=1,
            to_block=FIXTURE_BLOCK,
            chunk_size=100,
            rpc_url=rpc_url,
            progress_callback=lambda _progress: None,
            cancel_handle=handle,
        )

        update_aave_market(
            provider=provider,
            start_block=BOOTSTRAP_BLOCK + 1,
            end_block=FIXTURE_BLOCK,
            market=py_market,
            session=py_session,
            verify_block=False,
            verify_chunk=False,
            show_progress=False,
        )

        py_session.commit()
        rust_rows = [
            r for r in dump_contract_rows(rust_session) if r["name"] == "POOL_DATA_PROVIDER"
        ]
        py_rows = [
            r for r in dump_contract_rows(py_session) if r["name"] == "POOL_DATA_PROVIDER"
        ]

    assert len(rust_rows) == 1
    assert len(py_rows) == 1
    assert rust_rows == py_rows, (
        f"PoolDataProviderUpdated(insert) divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )


def test_pool_data_provider_updated_update_rust_matches_python_oracle(tmp_path: Path) -> None:
    """UPDATE path: both paths bump the existing row's address old→new."""
    new_addr = "0x" + "72" * 20
    fixture_log = make_pool_data_provider_updated_log(
        new_address=new_addr, old_address=POOL_DATA_PROVIDER_ADDRESS
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        # Pre-seed the POOL_DATA_PROVIDER at the OLD address (the update target).
        _seed_pool_data_provider(rust_session, POOL_DATA_PROVIDER_ADDRESS)
        _seed_pool_data_provider(py_session, POOL_DATA_PROVIDER_ADDRESS)
        rust_session.commit()
        py_session.commit()

        provider = ProviderAdapter.from_alloy(AlloyProvider(rpc_url, 0))
        py_market = _load_market(py_session)

        handle = CancelHandle()
        run_aave_update(
            database_path=str(rust_path),
            chain_id=1,
            market_id=1,
            to_block=FIXTURE_BLOCK,
            chunk_size=100,
            rpc_url=rpc_url,
            progress_callback=lambda _progress: None,
            cancel_handle=handle,
        )

        update_aave_market(
            provider=provider,
            start_block=BOOTSTRAP_BLOCK + 1,
            end_block=FIXTURE_BLOCK,
            market=py_market,
            session=py_session,
            verify_block=False,
            verify_chunk=False,
            show_progress=False,
        )

        py_session.commit()
        rust_rows = [
            r for r in dump_contract_rows(rust_session) if r["name"] == "POOL_DATA_PROVIDER"
        ]
        py_rows = [
            r for r in dump_contract_rows(py_session) if r["name"] == "POOL_DATA_PROVIDER"
        ]

    assert len(rust_rows) == 1
    assert len(py_rows) == 1
    assert rust_rows == py_rows, (
        f"PoolDataProviderUpdated(update) divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
