"""The `run_pool_update` seam's terminal outcome: a cancel RETURNS a report.

The Rust chunk loop polls `cancel_handle` only at chunk boundaries — never
mid-chunk, where the in-flight chunk completes atomically first
(`docs/architecture/chunk-atomicity.md`). What a driver observes at the Python
edge is a dict either way: `status: "completed"` when the run reached
`to_block`, `status: "cancelled"` when the flag stopped it, with
`committed_through` naming the last durable chunk end (`0` when nothing
committed). A cancelled run therefore raises nothing — catching `RuntimeError`
is not how a driver detects a cancel, and the cancelled dict is the only signal
a completion-time action (a backup, a snapshot) may key on, which is why the
status is the discriminator rather than an absent key.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.db import db_set_exchange_active, db_upgrade_database, db_upsert_exchange
from degenbot.updater import CancelHandle, run_pool_update

if TYPE_CHECKING:
    import pathlib

CHAIN_ID = 8453
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
TARGET_BLOCK = 100
CHUNK_SIZE = 10
UNROUTABLE_RPC = "http://127.0.0.1:1"


def _db_with_pending_work(path: pathlib.Path) -> pathlib.Path:
    """A migrated DB holding one ACTIVE `uniswap_v3` exchange behind the target."""
    db_upgrade_database(str(path))
    exchange = db_upsert_exchange(
        database_path=str(path),
        chain_id=CHAIN_ID,
        name="uniswap_v3",
        factory=UNISWAP_V3_FACTORY,
        deployer=None,
    )
    db_set_exchange_active(str(path), exchange_id=exchange.id, active=True)
    return path


def test_cancelled_run_returns_a_cancelled_status_dict(tmp_path: pathlib.Path) -> None:
    """A cancelled run returns the report dict with `status: "cancelled"`.

    The cancel flag is raised before the first chunk boundary, so the run
    commits nothing and reaches the boundary check before any RPC fetch — the
    unroutable endpoint is never dialed (the HTTP transport is lazy).
    """
    db = _db_with_pending_work(tmp_path / "cancelled.db")
    cancel = CancelHandle()
    cancel.cancel()

    report = run_pool_update(str(db), CHAIN_ID, TARGET_BLOCK, CHUNK_SIZE, UNROUTABLE_RPC, cancel)

    assert report["status"] == "cancelled"
    assert report["committed_through"] == 0
    assert report["chunks_committed"] == 0
    assert report["total_pools_written"] == 0
    assert report["total_liquidity_applies"] == 0
    assert report["chain_id"] == CHAIN_ID
    assert report["to_block"] == TARGET_BLOCK


def test_cancelled_run_reports_no_completion_signal(tmp_path: pathlib.Path) -> None:
    """A cancelled run's dict is not a completion: `status` alone decides.

    `committed_through` stays behind `to_block`, so a driver gating a
    completion-time action on durable progress sees the gap rather than reading
    the requested target as reached.
    """
    db = _db_with_pending_work(tmp_path / "cancelled_partial.db")
    cancel = CancelHandle()
    cancel.cancel()

    report = run_pool_update(str(db), CHAIN_ID, TARGET_BLOCK, CHUNK_SIZE, UNROUTABLE_RPC, cancel)

    assert report["status"] != "completed"
    assert report["committed_through"] < report["to_block"]
