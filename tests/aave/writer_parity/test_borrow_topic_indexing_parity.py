"""§4.2 topic-indexing parity: Borrow with `user ≠ onBehalfOf` + non-zero
`referralCode` (7UFMZX — the test that would have caught the
`decode_borrow`/`decode_borrow_pool_event` topic[3]-vs-topic[2] bug).

The Aave V3 Borrow signature has 3 indexed params (reserve, onBehalfOf,
referralCode) → 4 topics: ``[sig, reserve(topic1), onBehalfOf(topic2),
referralCode(topic3)]``. The Rust decoders previously read ``on_behalf_of =
topics[3]`` (the referralCode slot) + ``referral_code = topics[4]`` (which
doesn't exist on 4-topic events → silently 0). On the writer_parity fixtures
this was masked because ``make_borrow_log`` defaulted ``user = on_behalf_of``
→ ``topic[2] == topic[3]`` → the buggy topic[3] read coincidentally matched the
correct topic[2] read the Python does.

This fixture sets ``user`` DISTINCT from ``on_behalf_of`` + a NON-ZERO
``referral_code`` so the topic[2] (onBehalfOf) + topic[3] (referralCode=42)
slots differ — the Rust's correct topic[2] read must equal the Python's
topic[2] read, byte-IDENTICAL, else the debt position's ``user_id`` diverges
(Rust would create a user for ``0x00…002a`` from the referralCode-as-address;
the Python for the real onBehalfOf). The Python reference is correct
(``decode_address(event["topics"][2])`` — operations_parser.py:1063).

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from sqlalchemy import select

from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    DEBT_TOKEN_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    GHO_TOKEN_ADDRESS,
    GHO_VTOKEN_ADDRESS,
    USER_ADDRESS,
    dump_debt_position_rows,
    dump_user_rows,
    make_borrow_log,
    make_erc20_transfer_log,
    make_scaled_token_mint_log,
    mock_rpc_server,
    seed_gho_asset,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

# The Ray-scale (1e27) — the variable-borrow index the vToken Mint carries.
_RAY = 10**27
# 1000 GHO borrowed (18 decimals).
_BORROW_AMOUNT = 1000 * 10**18

# A distinct tx hash for the single-tx-group fixture.
_TX = "0x" + b"tb".rjust(32, b"\x00").hex()

# The Borrow's `user` (data word 0) — DISTINCT from `on_behalf_of` so the
# topic[2] (onBehalfOf) + topic[3] (referralCode) slots don't collide with it.
# (If `user == on_behalf_of`, the buggy topic[3] read could coincidentally
# match the Python's correct topic[2] read — the mask this test removes.)
_DEPOSITOR = "0x" + "cc" * 20

# A NON-ZERO referral code — the load-bearing value. With the bug, the Rust
# read `on_behalf_of = topics[3]` = 42 → garbage address `0x00…002a`; the Python
# read `topics[2]` = the real onBehalfOf. Distinct + non-zero catches it.
_REFERRAL_CODE = 42

# The GHO vToken revision pre-upgrade (discount supported; < deprecation rev 4).
_PRE_UPGRADE_REV = 3
# The revision the mock RPC reports for the GHO vToken (stable token, unchanged).
_REVISION_RESPONSE = "0x" + _PRE_UPGRADE_REV.to_bytes(32, "big").hex()


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _build_chunk_logs() -> list[dict[str, object]]:
    """The single-tx-group chunk at FIXTURE_BLOCK, ordered by log_index.

    tx: Pool Borrow(user=DEPOSITOR, onBehalfOf=USER, referralCode=42) +
    GHO vToken Mint(onBehalfOf=USER, value) + GHO token Transfer(0x0→USER).
    """
    return [
        make_borrow_log(
            reserve=GHO_TOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            user=_DEPOSITOR,
            amount=_BORROW_AMOUNT,
            referral_code=_REFERRAL_CODE,
            log_index=0,
            tx_hash=_TX,
        ),
        make_scaled_token_mint_log(
            token_address=GHO_VTOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            value=_BORROW_AMOUNT,
            balance_increase=0,
            index=_RAY,
            log_index=1,
            tx_hash=_TX,
        ),
        # The companion Transfer (the minted-debt companion the Borrow asserts).
        make_erc20_transfer_log(
            token_address=GHO_VTOKEN_ADDRESS,
            from_address="0x" + "00" * 20,
            to_address=USER_ADDRESS,
            value=_BORROW_AMOUNT,
            log_index=2,
            tx_hash=_TX,
        ),
    ]


def _run_both(rust_path: Path, rust_session: Session, py_session: Session, rpc_url: str) -> None:
    seed_gho_asset(rust_session, v_token_revision=_PRE_UPGRADE_REV)
    seed_gho_asset(py_session, v_token_revision=_PRE_UPGRADE_REV)
    rust_session.commit()
    py_session.commit()
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


def test_borrow_topic_indexing_rust_matches_python_oracle(tmp_path: Path) -> None:
    """Both paths write the debt position under USER (onBehalfOf=topic[2]),
    byte-identical — NOT under the referralCode-as-address (topic[3]=42).

    Catches the ``decode_borrow``/``decode_borrow_pool_event`` topic-indexing
    bug: with ``user ≠ onBehalfOf`` + ``referral_code ≠ 0``, the buggy
    ``on_behalf_of = topics[3]`` read would create a user for
    ``0x00…002a`` (referralCode 42 as an address) → a divergent ``user_id``
    on the debt position row → non-byte-IDENTICAL.
    """
    logs = _build_chunk_logs()
    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=logs,
            block_number=FIXTURE_BLOCK,
            eth_call_responses={DEBT_TOKEN_REVISION_SELECTOR: _REVISION_RESPONSE},
        ) as rpc_url,
    ):
        _run_both(rust_path, rust_session, py_session, rpc_url)
        rust_rows = dump_debt_position_rows(rust_session)
        py_rows = dump_debt_position_rows(py_session)
        rust_users = dump_user_rows(rust_session)
        py_users = dump_user_rows(py_session)

    assert rust_rows == py_rows, (
        f"Borrow topic-indexing divergence (user={_DEPOSITOR}, "
        f"onBehalfOf={USER_ADDRESS}, referralCode={_REFERRAL_CODE}):\n"
        f"  Rust:   {rust_rows}\n  Python: {py_rows}"
    )
    assert len(rust_rows) == 1, "exactly one GHO debt position should exist"
    # The debt position's user_id must resolve to the onBehalfOf (USER_ADDRESS),
    # NOT the referralCode-as-address (0x00…002a = topic[3]=42) the buggy
    # topic[3] read would produce. Assert exactly one user row (no garbage user
    # created) + the address matches across paths.
    assert rust_users == py_users, (
        f"user-row divergence:\n  Rust:   {rust_users}\n  Python: {py_users}"
    )
    assert len(rust_users) == 1, (
        f"expected 1 user (onBehalfOf); got {len(rust_users)} (a garbage "
        f"referralCode-as-address user would be the 7UFMZX topic[3] bug)"
    )
    # The single user's address must be the onBehalfOf (USER_ADDRESS),
    # case-insensitively (the stored address is EIP-55 checksummed; both paths
    # checksum identically → byte-IDENTITY). The buggy topic[3] read would have
    # written the referralCode (42) as an address instead.
    assert rust_users[0]["address"].lower() == USER_ADDRESS.lower(), (
        f"user_address {rust_users[0]['address']!r} is not onBehalfOf "
        f"{USER_ADDRESS!r} (the 7UFMZX topic[3] bug would write the "
        f"referralCode={_REFERRAL_CODE} as address 0x00…002a)"
    )
