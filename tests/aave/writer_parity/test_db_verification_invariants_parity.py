"""U5YIBG #5 — `db_verification.py` invariants pass against Rust-written rows.

The ``verify_gho_discount_amounts`` invariant (``cli/aave/db_verification.py``)
is a *stays-python* invariant (per AZGJUN scope) that READS Rust-written rows:
it queries users with GHO debt positions, RPC-calls ``getDiscountPercent(user)``
on the GHO vToken, + asserts the DB ``aave_v3_users.gho_discount`` matches the
contract. This test closes the lens — it drives the Rust writer
(``run_aave_update``) to WRITE ``gho_discount`` via a ``DiscountPercentUpdated``
event (the wired config path: ``dispatch_discount_percent_updated`` →
``apply_gho_discount_percent_updated``), then runs the Python invariant against
the Rust-written rows with a mocked ``getDiscountPercent`` returning the
Rust-written value. The invariant passing is the proof: the Python stays-python
verification code correctly reads + validates Rust-written rows.

The column under verification (``gho_discount``) is Rust-WRITTEN (the Rust apply
fn set it from 1000 → 2500 via the event). The supporting rows (the seeded GHO
debt position that makes the invariant's JOIN find the user, the
``v_gho_discount_token`` precondition) are seeded by the test — they're JOIN
predicates / preconditions, not the value under verification.

#5 ``verify_stk_aave_balances``: RESOLVED by YMWN5V — wired the
``Staked``/``Redeem`` dispatch arms in ``config_dispatch`` (previously they
fell to the ``_ =>`` catch-all → ``Ok(None)`` → the Rust never wrote
``stk_aave_balance`` from the pipeline). The companion test below drives a
``Staked`` log through ``run_aave_update`` → the Rust apply writes
``aave_v3_users.stk_aave_balance``, then runs the stays-python invariant
(mocked ``balanceOf`` → the Rust-written value) → PASSES (non-vacuous).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from sqlalchemy import select, text

from degenbot.cli.aave.db_assets import get_gho_asset
from degenbot.cli.aave.db_verification import (
    verify_gho_discount_amounts,
    verify_stk_aave_balances,
)
from degenbot.database.models.aave import AaveV3Market, AaveV3User
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    FIXTURE_BLOCK,
    STK_AAVE_ADDRESS,
    USER_ADDRESS,
    make_discount_percent_updated_log,
    make_staked_log,
    mock_rpc_server,
    seed_gho_asset,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

# `getDiscountPercent(address)` selector (keccak of the signature, first 4 bytes).
_GET_DISCOUNT_PERCENT_SELECTOR = "0x6c53272b"
# `balanceOf(address)` selector — registered → the Rust-written amount so the
# `verify_stk_aave_balances` invariant (RPC `balanceOf` at `block_number`) reads
# the Rust-written value (the Rust apply treats NULL→0, no RPC during the write).
_BALANCE_OF_SELECTOR = "0x70a08231"


def _encode_uint256(value: int) -> str:
    """Encode an int as a 32-byte (64-hex-char) word, 0x-prefixed."""
    return "0x" + format(value, "064x")


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _seed_gho_debt_position(session: Session) -> None:
    """Seed a GHO debt position so the invariant's JOIN finds the user.

    The ``gho_discount`` VALUE under verification is Rust-written (the event
    sets it); this debt-position row is the JOIN predicate to locate the user
    through ``aave_v3_debt_positions JOIN aave_v3_assets ON v_token_id``.
    asset_id=2 is the GHO asset seeded by ``seed_gho_asset`` (v_token_id=5).
    """
    session.execute(
        text(
            "INSERT INTO aave_v3_debt_positions "
            "(id, user_id, asset_id, balance, last_index) "
            "VALUES (1, 1, 2, '1000000000000000000000', '1000000000000000000000000000')"
        ),
    )


def test_verify_gho_discount_amounts_passes_against_rust_written_rows(
    tmp_path: Path,
) -> None:
    """The stays-python GHO-discount invariant reads + validates Rust-written rows.

    Drives ``run_aave_update`` with a ``DiscountPercentUpdated(new=2500)``
    event → the Rust apply fn writes ``aave_v3_users.gho_discount = 2500``
    (the seeded value was 1000). Then runs ``verify_gho_discount_amounts``
    (mocked ``getDiscountPercent`` → 2500) + asserts it passes — proving the
    Python verification code correctly reads the Rust-written ``gho_discount``.
    """
    new_discount = 2500
    fixture_log = make_discount_percent_updated_log(
        user_address=USER_ADDRESS,
        new_discount_percent=new_discount,
        old_discount_percent=1000,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={
                _GET_DISCOUNT_PERCENT_SELECTOR: _encode_uint256(new_discount),
            },
        ) as rpc_url,
    ):
        seed_gho_asset(
            rust_session,
            v_token_revision=3,  # < GHO_DISCOUNT_DEPRECATION_REVISION (4) → invariant runs
            gho_discount_percent=1000,  # the PRE-event seeded value (Rust writes 2500)
        )
        # The invariant asserts `gho_asset.v_gho_discount_token is not None`
        # (a precondition — it's `verify_stk_aave_balances` that USES it).
        rust_session.execute(
            text("UPDATE aave_gho_tokens SET v_gho_discount_token = :stk WHERE id = 1"),
            {"stk": "0x" + "ee" * 20},
        )
        _seed_gho_debt_position(rust_session)
        rust_session.commit()

        # Drive the Rust writer: DiscountPercentUpdated → apply_gho_discount_percent_updated
        # writes `gho_discount = 2500` on the user. The discount-config fetch is
        # chain-wide (no address filter); the event is pure-decode (no RPC) — the
        # `getDiscountPercent` mock response is ONLY consumed by the invariant below.
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

        # Refresh the ORM session to see the Rust-written rows (run_aave_update
        # committed on its own connection to the same SQLite file).
        rust_session.expire_all()
        market = _load_market(rust_session)
        gho_asset = get_gho_asset(session=rust_session, market=market)
        user = rust_session.scalars(select(AaveV3User).where(AaveV3User.id == 1)).one()

        # Explicit confirmation the row was Rust-WRITTEN: gho_discount is 2500
        # (the event value), NOT the seeded 1000.
        assert user.gho_discount == new_discount, (
            f"gho_discount {user.gho_discount} != Rust-written {new_discount} "
            f"(the DiscountPercentUpdated event didn't write it)"
        )

        # The stays-python invariant reads the Rust-written row + RPC-verifies
        # (getDiscountPercent → mocked to the Rust-written value). Passing (no
        # AssertionError) proves the Python verification reads Rust-written rows.
        provider = ProviderAdapter.from_alloy(AlloyProvider(rpc_url, 0))
        verify_gho_discount_amounts(
            provider=provider,
            session=rust_session,
            market=market,
            gho_asset=gho_asset,
            block_number=FIXTURE_BLOCK,
            show_progress=False,
        )


def _seed_discount_token(session: Session) -> None:
    """Set `v_gho_discount_token` = STK_AAVE_ADDRESS so the Rust's
    `fetch_stk_aave_logs` returns the `Staked` log (the invariant also
    asserts `gho_asset.v_gho_discount_token is not None`)."""
    from degenbot.checksum_cache import get_checksum_address

    session.execute(
        text("UPDATE aave_gho_tokens SET v_gho_discount_token = :stk WHERE id = 1"),
        {"stk": get_checksum_address(STK_AAVE_ADDRESS)},
    )


def test_verify_stk_aave_balances_passes_against_rust_written_rows(
    tmp_path: Path,
) -> None:
    """YMWN5V-closed: the stays-python stkAAVE-balance invariant reads +
    validates Rust-written rows.

    Drives ``run_aave_update`` with a ``Staked(user, amount)`` log → the Rust
    config path (``dispatch_stk_aave_staked`` → ``apply_stk_aave_staked_on_conn``,
    wired by YMWN5V) WRITES ``aave_v3_users.stk_aave_balance = amount`` (the
    seeded value was NULL → 0 + amount). Then runs ``verify_stk_aave_balances``
    (mocked ``balanceOf`` → the Rust-written amount) → PASSES (non-vacuous).
    This proves the Python stays-python verification code correctly reads the
    Rust-written ``stk_aave_balance`` row.
    """
    amount = 5000
    fixture_log = make_staked_log(
        staker=USER_ADDRESS,
        on_behalf_of=USER_ADDRESS,
        amount=amount,
    )

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={
                _BALANCE_OF_SELECTOR: _encode_uint256(amount),
            },
        ) as rpc_url,
    ):
        seed_gho_asset(
            rust_session,
            v_token_revision=3,
            gho_discount_percent=1000,
        )
        _seed_discount_token(rust_session)
        rust_session.commit()

        # Drive the Rust writer: Staked → apply_stk_aave_staked writes
        # `stk_aave_balance = 0 + amount = amount` on the user. The Staked log
        # is pure-decode (no RPC); the `balanceOf` mock response is ONLY
        # consumed by the invariant below.
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

        rust_session.expire_all()
        market = _load_market(rust_session)
        gho_asset = get_gho_asset(session=rust_session, market=market)
        user = rust_session.scalars(select(AaveV3User).where(AaveV3User.id == 1)).one()

        # Explicit confirmation the row was Rust-WRITTEN: stk_aave_balance is
        # `amount` (NULL → 0 + amount), NOT NULL (the seeded value).
        assert user.stk_aave_balance == amount, (
            f"stk_aave_balance {user.stk_aave_balance} != Rust-written {amount} "
            f"(the Staked event didn't write it)"
        )

        # The stays-python invariant reads the Rust-written row + RPC-verifies
        # (balanceOf → mocked to the Rust-written value). Passing (no
        # AssertionError) proves the Python verification reads Rust-written rows.
        provider = ProviderAdapter.from_alloy(AlloyProvider(rpc_url, 0))
        verify_stk_aave_balances(
            provider=provider,
            session=rust_session,
            market=market,
            gho_asset=gho_asset,
            block_number=FIXTURE_BLOCK,
            show_progress=False,
        )
