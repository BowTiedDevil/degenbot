"""§4.2 W2S3WH: scaled-token fetch staleness — ReserveInitialized + first
Supply in the SAME chunk (the "missing CollateralMint" crash).

`build_fetch_spec` (`run.rs:1234`) reads `fetch_aave_scaled_token_addresses`
from the DB ONCE at run-start (BEFORE the chunk loop). So when a chunk
contains a `ReserveInitialized` (creating an asset + its a_token) FOLLOWED BY
a `Supply` (with the matching aToken `Mint` companion) in the same chunk,
the new a_token is NOT in the frozen fetch set → the `eth_getLogs` fetch
(filtered by address) withholds the aToken `Mint` → `create_supply_operation`
crashes `matching scaled-token event not found: SUPPLY … missing
CollateralMint`. The Python re-builds the set per-chunk (commands.py:1153)
so it's LESS stale (chunk N+1 sees chunk N's commits), but it ALSO can't
handle the same-chunk case (the Python crashes at 16496792 first, so its
behavior here is untestable as a parity reference).

W2S3WH fixes (both required — see the orchestrator's direction):
* **(a)** rebuild `scaled_token_addresses` from conn at the START of each
  chunk (matches the Python's per-chunk rebuild). Fixes cross-chunk + run-
  spanning staleness. (This single-chunk test doesn't exercise (a); the
  real-RPC re-drive `--to 16596000` spanning chunk-21→22 does.)
* **(b)** per-tx re-fetch when `dispatch_config_events` registers a NEW
  a_token/v_token (same-chunk, mid-chunk-creation). THIS test exercises (b):
  the `ReserveInitialized` (tx 1) creates the asset mid-chunk; (b) re-fetches
  the new a_token's `Mint`/`Transfer` logs for the chunk range + merges them
  into the later tx's group → the `Supply` finds its `CollateralMint`.

RED state (before the fix): the Rust raises `ValueError … missing
CollateralMint`. GREEN (after (a)+(b)): the collateral position lands under
the onBehalfOf (`USER_ADDRESS`), NOT under the referralCode-as-address or a
garbage slot — the same byte-IDENTITY guarantee as 7UFMZX's parity test.

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select

from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import CancelHandle, run_aave_update
from tests.aave.writer_parity.harness import (
    ATOKEN_REVISION_SELECTOR,
    DEBT_TOKEN_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    GET_SOURCE_OF_ASSET_SELECTOR,
    USER_ADDRESS,
    make_erc20_transfer_log,
    make_reserve_initialized_log,
    make_scaled_token_mint_log,
    make_supply_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

# The Ray-scale (1e27) — the variable-borrow index the vToken Mint carries
# (also used as the liquidity index the aToken Mint carries).
_RAY = 10**27
# 1000 WETH supplied (18 decimals).
_SUPPLY_AMOUNT = 1000 * 10**18

# The Supply's `user` (data word 0) — DISTINCT from `on_behalf_of` so the
# topic[2] (onBehalfOf) + topic[3] (referralCode) slots don't collide (the
# 7UFMZX topic-indexing parity test's mask-removal, reused here).
_DEPOSITOR = "0x" + "cc" * 20
# A NON-ZERO referral code — exercises the 7UFMZX-corrected topic[3] decode.
_REFERRAL_CODE = 42

# The new reserve (WETH underlying) + its aToken/vToken — created mid-chunk
# by the ReserveInitialized. The aToken is the load-bearing address: it
# emits the `Mint` + `Transfer` companions the Supply's ops parser must find,
# but it's NOT in the frozen run-start fetch set (the asset doesn't exist
# until the ReserveInitialized is dispatched).
_WETH = "0x" + "c0" * 20
_A_WETH = "0x" + "77" * 20  # the aToken (must match the Mint/Transfer emitter)
_V_WETH = "0x" + "88" * 20  # the variable-debt token

# Two distinct tx hashes — ReserveInitialized in tx 1, Supply+Mint+Transfer
# in tx 2 (the ops parser groups logs by transactionHash).
_TX1 = "0x" + b"r1".rjust(32, b"\x00").hex()  # ReserveInitialized
_TX2 = "0x" + b"s2".rjust(32, b"\x00").hex()  # Supply + Mint + Transfer

# The revisions the mock RPC reports for the new aToken/vToken (dispatch's
# ATOKEN_REVISION()/DEBT_TOKEN_REVISION() eth_calls during ReserveInitialized).
_A_REV = 1
_V_REV = 1
_PRICE_SOURCE = "0x" + "d0" * 20


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _eth_call_responses() -> dict[str, str]:
    return {
        ATOKEN_REVISION_SELECTOR: "0x" + _A_REV.to_bytes(32, "big").hex(),
        DEBT_TOKEN_REVISION_SELECTOR: "0x" + _V_REV.to_bytes(32, "big").hex(),
        # getGetSourceOfAsset returns (address source, bool isOracle) — the
        # Python lowercases then re-checksums; serve checksummed so both paths
        # compare identically (per test_reserve_initialized_parity.py).
        GET_SOURCE_OF_ASSET_SELECTOR: "0x" + "00" * 12 + _PRICE_SOURCE[2:],
    }


def _build_chunk_logs() -> list[dict[str, object]]:
    """The chunk at FIXTURE_BLOCK, ordered by log_index.

    tx 1 (default tx hash): ReserveInitialized on the Pool Configurator
    (creates the WETH asset + its aToken/vToken erc20 rows).
    tx 2 (_TX2): Pool Supply(user=DEPOSITOR, onBehalfOf=USER,
    referralCode=42) + aToken Mint(onBehalfOf=USER, value) + aToken
    Transfer(0x0→USER) — the Supply's CollateralMint + mint-from-zero
    companion.
    """
    return [
        # `make_reserve_initialized_log` has no tx_hash param → its own tx
        # group (the default tx hash, distinct from _TX2). group_logs_by_tx
        # keys on transactionHash → 2 tx groups, ReserveInitialized first.
        make_reserve_initialized_log(
            asset=_WETH,
            a_token=_A_WETH,
            v_token=_V_WETH,
            log_index=0,
            block=FIXTURE_BLOCK,
        ),
        make_supply_log(
            reserve=_WETH,
            on_behalf_of=USER_ADDRESS,
            user=_DEPOSITOR,
            amount=_SUPPLY_AMOUNT,
            referral_code=_REFERRAL_CODE,
            log_index=1,
            block=FIXTURE_BLOCK,
            tx_hash=_TX2,
        ),
        make_scaled_token_mint_log(
            token_address=_A_WETH,
            on_behalf_of=USER_ADDRESS,
            value=_SUPPLY_AMOUNT,
            balance_increase=0,
            index=_RAY,
            log_index=2,
            block=FIXTURE_BLOCK,
            tx_hash=_TX2,
        ),
        make_erc20_transfer_log(
            token_address=_A_WETH,
            from_address="0x" + "00" * 20,
            to_address=USER_ADDRESS,
            value=_SUPPLY_AMOUNT,
            log_index=3,
            block=FIXTURE_BLOCK,
            tx_hash=_TX2,
        ),
    ]


def test_supply_same_chunk_as_reserve_init_missing_collateral_mint(tmp_path: Path) -> None:
    """RED (pre-fix): the Rust raises `missing CollateralMint` because the
    new aToken's Mint isn't in the frozen run-start fetch set.

    GREEN (post-fix (a)+(b)): the collateral position lands under USER_ADDRESS
    (the onBehalfOf), byte-identical Rust-vs-Python — implemented after the
    red assertion is confirmed.
    """
    logs = _build_chunk_logs()
    with (
        seeded_db(tmp_path, name="rust", with_price_oracle=True) as (rust_path, _),
        mock_rpc_server(
            logs=logs,
            block_number=FIXTURE_BLOCK,
            eth_call_responses=_eth_call_responses(),
        ) as rpc_url,
        pytest.raises(ValueError, match="missing CollateralMint"),
    ):
        # RED (pre-fix): the Rust crashes because the new aToken's Mint isn't
        # in the frozen run-start fetch spec → the Supply's ops parser can't
        # find the CollateralMint. After W2S3WH (a)+(b), this `pytest.raises`
        # is replaced by a byte-IDENTITY collateral-position assertion (GREEN).
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
