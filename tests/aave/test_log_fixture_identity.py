"""Pins the canned Aave log vocabulary's EXACT transaction identity.

One transaction has ONE position in its block, however many logs it emitted. The
canned builders used to state the log's own index as the transaction index, so a
fixture whose transaction emitted several logs claimed several block positions for
one transaction — and the updater refuses exactly that (`ForeignTransactionInGroup`
in the chunk's log-identity gate). These assertions keep the fixture vocabulary
honest: the log index stays the log's, the transaction index stays the
transaction's, and a multi-transaction fixture gives each group its own index.
"""

from __future__ import annotations

from typing import Any

from tests.helpers.aave_logs import (
    A_TOKEN_ADDRESS,
    FIXTURE_BLOCK,
    USER_ADDRESS,
    ZERO_ADDRESS,
    make_erc20_transfer_log,
    make_reserve_initialized_log,
    make_scaled_token_mint_log,
    make_supply_log,
    make_user_e_mode_set_log,
)

_TX = "0x" + b"t1".rjust(32, b"\x00").hex()
_OTHER_TX = "0x" + b"t2".rjust(32, b"\x00").hex()


def _transaction_index(log: dict[str, Any]) -> int:
    return int(str(log["transactionIndex"]), 16)


def _log_index(log: dict[str, Any]) -> int:
    return int(str(log["logIndex"]), 16)


def test_one_transaction_has_one_index_however_many_logs() -> None:
    """A supply's three logs are one transaction: one index, three log positions."""
    logs = [
        make_supply_log(
            reserve=A_TOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            amount=1,
            block=FIXTURE_BLOCK,
            log_index=1,
            tx_hash=_TX,
        ),
        make_scaled_token_mint_log(
            token_address=A_TOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            value=1,
            index=1,
            block=FIXTURE_BLOCK,
            log_index=2,
            tx_hash=_TX,
        ),
        make_erc20_transfer_log(
            token_address=A_TOKEN_ADDRESS,
            from_address=ZERO_ADDRESS,
            to_address=USER_ADDRESS,
            value=1,
            block=FIXTURE_BLOCK,
            log_index=3,
            tx_hash=_TX,
        ),
    ]

    assert {_transaction_index(log) for log in logs} == {0}, (
        "one transaction cannot hold three block positions"
    )
    assert [_log_index(log) for log in logs] == [1, 2, 3], "each log keeps its own block-wide index"


def test_distinct_transactions_in_one_block_get_distinct_indexes() -> None:
    """Two transactions in one chunk state two distinct positions."""
    logs = [
        make_reserve_initialized_log(
            asset=A_TOKEN_ADDRESS,
            a_token=A_TOKEN_ADDRESS,
            v_token=A_TOKEN_ADDRESS,
            block=FIXTURE_BLOCK,
            log_index=0,
        ),
        make_supply_log(
            reserve=A_TOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            amount=1,
            block=FIXTURE_BLOCK,
            log_index=1,
            tx_hash=_OTHER_TX,
            tx_index=1,
        ),
    ]

    positions = [(_transaction_index(log), _log_index(log)) for log in logs]
    assert positions == [(0, 0), (1, 1)], (
        "each transaction group carries its own index, not its log's"
    )


def test_the_user_e_mode_builder_honors_its_tx_index() -> None:
    """The builder's `tx_index` parameter is its transaction's block position."""
    log = make_user_e_mode_set_log(
        user_address=USER_ADDRESS,
        category_id=1,
        block=FIXTURE_BLOCK,
        log_index=9,
        tx_index=3,
    )

    assert _transaction_index(log) == 3
    assert _log_index(log) == 9
