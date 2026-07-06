#!/usr/bin/env python3
"""Real-RPC drive + compare + bisect harness for Aave V3 writer divergence hunting.

Three subcommands (see ``--help``):

- ``compare <ref_db> <cand_db>``  — diff two SQLite DBs row-by-row column-by-
  column (offline; the §4.2 real-RPC parity gate). Exit 0 GREEN, 1 divergence,
  2 fatal.
- ``drive --from B --to T``       — replay mainnet blocks ``[B, T]`` through
  BOTH the Rust writer (``run_aave_update`` → ``cand.db``) + the Python oracle
  (``update_aave_market`` → ``ref.db``); print a one-line JSON summary.
- ``bisect --from B --to T``      — drive+compare, narrow the EARLIEST
  divergence to the smallest block range; print the verbatim per-tx events.

Defaults: RPC endpoint = ``$DEGENBOT_RPC_HTTP_CHAINID_1``; Aave V3 deploy block
``16291070`` (drives start at ``16291071``). See ergo ``JGQHBX``.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sqlite3
import sys
import tempfile
import threading
import time
from collections import defaultdict
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from sqlalchemy import create_engine, inspect, text
from sqlalchemy.orm import Session

from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.database.models import Base  # noqa: F401  (registers models on Base.metadata)
from degenbot.degenbot_rs import CancelHandle, db_upgrade_database, run_aave_update

if TYPE_CHECKING:
    from collections.abc import Callable

# ── constants ───────────────────────────────────────────────────────────────

AAVE_V3_DEPLOY_BLOCK = 16_291_070  # pool address provider deployed block; ref drives +1
#: ref.db (Python oracle) drives the FULL range from the deploy — its Phase-1
#: self-bootstraps POOL/POOL_CONFIGURATOR via ProxyCreated.
REF_FROM_BLOCK = AAVE_V3_DEPLOY_BLOCK + 1  # 16291071
#: cand.db (Rust) drives POST-bootstrap — the Rust's ``build_fetch_spec``
#: hard-requires POOL/POOL_CONFIGURATOR pre-seeded (run.rs:1060, once before the
#: loop). The harness pre-seeds them from RPC-fetched ProxyCreated + sets cand's
#: stamp to ``BOOTSTRAP_END_BLOCK``, so cand drives ``[BLOCK_AFTER_PROXY_CREATED, TO]``
#: + never re-sees the ProxyCreated events → no duplicate-row divergence.
#:
#: The ProxyCreated bootstrap events land at blocks 16291127 / 16291130 / 16291136
#: (confirmed by RPC probe — NOT at the 16291071 address-provider deploy; the
#: first ~56 blocks are event-free). BOOTSTRAP_END_BLOCK = 16291136.
BOOTSTRAP_END_BLOCK = 16_291_136
BLOCK_AFTER_PROXY_CREATED = BOOTSTRAP_END_BLOCK + 1  # 16291137 — cand's from
DEFAULT_CHUNK_SIZE = 10_000
DEFAULT_MAX_BISECT_RANGE = 50_000

#: The envvar holding the chain-1 archive node URL.
RPC_ENVVAR = "DEGENBOT_RPC_HTTP_CHAINID_1"

# The 11 Aave-writer tables + erc20_tokens (the writer-touch surface). The
# business-key spec is heuristically derived from the model FKs (no UNIQUE
# constraints exist beyond the surrogate PKs). Order is comparison-stable.
_TABLE_ORDER = [
    "erc20_tokens",
    "aave_v3_markets",
    "aave_v3_contracts",
    "aave_v3_emode_categories",
    "aave_v3_users",
    "aave_v3_assets",
    "aave_v3_asset_configs",
    "aave_v3_collateral_positions",
    "aave_v3_debt_positions",
    "aave_v3_user_collateral_configs",
    "aave_gho_tokens",
]

# Resolver kind codes for a (column, kind) pair in a table spec:
#   "d" direct value; "e" erc20 id→address; "m" market id→(chain_id,name);
#   "u" user id→address; "a" asset id→underlying address; "g" emode_category id→category_id.
_TABLE_SPECS: dict[str, dict[str, list[tuple[str, str]]]] = {
    "erc20_tokens": {
        "key": [("chain", "d"), ("address", "d")],
        "compare": [("name", "d"), ("symbol", "d"), ("decimals", "d")],
    },
    "aave_v3_markets": {
        "key": [("chain_id", "d"), ("name", "d")],
        # `last_update_block` is EXCLUDED — the Python ``update_aave_market`` no
        # longer advances it (Rust-owned per commands.py:432 — "was Python");
        # so ref.db keeps the seed stamp while cand.db (Rust) advances it. This
        # is an ownership-boundary artifact, NOT a writer-state divergence.
        "compare": [("active", "d")],
    },
    "aave_v3_contracts": {
        "key": [("market_id", "m"), ("name", "d")],
        "compare": [("address", "d"), ("revision", "d")],
    },
    "aave_v3_emode_categories": {
        "key": [("market_id", "m"), ("category_id", "d")],
        "compare": [
            ("label", "d"),
            ("ltv", "d"),
            ("liquidation_threshold", "d"),
            ("liquidation_bonus", "d"),
            ("price_source", "d"),
        ],
    },
    "aave_v3_users": {
        "key": [("market_id", "m"), ("address", "d")],
        "compare": [
            ("e_mode", "d"),
            ("gho_discount", "d"),
            ("stk_aave_balance", "d"),
            ("isolation_mode_collateral_asset_id", "a"),
            ("isolation_mode_debt", "d"),
        ],
    },
    "aave_v3_assets": {
        "key": [("market_id", "m"), ("underlying_asset_id", "e")],
        "compare": [
            ("a_token_revision", "d"),
            ("v_token_revision", "d"),
            ("e_mode_category_id", "g"),
            ("price_source", "d"),
            ("last_update_block", "d"),
            ("liquidity_index", "d"),
            ("liquidity_rate", "d"),
            ("borrow_index", "d"),
            ("borrow_rate", "d"),
        ],
    },
    "aave_v3_asset_configs": {
        "key": [("asset_id", "a")],
        "compare": [
            ("ltv", "d"),
            ("liquidation_threshold", "d"),
            ("liquidation_bonus", "d"),
            ("e_mode_category_id", "g"),
            ("borrowing_enabled", "d"),
            ("stable_borrowing_enabled", "d"),
            ("flash_loan_enabled", "d"),
            ("isolation_mode", "d"),
            ("borrowable_in_isolation", "d"),
            ("debt_ceiling", "d"),
        ],
    },
    "aave_v3_collateral_positions": {
        "key": [("user_id", "u"), ("asset_id", "a")],
        "compare": [("balance", "d"), ("last_index", "d")],
    },
    "aave_v3_debt_positions": {
        "key": [("user_id", "u"), ("asset_id", "a")],
        "compare": [("balance", "d"), ("last_index", "d")],
    },
    "aave_v3_user_collateral_configs": {
        "key": [("user_id", "u"), ("asset_id", "a")],
        "compare": [("enabled", "d")],
    },
    "aave_gho_tokens": {
        "key": [("token_id", "e")],
        "compare": [
            ("v_token_id", "e"),
            ("v_gho_discount_rate_strategy", "d"),
            ("v_gho_discount_token", "d"),
        ],
    },
}


# ── resolver maps ────────────────────────────────────────────────────────────


@dataclass
class Resolvers:
    """Per-DB surrogate-id → business-identity reverse maps."""

    erc20: dict[int, str | None] = field(default_factory=dict)  # id → address
    market: dict[int, tuple[Any, ...]] = field(default_factory=dict)  # id → (chain_id, name)
    user: dict[int, str | None] = field(default_factory=dict)  # id → address
    asset: dict[int, str | None] = field(default_factory=dict)  # id → underlying address
    emode_cat: dict[int, Any] = field(default_factory=dict)  # id → category_id


def _build_resolvers(rows_by_table: dict[str, list[dict[str, Any]]]) -> Resolvers:
    """Build the surrogate-id → business-identity reverse maps for one DB."""
    res = Resolvers()
    for r in rows_by_table.get("erc20_tokens", []):
        res.erc20[r["id"]] = r["address"]
    for r in rows_by_table.get("aave_v3_markets", []):
        res.market[r["id"]] = (r["chain_id"], r["name"])
    for r in rows_by_table.get("aave_v3_users", []):
        res.user[r["id"]] = r["address"]
    # assets resolve through the erc20 map (underlying_asset_id → address).
    for r in rows_by_table.get("aave_v3_assets", []):
        uw = r.get("underlying_asset_id")
        res.asset[r["id"]] = res.erc20.get(uw) if uw is not None else None
    for r in rows_by_table.get("aave_v3_emode_categories", []):
        res.emode_cat[r["id"]] = r["category_id"]
    return res


def _resolve(value: Any, kind: str, res: Resolvers) -> Any:
    """Resolve a raw column value to its business-identity form."""
    if value is None:
        return None
    match kind:
        case "d":
            return value
        case "e":
            return res.erc20.get(value)
        case "m":
            return res.market.get(value)
        case "u":
            return res.user.get(value)
        case "a":
            return res.asset.get(value)
        case "g":
            return res.emode_cat.get(value)
        case _:
            return value


def _key_of(row: dict[str, Any], spec: list[tuple[str, str]], res: Resolvers) -> tuple:
    return tuple(_resolve(row.get(c), k, res) for c, k in spec)


# ── table loading ───────────────────────────────────────────────────────────


def _all_aave_tables(engine: Any) -> list[str]:
    """The aave_* + erc20_tokens tables actually present in a DB."""
    insp = inspect(engine)
    present = set(insp.get_table_names())
    return [t for t in _TABLE_ORDER if t in present]


def _load_rows(session: Session, table: str) -> list[dict[str, Any]]:
    rows = session.execute(text(f'SELECT * FROM "{table}"')).all()
    return [dict(r._mapping) for r in rows]


# ── the compare core ─────────────────────────────────────────────────────────


def _json_default(obj: Any) -> Any:
    """JSON encoder fallback for Decimal/bytes/datetime."""
    from decimal import Decimal

    if isinstance(obj, Decimal):
        return str(obj)
    if isinstance(obj, (bytes, bytearray)):
        return obj.hex()
    import datetime as _dt

    if isinstance(obj, (_dt.datetime, _dt.date)):
        return obj.isoformat()
    return repr(obj)


def _canon(value: Any) -> Any:
    """Canonicalize a value for comparison: bytes→hex, Decimal/int→str, None→None.

    Surrogate FK values are resolved upstream by the spec; here we only need to
    make sqlalchemy/python numeric / byte types comparable across both DBs
    (sqlite is loosely-typed: a VARCHAR column may hold TEXT or INTEGER).
    """
    if value is None:
        return None
    if isinstance(value, (bytes, bytearray)):
        return value.hex()
    if isinstance(value, bool):
        return int(value)
    if isinstance(value, int):
        return str(value)
    return value  # str / Decimal (str() on encode), float


def compare_dbs(
    ref_path: str,
    cand_path: str,
    *,
    limit: int = 50,
) -> dict[str, Any]:
    """Diff two Aave-writer SQLite DBs row-by-row column-by-column.

    Rows are matched on the per-table **business key** (FKs resolved to
    addresses / (chain_id, name) / category_id — surrogate ``id`` PKs are
    excluded from BOTH the key + the value comparison so autoincrement ID
    drift across two independently-written DBs isn't a spurious divergence).

    Returns a report dict::

        {
            "exit_code": 0 | 1,
            "ref_path": ...,
            "cand_path": ...,
            "total_divergences": N,
            "per_table": {table: {...}},
            "divergences": [...],  # flat list, capped at `limit`
        }

    ``exit_code``: 0 GREEN, 1 divergence.
    """
    ref_engine = create_engine(f"sqlite:///{ref_path}")
    cand_engine = create_engine(f"sqlite:///{cand_path}")
    try:
        ref_session = Session(ref_engine)
        cand_session = Session(cand_engine)
        try:
            tables = _all_aave_tables(ref_engine)
            # Build the FK resolver maps ONCE per DB (not per-table).
            ref_all_rows = _load_all(ref_session)
            cand_all_rows = _load_all(cand_session)
            # tables present in cand but not ref → note (schema drift).
            cand_tables = set(_all_aave_tables(cand_engine))
            per_table: dict[str, Any] = {}
            flat_divs: list[dict[str, Any]] = []
            for table in tables:
                spec = _TABLE_SPECS.get(table)
                ref_rows = _load_rows(ref_session, table)
                cand_rows = _load_rows(cand_session, table)
                if spec is not None:
                    res_ref = _build_resolvers(ref_all_rows)
                    res_cand = _build_resolvers(cand_all_rows)
                    key_cols = spec["key"]
                    cmp_cols = spec["compare"]
                    ref_map = {_key_of(r, key_cols, res_ref): r for r in ref_rows}
                    cand_map = {_key_of(r, key_cols, res_cand): r for r in cand_rows}
                else:
                    # Fallback: surrogate id key + all non-id cols direct.
                    cmp_cols = [
                        (c, "d")
                        for c in (ref_rows[0].keys() if ref_rows or cand_rows else [])
                        if c != "id"
                    ] or [(c, "d") for c in (cand_rows[0].keys() if cand_rows else []) if c != "id"]
                    res_ref = Resolvers()
                    res_cand = Resolvers()
                    ref_map = {(r.get("id"),): r for r in ref_rows}
                    cand_map = {(r.get("id"),): r for r in cand_rows}
                ref_keys = set(ref_map)
                cand_keys = set(cand_map)
                shared = ref_keys & cand_keys
                ref_only = [list(k) for k in sorted(ref_keys - shared, key=_key_sort)]
                cand_only = [list(k) for k in sorted(cand_keys - shared, key=_key_sort)]
                table_divs: list[dict[str, Any]] = []
                for k in sorted(shared, key=_key_sort):
                    rr, cr = ref_map[k], cand_map[k]
                    for col, kind in cmp_cols:
                        rv = _canon(_resolve(rr.get(col), kind, res_ref))
                        cv = _canon(_resolve(cr.get(col), kind, res_cand))
                        if rv != cv:
                            table_divs.append({
                                "table": table,
                                "key": list(k),
                                "column": col,
                                "ref_value": rv,
                                "cand_value": cv,
                            })
                # Count mismatch (e.g. a symmetric business-key collapse hides a
                # count drift — surface it as a divergence so it's never
                # false-green).
                count_mismatch = len(ref_rows) - len(cand_rows)
                if count_mismatch != 0:
                    table_divs.append({
                        "table": table,
                        "key": "<count>",
                        "column": "row_count",
                        "ref_value": len(ref_rows),
                        "cand_value": len(cand_rows),
                    })
                per_table[table] = {
                    "ref_count": len(ref_rows),
                    "cand_count": len(cand_rows),
                    "ref_only": ref_only,
                    "cand_only": cand_only,
                    "divergences": table_divs,
                }
                flat_divs.extend(table_divs)
            cand_only_tables = sorted(cand_tables - set(tables))
            total = len(flat_divs) + sum(
                len(per_table[t]["ref_only"]) + len(per_table[t]["cand_only"]) for t in per_table
            )
            report = {
                "exit_code": 1 if (total > 0 or cand_only_tables) else 0,
                "ref_path": ref_path,
                "cand_path": cand_path,
                "total_divergences": total,
                "per_table": per_table,
                "divergences": flat_divs[:limit],
                "truncated": max(0, len(flat_divs) - limit),
                "cand_only_tables": cand_only_tables,
            }
            return report
        finally:
            ref_session.close()
            cand_session.close()
    finally:
        ref_engine.dispose()
        cand_engine.dispose()


def _load_all(session: Session) -> dict[str, list[dict[str, Any]]]:
    """Load every Aave-writer table's rows into a dict (for resolver building)."""
    insp = inspect(session.bind)
    present = set(insp.get_table_names())
    out: dict[str, list[dict[str, Any]]] = {}
    for t in _TABLE_ORDER:
        if t in present:
            out[t] = _load_rows(session, t)
    return out


def _key_sort(k: tuple) -> tuple:
    """Sort keys deterministically (mixed None/str/int)."""
    return tuple((s is None, str(s)) for s in k)


# ── drive ───────────────────────────────────────────────────────────────────


#: The ``ProxyCreated`` event topic (keccak of ``ProxyCreated(bytes32,address,address)``).
PROXY_CREATED_TOPIC = "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478"
#: The right-padded ASCII bytes32 ids the PoolAddressProvider emits for POOL /
#: POOL_CONFIGURATOR (NOT keccak — §4.2 finding).
_POOL_PROXY_ID = b"POOL".ljust(32, b"\x00")
_POOL_CONFIGURATOR_PROXY_ID = b"POOL_CONFIGURATOR".ljust(32, b"\x00")


def _run_python_writer(
    db_path: str, start_block: int, end_block: int, rpc_url: str, *, market_name: str
) -> None:
    """Drive the Python ``update_aave_market`` oracle on ``[start_block, end_block]``.

    The market row (seeded by ``_seed_market_db``) must already exist. The
    Python does NOT advance ``last_update_block`` (it's Rust-owned now —
    commands.py:432); the caller bumps the stamp separately when resuming.
    Used BOTH for the ref.db full-range drive AND for cand.db's bootstrap pass
    (driving the Python on ``[REF_FROM_BLOCK, BOOTSTRAP_END_BLOCK]`` creates the
    SAME contract rows the ref.db bootstrap produces → no bootstrap-contract
    divergence + no fake duplicate). Mirrors the production ``aave activate``
    bootstrap's contract-discovery (the Rust core itself CANNOT cold-boot a
    fresh market — ``build_fetch_spec`` errors on a missing POOL row).
    """
    from sqlalchemy import select
    from web3 import Web3
    from web3.providers.rpc import HTTPProvider

    from degenbot.cli.aave.commands import update_aave_market
    from degenbot.database.models import AaveV3Market
    from degenbot.provider.sync_adapter import ProviderAdapter

    engine = create_engine(f"sqlite:///{db_path}")
    session = Session(engine)
    try:
        market = session.scalar(select(AaveV3Market))
        assert market is not None, "seeded market row missing"
        w3 = Web3(HTTPProvider(rpc_url))
        provider = ProviderAdapter.from_web3(w3)
        update_aave_market(
            provider=provider,
            start_block=start_block,
            end_block=end_block,
            market=market,
            session=session,
            verify_block=False,
            verify_chunk=False,
            show_progress=False,
        )
        session.commit()
    finally:
        session.close()
        engine.dispose()


def _bump_stamp(db_path: str, stamp: int) -> None:
    """Set the market row's ``last_update_block`` (the Rust's resume-from-stamp)."""
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "UPDATE aave_v3_markets SET last_update_block = ? WHERE 1",
            (stamp,),
        )
        conn.commit()
    finally:
        conn.close()


