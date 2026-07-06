"""§4.2 + §4.3 VV5OTV — `verify_touched_positions_on_chain` value-correctness
gate.

The per-chunk value-correctness gate (the minimal BE474R slice — port of
Python `verification.py::verify_scaled_token_positions`). Mirrors the
Python's assertion contract: `scaledBalanceOf(user)` +
`getPreviousIndex(user)` at `block_number` MUST equal the DB position row's
`balance` + `last_index`.

The RED case proves the gate catches the masked wrong-value bugs the crash-
gate STRUCTURALLY cannot: corrupt the DB row's balance, run the gate, it
surfaces a NAMED divergence (kind, position_id, user_address, token_address,
field, expected, actual) BEFORE the chunk's wrong balance could compile-up
into a future "balance would go negative" crash somewhere downstream.

The GREEN case proves the gate stays clean when the DB matches on-chain
truth — so a future regression that re-introduces the NMWPI6/3MF5QM-class
masked bugs (which left balances SILENTLY wrong, never negative) would be
caught RED by this gate, not just-green.

Login: the fixture is one Supply at `liquidity_index = 2 * _RAY` (so the
scaled balance != underlying — the NMWPI6-shaped discriminator). The
MockRpcServer's `eth_call_responses` are keyed by selector — sufficient for
this one-user fixture: `scaledBalanceOf` + `getPreviousIndex` both serve the
canonical on-chain truth = the Rust writer's scaled-balance output.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot.degenbot_rs import (
    CancelHandle,
    run_aave_update,
    verify_touched_positions_on_chain,
)
from tests.aave.writer_parity.harness import (
    FIXTURE_BLOCK,
    make_erc20_transfer_log,
    make_reserve_initialized_log,
    make_scaled_token_mint_log,
    make_supply_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path


# scaledBalanceOf(address) selector (keccak256 first 4 bytes).
SCALED_BALANCE_OF_SELECTOR = "0x1da24f3e"
# getPreviousIndex(address) selector (keccak256 first 4 bytes).
GET_PREVIOUS_INDEX_SELECTOR = "0xe0753986"


_RAY = 10**27  # the liquidity index steady-state value.
_INDEX = 2 * _RAY  # 2*RAY — the discriminator (scaled != underlying at non-1).
_AMOUNT = 10**15  # the SCALED balance post-supply (the DB row's expected value).
_SUPPLY_UNDERLYING = _AMOUNT * _INDEX // _RAY  # = 2 * _AMOUNT (canonical).

_WETH = "0x" + "c0" * 20
_A_WETH = "0x" + "77" * 20
_V_WETH = "0x" + "88" * 20
_SENDER = "0x" + "e4" * 20
_TX_SUPPLY = "0x" + b"s1".rjust(32, b"\x00").hex()

# Post-supply, SENDER's on-chain scaled balance = _AMOUNT; the liquidity
# index at supply-block = _INDEX (matches the Mint event's index + the
# reserve's liquidity_index field). The MockRpcServer returns these for ANY
# caller (one-user fixture — the selector-keyed map is sufficient).
_ON_CHAIN_SCALED_BALANCE = _AMOUNT
_ON_CHAIN_LAST_INDEX = _INDEX


def _u256_hex(value: int) -> str:
    """Pack a uint256 as `0x` + 64-hex (the ABI return encoding)."""
    return "0x" + format(value, "064x")


def _eth_call_responses() -> dict[str, str]:
    # Mirror test_supply_same_chunk_stale_fetch's responses (asset lookups
    # for ReserveInitialized's revision + price-source resolution) AND
    # add the verify fn's scaledBalanceOf + getPreviousIndex selectors. The
    # verify fn's responses return the on-chain truth = _ON_CHAIN_* matches
    # the writer's post-chunk DB state on the GREEN arm.
    from tests.aave.writer_parity.harness import (
        ATOKEN_REVISION_SELECTOR,
        DEBT_TOKEN_REVISION_SELECTOR,
        GET_SOURCE_OF_ASSET_SELECTOR,
    )
    a_rev = 1
    v_rev = 1
    price_source = "0x" + "ab" * 20
    return {
        ATOKEN_REVISION_SELECTOR: "0x" + a_rev.to_bytes(32, "big").hex(),
        DEBT_TOKEN_REVISION_SELECTOR: "0x" + v_rev.to_bytes(32, "big").hex(),
        GET_SOURCE_OF_ASSET_SELECTOR: "0x" + "00" * 12 + price_source[2:],
        SCALED_BALANCE_OF_SELECTOR: _u256_hex(_ON_CHAIN_SCALED_BALANCE),
        GET_PREVIOUS_INDEX_SELECTOR: _u256_hex(_ON_CHAIN_LAST_INDEX),
    }


def _build_chunk_logs() -> list[dict[str, object]]:
    """One chunk at FIXTURE_BLOCK: SENDER supplies WETH → Mint (scaled).

    The Supply pool_event amount = _SUPPLY_UNDERLYING (= 2 * _AMOUNT). The
    Mint event's `value` field is the UNDERLYING (= _SUPPLY_UNDERLYING) per
    Aave V3's spec — `amounts_match` checks `Mint.value - balance_increase
    == Supply.pool_amount`. The Rust writer's `dispatch_supply` then computes
    scaled_amount = ray_div(_SUPPLY_UNDERLYING, _INDEX) = _AMOUNT → the DB
    position.balance the MockRpcServer's `scaledBalanceOf` returns. So the
    on-chain truth = the DB state (GREEN when the writer's fixes are applied).
    """
    li = 0
    logs: list[dict[str, object]] = []

    # tx 1: ReserveInitialized (creates WETH + aWETH + vWETH).
    logs.append(
        make_reserve_initialized_log(asset=_WETH, a_token=_A_WETH, v_token=_V_WETH, log_index=li)
    )
    li += 1

    # tx 2: SENDER supplies WETH — Pool Supply + aWETH Mint (from zero) +
    # the mint-from-zero companion Transfer.
    logs.append(
        make_supply_log(
            reserve=_WETH,
            on_behalf_of=_SENDER,
            user=_SENDER,
            amount=_SUPPLY_UNDERLYING,
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
            value=_SUPPLY_UNDERLYING,  # Mint `value` is UNDERLYING (Aave V3 spec)
            balance_increase=0,
            index=_INDEX,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_SUPPLY,
        )
    )
    li += 1
    logs.append(
        make_erc20_transfer_log(
            token_address=_A_WETH,
            from_address="0x" + "00" * 20,
            to_address=_SENDER,
            value=_SUPPLY_UNDERLYING,
            log_index=li,
            block=FIXTURE_BLOCK,
            tx_hash=_TX_SUPPLY,
        )
    )
    return logs


def _drive_rust(rust_path: str, rpc_url: str) -> dict[str, object]:
    """Run the Rust writer on the fixture; return its report."""
    cancel = CancelHandle()
    return run_aave_update(
        database_path=str(rust_path),
        chain_id=1,
        market_id=1,
        to_block=FIXTURE_BLOCK,
        chunk_size=100,
        rpc_url=rpc_url,
        progress_callback=lambda _progress=None, **_kw: None,
        cancel_handle=cancel,
    )


def _corrupt_balance(rust_path: str, user_address: str, new_balance: int) -> None:
    """Mutate the SENDER's collateral position balance to `new_balance` in
    the DB. Used by the RED case to simulate a regression that left the DB
    state silently wrong."""
    engine = create_engine(f"sqlite:///{rust_path}")
    with Session(engine) as s:
        s.execute(
            text(
                "UPDATE aave_v3_collateral_positions \
                 SET balance = :b \
                 WHERE user_id IN (SELECT id FROM aave_v3_users WHERE LOWER(address) = LOWER(:a))"
            ),
            {"b": str(new_balance), "a": user_address},
        )
        s.commit()
    engine.dispose()


def _scaled_balance_of(rust_path: str, user_address: str) -> int:
    """Helper: read the SENDER's collateral position balance from the DB
    (to confirm the writer seeded the expected scaled balance)."""
    engine = create_engine(f"sqlite:///{rust_path}")
    with Session(engine) as s:
        row = s.execute(
            text(
                "SELECT cp.balance FROM aave_v3_collateral_positions cp \
                 JOIN aave_v3_users u ON u.id = cp.user_id WHERE LOWER(u.address) = LOWER(:a)"
            ),
            {"a": user_address},
        ).one()
        result = int(row[0])
    engine.dispose()
    return result


@pytest.mark.parametrize("corrupt_db", [False, True], ids=["green", "red"])
def test_verify_touched_positions_on_chain_catches_corrupted_balance(
    tmp_path: Path,
    *,
    corrupt_db: bool,
) -> None:
    """GREEN: DB matches on-chain truth → no divergences.
    RED: corrupt the DB balance + verify surfaces a NAMED divergence
    (proving the gate catches masked wrong-value bugs the crash-gate cannot).
    """
    logs = _build_chunk_logs()
    with (
        seeded_db(tmp_path, name="rust", with_price_oracle=True) as (rust_path, _rust_session),
        mock_rpc_server(
            logs=logs,
            block_number=FIXTURE_BLOCK,
            eth_call_responses=_eth_call_responses(),
        ) as rpc_url,
    ):
        # Drive the chunk through the Rust writer — FIX-in-place (the writer
        # writes _AMOUNT as SENDER's scaled balance).
        report_dict = _drive_rust(rust_path, rpc_url)
        assert report_dict["chunks_committed"] == 1, (
            f"fixture setup failed: rust writer didn't commit its chunk; "
            f"report={report_dict}"
        )

        # The writer's DB state post-chunk.
        db_balance = _scaled_balance_of(str(rust_path), _SENDER)
        # The MockRpcServer's on-chain truth.
        on_chain = _ON_CHAIN_SCALED_BALANCE
        assert db_balance == on_chain, (
            f"fixture setup failed: DB balance {db_balance} != on-chain {on_chain}"
        )

        if corrupt_db:
            # RED arm: corrupt the DB balance to a wrong value (simulating a
            # masked wrong-value bug — e.g. the pre-NMWPI6 user-as-int U256
            # would have left balance = a wildly wrong but always-positive
            # value, structurally invisible to the crash-gate).
            _corrupt_balance(str(rust_path), _SENDER, db_balance + 1)

        # The gate fires: verify_touched_positions_on_chain.
        # NOTE: `None` for `touched_users` instructs the verify fn to check
        # ALL nonzero positions (verify-all mode — the orchestrator-required
        # surface-44JTVN-before-crash path: positions whose divergence was
        # introduced in EARLIER chunks surface before their
        # crash-cycle chunk commits). The touched-filter would miss p534
        # here (it wasn't touched in THIS chunk).
        divergences = verify_touched_positions_on_chain(
            database_path=str(rust_path),
            rpc_url=rpc_url,
            market_id=1,
            chain_id=1,
            block_number=FIXTURE_BLOCK,
            touched_users=None,
        )

        if corrupt_db:
            # RED — the gate surfaces the SENDER's collateral balance
            # divergence (proving it catches the masked wrong-value class).
            assert len(divergences) >= 1, "expected at least one divergence on RED arm"
            bal_divs = [d for d in divergences if d["field"] == "balance"]
            assert bal_divs, "expected a balance divergence on RED arm"
            d = bal_divs[0]
            assert d["kind"] == "collateral"
            assert d["user_address"].lower() == _SENDER.lower()
            assert d["token_address"].lower() == _A_WETH.lower()
            assert d["block_number"] == FIXTURE_BLOCK
            assert int(d["expected"]) == db_balance + 1  # the corrupted value
            assert int(d["actual"]) == on_chain  # the on-chain truth (_AMOUNT)
        else:
            # GREEN — the on-chain truth matches the DB state.
            assert divergences == [], (
                f"expected no divergences on GREEN arm; got: {divergences}"
            )
