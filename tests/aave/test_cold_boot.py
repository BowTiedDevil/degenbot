"""Cold-boot bootstrap tests for `run_aave_update` (O4BOST).

Verifies the Rust updater self-bootstraps the `POOL`/`POOL_CONFIGURATOR`
contract rows on a fresh market (only `POOL_ADDRESS_PROVIDER` + the GHO token
seeded), via a mock JSON-RPC server serving the `ProxyCreated` events + the
`POOL_REVISION()`/`CONFIGURATOR_REVISION()` `eth_call` returns.

The bootstrap pass fetches `ProxyCreated` over `[from_block, from_block +
BOOTSTRAP_WINDOW]` + applies the rows idempotently. The chunk loop then
re-encounters the same `ProxyCreated` events (the chunk's address-provider
fetch overlaps the bootstrap window) — the `ContractInserted` arm must be
idempotent for `name ∈ {POOL, POOL_CONFIGURATOR}` so no duplicate row appears.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot._ffi import CancelHandle, db_upgrade_database, run_aave_update
from tests.aave.writer_parity.harness import (
    CONFIGURATOR_REVISION_SELECTOR,
    POOL_ADDRESS,
    POOL_ADDRESS_PROVIDER_ADDRESS,
    POOL_CONFIGURATOR_ADDRESS,
    POOL_REVISION_SELECTOR,
    _make_log,
    _pad_address,
    _u256,
    mock_rpc_server,
)

# The ProxyCreated event topic (keccak of the signature) — see
# `degenbot_decoders::aave_event_decoder::PROXY_CREATED_TOPIC`.
PROXY_CREATED_TOPIC = "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478"

# The `id` bytes32 values the bootstrap matches against — right-padded ASCII
# (NOT keccak — see `match_proxy_id` + the §4.2 finding). b"POOL" = 0x504f4f4c,
# b"POOL_CONFIGURATOR" = 0x504f4f4c5f434f4e464947555241544f52.
_POOL_ID = "0x" + b"POOL".hex().ljust(64, "0")
_POOL_CONFIGURATOR_ID = "0x" + b"POOL_CONFIGURATOR".hex().ljust(64, "0")

# Distinct implementation addresses (the ProxyCreated topics[3]); the mock
# dispatches `eth_call` by selector, so the target address is immaterial, but
# distinct impls keep the test self-documenting.
_POOL_IMPL = "0x" + "12" * 20
_CONFIGURATOR_IMPL = "0x" + "23" * 20


def _make_proxy_created_log(
    *,
    proxy_id: str,
    proxy_address: str,
    implementation_address: str,
    block: int,
    log_index: int,
) -> dict[str, Any]:
    """Build a canned ProxyCreated log (emitted by the PoolAddressProvider)."""
    return _make_log(
        address=POOL_ADDRESS_PROVIDER_ADDRESS.lower(),
        topics=[
            PROXY_CREATED_TOPIC,
            proxy_id,
            _pad_address(proxy_address),
            _pad_address(implementation_address),
        ],
        data="0x",  # ProxyCreated has no data (all 3 args indexed).
        block=block,
        log_index=log_index,
    )


def _seed_cold_boot_db(db_path: Path, *, market_id: int = 1, last_update_block: int = 1000) -> None:
    """Seed a fresh schema with ONLY the market + POOL_ADDRESS_PROVIDER + GHO.

    Mirrors `aave activate ethereum_aave_v3`'s thin seed (no POOL/
    POOL_CONFIGURATOR — the cold-boot gap O4BOST closes).
    """
    db_upgrade_database(str(db_path))
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        session.execute(
            text(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, "
                "last_update_block) VALUES (:id, 1, 'aave', 1, :block)"
            ),
            {"id": market_id, "block": last_update_block},
        )
        session.execute(
            text(
                "INSERT INTO aave_v3_contracts (market_id, name, address) "
                "VALUES (:m, 'POOL_ADDRESS_PROVIDER', :a)"
            ),
            {"m": market_id, "a": POOL_ADDRESS_PROVIDER_ADDRESS},
        )
        # The GHO token + aave_gho_tokens row (build_fetch_spec fetches the
        # chain GHO asset; the Rust treats it as Option but the column must
        # exist for the chain).
        session.execute(
            text("INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, :a)"),
            {"a": "0x" + "55" * 20},
        )
        session.execute(
            text(
                "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, "
                "v_gho_discount_rate_strategy, v_gho_discount_token) "
                "VALUES (1, 1, NULL, NULL, NULL)"
            ),
        )
        session.commit()
    engine.dispose()


def _dump_contracts(db_path: Path) -> list[dict[str, Any]]:
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        rows = session.execute(
            text("SELECT name, address, revision FROM aave_v3_contracts ORDER BY name")
        ).all()
        out = [dict(r._mapping) for r in rows]
    engine.dispose()
    return out


def _stamp(db_path: Path) -> int | None:
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        row = session.execute(
            text("SELECT last_update_block FROM aave_v3_markets WHERE id = 1")
        ).one()
        out = row[0]
    engine.dispose()
    return out


def test_cold_boot_creates_pool_and_configurator_rows() -> None:
    """A fresh market self-bootstraps POOL/POOL_CONFIGURATOR via ProxyCreated."""
    import tempfile

    tmp = Path(tempfile.mkdtemp(prefix="cold-boot-"))
    db_path = tmp / "cand.db"
    _seed_cold_boot_db(db_path, last_update_block=1000)
    # from_block = 1001; the bootstrap fetches [1001, 3001]. The ProxyCreated
    # events land at 1001 (within the window). to_block = 1002 so the chunk
    # loop re-encounters the ProxyCreated at 1001 (overlapping the bootstrap
    # window) — exercising the idempotent chunk arm.
    logs = [
        _make_proxy_created_log(
            proxy_id=_POOL_ID,
            proxy_address=POOL_ADDRESS,
            implementation_address=_POOL_IMPL,
            block=1001,
            log_index=0,
        ),
        _make_proxy_created_log(
            proxy_id=_POOL_CONFIGURATOR_ID,
            proxy_address=POOL_CONFIGURATOR_ADDRESS,
            implementation_address=_CONFIGURATOR_IMPL,
            block=1001,
            log_index=1,
        ),
    ]
    eth_calls = {
        POOL_REVISION_SELECTOR: _u256(1),
        CONFIGURATOR_REVISION_SELECTOR: _u256(1),
    }
    with mock_rpc_server(logs=logs, block_number=1002, eth_call_responses=eth_calls) as rpc_url:
        report = run_aave_update(
            database_path=str(db_path),
            chain_id=1,
            market_id=1,
            to_block=1002,
            chunk_size=10000,
            rpc_url=rpc_url,
            progress_callback=lambda **_kw: None,
            cancel_handle=CancelHandle(),
        )
    assert report["chunks_committed"] >= 1, report
    # Both bootstrap rows created with the proxy address + revision 1.
    contracts = {c["name"]: c for c in _dump_contracts(db_path)}
    assert "POOL" in contracts, contracts
    assert "POOL_CONFIGURATOR" in contracts, contracts
    assert "POOL_ADDRESS_PROVIDER" in contracts, contracts
    assert contracts["POOL"]["address"] == POOL_ADDRESS, contracts
    assert contracts["POOL_CONFIGURATOR"]["address"] == POOL_CONFIGURATOR_ADDRESS, contracts
    assert contracts["POOL"]["revision"] == 1, contracts
    assert contracts["POOL_CONFIGURATOR"]["revision"] == 1, contracts
    # Exactly ONE row per bootstrap contract (no duplicate from the chunk
    # re-encounter — the idempotent ContractInserted arm).
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        pool_count = session.execute(
            text("SELECT COUNT(*) FROM aave_v3_contracts WHERE name = 'POOL' AND address = :a"),
            {"a": POOL_ADDRESS},
        ).scalar()
        cfg_count = session.execute(
            text(
                "SELECT COUNT(*) FROM aave_v3_contracts WHERE name = 'POOL_CONFIGURATOR' "
                "AND address = :a"
            ),
            {"a": POOL_CONFIGURATOR_ADDRESS},
        ).scalar()
    engine.dispose()
    assert pool_count == 1, f"POOL row duplicated: {pool_count}"
    assert cfg_count == 1, f"POOL_CONFIGURATOR row duplicated: {cfg_count}"
    # The Rust advanced the stamp to to_block (the Python doesn't stamp —
    # Rust-owned; this is the cold-boot proof the loop ran past build_fetch_spec).
    assert _stamp(db_path) == 1002, _stamp(db_path)


def test_max_chunks_caps_loop_at_n_committed_chunks() -> None:
    """``max_chunks=1`` stops the Rust loop after committing ONE chunk.

    The Rust core owns chunking; before the ``max_chunks`` cap the Python
    ``--one-chunk`` flag was downgraded to a warning + the run advanced all
    the way to ``to_block``. With ``max_chunks`` wired through, the loop breaks
    after the Nth committed chunk + ``last_update_block`` is stamped to that
    chunk's end (so the next run resumes from there).
    """
    import tempfile

    tmp = Path(tempfile.mkdtemp(prefix="max-chunks-"))
    db_path = tmp / "cand.db"
    # from_block = 1001; to_block = 1009; chunk_size = 2 → would be
    # chunks [1001-1002],[1003-1004],[1005-1006],[1007-1008],[1009] (5 chunks).
    # max_chunks=1 → must stop after the FIRST committed chunk (1001-1002).
    _seed_cold_boot_db(db_path, last_update_block=1000)
    logs = [
        _make_proxy_created_log(
            proxy_id=_POOL_ID,
            proxy_address=POOL_ADDRESS,
            implementation_address=_POOL_IMPL,
            block=1001,
            log_index=0,
        ),
        _make_proxy_created_log(
            proxy_id=_POOL_CONFIGURATOR_ID,
            proxy_address=POOL_CONFIGURATOR_ADDRESS,
            implementation_address=_CONFIGURATOR_IMPL,
            block=1001,
            log_index=1,
        ),
    ]
    eth_calls = {
        POOL_REVISION_SELECTOR: _u256(1),
        CONFIGURATOR_REVISION_SELECTOR: _u256(1),
    }
    with mock_rpc_server(logs=logs, block_number=1009, eth_call_responses=eth_calls) as rpc_url:
        report = run_aave_update(
            database_path=str(db_path),
            chain_id=1,
            market_id=1,
            to_block=1009,
            chunk_size=2,
            rpc_url=rpc_url,
            progress_callback=lambda **_kw: None,
            cancel_handle=CancelHandle(),
            max_chunks=1,
        )
    # Exactly ONE chunk committed (the cap fired before the 2nd chunk).
    assert report["chunks_committed"] == 1, report
    # The stamp advanced to the FIRST chunk's end (1002), NOT to_block (1009).
    assert _stamp(db_path) == 1002, ("max_chunks must cap the stamp at chunk_end", _stamp(db_path))
    # The report's to_block must reflect where it actually stopped (the capped
    # chunk's end), NOT the requested to_block.
    assert report["to_block"] == 1002, ("to_block must reflect the cap", report)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