def _read_stamp(db_path: str) -> int | None:
    """Read the market row's ``last_update_block`` (raw — bypasses ORM staleness)."""
    conn = sqlite3.connect(db_path)
    try:
        return conn.execute("SELECT last_update_block FROM aave_v3_markets").fetchone()[0]
    finally:
        conn.close()


# The GHO token address on Ethereum mainnet (seeded alongside the market —
# `update_aave_market` asserts `gho_asset is not None`).
_GHO_TOKEN_ADDRESS = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f"
_POOL_ADDRESS_PROVIDER = EthereumMainnetAaveV3.pool_address_provider


def _seed_market_db(db_path: str, *, market_name: str, last_update_block: int) -> int:
    """Init schema + seed the market + POOL_ADDRESS_PROVIDER contract + the
    GHO erc20 token + the ``aave_gho_tokens`` row — the minimum both writers'
    lookups need (``update_aave_market`` asserts ``gho_asset is not None`` +
    ``pool_address_provider is not None``; the Rust fetch spec looks up the
    discount token via the GHO row). Returns the market's id.
    """
    db_upgrade_database(db_path)
    engine = create_engine(f"sqlite:///{db_path}")
    session = Session(engine)
    try:
        session.execute(
            text(
                "INSERT INTO aave_v3_markets (chain_id, name, active, "
                "last_update_block) VALUES (1, :name, 1, :block)"
            ),
            {"name": market_name, "block": last_update_block},
        )
        # The market's autoincrement id.
        market_id = int(session.execute(text("SELECT id FROM aave_v3_markets")).one()[0])
        # POOL_ADDRESS_PROVIDER contract (the real mainnet address).
        session.execute(
            text(
                "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
                "VALUES (:m, 'POOL_ADDRESS_PROVIDER', :addr, NULL)"
            ),
            {"m": market_id, "addr": _POOL_ADDRESS_PROVIDER},
        )
        # GHO erc20 token + the chain-unique aave_gho_tokens row.
        session.execute(
            text("INSERT INTO erc20_tokens (chain, address) VALUES (1, :addr)"),
            {"addr": _GHO_TOKEN_ADDRESS},
        )
        gho_tok_id = int(session.execute(text("SELECT id FROM erc20_tokens")).one()[0])
        session.execute(
            text(
                "INSERT INTO aave_gho_tokens (token_id, v_token_id, "
                "v_gho_discount_rate_strategy, v_gho_discount_token) "
                "VALUES (:tid, NULL, NULL, NULL)"
            ),
            {"tid": gho_tok_id},
        )
        session.commit()
        return market_id
    finally:
        session.close()
        engine.dispose()


