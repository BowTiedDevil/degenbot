"""§4.2 live-vs-live parity: Rust ``run_aave_update`` vs Python oracle ``update_aave_market``.

The MVP event: ``UserEModeSet`` (pure — no eth_call; decodes the category_id from
the event data + creates the user with ``e_mode`` set). Drives BOTH the Rust
driver + the unrouted Python oracle against the SAME mock RPC serving the SAME
canned log, into two identically-seeded temp DBs; asserts the ``aave_v3_users``
row state is byte-identical (column-by-column, including the checksummed
``address``, ``e_mode``, ``gho_discount``, ``stk_aave_balance``, +
``isolation_mode_*``).

Per §4.3, this parity test is TEMPORARY — once GREEN, CZM7TI retires the Python
oracle (``update_aave_market`` + the ``_process_*`` handlers) + this test
together (the Rust ``#[cfg(test)]`` corpus in ``write.rs`` stays permanent).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    dump_user_rows,
    make_user_e_mode_set_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

_USER_ADDRESS = "0x" + "ab" * 20


def _load_market(session: Session) -> AaveV3Market:
    """Load the seeded AaveV3Market ORM object (update_aave_market takes it)."""
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize("category_id", [0, 1, 2, 255], ids=["zero", "one", "two", "max"])
def test_user_e_mode_set_rust_matches_python_oracle(
    tmp_path: Path,
    category_id: int,
) -> None:
    """The Rust driver + the Python oracle produce byte-identical user rows for UserEModeSet."""
    fixture_log = make_user_e_mode_set_log(
        user_address=_USER_ADDRESS,
        category_id=category_id,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        provider = ProviderAdapter.from_alloy(AlloyProvider(rpc_url, 0))
        py_market = _load_market(py_session)

        # Rust path.
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

        # Python oracle path (the unrouted `update_aave_market`).
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

        # Commit the Python oracle's writes (mirroring the production caller's
        # per-chunk `session.commit()` — `update_aave_market` itself does NOT
        # commit; the old `aave_update` chunk loop did). The Rust path commits
        # internally per chunk.
        py_session.commit()
        rust_users = dump_user_rows(rust_session)
        py_users = dump_user_rows(py_session)

    # Both created exactly one user for the fixture's user address.
    assert len(rust_users) == 1, f"Rust created {len(rust_users)} users"
    assert len(py_users) == 1, f"Python created {len(py_users)} users"

    rust_user = rust_users[0]
    py_user = py_users[0]

    # Byte-for-byte column parity (the §4.2 contract).
    assert rust_user == py_user, (
        f"UserEModeSet(category={category_id}) row divergence:\n"
        f"  Rust:   {rust_user}\n  Python: {py_user}"
    )
    # Spot-check the key fields (the error message above is the real assertion;
    # these make a divergence self-explanatory).
    expected_address = get_checksum_address(_USER_ADDRESS)
    assert rust_user["address"] == expected_address
    assert rust_user["e_mode"] == category_id
    assert rust_user["gho_discount"] == 0
    assert rust_user["stk_aave_balance"] is None
