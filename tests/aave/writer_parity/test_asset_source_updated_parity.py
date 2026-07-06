"""§4.2 live-vs-live parity: ``AssetSourceUpdated``.

`AssetSourceUpdated(address indexed asset, address indexed source)` — emitted
by the AaveOracle contract (the `PRICE_ORACLE` row's address). Both paths look
up the asset by its underlying address + set the `aave_v3_assets.price_source`
column to the checksummed `source`.

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select

from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    UNDERLYING_ADDRESS,
    dump_asset_rows,
    make_asset_source_updated_log,
    mock_rpc_server,
    seed_asset_and_user,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize(
    "source",
    ["0x" + "81" * 20, "0x" + "9a" * 20],
    ids=["chainlink", "custom"],
)
def test_asset_source_updated_rust_matches_python_oracle(
    tmp_path: Path,
    source: str,
) -> None:
    """Both paths set byte-identical `aave_v3_assets.price_source`."""
    fixture_log = make_asset_source_updated_log(
        asset_address=UNDERLYING_ADDRESS, source_address=source
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        seed_asset_and_user(rust_session)
        seed_asset_and_user(py_session)
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
        rust_rows = dump_asset_rows(rust_session)
        py_rows = dump_asset_rows(py_session)

    assert len(rust_rows) == 1
    assert len(py_rows) == 1
    assert rust_rows == py_rows, (
        f"AssetSourceUpdated(source={source}) divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