def drive(
    *,
    to_block: int,
    rpc_url: str | None = None,
    out_dir: str | None = None,
    chunk_size: int = DEFAULT_CHUNK_SIZE,
    rust_only: bool = False,
    python_only: bool = False,
    market_name: str = "ethereum-aave-v3",
    quiet: bool = False,
    progress_callback: Callable[..., None] | None = None,
) -> dict[str, Any]:
    """Replay mainnet blocks through both writers.

    - ref.db (Python ``update_aave_market``) drives the FULL ``[REF_FROM_BLOCK, to_block]``
      range — its Phase-1 self-bootstraps POOL/POOL_CONFIGURATOR via ProxyCreated.
    - cand.db (Rust ``run_aave_update``) drives ``[BLOCK_AFTER_PROXY_CREATED, to_block]``
      POST-bootstrap — the Rust's ``build_fetch_spec`` requires POOL/POOL_CONFIGURATOR
      pre-seeded, so the harness ``_bootstrap_rust_contracts`` seeds them from
      RPC-fetched ProxyCreated + sets cand's stamp to ``BOOTSTRAP_END_BLOCK``.

    Both writers process IDENTICAL event streams after ``BOOTSTRAP_END_BLOCK``
    (no duplicate-row divergence; the bootstrap range ``[16291071, 16291136]`` is
    construction-only — just ProxyCreated contract-row inserts, no user math).
    Returns a one-line JSON-serializable summary.
    """
    rpc = rpc_url or os.environ.get(RPC_ENVVAR)
    if not rpc:
        raise SystemExit(f"No RPC: set ${RPC_ENVVAR} or pass --rpc-url.")
    if out_dir is None:
        out_dir = tempfile.mkdtemp(prefix="aave-replay-")
    pathlib.Path(out_dir).mkdir(exist_ok=True, parents=True)
    ref_path = os.path.join(out_dir, "ref.db")
    cand_path = os.path.join(out_dir, "cand.db")
    if not quiet:
        print(
            f"[*] out_dir={out_dir} rpc={rpc} "
            f"ref=[{REF_FROM_BLOCK},{to_block}] cand=[{BLOCK_AFTER_PROXY_CREATED},{to_block}]",
            file=sys.stderr,
        )

    summary: dict[str, Any] = {
        "mode": "drive",
        "ref_from": REF_FROM_BLOCK,
        "cand_from": BLOCK_AFTER_PROXY_CREATED,
        "to": to_block,
        "out_dir": out_dir,
        "cand_db": cand_path,
        "ref_db": ref_path,
        "cand_ok": None,
        "ref_ok": None,
        "cand_report": None,
        "cand_stamp": None,
        "ref_stamp": None,
    }

    # --- Python oracle (ref.db) — full range, self-bootstraps ---
    if not rust_only:
        try:
            _seed_market_db(ref_path, market_name=market_name, last_update_block=REF_FROM_BLOCK - 1)
            _run_python_writer(ref_path, REF_FROM_BLOCK, to_block, rpc, market_name=market_name)
            summary["ref_ok"] = True
            summary["ref_stamp"] = _read_stamp(ref_path)
        except Exception as exc:
            summary["ref_ok"] = False
            summary["ref_error"] = f"{type(exc).__name__}: {exc}"
            _emit(summary)
            raise

    # --- Rust writer (cand.db) — Python-bootstrapped, then Rust POST-bootstrap ---
    if not python_only:
        try:
            market_id = _seed_market_db(
                cand_path,
                market_name=market_name,
                last_update_block=REF_FROM_BLOCK - 1,
            )
            # Bootstrap cand.db with the SAME Python pass the ref.db bootstrap
            # runs (ProxyCreated / PriceOracleUpdated / PoolDataProviderUpdated →
            # identical contract rows → no bootstrap-contract divergence).
            _run_python_writer(
                cand_path, REF_FROM_BLOCK, BOOTSTRAP_END_BLOCK, rpc, market_name=market_name
            )
            # The Python doesn't stamp (Rust-owned); bump to BOOTSTRAP_END_BLOCK
            # so the Rust resumes from BLOCK_AFTER_PROXY_CREATED.
            _bump_stamp(cand_path, BOOTSTRAP_END_BLOCK)
            cancel = CancelHandle()
            cb: Callable[..., None] = progress_callback or (lambda _progress=None, **_kw: None)
            report: dict[str, Any] = {}
            err: BaseException | None = None

            def _runner() -> None:
                nonlocal report, err
                try:
                    report = run_aave_update(
                        database_path=cand_path,
                        chain_id=1,
                        market_id=market_id,
                        to_block=to_block,
                        chunk_size=chunk_size,
                        rpc_url=rpc,
                        progress_callback=cb,
                        cancel_handle=cancel,
                    )
                except BaseException as exc:  # noqa: BLE001
                    err = exc

            t = threading.Thread(target=_runner, daemon=True)
            t.start()
            t.join()
            if err is not None:
                raise err
            summary["cand_ok"] = True
            summary["cand_report"] = report
            # read back the market stamp
            eng = create_engine(f"sqlite:///{cand_path}")
            try:
                with eng.connect() as conn:
                    row = conn.execute(text("SELECT last_update_block FROM aave_v3_markets")).one()
                    summary["cand_stamp"] = row[0]
            finally:
                eng.dispose()
        except Exception as exc:
            summary["cand_ok"] = False
            summary["cand_error"] = f"{type(exc).__name__}: {exc}"
            _emit(summary)
            raise

    _emit(summary)
    return summary


