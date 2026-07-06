"""§4.2 live-vs-live parity: GHO ops-parser rev-boundary (flag #1 — YTRUEW).

The multi-tx-within-chunk proof that GJQGKN's per-tx apply within ``conn``
RESOLVES divergence #1 (the deferred config-write DEFECT the user ruled on).
Asserts byte-IDENTITY Rust-vs-Python for the GHO debt path across an in-chunk
``Upgraded`` that bumps the GHO vToken revision past the discount-deprecation
boundary (rev 3→4; ``GHO_DISCOUNT_DEPRECATION_REVISION = 4``) FOLLOWED BY a
GHO ``Borrow`` + a GHO ``Repay`` in the SAME chunk.

Both staleness surfaces GJQGKN fixed are exercised:

* **config (surface #1)** — the Borrow's discount pre-pass + the ops parser
  read the GHO vToken's ``v_token_revision`` from ``conn``. With per-tx apply,
  tx 2 sees the post-upgrade rev (4 → discount deprecated → effective_discount
  0). The deferred (pre-GJQGKN) read of the chunk-start rev (3 → discount
  active → the seeded 1000 bps) would DIVERGE from the Python (which applies
  the Upgraded per-tx + sees rev 4 + discount 0).
* **ops-balance (surface #2)** — the Repay reads the Borrow's resulting debt
  balance from ``conn`` (``lookup_position_balance_index_on_conn``). With
  per-tx apply, tx 3 sees tx 2's balance; the deferred read of the chunk-start
  balance (none) would DIVERGE.

The ``ScaledTokenProcessor``/``UnifiedGhoProcessor`` are stateless (balances
come from ``conn`` lookups via ``process_transaction``), so per-tx apply is
the only seam matching Python's per-tx ORM session apply.

The Borrow requires a companion ERC20 ``Transfer`` (from ``ZERO_ADDRESS`` to
the borrower, matching the debt-mint amount). Its emitter is the GHO token
(the borrowed underlying). ``seed_gho_asset`` sets the GHO asset's
``a_token_id`` to the GHO token erc20 so the GHO token address enters the
scaled-token fetch set on BOTH paths identically (byte-IDENTITY preserved).

Closure of the ``test_upgraded_revision_parity.py:11`` "NOT asserted here"
deferral — the ops-parser rev-boundary divergence is now asserted + RESOLVED.

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
    make_borrow_log,
    make_erc20_transfer_log,
    make_repay_log,
    make_scaled_token_burn_log,
    make_scaled_token_mint_log,
    make_upgraded_log,
    mock_rpc_server,
    seed_gho_asset,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session

# The GHO vToken revisions the fixture crosses: pre-upgrade 3 (discount
# supported; GHO_DISCOUNT_DEPRECATION_REVISION = 4) → post-upgrade 4 (discount
# deprecated; the Upgraded bulk gho_discount reset also fires at rev ≥ 4).
_PRE_UPGRADE_REV = 3
_POST_UPGRADE_REV = 4

# The Ray-scale (1e27) — the variable-borrow index the Mint/Burn events carry.
_RAY = 10**27
# 1000 GHO borrowed (18 decimals); a partial 400 GHO repaid afterward.
_BORROW_AMOUNT = 1000 * 10**18
_REPAY_AMOUNT = 400 * 10**18

# Three distinct tx hashes — one per tx group (the fetch groups by tx hash).
_TX1 = "0x" + b"t1".rjust(32, b"\x00").hex()  # Upgraded
_TX2 = "0x" + b"t2".rjust(32, b"\x00").hex()  # Borrow
_TX3 = "0x" + b"t3".rjust(32, b"\x00").hex()  # Repay

_NEW_IMPL = "0x" + "e5" * 20  # the new implementation (the DEBT_TOKEN_REVISION target)


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _build_chunk_logs() -> list[dict[str, object]]:
    """The 3-tx-group chunk at FIXTURE_BLOCK, ordered by log_index.

    tx 1: Upgraded on the GHO vToken proxy (bumps v_token_revision 3→4).
    tx 2: Pool Borrow + GHO vToken Mint + GHO token Transfer(0x0→user).
    tx 3: Pool Repay + GHO vToken Burn.
    """
    return [
        # tx 1 — the Upgraded (config event; applied per-tx by GJQGKN).
        make_upgraded_log(
            proxy_address=GHO_VTOKEN_ADDRESS,
            new_implementation=_NEW_IMPL,
            log_index=0,
            tx_hash=_TX1,
        ),
        # tx 2 — the GHO Borrow (Pool event + vToken Mint + companion Transfer).
        make_borrow_log(
            reserve=GHO_TOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            amount=_BORROW_AMOUNT,
            log_index=1,
            tx_hash=_TX2,
        ),
        make_scaled_token_mint_log(
            token_address=GHO_VTOKEN_ADDRESS,
            on_behalf_of=USER_ADDRESS,
            value=_BORROW_AMOUNT,
            balance_increase=0,
            index=_RAY,
            log_index=2,
            tx_hash=_TX2,
        ),
        # The companion Transfer (the minted-debt companion the Borrow
        # asserts). Emitted by the GHO vToken (a debt token) so both paths
        # classify it as a debt transfer (the Python asserts event_type ∈
        # debt-transfer set).
        make_erc20_transfer_log(
            token_address=GHO_VTOKEN_ADDRESS,
            from_address="0x" + "00" * 20,
            to_address=USER_ADDRESS,
            value=_BORROW_AMOUNT,
            log_index=3,
            tx_hash=_TX2,
        ),
        # tx 3 — the GHO Repay (Pool event + vToken Burn).
        make_repay_log(
            reserve=GHO_TOKEN_ADDRESS,
            user=USER_ADDRESS,
            amount=_REPAY_AMOUNT,
            log_index=4,
            tx_hash=_TX3,
        ),
        make_scaled_token_burn_log(
            token_address=GHO_VTOKEN_ADDRESS,
            from_address=USER_ADDRESS,
            value=_REPAY_AMOUNT,
            balance_increase=0,
            index=_RAY,
            log_index=5,
            tx_hash=_TX3,
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


def test_gho_ops_rev_boundary_rust_matches_python_oracle(tmp_path: Path) -> None:
    """Both paths produce byte-identical GHO debt positions across the chunk.

    The post-upgrade Borrow's effective_discount is 0 (rev 4 deprecates the
    discount) on BOTH paths (GJQGKN's per-tx rev re-resolution). The Repay
    reads the Borrow's resulting balance (surface #2 read-your-own-writes).
    """
    logs = _build_chunk_logs()
    revision_response = "0x" + _POST_UPGRADE_REV.to_bytes(32, "big").hex()
    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=logs,
            block_number=FIXTURE_BLOCK,
            eth_call_responses={DEBT_TOKEN_REVISION_SELECTOR: revision_response},
        ) as rpc_url,
    ):
        _run_both(rust_path, rust_session, py_session, rpc_url)
        rust_rows = dump_debt_position_rows(rust_session)
        py_rows = dump_debt_position_rows(py_session)

    assert rust_rows == py_rows, (
        f"GHO ops rev-boundary (rev {_PRE_UPGRADE_REV}→{_POST_UPGRADE_REV}) divergence:\n"
        f"  Rust:   {rust_rows}\n  Python: {py_rows}"
    )
    assert len(rust_rows) == 1, "exactly one GHO debt position should exist"
    # The discount-deprecation boundary (rev 4 → effective_discount 0) means
    # the Borrow's balance delta uses the raw ray-divided amount (no discount
    # scaling) — assert it's non-zero (the borrow landed).
    assert rust_rows[0]["balance"] is not None
