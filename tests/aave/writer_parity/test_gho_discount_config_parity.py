"""§4.2 live-vs-live parity: GHO ``DiscountTokenUpdated`` + ``DiscountRateStrategyUpdated``.

- ``DiscountTokenUpdated(address indexed oldDiscountToken, address indexed
  newDiscountToken)`` — sets ``aave_gho_tokens.v_gho_discount_token``.
- ``DiscountRateStrategyUpdated(address indexed oldDiscountRateStrategy,
  address indexed newDiscountRateStrategy)`` — sets
  ``aave_gho_tokens.v_gho_discount_rate_strategy``.

Both events are emitted by the GHO variable-debt token. The Python validates
the emitter == ``gho_asset.v_token.address`` (ignoring events from non-canonical
contracts); the Rust's ``dispatch_discount_*_updated`` does NOT (a §4.2 parity
gap — recorded in the corpus summary). This identity test seeds the GHO vToken +
emits from the matching address, so both paths process the event → identical
rows.

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from sqlalchemy import select

from degenbot.cli.aave.commands import update_aave_market
from degenbot.database.models.aave import AaveV3Market
from degenbot.degenbot_rs import AlloyProvider, CancelHandle, run_aave_update
from degenbot.provider.sync_adapter import ProviderAdapter
from tests.aave.writer_parity.harness import (
    BOOTSTRAP_BLOCK,
    FIXTURE_BLOCK,
    dump_gho_token_rows,
    make_discount_rate_strategy_updated_log,
    make_discount_token_updated_log,
    mock_rpc_server,
    seed_gho_vtoken,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


@pytest.mark.parametrize(
    "new_token",
    ["0x" + "a1" * 20, "0x" + "b2" * 20],
    ids=["stk-aave", "alt"],
)
def test_discount_token_updated_rust_matches_python_oracle(
    tmp_path: Path,
    new_token: str,
) -> None:
    """Both paths set byte-identical `v_gho_discount_token`."""
    fixture_log = make_discount_token_updated_log(new_discount_token=new_token)

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        seed_gho_vtoken(rust_session)
        seed_gho_vtoken(py_session)
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
        rust_rows = dump_gho_token_rows(rust_session)
        py_rows = dump_gho_token_rows(py_session)

    assert rust_rows == py_rows, (
        f"DiscountTokenUpdated(new={new_token}) divergence:\n"
        f"  Rust:   {rust_rows}\n  Python: {py_rows}"
    )
    assert rust_rows[0]["v_gho_discount_token"] is not None


@pytest.mark.parametrize(
    "new_strategy",
    ["0x" + "c3" * 20, "0x" + "d4" * 20],
    ids=["strategy-a", "strategy-b"],
)
def test_discount_rate_strategy_updated_rust_matches_python_oracle(
    tmp_path: Path,
    new_strategy: str,
) -> None:
    """Both paths set byte-identical `v_gho_discount_rate_strategy`."""
    fixture_log = make_discount_rate_strategy_updated_log(new_strategy=new_strategy)

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(logs=[fixture_log], block_number=FIXTURE_BLOCK) as rpc_url,
    ):
        seed_gho_vtoken(rust_session)
        seed_gho_vtoken(py_session)
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
        rust_rows = dump_gho_token_rows(rust_session)
        py_rows = dump_gho_token_rows(py_session)

    assert rust_rows == py_rows, (
        f"DiscountRateStrategyUpdated(new={new_strategy}) divergence:\n"
        f"  Rust:   {rust_rows}\n  Python: {py_rows}"
    )
    assert rust_rows[0]["v_gho_discount_rate_strategy"] is not None