def _emit(obj: dict[str, Any]) -> None:
    print(json.dumps(obj, default=_json_default))


# ── bisect ───────────────────────────────────────────────────────────────────


def bisect(
    *,
    to_block: int,
    rpc_url: str | None = None,
    out_dir: str | None = None,
    chunk_size: int = DEFAULT_CHUNK_SIZE,
    max_blocks: int = 1,
) -> dict[str, Any]:
    """Narrow the EARLIEST divergence in ``[BLOCK_AFTER_PROXY_CREATED, to_block]``.

    Binary-searches on ``hi``: ``drive(to=mid)`` is a FRESH from-seed drive (so
    the GREEN prefix re-applies idempotently — no snapshot-resume needed for
    correctness; the checkpoint-resume optimization is a TODO). When
    ``drive(to=mid)`` is divergent → recurse ``hi=mid``; GREEN → recurse
    ``lo=mid+1``. Stops at ``max_blocks`` granularity, then fetches the
    verbatim per-tx events for the divergent range.
    """
    rpc = rpc_url or os.environ.get(RPC_ENVVAR)
    if not rpc:
        raise SystemExit(f"No RPC: set ${RPC_ENVVAR} or pass --rpc-url.")
    if out_dir is None:
        out_dir = tempfile.mkdtemp(prefix="aave-bisect-")

    def drive_at(hi: int) -> dict[str, Any]:
        rng_dir = os.path.join(out_dir, f"at_{hi}")
        pathlib.Path(rng_dir).mkdir(exist_ok=True, parents=True)
        return drive(
            to_block=hi,
            rpc_url=rpc,
            out_dir=rng_dir,
            chunk_size=chunk_size,
            market_name="ethereum-aave-v3",
            quiet=True,
        )

    # Initial full-range drive.
    s = drive_at(to_block)
    rep = compare_dbs(s["ref_db"], s["cand_db"])
    if rep["exit_code"] == 0:
        print(
            json.dumps(
                {
                    "mode": "bisect",
                    "result": "GREEN",
                    "range": [BLOCK_AFTER_PROXY_CREATED, to_block],
                },
                default=_json_default,
            )
        )
        return {"result": "GREEN", "range": [BLOCK_AFTER_PROXY_CREATED, to_block]}

    lo, hi = BLOCK_AFTER_PROXY_CREATED, to_block
    while hi - lo + 1 > max_blocks:
        mid = (lo + hi) // 2
        sm = drive_at(mid)
        rm = compare_dbs(sm["ref_db"], sm["cand_db"])
        if rm["exit_code"] != 0:
            hi = mid  # recurse left
        else:
            lo = mid + 1  # GREEN → earliest divergence is in (mid, hi]

    # Final divergent range [lo, hi].
    final = drive_at(hi)
    final_rep = compare_dbs(final["ref_db"], final["cand_db"])
    events = _fetch_block_tx_events(lo, hi, rpc)
    result = {
        "mode": "bisect",
        "divergent_range": [lo, hi],
        "events": events,
        "divergence": final_rep.get("divergences", []),
        "cand_db": final["cand_db"],
        "ref_db": final["ref_db"],
    }
    print(json.dumps(result, default=_json_default))
    return result


