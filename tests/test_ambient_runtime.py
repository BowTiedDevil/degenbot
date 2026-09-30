"""Pin for the ambient-runtime driver seam (`call_on_ambient_runtime`).

The verify seams (`verify_touched_positions_on_chain`, `verify_v3/v4_liquidity_map`)
refuse to build a per-call tokio runtime (the dead-worker churn source) and
fail with a typed ValueError when the calling thread has no ambient runtime.
Rust consumers run on the shared runtime natively; a Python driver shell
satisfies the policy through `degenbot._ffi.call_on_ambient_runtime`, which
enters the shared degenbot-core runtime around a zero-argument callable.
"""

from __future__ import annotations

from functools import partial
from typing import Any

import pytest

from degenbot._ffi import call_on_ambient_runtime
from degenbot._ffi.aave import verify_touched_positions_on_chain

_UNROUTABLE_RPC = "http://127.0.0.1:1"


def test_missing_ambient_runtime_is_typed_value_error(tmp_path: Any) -> None:
    """With NO ambient runtime the seam must fail loudly BEFORE any
    per-call runtime build (or DB work) — the policy the Rust side pins."""
    with pytest.raises(ValueError, match="no ambient tokio runtime"):
        verify_touched_positions_on_chain(
            database_path=str(tmp_path / "db.sqlite"),
            rpc_url=_UNROUTABLE_RPC,
            market_id=1,
            chain_id=1,
            block_number=1,
            touched_users=None,
        )


def test_call_on_ambient_runtime_supplies_the_ambient_runtime(tmp_path: Any) -> None:
    """With the seam, the call gets PAST the runtime policy: it proceeds to
    the DB/RPC work (returning, or failing on the bogus endpoint — never
    with the no-ambient-runtime error)."""
    try:
        result = call_on_ambient_runtime(
            partial(
                verify_touched_positions_on_chain,
                database_path=str(tmp_path / "db.sqlite"),
                rpc_url=_UNROUTABLE_RPC,
                market_id=1,
                chain_id=1,
                block_number=1,
                touched_users=None,
            )
        )
    except ValueError as e:
        assert "no ambient tokio runtime" not in str(e), f"got: {e}"
    else:
        assert isinstance(result, list)
