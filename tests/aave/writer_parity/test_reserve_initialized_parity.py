"""§4.2 live-vs-live parity: ``ReserveInitialized`` (the regular-reserve slice).

``ReserveInitialized(address indexed asset, address indexed aToken,
address stableDebtToken, address variableDebtToken,
address interestRateStrategyAddress)`` — emitted by the PoolConfigurator.
Both paths create 3 erc20 rows (asset / aToken / vToken — NULL metadata,
both paths' `_fetch_erc20_token_metadata`/`fetch_erc20_metadata` get the
mock's empty `0x` return + return None) + a new ``aave_v3_assets`` row
(a/v token revisions via EIP-1967 → ``ATOKEN_REVISION()``/
``DEBT_TOKEN_REVISION()`` eth_calls + price_source via
``getSourceOfAsset(address)`` eth_call on the PRICE_ORACLE contract).

This is the divergence-free slice: the asset's underlying is NOT the GHO
token → the Python's GHO-vToken-FK-link branch (event_handlers.py:689-698)
is dead → both paths write byte-identical rows.

DIVERGENCE #8 (the GHO-asset path): when the ReserveInitialized event's
underlying IS the GHO token, the Python sets ``aave_gho_tokens.v_token_id``
to the new vToken's erc20 id; the Rust's
``resolve_reserve_initialized``/``apply_reserve_initialized_on_conn`` do
NOT. This is a DEFECT (task ``2QGL6G``) — the missing FK is a precondition
for ULDUAC's emitter-validation guard (which compares against
``gho_asset.v_token_address`` resolved via the FK). Tested separately +
flagged; this test covers only the regular-reserve slice.

Per §4.3, TEMPORARY — retired with the Python oracle in CZM7TI (which now
deps on U5YIBG + ULDUAC + 2QGL6G).
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
    ATOKEN_REVISION_SELECTOR,
    BOOTSTRAP_BLOCK,
    DEBT_TOKEN_REVISION_SELECTOR,
    FIXTURE_BLOCK,
    GET_SOURCE_OF_ASSET_SELECTOR,
    GHO_TOKEN_ADDRESS,
    dump_gho_token_rows,
    make_reserve_initialized_log,
    mock_rpc_server,
    seeded_db,
)

if TYPE_CHECKING:
    from pathlib import Path

    from sqlalchemy.orm import Session


def _load_market(session: Session) -> AaveV3Market:
    return session.scalars(select(AaveV3Market).where(AaveV3Market.id == 1)).one()


def _dump_asset_and_tokens(session: Session) -> dict[str, list[dict[str, object]]]:
    """Dump the ReserveInitialized-set asset columns + the 3 new erc20 rows."""
    assets = [
        dict(row._mapping)
        for row in session.execute(
            text(
                "SELECT id, market_id, underlying_asset_id, a_token_id, "
                "a_token_revision, v_token_id, v_token_revision, price_source, "
                "liquidity_index, liquidity_rate, borrow_index, borrow_rate "
                "FROM aave_v3_assets ORDER BY id"
            )
        ).all()
    ]
    # The 3 newly-created erc20 rows (excludes the seeded GHO token id=1).
    tokens = [
        dict(row._mapping)
        for row in session.execute(
            text(
                "SELECT id, chain, address, name, symbol, decimals "
                "FROM erc20_tokens WHERE id > 1 ORDER BY id"
            )
        ).all()
    ]
    return {"assets": assets, "tokens": tokens}


def test_reserve_initialized_regular_reserve_rust_matches_python_oracle(
    tmp_path: Path,
) -> None:
    """Both paths create byte-identical erc20 + aave_v3_assets rows."""
    # Fresh addresses (none == GHO_TOKEN_ADDRESS 0x55…55) — the Python's
    # GHO-link branch (event_handlers.py:689) is dead → byte-identical.
    asset_addr = "0x" + "aa" * 20
    a_token_addr = "0x" + "bb" * 20
    v_token_addr = "0x" + "cc" * 20
    assert asset_addr != GHO_TOKEN_ADDRESS, "fixture collision with GHO underlying"

    fixture_log = make_reserve_initialized_log(
        asset=asset_addr, a_token=a_token_addr, v_token=v_token_addr
    )
    a_rev, v_rev, price_source = 1, 2, "0x" + "d0" * 20

    eth_calls = {
        ATOKEN_REVISION_SELECTOR: "0x" + a_rev.to_bytes(32, "big").hex(),
        DEBT_TOKEN_REVISION_SELECTOR: "0x" + v_rev.to_bytes(32, "big").hex(),
        # The Python's get_checksum_address lowercases then re-checksums;
        # serve the checksummed form so both paths compare identically.
        GET_SOURCE_OF_ASSET_SELECTOR: "0x" + "00" * 12 + price_source[2:],
    }

    with (
        seeded_db(tmp_path, name="rust", with_price_oracle=True) as (rust_path, rust_session),
        seeded_db(tmp_path, name="py", with_price_oracle=True) as (_py_path, py_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses=eth_calls,
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
        rust_dump = _dump_asset_and_tokens(rust_session)
        py_dump = _dump_asset_and_tokens(py_session)

    assert rust_dump == py_dump, (
        f"ReserveInitialized divergence:\n"
        f"  Rust assets:   {rust_dump['assets']}\n"
        f"  Python assets: {py_dump['assets']}\n"
        f"  Rust tokens:   {rust_dump['tokens']}\n"
        f"  Python tokens: {py_dump['tokens']}"
    )
    # Spot-check the revisions + price_source landed (not just NULL defaults).
    [rust_asset] = rust_dump["assets"]
    assert rust_asset["a_token_revision"] == a_rev
    assert rust_asset["v_token_revision"] == v_rev
    assert rust_asset["price_source"] is not None


@pytest.mark.parametrize("gho_link", [True, False], ids=["gho-asset", "regular"])
def test_reserve_initialized_gho_vtoken_fk_link_rust_matches_python_oracle(
    tmp_path: Path,
    gho_link: bool,  # noqa: FBT001 (pytest parametrize fixture, not an API arg)
) -> None:
    """Both paths set `aave_gho_tokens.v_token_id` identically (2QGL6G).

    When the ReserveInitialized event's underlying IS the GHO token, the
    Python's `_process_reserve_initialized_event` (event_handlers.py:689-698)
    links `aave_gho_tokens.v_token_id` to the new vToken's erc20 id; the
    Rust (after the 2QGL6G fix) mirrors it. The `gho_link=False` variant
    re-asserts a regular reserve leaves the FK NULL (the divergence-free
    slice). The asset/aToken/vToken erc20 rows + the `aave_v3_assets` row
    are also asserted byte-identical.
    """
    if gho_link:
        asset_addr = GHO_TOKEN_ADDRESS  # 0x55…55 — matches the seeded GHO token
    else:
        asset_addr = "0x" + "ee" * 20  # a regular reserve
        assert asset_addr != GHO_TOKEN_ADDRESS
    a_token_addr = "0x" + "bb" * 20
    v_token_addr = "0x" + "cc" * 20

    fixture_log = make_reserve_initialized_log(
        asset=asset_addr, a_token=a_token_addr, v_token=v_token_addr
    )
    a_rev, v_rev, price_source = 1, 2, "0x" + "d0" * 20
    eth_calls = {
        ATOKEN_REVISION_SELECTOR: "0x" + a_rev.to_bytes(32, "big").hex(),
        DEBT_TOKEN_REVISION_SELECTOR: "0x" + v_rev.to_bytes(32, "big").hex(),
        GET_SOURCE_OF_ASSET_SELECTOR: "0x" + "00" * 12 + price_source[2:],
    }

    with (
        seeded_db(tmp_path, name="rust", with_price_oracle=True) as (rust_path, rust_session),
        seeded_db(tmp_path, name="py", with_price_oracle=True) as (_py_path, py_session),
        mock_rpc_server(
            logs=[fixture_log],
            block_number=FIXTURE_BLOCK,
            eth_call_responses=eth_calls,
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
        rust_asset_tokens = _dump_asset_and_tokens(rust_session)
        py_asset_tokens = _dump_asset_and_tokens(py_session)
        rust_gho = dump_gho_token_rows(rust_session)
        py_gho = dump_gho_token_rows(py_session)

    assert rust_asset_tokens == py_asset_tokens, (
        f"ReserveInitialized (gho_link={gho_link}) asset/token divergence:\n"
        f"  Rust:   {rust_asset_tokens}\n  Python: {py_asset_tokens}"
    )
    assert rust_gho == py_gho, (
        f"ReserveInitialized (gho_link={gho_link}) aave_gho_tokens divergence:\n"
        f"  Rust:   {rust_gho}\n  Python: {py_gho}"
    )
    if gho_link:
        # The FK must be linked to the new vToken's erc20 id (the 2QGL6G fix).
        [row] = rust_gho
        assert row["v_token_id"] is not None, (
            "GHO-asset path: aave_gho_tokens.v_token_id should be linked"
        )
    else:
        [row] = rust_gho
        assert row["v_token_id"] is None, (
            "Regular-reserve path: aave_gho_tokens.v_token_id must stay NULL"
        )
