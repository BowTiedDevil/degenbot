"""§4.2 byte-IDENTITY parity for stkAAVE `Staked`/`Redeem` (YMWN5V).

The Rust config path processes the semantic `Staked`/`Redeem` events
(``dispatch_stk_aave_staked``/``dispatch_stk_aave_redeem`` →
``apply_stk_aave_staked_on_conn``/``Redeem``); the Python
``transaction_processor`` processes the coinciding ERC20 ``Transfer`` on the
discount token (the mint arm ``Transfer(0→X)`` for a stake; the burn arm
``Transfer(X→0)`` for a redeem). For a stake, both increment ``onBehalfOf``; for
a redeem, both decrement ``from`` (the redeemer). This test drives BOTH paths
with the ``Staked``/``Redeem`` log + the coinciding ``Transfer`` + asserts
byte-IDENTITY ``stk_aave_balance`` (the column both paths write).

YMWN5V wired the dispatch arms (previously ``Staked``/``Redeem`` fell to the
``_ =>`` catch-all in ``config_dispatch`` → ``Ok(None)`` → the Rust never wrote
``stk_aave_balance`` from the pipeline). If the Rust apply diverges from the
Python when fed REAL logs (vs the unit tests' directly-constructed chunk
events), this test surfaces it as a new flag.

The ``balanceOf`` eth_call is mocked to ``uint256(0)`` so the Python's
``get_or_init_stk_aave_balance`` (RPC at ``block_number - 1`` when the balance
is ``NULL``) initializes to 0 — matching the Rust apply's ``NULL → 0`` (no RPC).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select, text

from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    STK_AAVE_ADDRESS,
    USER_ADDRESS,
    ZERO_ADDRESS,
    dump_user_rows,
    make_erc20_transfer_log,
    make_redeem_log,
    make_staked_log,
    mock_rpc_server,
    seed_gho_asset,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

# `balanceOf(address)` selector (keccak of the signature, first 4 bytes) —
# registered → uint256(0) so the Python's `get_or_init_stk_aave_balance`
# (RPC at block-1 when the balance is NULL) decodes 0 (not the crash-on-empty
# default "0x").
_BALANCE_OF_SELECTOR = "0x70a08231"
_UINT256_ZERO = "0x" + "0" * 64


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _seed_discount_token(session: Session) -> None:
    """Set `v_gho_discount_token` so `fetch_stk_aave_logs`/`fetch_stk_aave_events`
    return the Staked/Redeem/Transfer logs (filtered to that address)."""
    from degenbot.checksum_cache import get_checksum_address

    session.execute(
        text("UPDATE aave_gho_tokens SET v_gho_discount_token = :stk WHERE id = 1"),
        {"stk": get_checksum_address(STK_AAVE_ADDRESS)},
    )


def _run_both(
    rust_path: Path,
    py_session: Session,
    rpc_url: str,
) -> None:
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


@pytest.mark.parametrize("amount", [5000, 123_456_789_012_345_678_901], ids=["small", "large"])
def test_staked_rust_matches_python_oracle(tmp_path: Path, amount: int) -> None:
    """`Staked` (Rust) + `Transfer(0→onBehalfOf)` (Python) → byte-IDENTITY balance == amount."""
    staked_log = make_staked_log(
        staker=USER_ADDRESS,
        on_behalf_of=USER_ADDRESS,
        amount=amount,
        log_index=0,
    )
    transfer_log = make_erc20_transfer_log(
        token_address=STK_AAVE_ADDRESS,
        from_address=ZERO_ADDRESS,
        to_address=USER_ADDRESS,
        value=amount,
        log_index=1,
    )  # same default tx_hash → same tx group (both paths process per-tx)

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=[staked_log, transfer_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={_BALANCE_OF_SELECTOR: _UINT256_ZERO},
        ) as rpc_url,
    ):
        seed_gho_asset(rust_session)
        seed_gho_asset(py_session)
        _seed_discount_token(rust_session)
        _seed_discount_token(py_session)
        rust_session.commit()
        py_session.commit()
        _run_both(rust_path, py_session, rpc_url)
        py_session.commit()
        rust_rows = dump_user_rows(rust_session)
        py_rows = dump_user_rows(py_session)

    assert rust_rows == py_rows, (
        f"Staked(amount={amount}) divergence:\n  Rust: {rust_rows}\n  Python: {py_rows}"
    )
    # the balance was NULL (seeded); Staked → 0 + amount = amount (both paths).
    assert rust_rows[0]["stk_aave_balance"] == str(amount), rust_rows


@pytest.mark.parametrize("amount", [5000, 123_456_789_012_345_678_901], ids=["small", "large"])
def test_redeem_rust_matches_python_oracle(tmp_path: Path, amount: int) -> None:
    """`Redeem` (Rust) + `Transfer(redeemer→0)` (Python) → byte-IDENTITY balance == 0.

    The user's balance is pre-seeded to `amount` (non-NULL) so the redeem's
    decrement is valid (the Python asserts `>= 0` before; the Rust apply errors
    on underflow). Pre-seeded → the Python's `get_or_init` is SKIPPED (no
    `balanceOf` RPC); the Rust reads `amount` directly.
    """
    redeem_log = make_redeem_log(
        redeemer=USER_ADDRESS,
        to=ZERO_ADDRESS,
        amount=amount,
        log_index=0,
    )
    transfer_log = make_erc20_transfer_log(
        token_address=STK_AAVE_ADDRESS,
        from_address=USER_ADDRESS,
        to_address=ZERO_ADDRESS,
        value=amount,
        log_index=1,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=[redeem_log, transfer_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={_BALANCE_OF_SELECTOR: _UINT256_ZERO},
        ) as rpc_url,
    ):
        seed_gho_asset(rust_session)
        seed_gho_asset(py_session)
        _seed_discount_token(rust_session)
        _seed_discount_token(py_session)
        # pre-seed balance = amount (the redeem decrements it → 0).
        for s in (rust_session, py_session):
            s.execute(
                text("UPDATE aave_v3_users SET stk_aave_balance = :bal WHERE id = 1"),
                {"bal": str(amount)},
            )
        rust_session.commit()
        py_session.commit()
        _run_both(rust_path, py_session, rpc_url)
        py_session.commit()
        rust_rows = dump_user_rows(rust_session)
        py_rows = dump_user_rows(py_session)

    assert rust_rows == py_rows, (
        f"Redeem(amount={amount}) divergence:\n  Rust: {rust_rows}\n  Python: {py_rows}"
    )
    assert rust_rows[0]["stk_aave_balance"] == "0", rust_rows


# S3X2I2 — a second test user (id 2, balance '0'). Pre-seeded so neither path
# get_or_creates it (avoids the new-user-default-divergence surface) + the
# non-NULL balance means neither path RPCs `balanceOf` via `get_or_init`.
RECIPIENT_ADDRESS = "0x" + "22" * 20


def _seed_second_user(session: Session, *, address: str) -> None:
    """Seed user id 2 with `stk_aave_balance = '0'` (non-NULL)."""
    from degenbot.checksum_cache import get_checksum_address

    session.execute(
        text(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, "
            "gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, "
            "isolation_mode_debt) "
            "VALUES (2, 1, :addr, 0, 0, '0', NULL, '0')"
        ),
        {"addr": get_checksum_address(address)},
    )


@pytest.mark.parametrize("amount", [5000, 123_456_789_012_345_678_901], ids=["small", "large"])
def test_transfer_both_legs_rust_matches_python_oracle(
    tmp_path: Path,
    amount: int,
) -> None:
    """S3X2I2: user-to-user stkAAVE `Transfer(A→B)` (neither zero) → byte-IDENTITY
    BOTH legs: `A.stk_aave_balance -= amount` AND `B.stk_aave_balance += amount`.

    Drives BOTH `run_aave_update` (Rust, the new `Erc20Transfer` dispatch arm →
    `apply_stk_aave_transfer_on_conn`) + `update_aave_market` (Python,
    `process_stk_aave_transfer_event` the both-legs path) with a user-to-user
    `Transfer(USER_ADDRESS→RECIPIENT_ADDRESS, amount)` + asserts
    `dump_user_rows(rust) == dump_user_rows(py)` (both legs).

    A's balance is pre-seeded to `amount` (non-NULL → neither path RPCs
    `balanceOf` via `get_or_init`; sufficient so the from-leg decrement
    doesn't underflow — sidesteps the latent `value > balance` divergence
    where the Python allows negative + the Rust underflow-errors).
    """
    transfer_log = make_erc20_transfer_log(
        token_address=STK_AAVE_ADDRESS,
        from_address=USER_ADDRESS,
        to_address=RECIPIENT_ADDRESS,
        value=amount,
        log_index=0,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=[transfer_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={_BALANCE_OF_SELECTOR: _UINT256_ZERO},
        ) as rpc_url,
    ):
        seed_gho_asset(rust_session)
        seed_gho_asset(py_session)
        _seed_discount_token(rust_session)
        _seed_discount_token(py_session)
        _seed_second_user(rust_session, address=RECIPIENT_ADDRESS)
        _seed_second_user(py_session, address=RECIPIENT_ADDRESS)
        # Pre-seed A's balance = amount (sufficient for the from-leg decrement).
        for s in (rust_session, py_session):
            s.execute(
                text("UPDATE aave_v3_users SET stk_aave_balance = :bal WHERE id = 1"),
                {"bal": str(amount)},
            )
        rust_session.commit()
        py_session.commit()
        _run_both(rust_path, py_session, rpc_url)
        py_session.commit()
        rust_rows = dump_user_rows(rust_session)
        py_rows = dump_user_rows(py_session)

    assert rust_rows == py_rows, (
        f"Transfer(amount={amount}) divergence:\n  Rust: {rust_rows}\n  Python: {py_rows}"
    )
    # Both legs: A (sender) - amount = 0; B (recipient) + amount = amount.
    by_addr = {r["address"]: r for r in rust_rows}
    from degenbot.checksum_cache import get_checksum_address

    sender = by_addr[get_checksum_address(USER_ADDRESS)]
    recipient = by_addr[get_checksum_address(RECIPIENT_ADDRESS)]
    assert sender["stk_aave_balance"] == "0", rust_rows
    assert recipient["stk_aave_balance"] == str(amount), rust_rows
