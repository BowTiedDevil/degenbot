"""§4.2 live-vs-live parity: ``PoolUpdated`` / ``PoolConfiguratorUpdated`` →
``ContractRevisionUpdated``.

`PoolUpdated(address indexed oldAddress, address indexed newAddress)` /
`PoolConfiguratorUpdated(...)` — emitted by the PoolAddressesProvider. Both
paths RPC `POOL_REVISION()` / `CONFIGURATOR_REVISION()` (no-arg) on the
new address + UPDATE the `aave_v3_contracts` row's `revision` (NOT `address`
— the proxy address is stable; the `newAddress` is used only for the RPC).

The second event exercising the mock's `eth_call` selector dispatch (the
`*_REVISION()` no-arg selectors).

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
    CONFIGURATOR_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    POOL_REVISION_SELECTOR,
    dump_contract_rows,
    make_pool_configurator_updated_log,
    make_pool_updated_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize(
    ("name", "log_fn", "selector", "new_revision"),
    [
        ("POOL", make_pool_updated_log, POOL_REVISION_SELECTOR, 2),
        (
            "POOL_CONFIGURATOR",
            make_pool_configurator_updated_log,
            CONFIGURATOR_REVISION_SELECTOR,
            3,
        ),
    ],
    ids=["pool", "configurator"],
)
def test_contract_revision_updated_rust_matches_python_oracle(
    tmp_path: Path,
    name: str,
    log_fn: object,
    selector: str,
    new_revision: int,
) -> None:
    """Both paths set byte-identical `aave_v3_contracts.revision`."""
    new_impl = "0x" + "f1" * 20
    builder = make_pool_updated_log if name == "POOL" else make_pool_configurator_updated_log
    fixture_log = builder(new_address=new_impl)
    # The mock serves the same `*_REVISION()` uint256 return to both paths.
    revision_response = "0x" + new_revision.to_bytes(32, "big").hex()

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={selector: revision_response},
        ) as rpc_url,
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
        rust_rows = [r for r in dump_contract_rows(rust_session) if r["name"] == name]
        py_rows = [r for r in dump_contract_rows(py_session) if r["name"] == name]

    assert len(rust_rows) == 1
    assert len(py_rows) == 1
    assert rust_rows == py_rows, (
        f"ContractRevisionUpdated(name={name}) divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
    assert rust_rows[0]["revision"] == new_revision
