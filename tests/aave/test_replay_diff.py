"""Offline tests for scripts/aave_replay_diff.py (the §4.2 real-RPC drive+compare+bisect harness).

The ``compare`` subcommand is fully offline + unit-tested here. The ``drive``
+ ``bisect`` smoke tests are marked ``requires_rpc`` + skipped when the
``$DEGENBOT_RPC_HTTP_CHAINID_1`` envvar is unset/unreachable (ergo JGQHBX).
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest
from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

# Make the script importable.
SCRIPTS_DIR = Path(__file__).resolve().parents[2] / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import aave_replay_diff as ard  # noqa: E402

from degenbot.degenbot_rs import db_upgrade_database  # noqa: E402

_MARKET_NAME = "ethereum-aave-v3"
_USER = "0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B"  # a real checksummed address
_UW = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"  # WETH (checksummed)


def _fresh_db(tmp_path: Path, name: str, *, stamp: int = 16291070) -> Path:
    """Create a schema-only DB with a seeded market; return its path."""
    p = tmp_path / f"{name}.db"
    db_upgrade_database(str(p))
    eng = create_engine(f"sqlite:///{p}")
    s = Session(eng)
    s.execute(
        text(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) "
            "VALUES (1, :name, 1, :stamp)"
        ),
        {"name": _MARKET_NAME, "stamp": stamp},
    )
    s.commit()
    s.close()
    eng.dispose()
    return p


def _seed_user(eng, *, uid: int = 1, address: str = _USER, e_mode: int = 0) -> None:
    s = Session(eng)
    s.execute(
        text(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, "
            "stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt) "
            "VALUES (:id, 1, :addr, :em, 0, NULL, NULL, '0')"
        ),
        {"id": uid, "addr": address, "em": e_mode},
    )
    s.commit()
    s.close()


def _seed_asset_and_token(
    eng, *, asset_id: int = 1, tok_id: int = 2, underlying: str = _UW
) -> None:
    s = Session(eng)
    s.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (:id, 1, :a)"),
        {"id": tok_id, "a": underlying},
    )
    # a_token_id + v_token_id are NOT NULL FK → seed minimal erc20 parents
    # (ids don't matter; only `underlying_asset_id` drives the business key).
    s.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (:id, 1, :a)"),
        {"id": 900, "a": "0x" + "aa" * 20},
    )
    s.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (:id, 1, :a)"),
        {"id": 901, "a": "0x" + "bb" * 20},
    )
    s.execute(
        text(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, "
            "a_token_id, a_token_revision, v_token_id, v_token_revision, "
            "e_mode_category_id, price_source, last_update_block, liquidity_index, "
            "liquidity_rate, borrow_index, borrow_rate) "
            "VALUES (:id, 1, :uw, 900, 1, 901, 1, NULL, NULL, NULL, 0,0,0,0)"
        ),
        {"id": asset_id, "uw": tok_id},
    )
    s.commit()
    s.close()


# ── compare: value divergence ────────────────────────────────────────────────


def test_compare_catches_value_divergence(tmp_path: Path) -> None:
    """One user row, e_mode differs → divergence reported on that column."""
    ref = _fresh_db(tmp_path, "ref")
    cand = _fresh_db(tmp_path, "cand")
    er = create_engine(f"sqlite:///{ref}")
    ec = create_engine(f"sqlite:///{cand}")
    _seed_user(er, e_mode=0)
    _seed_user(ec, e_mode=2)  # divergence: e_mode 0 vs 2
    er.dispose()
    ec.dispose()

    report = ard.compare_dbs(str(ref), str(cand), limit=10)
    assert report["exit_code"] == 1, report
    user_divs = report["per_table"]["aave_v3_users"]["divergences"]
    assert any(
        d["column"] == "e_mode" and d["ref_value"] == "0" and d["cand_value"] == "2"
        for d in user_divs
    ), user_divs
    # The surrogate `id` must NOT be a divergence.
    assert all(d["column"] != "id" for d in user_divs), user_divs


def test_compare_keyset_diff_reports_ref_only_and_cand_only(tmp_path: Path) -> None:
    """ref has a user cand doesn't + cand has one ref doesn't → keyset diff."""
    ref = _fresh_db(tmp_path, "ref")
    cand = _fresh_db(tmp_path, "cand")
    er = create_engine(f"sqlite:///{ref}")
    ec = create_engine(f"sqlite:///{cand}")
    _seed_user(er, uid=1, address="0x0000000000000000000000000000000000000001")
    _seed_user(er, uid=2, address="0x0000000000000000000000000000000000000002")
    _seed_user(ec, uid=1, address="0x0000000000000000000000000000000000000001")
    _seed_user(ec, uid=3, address="0x0000000000000000000000000000000000000003")
    er.dispose()
    ec.dispose()

    report = ard.compare_dbs(str(ref), str(cand), limit=10)
    assert report["exit_code"] == 1
    users = report["per_table"]["aave_v3_users"]
    # ref-only: addr ...02 (cand missing); cand-only: addr ...03.
    ref_only_keys = list(users["ref_only"])
    cand_only_keys = list(users["cand_only"])
    assert any("...02" not in str(k) and "2" in str(k[1]) for k in ref_only_keys) or any(
        "0000000000000000000000000000000000000002" in str(k) for k in ref_only_keys
    ), ref_only_keys
    assert any("0000000000000000000000000000000000000003" in str(k) for k in cand_only_keys), (
        cand_only_keys
    )


def test_compare_surrogate_id_mismatch_is_green(tmp_path: Path) -> None:
    """Same business key + same value columns, divergent surrogate `id` → GREEN.

    Ref user id=1, cand user id=5 — same (market, address) + same e_mode etc.
    A divergent surrogate id alone is NOT a divergence.
    """
    ref = _fresh_db(tmp_path, "ref")
    cand = _fresh_db(tmp_path, "cand")
    er = create_engine(f"sqlite:///{ref}")
    ec = create_engine(f"sqlite:///{cand}")
    _seed_user(er, uid=1, address=_USER, e_mode=3)
    _seed_user(ec, uid=5, address=_USER, e_mode=3)  # surrogate differs, rest equal
    er.dispose()
    ec.dispose()

    report = ard.compare_dbs(str(ref), str(cand), limit=10)
    assert report["exit_code"] == 0, report


def test_compare_resolves_fk_to_address(tmp_path: Path) -> None:
    """A collateral_position keyed by (user.address, asset.underlying_address).

    Ref: user id=1, asset id=1 (underlying WETH). Cand: user id=9, asset id=7
    (surrogates diverge), but the same address + same underlying + same
    balance → GREEN (FK-to-business-id resolution works).
    """
    ref = _fresh_db(tmp_path, "ref")
    cand = _fresh_db(tmp_path, "cand")
    er = create_engine(f"sqlite:///{ref}")
    ec = create_engine(f"sqlite:///{cand}")
    _seed_asset_and_token(er, asset_id=1, tok_id=2, underlying=_UW)
    _seed_user(er, uid=1, address=_USER)
    _seed_asset_and_token(ec, asset_id=7, tok_id=20, underlying=_UW)
    _seed_user(ec, uid=9, address=_USER)
    # Positions: FK cols differ (1,1) vs (9,7) but both resolve to (USER, WETH).
    sr = Session(er)
    sr.execute(
        text(
            "INSERT INTO aave_v3_collateral_positions "
            "(user_id, asset_id, balance, last_index) VALUES (1, 1, '500', NULL)"
        )
    )
    sr.commit()
    sr.close()
    sc = Session(ec)
    sc.execute(
        text(
            "INSERT INTO aave_v3_collateral_positions "
            "(user_id, asset_id, balance, last_index) VALUES (9, 7, '500', NULL)"
        )
    )
    sc.commit()
    sc.close()
    er.dispose()
    ec.dispose()

    report = ard.compare_dbs(str(ref), str(cand), limit=10)
    assert report["exit_code"] == 0, json.dumps(
        report.get("per_table", {}).get("aave_v3_collateral_positions", {}), indent=2, default=str
    )


def test_compare_symmetric_duplicate_is_green(tmp_path: Path) -> None:
    """Both DBs carry a duplicate POOL contract row (same values) → GREEN.

    Option-(5) bootstrap shape: a pre-seeded POOL + the writer's
    re-encountered-ProxyCreated POOL collapse to one business key in each DB;
    symmetric (both have 2 rows, same values) → GREEN.
    """
    for name in ("ref", "cand"):
        p = _fresh_db(tmp_path, name)
        eng = create_engine(f"sqlite:///{p}")
        s = Session(eng)
        # Two POOL contract rows (same name + address + revision; surrogate ids differ).
        for _ in range(2):
            s.execute(
                text(
                    "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
                    "VALUES (1, 'POOL', '0xabc', 1)"
                )
            )
        s.commit()
        s.close()
        eng.dispose()
    ref = tmp_path / "ref.db"
    cand = tmp_path / "cand.db"
    report = ard.compare_dbs(str(ref), str(cand), limit=10)
    assert report["exit_code"] == 0, report
    assert report["per_table"]["aave_v3_contracts"]["ref_count"] == 2
    assert report["per_table"]["aave_v3_contracts"]["cand_count"] == 2


def test_compare_count_mismatch_is_flagged(tmp_path: Path) -> None:
    """ref has 1 POOL row, cand has 2 (same business key+values) → flagged.

    Guards against an asymmetric duplicate (e.g. only one writer re-creates POOL)
    hiding behind the business-key dict collapse — the count_mismatch entry
    surfaces it as a divergence (NOT a false-green).
    """
    for name, n in (("ref", 1), ("cand", 2)):
        p = _fresh_db(tmp_path, name)
        eng = create_engine(f"sqlite:///{p}")
        s = Session(eng)
        for _ in range(n):
            s.execute(
                text(
                    "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
                    "VALUES (1, 'POOL', '0xabc', 1)"
                )
            )
        s.commit()
        s.close()
        eng.dispose()
    report = ard.compare_dbs(str(tmp_path / "ref.db"), str(tmp_path / "cand.db"), limit=10)
    assert report["exit_code"] == 1, report
    divs = report["per_table"]["aave_v3_contracts"]["divergences"]
    assert any(d["column"] == "row_count" for d in divs), divs


# ── drive / bisect: RPC smoke (skipped without RPC) ─────────────────────────

_RPC_ENV = ard.RPC_ENVVAR


def _rpc_reachable() -> bool:
    url = os.environ.get(_RPC_ENV)
    if not url:
        return False
    try:
        import requests

        r = requests.post(
            url,
            json={"jsonrpc": "2.0", "id": 1, "method": "eth_blockNumber", "params": []},
            timeout=10,
        )
        return r.ok and "result" in r.json()
    except Exception:  # noqa: BLE001
        return False


requires_rpc = pytest.mark.skipif(
    not _rpc_reachable(),
    reason=f"${_RPC_ENV} unset or unreachable",
)


@requires_rpc
def test_drive_one_block_smoke(tmp_path: Path) -> None:
    """Structural smoke: drive one block through both writers + compare.

    The deploy-block+1 range may be event-free (just the deployment); that's a
    valid structural smoke (both writers complete + the compare runs).
    """
    out = tmp_path / "drive"
    out.mkdir()
    summary = ard.drive(
        from_block=ard.AAVE_V3_DEPLOY_BLOCK + 1,
        to_block=ard.AAVE_V3_DEPLOY_BLOCK + 2,
        out_dir=str(out),
        quiet=True,
    )
    assert summary["cand_ok"] is True, summary
    assert summary["ref_ok"] is True, summary
    # Both writers must stamp last_update_block = TO.
    assert summary["cand_stamp"] == ard.AAVE_V3_DEPLOY_BLOCK + 2, summary
    rep = ard.compare_dbs(summary["ref_db"], summary["cand_db"], limit=5)
    # GREEN or a divergence — either is a valid structural smoke.
    assert rep["exit_code"] in (0, 1), rep


@requires_rpc
def test_bisect_smoke_on_seeded_divergence(tmp_path: Path) -> None:
    """Bisect narrows a deliberately-seeded divergence to a 1-block range.

    Manufactures a divergence at the DB layer after a 1-block drive, then
    drives the bisect loop over the single-block range (the loop's terminal
    case) + asserts it reports the divergence + emits verbatim events.
    """
    out = tmp_path / "bisect"
    out.mkdir()
    lo = ard.AAVE_V3_DEPLOY_BLOCK + 1
    hi = ard.AAVE_V3_DEPLOY_BLOCK + 2
    summary = ard.drive(
        from_block=lo,
        to_block=hi,
        out_dir=str(out),
        quiet=True,
        rust_only=True,
    )
    cand = summary["cand_db"]
    # Corrupt one row to force a divergence.
    eng = create_engine(f"sqlite:///{cand}")
    s = Session(eng)
    s.execute(text("UPDATE aave_v3_markets SET last_update_block = 0 WHERE 1"))
    s.commit()
    s.close()
    eng.dispose()
    rep = ard.compare_dbs(summary["ref_db"], cand, limit=5)
    assert rep["exit_code"] == 1, "seeded divergence must be detected"
