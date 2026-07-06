"""§4.2 live-vs-live parity: ``CollateralConfigurationChanged``.

`CollateralConfigurationChanged(address indexed asset, uint256 ltv, uint256
liquidationThreshold, uint256 liquidationBonus)` — emitted by the Pool
Configurator. Both paths IGNORE the event's ltv/lt/bonus data (the Rust dispatch
comment: "a pool upgrade can emit stale values") + RPC-fetch the FULL config
bitmap via ``getConfiguration(address)`` against the Pool contract, decode it
(via the `_decode_reserve_configuration_bitmap` / Rust equivalent — proven
byte-identical by the flag-#5 bit-decode parity), and write the
``aave_v3_asset_configs`` row.

This is the FIRST event exercising the mock's ``eth_call`` dispatch (keyed by
the calldata selector) — the infrastructure the remaining config events reuse.

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
    GET_CONFIGURATION_SELECTOR,
    UNDERLYING_ADDRESS,
    dump_asset_config_rows,
    make_collateral_configuration_changed_log,
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


def _set_bits(shift: int, value: int) -> int:
    """Place `value` at bit `shift` (the corpus bitmap helper, mirrored)."""
    mask = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF
    return (value & mask) << shift


def _realistic_bitmap() -> int:
    """A realistic-ish Aave reserve config bitmap (ltv=8000, active+borrowing)."""
    return (
        _set_bits(0, 8000)
        | _set_bits(16, 8250)
        | _set_bits(32, 10500)
        | _set_bits(48, 18)
        | (1 << 56)  # active
        | (1 << 58)  # borrowing_enabled
        | _set_bits(64, 1000)
    )


@pytest.mark.parametrize(
    "bitmap",
    [0, (1 << 252) - 1, _realistic_bitmap()],
    ids=["zero", "all-ones", "realistic"],
)
def test_collateral_configuration_changed_rust_matches_python_oracle(
    tmp_path: Path,
    bitmap: int,
) -> None:
    """Both paths write byte-identical `aave_v3_asset_configs` rows."""
    fixture_log = make_collateral_configuration_changed_log(asset_address=UNDERLYING_ADDRESS)
    # The mock serves the SAME `getConfiguration(address)` bitmap to both paths.
    bitmap_response = "0x" + bitmap.to_bytes(32, "big").hex()

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={GET_CONFIGURATION_SELECTOR: bitmap_response},
        ) as rpc_url,
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
        rust_rows = dump_asset_config_rows(rust_session)
        py_rows = dump_asset_config_rows(py_session)

    assert len(rust_rows) == 1, f"Rust created {len(rust_rows)} asset configs"
    assert len(py_rows) == 1, f"Python created {len(py_rows)} asset configs"
    assert rust_rows == py_rows, (
        f"CollateralConfigurationChanged(bitmap={bitmap:#x}) row divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