def _fetch_block_tx_events(lo: int, hi: int, rpc_url: str) -> list[dict[str, Any]]:
    """Fetch the Aave V3 event-emitting contracts' logs for [lo,hi], grouped by tx."""
    try:
        from degenbot.cli.aave.event_fetchers import ETH_GETLOGS_MAX_BLOCK_RANGE  # noqa
    except Exception:  # noqa: BLE001
        pass
    # The 7 Aave contracts + topic union — sourced from the writer_parity
    # harness / the fetch spec. We fetch all logs for the emitting contracts
    # in the range, then group by tx.
    import requests

    contracts = _aave_emitter_addresses()
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getLogs",
        "params": [{"fromBlock": hex(lo), "toBlock": hex(hi), "address": contracts}],
    }
    r = requests.post(rpc_url, json=payload, timeout=120)
    r.raise_for_status()
    data = r.json()
    logs = data.get("result", [])
    by_tx: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for log in logs:
        by_tx[log.get("transactionHash", "?")].append(log)
    out = []
    for tx, txlogs in sorted(by_tx.items()):
        out.append({
            "transactionHash": tx,
            "blockNumber": txlogs[0].get("blockNumber"),
            "logs": txlogs,
        })
    return out


def _aave_emitter_addresses() -> list[str]:
    """The Aave V3 event-emitting contract addresses (deployments)."""
    from degenbot.aave.deployments import EthereumMainnetAaveV3

    ap = EthereumMainnetAaveV3.pool_address_provider
    # Resolve pool / configurator / data-provider / oracle via raw RPC if
    # available; otherwise emit just the provider + a few known addresses.
    # For the bisect verbatim-events feature a superset is fine — extra logs
    # are harmless context. Known mainnet addresses:
    return list({
        ap,
        "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2",  # Pool
        "0x64b761D9487f95e2e9244a05B7F4D5688b0c2027",  # Configurator
        "0x7d2763dE63bF5a097be8e3E8bD924b73A3F0b077",  # legacy LendingPool (v2, harmless)
    })


