"""§4.2 live-vs-live parity: ``ReserveUsedAsCollateral{Enabled,Disabled}``.

`ReserveUsedAsCollateral{Enabled,Disabled}(address indexed reserve, address
indexed user)` — pure (no eth_call): the reserve identifies the asset, the user +
asset identify a `aave_v3_user_collateral_configs` row whose `enabled` flag is
set (`true` for Enabled, `false` for Disabled). On create, INSERT with the flag;
on existing, UPDATE the flag.

Divergence surface (the realistic case is tested — the user pre-exists): the
Rust ``dispatch_reserve_used_as_collateral`` ``get_or_create_user``s (no-op on a
pre-seeded user) while the Python ``assert user is not None`` (requires the user
to pre-exist — in production created by a prior Supply). The harness pre-seeds
the user so both paths find it. Both then write byte-identical
``aave_v3_user_collateral_configs`` rows.

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
    USER_ADDRESS,
    dump_collateral_config_rows,
    make_reserve_used_as_collateral_log,
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


@pytest.mark.parametrize("enabled", [True, False], ids=["enabled", "disabled"])
def test_reserve_used_as_collateral_rust_matches_python_oracle(
    tmp_path: Path,
    enabled: bool,  # noqa: FBT001
) -> None:
    """Both paths produce byte-identical `aave_v3_user_collateral_configs` rows."""
    fixture_log = make_reserve_used_as_collateral_log(
        user_address=USER_ADDRESS,
        reserve_address=UNDERLYING_ADDRESS,
        enabled=enabled,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        # Seed the asset + the pre-existing user (both paths need the asset;
        # the Python `assert user is not None` requires the user to pre-exist).
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
        rust_rows = dump_collateral_config_rows(rust_session)
        py_rows = dump_collateral_config_rows(py_session)

    assert len(rust_rows) == 1, f"Rust created {len(rust_rows)} collateral configs"
    assert len(py_rows) == 1, f"Python created {len(py_rows)} collateral configs"

    rust_row = rust_rows[0]
    py_row = py_rows[0]

    assert rust_row == py_row, (
        f"ReserveUsedAsCollateral(enabled={enabled}) row divergence:\n"
        f"  Rust:   {rust_row}\n  Python: {py_row}"
    )
    # Spot-checks (the error message above is the real assertion).
    assert rust_row["user_id"] == 1
    assert rust_row["asset_id"] == 1
    assert bool(rust_row["enabled"]) is enabled
