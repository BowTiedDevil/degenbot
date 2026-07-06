"""Shared harness for the Aave writer §4.2 parity tests (U5YIBG).

The offline mock-RPC live-vs-live harness (ADR-005 §4.2). Both the Rust driver
(``degenbot_rs.run_aave_update``) + the Python oracle
(``cli/aave/commands.py::update_aave_market``) are driven against the SAME mock
JSON-RPC server serving canned ``eth_getLogs`` + ``eth_blockNumber`` responses,
into two identically-seeded temp SQLite DBs. The resulting ``aave_*`` rows are
compared byte-for-byte.

Per §4.3, the parity tests here are TEMPORARY: once GREEN, CZM7TI retires the
Python oracle + these tests together. The Rust ``#[cfg(test)]`` corpus in
``write.rs`` stays permanent.
"""

from __future__ import annotations

import json
import threading
from contextlib import contextmanager
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import TYPE_CHECKING, Any

from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot.degenbot_rs import db_upgrade_database

if TYPE_CHECKING:
    from collections.abc import Generator
    from pathlib import Path

# The known contract addresses the seeded market + fixture logs use. Both the
# Rust fetch spec + the Python fetchers filter on these.
POOL_ADDRESS = "0x" + "11" * 20
POOL_CONFIGURATOR_ADDRESS = "0x" + "22" * 20
POOL_ADDRESS_PROVIDER_ADDRESS = "0x" + "33" * 20
PRICE_ORACLE_ADDRESS = "0x" + "44" * 20
GHO_TOKEN_ADDRESS = "0x" + "55" * 20
# The seeded asset's erc20 parents (underlying/a_token/v_token) + the
# pre-seeded user (the Python oracle `assert user is not None` requires the
# user to pre-exist; the Rust `get_or_create_user` is a no-op on it).
UNDERLYING_ADDRESS = "0x" + "66" * 20
A_TOKEN_ADDRESS = "0x" + "77" * 20
V_TOKEN_ADDRESS = "0x" + "88" * 20
USER_ADDRESS = "0x" + "ab" * 20

# The canonical Aave V3 UserEModeSet topic0
# (keccak of "UserEModeSet(address,uint8)").
USER_E_MODE_SET_TOPIC = "0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84"

# The bootstrap stamp — the seeded market's `last_update_block`. Both paths
# start from `stamp + 1` (flag #4: bootstrap the stamp so both paths align).
BOOTSTRAP_BLOCK = 1000
# The fixture events live at this block (one chunk, one event).
FIXTURE_BLOCK = 1001


def _hex(n: int) -> str:
    """Encode a non-negative int as a JSON-RPC hex string (`0x...`)."""
    return hex(n)


def _u256(value: int) -> str:
    """Encode a non-negative int as a 32-byte hex word (the ABI uint256 shape)."""
    return "0x" + value.to_bytes(32, "big").hex()


def _pad_address(addr: str) -> str:
    """Pad a 20-byte address to a 32-byte topic word."""
    return "0x" + addr[2:].rjust(64, "0").lower()


def _make_log(
    *,
    address: str,
    topics: list[str],
    data: str,
    block: int,
    log_index: int,
) -> dict[str, Any]:
    """Build a canned alloy/web3-shaped `eth_getLogs` entry."""
    tx_hash = "0x" + (b"tx" + b"\x00" * 30).hex()
    block_hash = "0x" + (b"bk" + b"\x00" * 30).hex()
    return {
        "address": address,
        "topics": topics,
        "data": data,
        "blockNumber": _hex(block),
        "logIndex": _hex(log_index),
        "transactionIndex": _hex(log_index),
        "transactionHash": tx_hash,
        "blockHash": block_hash,
        "removed": False,
    }


def make_user_e_mode_set_log(
    *,
    user_address: str,
    category_id: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a UserEModeSet event."""
    _ = tx_index
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[USER_E_MODE_SET_TOPIC, _pad_address(user_address)],
        data=_u256(category_id),
        block=block,
        log_index=log_index,
    )


# ReserveUsedAsCollateral{Enabled,Disabled}(address indexed reserve,
# address indexed user) — topic0 is the signature hash, topic1 is the reserve
# (the asset's underlying address), topic2 is the user. No data.
_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC = (
    "0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2"
)
_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC = (
    "0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd"
)


def make_reserve_used_as_collateral_log(
    *,
    user_address: str,
    reserve_address: str,
    enabled: bool,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a ReserveUsedAsCollateral event.

    `enabled=True` → the `...Enabled` topic0; `False` → the `...Disabled` topic0.
    """
    topic0 = (
        _RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC
        if enabled
        else _RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC
    )
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[topic0, _pad_address(reserve_address), _pad_address(user_address)],
        data="0x",
        block=block,
        log_index=log_index,
    )


