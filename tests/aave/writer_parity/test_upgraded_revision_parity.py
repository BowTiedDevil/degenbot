"""§4.2 live-vs-live parity: ``Upgraded`` (ScaledTokenUpgrade config-row identity).

`Upgraded(address indexed implementation)` — emitted by an aToken or vToken
proxy. Both paths look up the asset by the proxy address (a_token first, then
v_token) + RPC `ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()` on the new
implementation → UPDATE `aave_v3_assets.a_token_revision`/`v_token_revision`.

This test asserts the CONFIG-ROW identity the orchestrator authorized: the
revision column converges across paths. The seeded asset's v_token is NOT the
GHO vToken, so `deprecated_gho_token_id` is `None` (no GHO-deprecation side
effect). The ops-parser rev-boundary divergence is flag #1 (a separate,
deferred concern — NOT asserted here).

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI.
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
    A_TOKEN_ADDRESS,
    ATOKEN_REVISION_SELECTOR,
    BOOTSTRAP_BLOCK,
    DEBT_TOKEN_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    V_TOKEN_ADDRESS,
    make_upgraded_log,
    mock_rpc_server,
    seed_asset_and_user,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _dump_asset_revisions(session: Session) -> list[dict[str, object]]:
    rows = session.execute(
        text(
            "SELECT id, a_token_revision, v_token_revision "
            "FROM aave_v3_assets ORDER BY id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


@pytest.mark.parametrize(
    ("label", "proxy", "selector", "rev_fn", "new_revision"),
    [
        ("vtoken-bump", V_TOKEN_ADDRESS, DEBT_TOKEN_REVISION_SELECTOR, "DEBT_TOKEN_REVISION", 2),
        ("atoken-bump", A_TOKEN_ADDRESS, ATOKEN_REVISION_SELECTOR, "ATOKEN_REVISION", 3),
    ],
    ids=["vtoken", "atoken"],
)
def test_upgraded_revision_bump_rust_matches_python_oracle(
    tmp_path: Path,
    label: str,
    proxy: str,
    selector: str,
    rev_fn: str,
    new_revision: int,
) -> None:
    """Both paths set byte-identical `a_token_revision`/`v_token_revision`."""
    new_impl = "0x" + "e5" * 20
    fixture_log = make_upgraded_log(proxy_address=proxy, new_implementation=new_impl)
    revision_response = "0x" + new_revision.to_bytes(32, "big").hex()

    with (
        seeded_db(tmp_path, name="rust") as (rust_path, rust_session),
        seeded_db(tmp_path, name="py") as (_py_path, py_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses={selector: revision_response},
        ) as rpc_url,
    ):
        seed_asset_and_user(rust_session)
        seed_asset_and_user(py_session)
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
        rust_rows = _dump_asset_revisions(rust_session)
        py_rows = _dump_asset_revisions(py_session)

    assert rust_rows == py_rows, (
        f"Upgraded(proxy={proxy[:7]}…) divergence:\n"
        f"  Rust:   {rust_rows}\n  Python: {py_rows}"
    )
