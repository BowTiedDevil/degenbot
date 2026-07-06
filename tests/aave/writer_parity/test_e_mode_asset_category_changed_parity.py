"""§4.2 live-vs-live parity: ``EModeAssetCategoryChanged``.

`EModeAssetCategoryChanged(address indexed asset, uint8 oldCategoryId, uint8
newCategoryId)` — emitted by the Pool Configurator (older Aave). Sets the
`aave_v3_asset_configs.e_mode_category_id` to `newCategoryId`, or `None` when
`newCategoryId == 0` (the clear). The `oldCategoryId` is decoded + logged only
(not applied). Get-or-create the config row.

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select, text

from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    UNDERLYING_ADDRESS,
    dump_asset_config_rows,
    make_e_mode_asset_category_changed_log,
    mock_rpc_server,
    seed_asset_and_user,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _seed_asset_config(session: Session, *, e_mode_category_id: int | None) -> None:
    """Pre-seed an `aave_v3_asset_configs` row (for the existing-row cases)."""
    session.execute(
        text(
            "INSERT INTO aave_v3_asset_configs (asset_id, ltv, liquidation_threshold, "
            "liquidation_bonus, e_mode_category_id, borrowing_enabled, "
            "stable_borrowing_enabled, flash_loan_enabled, isolation_mode, "
            "borrowable_in_isolation, debt_ceiling) "
            "VALUES (1, 0, 0, 0, :em, 0, 0, 0, 0, 0, NULL)"
        ),
        {"em": e_mode_category_id},
    )
    session.flush()


@pytest.mark.parametrize(
    ("label", "new_category_id", "existing"),
    [
        ("set-on-empty", 4, None),
        ("clear-on-existing", 0, 5),
        ("set-on-existing", 9, 3),
    ],
    ids=["set-on-empty", "clear", "set-on-existing"],
)
def test_e_mode_asset_category_changed_rust_matches_python_oracle(
    tmp_path: Path,
    label: str,
    new_category_id: int,
    existing: int | None,
) -> None:
    """Both paths write byte-identical `e_mode_category_id` values."""
    expected = new_category_id if new_category_id > 0 else None
    fixture_log = make_e_mode_asset_category_changed_log(
        asset_address=UNDERLYING_ADDRESS, new_category_id=new_category_id
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        seed_asset_and_user(rust_session)
        seed_asset_and_user(py_session)
        if existing is not None:
            _seed_asset_config(rust_session, e_mode_category_id=existing)
            _seed_asset_config(py_session, e_mode_category_id=existing)
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
        rust_rows = dump_asset_config_rows(rust_session)
        py_rows = dump_asset_config_rows(py_session)

    assert rust_rows == py_rows, (
        f"EModeAssetCategoryChanged({label}) divergence:\n"
        f"  Rust:   {rust_rows}\n  Python: {py_rows}"
    )
    assert rust_rows[0]["e_mode_category_id"] == expected
