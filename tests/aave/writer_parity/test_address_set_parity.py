"""§4.2 live-vs-live parity: ``AddressSet``.

`AddressSet(bytes32 indexed id, address indexed oldAddress, address indexed
newAddress)` — emitted by the PoolAddressesProvider. Both paths ASCII-decode
the `id` bytes32 (right-padded) + strip the trailing NULs to recover the
contract name + INSERT an `aave_v3_contracts` row (name, address=newAddress,
revision=NULL). The Python asserts `oldAddress == ZERO_ADDRESS`; the Rust
returns a `DecodeShape` error otherwise (an identity-test precondition held).

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
    dump_contract_rows,
    make_address_set_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize(
    ("name", "new_addr"),
    [
        ("POOL_DATA_PROVIDER", "0x" + "61" * 20),
        ("EMODE_ADMIN", "0x" + "62" * 20),
    ],
    ids=["data-provider", "emode-admin"],
)
def test_address_set_rust_matches_python_oracle(
    tmp_path: Path,
    name: str,
    new_addr: str,
) -> None:
    """Both paths insert a byte-identical `aave_v3_contracts` row."""
    fixture_log = make_address_set_log(contract_id=name, new_address=new_addr)

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
        rust_rows = [r for r in dump_contract_rows(rust_session) if r["name"] == name]
        py_rows = [r for r in dump_contract_rows(py_session) if r["name"] == name]

    assert len(rust_rows) == 1, f"Rust created {len(rust_rows)} '{name}' contracts"
    assert len(py_rows) == 1, f"Python created {len(py_rows)} '{name}' contracts"
    assert rust_rows == py_rows, (
        f"AddressSet(name={name!r}) row divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