@dataclass
class MockRpcRegistry:
    """Holds the canned RPC responses for the parity mock server.

    `logs` is the full list of canned `eth_getLogs` entries; the server filters
    by the requested `address` + `topics[0]` group. `block_number` is the tip
    served for `eth_blockNumber`.
    """

    block_number: int
    logs: list[dict[str, Any]] = field(default_factory=list)


class _MockRpcHandler(BaseHTTPRequestHandler):
    """Serves canned `eth_getLogs` + `eth_blockNumber` (+ a generic fallback)."""

    registry: MockRpcRegistry  # set as a class attr by `mock_rpc_server`

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        req = json.loads(body) if body else {}
        # alloy may batch (a list of requests) — handle both shapes.
        if isinstance(req, list):
            responses = [self._handle_one(r) for r in req]
            payload = json.dumps(responses).encode()
        else:
            payload = json.dumps(self._handle_one(req)).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _handle_one(self, req: dict[str, Any]) -> dict[str, Any]:
        method = req.get("method")
        req_id = req.get("id")
        if not isinstance(method, str):
            method = ""
        result = self._canned_result(method, req)
        return {"jsonrpc": "2.0", "id": req_id, "result": result}

    def _canned_result(self, method: str, req: dict[str, Any]) -> Any:
        if method == "eth_blockNumber":
            return _hex(self.registry.block_number)
        if method == "eth_getLogs":
            return self._filtered_logs(req)
        if method == "eth_chainId":
            return _hex(1)
        # Generic OK for any auxiliary method (eth_getBlockByNumber, etc.).
        return "0x0"

    def _filtered_logs(self, req: dict[str, Any]) -> list[dict[str, Any]]:
        params = req.get("params", [])
        filt = params[0] if params and isinstance(params[0], dict) else {}
        # alloy sends `address` as a list (or a single string) + `topics` as
        # `[[topic0_a, topic0_b, ...]]` (the topic0 OR-group).
        raw_addr = filt.get("address")
        addresses: set[str] = set()
        if isinstance(raw_addr, str):
            addresses.add(raw_addr.lower())
        elif isinstance(raw_addr, list):
            addresses.update(
                a.lower() for a in raw_addr if isinstance(a, str)
            )
        topic0_group: set[str] = set()
        topics = filt.get("topics") or []
        if topics and isinstance(topics[0], list):
            topic0_group = {t.lower() for t in topics[0] if isinstance(t, str)}
        # Match canned logs by address (+ topic0 if a group was requested).
        out: list[dict[str, Any]] = []
        for log in self.registry.logs:
            if addresses and log["address"].lower() not in addresses:
                continue
            if topic0_group and log["topics"][0].lower() not in topic0_group:
                continue
            out.append(log)
        return out

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002
        return


@contextmanager
def mock_rpc_server(
    *,
    logs: list[dict[str, Any]],
    block_number: int,
) -> Generator[str, None, None]:
    """Start a local mock JSON-RPC server; yield its URL."""
    _MockRpcHandler.registry = MockRpcRegistry(block_number=block_number, logs=logs)
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockRpcHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@contextmanager
def seeded_db(
    tmp_path: Path,
    *,
    name: str,
    market_id: int = 1,
    chain_id: int = 1,
    last_update_block: int = BOOTSTRAP_BLOCK,
) -> Generator[tuple[Path, Session], None, None]:
    """Create a temp SQLite DB with the Rust schema + a seeded Aave market.

    Yields the DB path + an open SQLAlchemy ``Session`` (the ORM operates on
    the Rust-created schema; the two are interoperable per ADR-010). The market
    + the POOL/POOL_CONFIGURATOR/POOL_ADDRESS_PROVIDER/PRICE_ORACLE contracts +
    a minimal GHO asset (token + aave_gho_tokens row) are seeded — the minimum
    both ``run_aave_update`` (the fetch-spec lookup) + ``update_aave_market``
    (the ``assert gho_asset is not None`` requirement) need.

    Disposes the engine (closing all SQLite connections) on exit — the caller
    must finish using the session before the ``with`` block ends.
    """
    db_path = tmp_path / f"{name}.db"
    # Create the full Rust-owned schema (the source of truth) on an empty file.
    db_upgrade_database(str(db_path))
    engine = create_engine(f"sqlite:///{db_path}")
    session = Session(engine)
    try:
        session.execute(
            text(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, "
                "last_update_block) VALUES (:id, :chain, :name, 1, :block)"
            ),
            {
                "id": market_id,
                "chain": chain_id,
                "name": "aave",
                "block": last_update_block,
            },
        )
        # The contract revisions (in production set via the ProxyCreated
        # bootstrap's POOL_REVISION()/CONFIGURATOR_REVISION() RPC; in the
        # offline harness the substrate lookup requires them non-NULL — flag #2:
        # ProxyCreated EXCLUDED, contracts seeded with revisions).
        _seed_contract(session, market_id, "POOL", POOL_ADDRESS, revision=1)
        _seed_contract(
            session, market_id, "POOL_CONFIGURATOR", POOL_CONFIGURATOR_ADDRESS, revision=1
        )
        _seed_contract(session, market_id, "POOL_ADDRESS_PROVIDER", POOL_ADDRESS_PROVIDER_ADDRESS)
        _seed_contract(session, market_id, "PRICE_ORACLE", PRICE_ORACLE_ADDRESS)
        # The GHO token erc20 row + the aave_gho_tokens row (the Python oracle
        # asserts gho_asset is not None; the Rust treats it as optional).
        session.execute(
            text(
                "INSERT INTO erc20_tokens (id, chain, address) "
                "VALUES (1, :chain, :addr)"
            ),
            {"chain": chain_id, "addr": GHO_TOKEN_ADDRESS},
        )
        session.execute(
            text(
                "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, "
                "v_gho_discount_rate_strategy, v_gho_discount_token) "
                "VALUES (1, 1, NULL, NULL, NULL)"
            ),
        )
        session.commit()
        yield db_path, session
    finally:
        session.close()
        engine.dispose()


