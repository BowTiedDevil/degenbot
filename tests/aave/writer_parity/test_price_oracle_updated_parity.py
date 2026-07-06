"""§4.2 live-vs-live parity: ``PriceOracleUpdated``.

`PriceOracleUpdated(address indexed oldAddress, address indexed newAddress)` —
emitted by the PoolAddressesProvider. Both paths register a `PRICE_ORACLE`
`aave_v3_contracts` row (address = checksummed newAddress).

**Pre-condition:** the seeded DB must NOT have a `PRICE_ORACLE` contract row —
the Python handler `assert existing_oracle is None` (it always INSERTs, never
updates), while the Rust's `apply_price_oracle_updated_on_conn` is get-or-update.
With the oracle pre-seeded the Python path assertion-errors; without it both
paths INSERT → identical. So this test seeds with `with_price_oracle=False`.

(The Rust's UPDATE branch is therefore unreachable in the Python — a §4.2
parity gap recorded in the corpus summary: the Rust is more lenient. It does
not surface as a row divergence on the INSERT path tested here.)

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
    make_price_oracle_updated_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize(
    "new_oracle",
    ["0x" + "51" * 20, "0x" + "5e" * 20],
    ids=["chainlink", "custom"],
)
def test_price_oracle_updated_insert_rust_matches_python_oracle(
    tmp_path: Path,
    new_oracle: str,
) -> None:
    """Both paths register a byte-identical `PRICE_ORACLE` contract row."""
    fixture_log = make_price_oracle_updated_log(new_oracle_address=new_oracle)

    with (
        # No pre-seeded PRICE_ORACLE — the Python asserts none exists (INSERT path).
        seeded_db(tmp_path, name="rust", with_price_oracle=False) as (rust_path, rust_session),
        seeded_db(tmp_path, name="py", with_price_oracle=False) as (_py_path, py_session),
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
        rust_rows = [r for r in dump_contract_rows(rust_session) if r["name"] == "PRICE_ORACLE"]
        py_rows = [r for r in dump_contract_rows(py_session) if r["name"] == "PRICE_ORACLE"]

    assert len(rust_rows) == 1
    assert len(py_rows) == 1
    assert rust_rows == py_rows, (
        f"PriceOracleUpdated(insert) divergence:\n"
        f"  Rust:   {rust_rows[0]}\n  Python: {py_rows[0]}"
    )
