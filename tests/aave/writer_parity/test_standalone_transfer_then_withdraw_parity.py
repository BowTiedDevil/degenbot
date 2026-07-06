"""§4.2 live-vs-live parity: standalone aToken Transfer-credit-then-Withdraw
(the YUPSIB fixture shape — locks down the Python's at-line-2001 conditional).

Aave V3 moves collateral between users via aToken `Transfer` (+ the
companion `BalanceTransfer` scaled event) — a STANDALONE operation (no Pool
Supply/Withdraw). The recipient's collateral position is credited; the
sender's is debited. The Python's `_create_deficit_coverage_operations`
(operations_parser.py:1900-2022) only marks a BalanceTransfer
`local_assigned` INSIDE `if paired_burn is not None:` (lines 2001-2003) —
UNPAIRED transfers fall through to `_create_transfer_operations`
(ops_parser.py:2232) as standalone TRANSFER ops, crediting the recipient.
The Rust pre-`2d96c514` had the `local_assigned.insert(bt_ev.log_index)`
mis-PLACED OUTSIDE the `if paired` block — stealing unpaired transfers
without creating an op → the recipient's credit never landed → the
recipient's Withdraw's CollateralBurn went negative → crash.

The Python is CORRECT here (verified: src/degenbot/cli/aave/operations_parser.py:
2001-2003 — `local_assigned.add` is nested inside `if paired_burn is not None:`;
no unconditional add outside). So the bug was a Rust-only port regression
(the (a) case): the YUPSIB bug class is parity-CATCHABLE — a Rust-vs-Python
fixture with a stand-alone Transfer-then-Withdraw shape would have caught it
RED (Python lands USER's collateral; Rust pre-fix crashed). This test is that
fixture — the regression guard.

**Fixture** (one chunk at FIXTURE_BLOCK, 3 transactions):
  tx 1 — ReserveInitialized (creates the WETH asset + aWETH/vWETH).
  tx 2 — SENDER supplies WETH (Pool Supply + aWETH Mint from zero +
         mint-from-zero Transfer). SENDER's collateral position lands + is
         the source of the subsequent standalone Transfer.
  tx 3 — standalone aToken Transfer SENDER→USER (+ its BalanceTransfer
         scaled companion; NO paired burn) THEN USER withdraws (Pool
         Withdraw + aWETH Burn from USER + outgoing Transfer-to-zero
         companion). USER's credit comes from the standalone Transfer;
         the Burn debits it.

**Parity assertion:** both paths write byte-IDENTICAL
`aave_v3_collateral_positions` rows (SENDER settles to balance 0 — supplied
+ transferred out; USER settles to balance 0 — received + withdrew).

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
    ATOKEN_REVISION_SELECTOR,
    BOOTSTRAP_BLOCK,
    DEBT_TOKEN_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    GET_SOURCE_OF_ASSET_SELECTOR,
    USER_ADDRESS,
    dump_collateral_position_rows,
    dump_user_rows,
    make_balance_transfer_log,
    make_erc20_transfer_log,
    make_reserve_initialized_log,
    make_scaled_token_burn_log,
    make_scaled_token_mint_log,
    make_supply_log,
    make_withdraw_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

_RAY = 10**27  # 1e27 — the liquidity index (steady-state).
_AMOUNT = 10**15  # 0.001 WETH (mirrors the on-chain-truth block 16496928).

# The new reserve (WETH) + its aToken/vToken — created by ReserveInitialized.
_WETH = "0x" + "c0" * 20
_A_WETH = "0x" + "77" * 20  # the aToken (the Transfer/Mint/Burn emitter)
_V_WETH = "0x" + "88" * 20

# SENDER supplies first (so the standalone Transfer's sender-debit doesn't
# itself go negative); then transfers to USER; USER withdraws.
_SENDER = "0x" + "e4" * 20  # mirrors mainnet 0xe4217…
# USER == USER_ADDRESS (0xab…ab); the recipient of the standalone Transfer.

_TX_SUPPLY = "0x" + b"s1".rjust(32, b"\x00").hex()
_TX_XFER = "0x" + b"x2".rjust(32, b"\x00").hex()  # standalone Transfer + Withdraw

_A_REV = 1
_V_REV = 1
_PRICE_SOURCE = "0x" + "d0" * 20


def _eth_call_responses() -> dict[str, str]:
    return {
        ATOKEN_REVISION_SELECTOR: "0x" + _A_REV.to_bytes(32, "big").hex(),
        DEBT_TOKEN_REVISION_SELECTOR: "0x" + _V_REV.to_bytes(32, "big").hex(),
        GET_SOURCE_OF_ASSET_SELECTOR: "0x" + "00" * 12 + _PRICE_SOURCE[2:],
    }


def _build_chunk_logs() -> list[dict[str, object]]:
    """One chunk at FIXTURE_BLOCK: SENDER supplies → SENDER transfers aWETH to
    USER (standalone; NO paired burn) → USER withdraws."""
    li = 0
    logs: list[dict[str, object]] = []

    # tx 1: ReserveInitialized (creates the WETH asset + aWETH/vWETH).
    logs.append(
        make_reserve_initialized_log(asset=_WETH, a_token=_A_WETH, v_token=_V_WETH, log_index=li)
    )
    li += 1

    # tx 2 (_TX_SUPPLY): SENDER supplies WETH — Pool Supply + aWETH Mint (from
    # zero) + the mint-from-zero companion Transfer.
    logs.append(
        make_supply_log(
            reserve=_WETH,
            on_behalf_of=_SENDER,
            user=_SENDER,
            amount=_AMOUNT,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_SUPPLY,
        )
    )
    li += 1
    logs.append(
        make_scaled_token_mint_log(
            token_address=_A_WETH,
            on_behalf_of=_SENDER,
            value=_AMOUNT,
            balance_increase=0,
            index=_RAY,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_SUPPLY,
        )
    )
    li += 1
    logs.append(  # mint-from-zero companion (ERC20 Transfer 0x0→SENDER)
        make_erc20_transfer_log(
            token_address=_A_WETH,
            from_address="0x" + "00" * 20,
            to_address=_SENDER,
            value=_AMOUNT,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_SUPPLY,
        )
    )
    li += 1

    # tx 3 (_TX_XFER): standalone aToken Transfer SENDER→USER + its
    # BalanceTransfer (the scaled bookkeeping — the standalone BalanceTransfer
    # op pairs the ERC20 Transfer + the BalanceTransfer). NO paired burn —
    # pre-fix, the deficit_coverage scavenger stole these (marks assigned
    # without creating an op) → USER's credit never landed.
    logs.append(
        make_erc20_transfer_log(
            token_address=_A_WETH,
            from_address=_SENDER,
            to_address=USER_ADDRESS,
            value=_AMOUNT,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_XFER,
        )
    )
    li += 1
    logs.append(
        make_balance_transfer_log(
            token_address=_A_WETH,
            from_address=_SENDER,
            to_address=USER_ADDRESS,
            value=_AMOUNT,
            index=_RAY,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_XFER,
        )
    )
    li += 1

    # tx 3 (continued): USER withdraws — Pool Withdraw + aWETH Burn
    # (CollateralBurn, debits USER) + the outgoing Transfer-to-zero companion.
    logs.append(
        make_withdraw_log(
            reserve=_WETH,
            user=USER_ADDRESS,
            to=USER_ADDRESS,
            amount=_AMOUNT,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_XFER,
        )
    )
    li += 1
    logs.append(  # the CollateralBurn (from=USER, target=USER self-burn)
        make_scaled_token_burn_log(
            token_address=_A_WETH,
            from_address=USER_ADDRESS,
            target=USER_ADDRESS,
            value=_AMOUNT,
            balance_increase=0,
            index=_RAY,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_XFER,
        )
    )
    li += 1
    logs.append(  # outgoing Transfer-to-zero companion
        make_erc20_transfer_log(
            token_address=_A_WETH,
            from_address=USER_ADDRESS,
            to_address="0x" + "00" * 20,
            value=_AMOUNT,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_XFER,
        )
    )
    return logs


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def test_standalone_transfer_then_withdraw_rust_matches_python_oracle(
    tmp_path: Path,
) -> None:
    """Both paths write byte-identical `aave_v3_collateral_positions` rows.

    The standalone Transfer-credit (USER) + the Withdraw-debit (USER) must
    balance to the SAME end-state in both paths: SENDER supplies + transfers
    out → 0; USER receives + withdraws → 0. Pre-fix Rust crashed
    (`balance would go negative`); post-fix (YUPSIB) it lands the same rows as
    the Python oracle.

    The parity assertion locks down the Python's at-line-2001 conditional
    (`local_assigned.add` inside `if paired_burn is not None:`). A future
    regression that re-introduces the unconditional scavenging would crash the
    Rust path → assertion fires (Rust raises; Python succeeds → divergence).
    """
    logs = _build_chunk_logs()
    with (
        seeded_db(tmp_path, name="rust", with_price_oracle=True) as (rust_path, rust_session),
        seeded_db(tmp_path, name="py", with_price_oracle=True) as (_py_path, py_session),
        mock_rpc_server(
            logs=logs,
            block_number=FIXTURE_BLOCK,
            eth_call_responses=_eth_call_responses(),
        ) as rpc_url,
    ):
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
        rust_positions = dump_collateral_position_rows(rust_session)
        py_positions = dump_collateral_position_rows(py_session)
        rust_users = dump_user_rows(rust_session)
        py_users = dump_user_rows(py_session)

    assert rust_positions == py_positions, (
        f"Transfer-then-Withdraw collateral-position divergence:\n"
        f"  Rust:   {rust_positions}\n  Python: {py_positions}"
    )
    assert rust_users == py_users, (
        f"Transfer-then-Withdraw user-row divergence:\n"
        f"  Rust:   {rust_users}\n  Python: {py_users}"
    )
    # Spot-check the load-bearing end-state: 2 positions (SENDER+USER),
    # both settle to balance 0.
    assert len(rust_positions) == 2, (
        f"expected 2 collateral positions (SENDER+USER); got {len(rust_positions)}"
    )
    for pos in rust_positions:
        assert int(pos["balance"]) == 0, (
            f"expected end-state balance 0 (supply+transfer-out for SENDER; "
            f"receive+withdraw for USER); got {pos!r}"
        )
