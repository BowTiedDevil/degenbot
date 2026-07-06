"""§4.2 live-vs-live parity: ``EModeCategoryAdded``.

`EModeCategoryAdded(uint8 indexed categoryId, uint256 ltv, uint256
liquidationThreshold, uint256 liquidationBonus, address oracle, string label)`
— emitted by the Pool Configurator. Both paths decode the 5-word head + the
dynamic `string label` tail + INSERT (or UPDATE when the ``(market_id,
category_id)`` pair already exists) an ``aave_v3_emode_categories`` row.

This is a pure-decode event (no `eth_call`) — the cheapest config-event
identity test.

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
    dump_emode_category_rows,
    make_e_mode_category_added_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    """Load the seeded AaveV3Market ORM object (update_aave_market takes it)."""
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize(
    ("label", "category_id", "ltv", "lt", "bonus", "oracle"),
    [
        ("ETH", 1, 9000, 9300, 10200, "0x" + "11" * 20),
        ("stablecoins", 2, 9800, 9800, 10100, "0x" + "99" * 20),
    ],
    ids=["eth", "stablecoins"],
)
def test_e_mode_category_added_rust_matches_python_oracle(
    tmp_path: Path,
    label: str,
    category_id: int,
    ltv: int,
    lt: int,
    bonus: int,
    oracle: str | None,
) -> None:
    """Both paths write byte-identical `aave_v3_emode_categories` rows."""
    fixture_log = make_e_mode_category_added_log(
        category_id=category_id,
        ltv=ltv,
        liquidation_threshold=lt,
        liquidation_bonus=bonus,
        oracle_address=oracle,
        label=label,
    )

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
        rust_rows = dump_emode_category_rows(rust_session)
        py_rows = dump_emode_category_rows(py_session)

    assert len(rust_rows) == 1, f"Rust created {len(rust_rows)} emode categories"
    assert len(py_rows) == 1, f"Python created {len(py_rows)} emode categories"
    assert rust_rows == py_rows, (
        f"EModeCategoryAdded(category_id={category_id}) row divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
