"""Companion-layer re-exports of Rust updater + database FFI pyclasses.

The three-layer architecture (ADR-005) requires that driver / CLI code import
updater input pyclasses from the companion package ``degenbot.updater`` — not
from the PyO3 wrapper ``degenbot._ffi``.

These re-exports must be **direct aliases**, not Python subclasses — the Rust
engine constructs and consumes these pyclasses directly; subclassing would
break type identity at the FFI boundary.
"""

from __future__ import annotations

from degenbot._ffi.cancel import CancelHandle
from degenbot.db import (
    LiquidityUpdateEvent,
    V2PoolRowInput,
    V3PoolRowInput,
    V4PoolRowInput,
)

from degenbot.exceptions import DatabaseSchemaStale


def test_cancel_handle_alias() -> None:
    from degenbot.updater import CancelHandle as CompanionCancelHandle

    assert CompanionCancelHandle is CancelHandle


def test_liquidity_update_event_alias() -> None:
    from degenbot.updater import LiquidityUpdateEvent as CompanionLiquidityUpdateEvent

    assert CompanionLiquidityUpdateEvent is LiquidityUpdateEvent


def test_v2_pool_row_input_alias() -> None:
    from degenbot.updater import V2PoolRowInput as CompanionV2PoolRowInput

    assert CompanionV2PoolRowInput is V2PoolRowInput


def test_v3_pool_row_input_alias() -> None:
    from degenbot.updater import V3PoolRowInput as CompanionV3PoolRowInput

    assert CompanionV3PoolRowInput is V3PoolRowInput


def test_v4_pool_row_input_alias() -> None:
    from degenbot.updater import V4PoolRowInput as CompanionV4PoolRowInput

    assert CompanionV4PoolRowInput is V4PoolRowInput


def test_database_schema_stale_alias() -> None:
    """DatabaseSchemaStale is re-exported from degenbot.exceptions, identity-preserved."""
    from degenbot.db import DatabaseSchemaStale as FfiDatabaseSchemaStale

    assert DatabaseSchemaStale is FfiDatabaseSchemaStale
