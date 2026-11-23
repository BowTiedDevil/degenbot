"""Rust-path-only smoke for the parity harness (proves the mock drives run_aave_update offline).

Proves the mock RPC + the seeded temp DB drive
``degenbot._ffi.run_aave_update`` through the full fetch → decode → dispatch →
write path for a ``UserEModeSet`` event, producing the expected ``aave_v3_users``
row. Once this is GREEN, the Python-oracle side + the row comparison are added.
"""

from __future__ import annotations

import json
import urllib.request
from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from tests.aave.writer_parity.harness import (
    dump_user_rows,
    mock_rpc_server,
    seeded_db,
)
from tests.helpers.aave_logs import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    make_user_e_mode_set_log,
)
from tests.helpers.seam import CancelHandle, run_aave_update

if TYPE_CHECKING:
    from pathlib import Path

# The user address in the fixture event + the category_id it sets.
_USER_ADDRESS = "0x" + "ab" * 20
_CATEGORY_ID = 2


def _mock_rpc(url: str, method: str, params: list[Any]) -> dict[str, Any]:
    """POST one JSON-RPC request to the mock server and return its envelope."""
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    request = urllib.request.Request(  # ruff: ignore[suspicious-url-open-usage] — a loopback mock
        url,
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request) as response:  # ruff: ignore[suspicious-url-open-usage] — a loopback mock
        return json.loads(response.read())


def test_run_aave_update_offline_drives_user_e_mode_set(tmp_path: Path) -> None:
    """The Rust path creates the user + sets e_mode from a fixture UserEModeSet event."""
    fixture_log = make_user_e_mode_set_log(
        user_address=_USER_ADDRESS,
        category_id=_CATEGORY_ID,
    )
    with (
        seeded_db(tmp_path, name="rust") as (db_path, session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        handle = CancelHandle()
        report = run_aave_update(
            database_path=str(db_path),
            chain_id=1,
            market_id=1,
            to_block=FIXTURE_BLOCK,
            chunk_size=100,
            rpc_url=rpc_url,
            cancel_handle=handle,
        )
        users = dump_user_rows(session)
    # The run advanced one chunk, one event applied.
    assert report["from_block"] == BOOTSTRAP_BLOCK + 1
    assert report["to_block"] == FIXTURE_BLOCK
    assert report["total_events_applied"] >= 1
    assert len(users) == 1
    user = users[0]
    # Both paths store the EIP-55 checksummed address (a parity surface).
    assert user["address"] == get_checksum_address(_USER_ADDRESS)
    assert user["e_mode"] == _CATEGORY_ID
    assert user["gho_discount"] == 0
    assert user["stk_aave_balance"] is None


def test_mock_serves_a_complete_terminal_header_its_logs_carry() -> None:
    """`eth_getBlockByNumber` answers a complete header at the requested height,
    hashing to the block every canned log carries; a method the mock does not
    serve is a JSON-RPC error rather than a fabricated success."""
    fixture_log = make_user_e_mode_set_log(
        user_address=_USER_ADDRESS,
        category_id=_CATEGORY_ID,
    )
    with mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url:
        envelope = _mock_rpc(rpc_url, "eth_getBlockByNumber", [hex(FIXTURE_BLOCK), False])
        unserved = _mock_rpc(rpc_url, "eth_getProof", [])

    served = envelope["result"]
    assert served["number"] == hex(FIXTURE_BLOCK)
    assert served["hash"] == fixture_log["blockHash"]
    assert served["parentHash"] != served["hash"]
    assert int(served["parentHash"], 16) != 0
    assert unserved["error"]["code"] == -32601