# ── CLI ─────────────────────────────────────────────────────────────────────


def _cli_compare(args: argparse.Namespace) -> int:
    rep = compare_dbs(args.ref_db, args.cand_db, limit=args.limit)
    out_json = args.out_json or os.path.join(
        tempfile.gettempdir(), f"aave-diff-{int(time.time())}.json"
    )
    with pathlib.Path(out_json).open("w") as f:
        json.dump(rep, f, default=_json_default, indent=2)
    # Human summary.
    print(f"ref={args.ref_db}")
    print(f"cand={args.cand_db}")
    print(f"total_divergences={rep['total_divergences']} exit={rep['exit_code']}")
    for table, info in rep["per_table"].items():
        nd = len(info["divergences"])
        print(
            f"  {table}: ref={info['ref_count']} cand={info['cand_count']} "
            f"divergences={nd} ref_only={len(info['ref_only'])} "
            f"cand_only={len(info['cand_only'])}"
        )
    for d in rep["divergences"]:
        print(
            f"  {d['table']}.{d['column']} key={d['key']} "
            f"ref={d['ref_value']!r} cand={d['cand_value']!r}"
        )
    if rep["truncated"]:
        print(f"  ... ({rep['truncated']} more in {out_json})")
    print(f"full: {out_json}")
    return int(rep["exit_code"])


