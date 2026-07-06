"""§4.2 live-vs-live parity: ``ReserveDataUpdated``.

`ReserveDataUpdated(address indexed reserve, uint256 liquidityRate,
uint256 stableBorrowRate, uint256 variableBorrowRate, uint256 liquidityIndex,
uint256 variableBorrowIndex)` — pure (no eth_call): the reserve identifies the
asset, then the asset's `liquidity_rate`/`borrow_rate`/`liquidity_index`/
`borrow_index`/`last_update_block` columns are UPDATEd. `stableBorrowRate` is
decoded then discarded (deprecated on Aave V3).

Writes the `aave_v3_assets` state columns — exercises the multi-field uint256
event-data decode + the U256→decimal-string storage (the Rust stores
`U256.to_string()`; the Python stores `int` → SQLAlchemy VARCHAR → decimal
string). Parametrized with small ints + Ray-scale (>64-bit) values to test the
U256 path.

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from operator import itemgetter
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
    create_resered_data_values,
    dump_asset_rows,
    make_reserve_data_updated_log,
    mock_rpc_server,
    seed_asset_and_user,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    """Load the seeded AaveV3Market ORM object (update_aave_market takes it)."""
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize("values", create_resered_data_values(), ids=itemgetter(0))
def test_reserve_data_updated_rust_matches_python_oracle(
    tmp_path: Path,
    values: tuple[str, int, int, int, int, int],
) -> None:
    """Both paths produce byte-identical `aave_v3_assets` rate/index rows."""
    _label, liq_rate, stable_rate, var_rate, liq_index, var_index = values
    fixture_log = make_reserve_data_updated_log(
        reserve_address=UNDERLYING_ADDRESS,
        liquidity_rate=liq_rate,
        stable_borrow_rate=stable_rate,
        variable_borrow_rate=var_rate,
        liquidity_index=liq_index,
        variable_borrow_index=var_index,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        # The asset must pre-exist (both paths look it up by underlying).
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
    assert rust_rows == py_rows, (
        f"ReserveDataUpdated({values[0]}) row divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
    rust_row = rust_rows[0]
    assert rust_row["last_update_block"] == FIXTURE_BLOCK
    assert rust_row["liquidity_rate"] == str(liq_rate)
    assert rust_row["liquidity_index"] == str(liq_index)