def _seed_contract(
    session: Session, market_id: int, name: str, address: str, *, revision: int | None = None
) -> None:
    """Insert an aave_v3_contracts row (read by the Rust fetch spec + get_contract)."""
    session.execute(
        text(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
            "VALUES (:market, :name, :addr, :rev)"
        ),
        {"market": market_id, "name": name, "addr": address, "rev": revision},
    )


def dump_user_rows(session: Session) -> list[dict[str, Any]]:
    """Dump the ``aave_v3_users`` rows as comparable dicts (column-by-column)."""
    rows = session.execute(
        text(
            "SELECT id, market_id, address, e_mode, gho_discount, stk_aave_balance, "
            "isolation_mode_collateral_asset_id, isolation_mode_debt "
            "FROM aave_v3_users ORDER BY address"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def seed_asset_and_user(session: Session, *, market_id: int = 1, chain_id: int = 1) -> None:
    """Seed an AaveV3Asset (with erc20 parents) + a pre-seeded user.

    The asset's underlying_token address is ``UNDERLYING_ADDRESS`` (the
    `ReserveUsedAsCollateral`/`ReserveDataUpdated` events key on it). The user's
    address is ``USER_ADDRESS`` (matches the fixture events' `user`). Both rows
    are seeded with the SAME defaults the Rust `get_or_create_*` would write — so
    the Rust path's create-or-find is a no-op + the Python path's
    `assert ... is not None` passes.
    """
    from degenbot.checksum_cache import get_checksum_address

    uw = get_checksum_address(UNDERLYING_ADDRESS)
    aw = get_checksum_address(A_TOKEN_ADDRESS)
    vw = get_checksum_address(V_TOKEN_ADDRESS)
    # erc20 parents: id 2 = underlying, 3 = a_token, 4 = v_token
    # (id 1 is the GHO token seeded by `create_seeded_db`).
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (2, :c, :u)"),
        {"c": chain_id, "u": uw},
    )
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (3, :c, :a)"),
        {"c": chain_id, "a": aw},
    )
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (4, :c, :v)"),
        {"c": chain_id, "v": vw},
    )
    session.execute(
        text(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, "
            "a_token_id, a_token_revision, v_token_id, v_token_revision, "
            "e_mode_category_id, price_source, last_update_block, "
            "liquidity_index, liquidity_rate, borrow_index, borrow_rate) "
            "VALUES (1, :market, 2, 3, 1, 4, 1, NULL, NULL, NULL, 0, 0, 0, 0)"
        ),
        {"market": market_id},
    )
    # The pre-seeded user (checksummed — both paths store/lookup the EIP-55 form).
    session.execute(
        text(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, "
            "gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, "
            "isolation_mode_debt) "
            "VALUES (1, :market, :addr, 0, 0, NULL, NULL, '0')"
        ),
        {"market": market_id, "addr": get_checksum_address(USER_ADDRESS)},
    )
    session.flush()


def dump_collateral_config_rows(session: Session) -> list[dict[str, Any]]:
    """Dump ``aave_v3_user_collateral_configs`` rows as comparable dicts."""
    rows = session.execute(
        text(
            "SELECT id, user_id, asset_id, enabled "
            "FROM aave_v3_user_collateral_configs ORDER BY user_id, asset_id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]