def _cli_drive(args: argparse.Namespace) -> int:
    drive(
        to_block=args.to,
        rpc_url=args.rpc_url,
        out_dir=args.out_dir,
        chunk_size=args.chunk_size,
        rust_only=args.rust_only,
        python_only=args.python_only,
        quiet=args.quiet,
    )
    return 0


def _cli_bisect(args: argparse.Namespace) -> int:
    res = bisect(
        to_block=args.to,
        rpc_url=args.rpc_url,
        out_dir=args.out_dir,
        chunk_size=args.chunk_size,
        max_blocks=args.max_blocks,
    )
    return 0 if res.get("result") == "GREEN" else 1


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="aave_replay_diff", description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    pc = sub.add_parser("compare", help="diff two DBs")
    pc.add_argument("ref_db")
    pc.add_argument("cand_db")
    pc.add_argument("--out-json")
    pc.add_argument("--limit", type=int, default=50)
    pc.set_defaults(func=_cli_compare)

    pd = sub.add_parser("drive", help="replay a block range through both writers")
    pd.add_argument("--to", dest="to", type=int, required=True)
    pd.add_argument("--rpc-url")
    pd.add_argument("--out-dir")
    pd.add_argument("--chunk-size", type=int, default=DEFAULT_CHUNK_SIZE)
    pd.add_argument("--rust-only", action="store_true")
    pd.add_argument("--python-only", action="store_true")
    pd.add_argument("--quiet", action="store_true")
    pd.set_defaults(func=_cli_drive)

    pb = sub.add_parser("bisect", help="narrow the earliest divergence")
    pb.add_argument("--to", dest="to", type=int, required=True)
    pb.add_argument("--rpc-url")
    pb.add_argument("--out-dir")
    pb.add_argument("--chunk-size", type=int, default=DEFAULT_CHUNK_SIZE)
    pb.add_argument("--max-blocks", dest="max_blocks", type=int, default=1)
    pb.set_defaults(func=_cli_bisect)
    return p


def main() -> int:
    args = build_parser().parse_args()
    try:
        return int(args.func(args))
    except SystemExit:
        raise
    except Exception as exc:  # noqa: BLE001
        print(json.dumps({"error": f"{type(exc).__name__}: {exc}"}, default=_json_default))
        return 2


if __name__ == "__main__":
    sys.exit(main())
