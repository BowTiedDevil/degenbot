"""§4.2 YUPSIB: standalone aToken Transfer-credit stolen by the deficit_coverage
scavenger → the recipient's Withdraw crashes "balance would go negative".

Aave V3 moves collateral between users via aToken `Transfer` (+ the
companion `BalanceTransfer` scaled event). The recipient's collateral
position is credited; the sender's is debited. This is a STANDALONE
operation (no Pool Supply/Withdraw) — `create_transfer_operations` pairs the
ERC20 `Transfer` + the `BalanceTransfer` + `dispatch_balance_transfer` applies
the credit/debit.

**The bug:** `create_deficit_coverage_operations` (operations_parser.rs, step
4c — the "DeficitCoverage BalanceTransfer+Burn pairs" scavenger, the
Umbrella-protocol path) iterates ALL unassigned `CollateralTransfer`/
`Erc20CollateralTransfer` events + unconditionally marks each
`local_assigned.insert(bt_ev.log_index)` (then
`assigned_indices.extend(local_assigned)`) — for paired AND unpaired bt_evs.
So a standalone Transfer with NO paired burn (the normal aToken-transfer
case) is marked assigned WITHOUT creating a DeficitCoverage op. Then
`create_transfer_operations` (step 4e) skips it → NO standalone
BalanceTransfer op → the recipient's credit isn't applied → balance stays 0 →
the recipient's Withdraw's `CollateralBurn` goes negative → crash.

Surfaced by the W2S3WH re-drive `--to 16596000`: block 16496928 (chunk 21),
user `0x872f…30995` received aWETH via a standalone Transfer (from
`0xe4217…`) then withdrew — the credit wasn't applied → "balance would go
negative (current=0, delta=-1000000000000000)".

**RED (pre-fix):** the Rust raises `balance would go negative`.
**GREEN (post-fix):** the standalone Transfer op is created → the recipient
is credited → the Withdraw debits → non-negative. The Python can't be the
parity reference (crashes at 16496792); validated by on-chain-truth cast
probes (the (ii) gate): the user's aWETH balanceOf was 0 at block 16496927,
+ the Withdraw tx 0x4a88 contains the incoming Transfer (li=104/107)
crediting the user before the Burn (li=111).

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from sqlalchemy import text

from degenbot._ffi import CancelHandle, run_aave_update
from tests.aave.writer_parity.harness import (
    ATOKEN_REVISION_SELECTOR,
    DEBT_TOKEN_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    GET_SOURCE_OF_ASSET_SELECTOR,
    USER_ADDRESS,
    dump_collateral_position_rows,
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

_RAY = 10**27  # 1e27 — the liquidity index (steady-state).
_AMOUNT = 10**15  # 0.001 WETH (on-chain-truth value from block 16496928).

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
    USER (standalone) → USER withdraws. The standalone Transfer has NO paired
    burn (it's a normal aToken-transfer, not an Umbrella DeficitCoverage)."""
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
    # BalanceTransfer (the SCALED bookkeeping — the standalone BalanceTransfer
    # op pairs the ERC20 Transfer + the BalanceTransfer). NO paired burn — the
    # deficit_coverage scavenger (pre-fix) steals these (marks assigned without
    # creating an op) → USER's credit never lands.
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
    # Pre-fix: USER's balance=0 (the Transfer credit stolen) → the Burn →
    # negative → crash.
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


def test_standalone_transfer_credit_then_withdraw(tmp_path: Path) -> None:
    """RED (pre-fix): the Rust raises `balance would go negative` because the
    standalone aToken Transfer's credit was stolen by the deficit_coverage
    scavenger (marked assigned without creating an op).

    GREEN (post-fix YUPSIB): the standalone Transfer op is created → USER is
    credited → the Withdraw debits → non-negative → no crash + the positions
    land (SENDER + USER both settle to balance 0; the a_token == aWETH).
    """
    logs = _build_chunk_logs()
    with (
        seeded_db(tmp_path, name="rust", with_price_oracle=True) as (rust_path, rust_session),
        mock_rpc_server(
            logs=logs,
            block_number=FIXTURE_BLOCK,
            eth_call_responses=_eth_call_responses(),
        ) as rpc_url,
    ):
        run_aave_update(
            database_path=str(rust_path),
            chain_id=1,
            market_id=1,
            to_block=FIXTURE_BLOCK,
            chunk_size=100,
            rpc_url=rpc_url,
            progress_callback=lambda _progress: None,
            cancel_handle=CancelHandle(),
        )
        positions = dump_collateral_position_rows(rust_session)
        a_token_addr = rust_session.execute(
            text(
                "SELECT et.address FROM aave_v3_collateral_positions p "
                "JOIN aave_v3_assets a ON a.id = p.asset_id "
                "JOIN erc20_tokens et ON et.id = a.a_token_id "
                "WHERE p.id = (SELECT MIN(id) FROM aave_v3_collateral_positions)"
            )
        ).scalar()

    # GREEN: no crash (the standalone Transfer credited USER before the
    # Withdraw debited — the deficit_coverage scavenger no longer steals the
    # unpaired transfer). Two collateral positions land: SENDER (supplied +
    # transferred out → settles to 0) + USER (received + withdrew → 0). The
    # a_token == aWETH (the (b) re-fetch + the standalone Transfer op both
    # target the right token). Loads-bearing: the absence of the
    # `balance would go negative` crash is the YUPSIB regression guard.
    assert len(positions) == 2, f"expected 2 positions (SENDER+USER); got {len(positions)}"
    assert a_token_addr is not None, "no a_token resolved for the collateral position"
    assert a_token_addr.lower() == _A_WETH.lower(), (
        f"a_token {a_token_addr!r} ≠ {_A_WETH!r} (the WETH aToken)"
    )
    # GREEN (NMWPI6): both end-state balances are 0 (SENDER supplied
    # _AMOUNT then standalone-transferred it out; USER received then
    # withdrew). Pre-NMWPI6 the SENDER balance was the user's address
    # interpreted as a U256 (~1.3e57); the value assertion here locks the
    # correct scaled balance down (the existence-only gap that previously
    # masked the ~10^69 oddity).
    for pos in positions:
        assert int(pos["balance"]) == 0, (
            f"expected end-state balance 0 (supply+transfer-out for SENDER; "
            f"receive+withdraw for USER); got {pos!r}"
        )
