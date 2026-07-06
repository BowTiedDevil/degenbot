"""Rust-path-only smoke for the §4.2 harness (proves the mock drives run_aave_update offline).

Proves the mock RPC + the seeded temp DB drive
``degenbot_rs.run_aave_update`` through the full fetch → decode → dispatch →
write path for a ``UserEModeSet`` event, producing the expected ``aave_v3_users``
row. Once this is GREEN, the Python-oracle side + the row comparison are added.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import CancelHandle, run_aave_update
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    create_seeded_db,
    dump_user_rows,
    make_user_e_mode_set_log,
    mock_rpc_server,
)

if TYPE_CHECKING:
    from pathlib import Path

# The user address in the fixture event + the category_id it sets.
_USER_ADDRESS = "0x" + "ab" * 20
_CATEGORY_ID = 2


def test_run_aave_update_offline_drives_user_e_mode_set(tmp_path: Path) -> None:
    """The Rust path creates the user + sets e_mode from a fixture UserEModeSet event."""
    db_path, session = create_seeded_db(tmp_path, name="rust")
    fixture_log = make_user_e_mode_set_log(
        user_address=_USER_ADDRESS,
        category_id=_CATEGORY_ID,
    )
    with mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url:
        handle = CancelHandle()
        report = run_aave_update(
            database_path=str(db_path),
            chain_id=1,
            market_id=1,
            to_block=FIXTURE_BLOCK,
            chunk_size=100,
            rpc_url=rpc_url,
            progress_callback=lambda _progress: None,
            cancel_handle=handle,
        )
    # The run advanced one chunk, one event applied.
    assert report["from_block"] == BOOTSTRAP_BLOCK + 1
    assert report["to_block"] == FIXTURE_BLOCK
    assert report["total_events_applied"] >= 1
    users = dump_user_rows(session)
    assert len(users) == 1
    user = users[0]
    # Both paths store the EIP-55 checksummed address (a parity surface).
    assert user["address"] == get_checksum_address(_USER_ADDRESS)
    assert user["e_mode"] == _CATEGORY_ID
    assert user["gho_discount"] == 0
    assert user["stk_aave_balance"] is None
