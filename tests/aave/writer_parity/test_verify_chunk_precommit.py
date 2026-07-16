"""Pre-commit verification (`verify_chunk=True`) aborts the commit on divergence.

The Rust core runs ``verify_touched_positions_on_conn`` on the UNCOMMITTED
transaction, BEFORE ``tx.commit()``. A divergence drops the tx (rollback) so
``last_update_block`` does NOT advance → the next run re-processes the same
chunk. This test proves the fix: pre-commit verification prevents bad data from
becoming durable (the regression: old code committed first, THEN verified in a
post-commit callback → the bad commit was already durable, so the next run
advanced past the failed chunk).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select, text

from degenbot.database.models.aave import AaveV3Market
from degenbot._ffi.cancel import CancelHandle
from degenbot._ffi.aave import run_aave_update
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    USER_ADDRESS,
    make_user_e_mode_set_log,
    mock_rpc_server,
    seed_asset_and_user,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

# `scaledBalanceOf(address)` selector.
_SCALED_BALANCE_OF_SELECTOR = "0x1da24f3e"
# `getPreviousIndex(address)` selector.
_GET_PREVIOUS_INDEX_SELECTOR = "0xe0753986"


def _encode_uint256(value: int) -> str:
    return "0x" + format(value, "064x")


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _seed_debt_position(session: Session, *, balance: str, last_index: str) -> None:
    """Seed a debt position (asset 1) for user 1 with the given balance/index.

    The asset was created by ``seed_asset_and_user`` (asset id 1 with
    ``v_token_id=4`` → erc20 ``V_TOKEN_ADDRESS``). The verify loads
    `aave_v3_debt_positions JOIN aave_v3_assets ON asset_id` → `v_token_id`
    → `erc20_tokens`.
    """
    session.execute(
        text(
            "INSERT INTO aave_v3_debt_positions (id, user_id, asset_id, "
            "balance, last_index) VALUES (1, 1, 1, :bal, :idx)"
        ),
        {"bal": balance, "idx": last_index},
    )


def test_verify_chunk_divergence_rolls_back_and_does_not_advance_stamp(
    tmp_path: Path,
) -> None:
    """A verification divergence drops the tx (rollback) → stamp unchanged.

    Seeds a debt position whose DB `balance` does NOT match the on-chain
    `scaledBalanceOf` (the mock returns a different value). Drives
    ``run_aave_update`` with ``verify_chunk=True`` + a ``UserEModeSet`` event
    (touching the user so pre-commit verification runs). Asserts:

    1. An ``AssertionError`` is raised (the Rust core maps
       ``RunError::Verification`` to ``AssertionError``).
    2. ``last_update_block`` is STILL ``BOOTSTRAP_BLOCK`` (the chunk was
       rolled back — NOT advanced).
    3. The debt position's DB ``balance`` is unchanged (the rollback
       reverted the write).

    This is the CRITICAL invariant: a verification failure must NOT land a
    bad commit. Without pre-commit verify, the old code committed the chunk
    first, then found the divergence in a post-commit callback, then
    cancelled — but the bad data was already durable.
    """
    wrong_balance = "1000"  # DB says 1000
    wrong_index = "1000000000000000000000000000"
    on_chain_balance = 999  # chain says 999 (simulating a divergence)
    on_chain_index = 1000000000000000000000000000  # index matches (no index divergence)

    fixture_log = make_user_e_mode_set_log(
        user_address=USER_ADDRESS,
        category_id=1,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={
                _SCALED_BALANCE_OF_SELECTOR: _encode_uint256(on_chain_balance),
                _GET_PREVIOUS_INDEX_SELECTOR: _encode_uint256(on_chain_index),
            },
        ) as rpc_url,
    ):
        # Seed an asset + user (asset id 3, v_token at V_TOKEN_ADDRESS).
        seed_asset_and_user(rust_session)
        # Seed a debt position with the WRONG balance.
        _seed_debt_position(
            rust_session,
            balance=wrong_balance,
            last_index=wrong_index,
        )
        rust_session.commit()

        stamp_before = _load_market(rust_session).last_update_block
        assert stamp_before == BOOTSTRAP_BLOCK

        handle = CancelHandle()
        with pytest.raises(AssertionError, match="divergence"):
            run_aave_update(
                database_path=str(rust_path),
                chain_id=1,
                market_id=1,
                to_block=FIXTURE_BLOCK,
                chunk_size=100,
                rpc_url=rpc_url,
                progress_callback=lambda _progress: None,
                cancel_handle=handle,
                verify_chunk=True,
            )

        # The chunk was rolled back — the stamp did NOT advance.
        rust_session.expire_all()
        stamp_after = _load_market(rust_session).last_update_block
        assert stamp_after == BOOTSTRAP_BLOCK, (
            f"last_update_block advanced {stamp_before}→{stamp_after} "
            "despite a verification divergence (the bad chunk should have "
            "been rolled back)"
        )


def test_verify_chunk_pass_commits_and_advances_stamp(tmp_path: Path) -> None:
    """A verification PASS commits normally → stamp advances.

    The mirror of the divergence test: when the DB balance MATCHES the
    on-chain `scaledBalanceOf`, pre-commit verification passes, the tx
    commits, and the stamp advances. This proves ``verify_chunk=True``
    doesn't break the happy path.
    """
    correct_balance = "1000"
    correct_index = "1000000000000000000000000000"
    on_chain_balance = 1000  # matches DB → no divergence
    on_chain_index = 1000000000000000000000000000  # matches → no divergence

    fixture_log = make_user_e_mode_set_log(
        user_address=USER_ADDRESS,
        category_id=1,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={
                _SCALED_BALANCE_OF_SELECTOR: _encode_uint256(on_chain_balance),
                _GET_PREVIOUS_INDEX_SELECTOR: _encode_uint256(on_chain_index),
            },
        ) as rpc_url,
    ):
        seed_asset_and_user(rust_session)
        _seed_debt_position(
            rust_session,
            balance=correct_balance,
            last_index=correct_index,
        )
        rust_session.commit()

        handle = CancelHandle()
        report = run_aave_update(
            database_path=str(rust_path),
            chain_id=1,
            market_id=1,
            to_block=FIXTURE_BLOCK,
            chunk_size=100,
            rpc_url=rpc_url,
            progress_callback=lambda _progress: None,
            cancel_handle=handle,
            verify_chunk=True,
        )

        # The chunk committed — the stamp advanced.
        rust_session.expire_all()
        stamp_after = _load_market(rust_session).last_update_block
        assert stamp_after == FIXTURE_BLOCK, (
            f"last_update_block is {stamp_after}, expected {FIXTURE_BLOCK} "
            "(verification passed, the chunk should have committed)"
        )
        assert report["chunks_committed"] == 1
