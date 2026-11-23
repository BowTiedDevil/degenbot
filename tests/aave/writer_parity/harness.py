"""Shared mock-RPC harness for the Aave Rust-writer tests.

The offline mock-RPC harness (the parity gate,
``docs/architecture/parity-gate.md``). Drives the Rust writer
(``degenbot._ffi.run_aave_update``) against a mock JSON-RPC server serving
canned ``eth_getLogs`` + ``eth_blockNumber`` + ``eth_call`` responses, into a
temp SQLite DB. The resulting ``aave_*`` rows are asserted against expectations
(or compared against on-chain truth via the Rust ``verify_*`` seams).

Parity-gate retirement: the Python oracle
(``cli/aave.py::update_aave_market``) + the byte-for-byte Rust-vs-
Python parity tests that lived here have been DELETED — the Rust writer is
proven GREEN to the live chain tip. The mock-RPC infrastructure survives
to exercise the Rust path offline. The Rust ``#[cfg(test)]`` corpus in
``write.rs`` stays permanent.
"""

from __future__ import annotations

import json
import sqlite3
import threading
from contextlib import contextmanager
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

from degenbot.db import db_upgrade_database
from tests.helpers.aave_logs import (
    A_TOKEN_ADDRESS,
    BOOTSTRAP_BLOCK,
    DEFAULT_BLOCK_HASH,
    GHO_TOKEN_ADDRESS,
    GHO_VTOKEN_ADDRESS,
    POOL_ADDRESS,
    POOL_ADDRESS_PROVIDER_ADDRESS,
    POOL_CONFIGURATOR_ADDRESS,
    PRICE_ORACLE_ADDRESS,
    UNDERLYING_ADDRESS,
    USER_ADDRESS,
    V_TOKEN_ADDRESS,
    _hex,
    _u256,
)

#: The parent every served header names: nonzero and distinct from the block
#: hash, so a link check reads a real parent rather than the zero word.
DEFAULT_PARENT_BLOCK_HASH = "0x" + (b"pp" + b"\x00" * 30).hex()


def terminal_header(number: int) -> dict[str, str]:
    """The complete header the mock serves for one height.

    The hash is the one every canned log carries (``DEFAULT_BLOCK_HASH``), so the
    writer's resolved terminal anchor fences the logs at its height (ADR-105 D1).
    """
    return {
        "number": _hex(number),
        "hash": DEFAULT_BLOCK_HASH,
        "parentHash": DEFAULT_PARENT_BLOCK_HASH,
    }


def _block_tag_number(tag: Any) -> int | None:
    """Decode a JSON-RPC block tag: a hex quantity (``0x3e9``) or a decimal string."""
    if not isinstance(tag, str):
        return None
    try:
        return int(tag, 16) if tag.startswith("0x") else int(tag, 10)
    except ValueError:
        return None


class _UnservedMethod(Exception):
    """The mock serves no canned answer for a method: answered as a JSON-RPC
    method-not-found, never as a fabricated success."""


def _abi_string(text: str) -> str:
    """Pack `text` as an ABI dynamic-`string` return (offset word, length word, padded data)."""
    raw = text.encode()
    padded = raw.ljust(len(raw) + (32 - len(raw) % 32) % 32, b"\x00")
    return "0x" + ((32).to_bytes(32, "big") + len(raw).to_bytes(32, "big") + padded).hex()


#: `name()` / `symbol()` / `decimals()` for a reserve's underlying / aToken /
#: vToken rows, plus the upper-case spellings the `LowerUpperStopOnCallError`
#: metadata read issues alongside them. The dispatch refuses an all-absent
#: metadata triple (an outage is not token identity), so every fixture that
#: dispatches a `ReserveInitialized` answers them. The mock dispatches
#: `eth_call` by selector alone, so one plausible WETH triple serves all three
#: token rows; the parity assertions are about positions, balances and
#: verification, never token metadata.
ERC20_METADATA_SELECTORS: dict[str, str] = {
    "0x06fdde03": _abi_string("Wrapped Ether"),  # name()
    "0xa3f4df7e": _abi_string("Wrapped Ether"),  # NAME()
    "0x95d89b41": _abi_string("WETH"),  # symbol()
    "0xf76f8d78": _abi_string("WETH"),  # SYMBOL()
    "0x313ce567": _u256(18),  # decimals()
    "0x2e0f2625": _u256(18),  # DECIMALS()
}


def seed_gho_asset(
    session: sqlite3.Connection,
    *,
    market_id: int = 1,
    chain_id: int = 1,
    v_token_revision: int = 3,
    gho_discount_percent: int = 1000,
) -> None:
    """Seed the GHO debt asset + a pre-seeded user with a GHO discount.

    seed the GHO underlying asset row
    (aave_v3_assets) with `underlying_asset_id` = the GHO token erc20 (id 1),
    `v_token_id` = the GHO vToken erc20 (id 5), + `v_token_revision` as given
    (the pre-upgrade rev the discount pre-pass reads at chunk-start). The
    companion ERC20 `Transfer` (the Borrow's minted-debt companion the ops
    parser asserts) is emitted by the GHO vToken itself — both paths classify it
    as a debt transfer (the Rust matches by from/target/amount; the Python
    asserts `event_type ∈ {DEBT_*, ERC20_DEBT_*, GHO_DEBT_*}`).

    `gho_discount_percent` seeds the user's `aave_v3_users.gho_discount` (the
    path #1 DB-cache the discount pre-pass reads). Non-zero so the
    discount-active (pre-upgrade) vs deprecated (post-upgrade) divergence the
    fixture exercises is observable.
    """
    from degenbot.checksum_cache import get_checksum_address

    # A phantom GHO aToken erc20 (id 6) — the GHO asset's a_token_id. Kept
    # distinct from the GHO token (id 1, the underlying) so the GHO token is
    # NOT fetched as an aToken. The companion Transfer is emitted by the GHO
    # vToken (a debt token) so both paths classify it as a debt transfer.
    session.execute(
        "INSERT INTO erc20_tokens (id, chain, address) VALUES (6, :c, :a)",
        {"c": chain_id, "a": get_checksum_address("0x" + "56" * 20)},
    )
    seed_gho_vtoken(session, chain_id=chain_id)
    # The GHO underlying asset (id 2). a_token_id = phantom GHO aToken (id 6);
    # v_token_id = the GHO vToken erc20 (id 5) + the pre-upgrade rev.
    session.execute(
        "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, "
        "a_token_id, a_token_revision, v_token_id, v_token_revision, "
        "e_mode_category_id, price_source, last_update_block, "
        "liquidity_index, liquidity_rate, borrow_index, borrow_rate) "
        "VALUES (2, :market, 1, 6, 1, 5, :rev, NULL, NULL, NULL, 0, 0, 0, 0)",
        {"market": market_id, "rev": v_token_revision},
    )
    # The pre-seeded user with a GHO discount (path #1 DB-cache).
    session.execute(
        "INSERT INTO aave_v3_users (id, market_id, address, e_mode, "
        "gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, "
        "isolation_mode_debt) "
        "VALUES (1, :market, :addr, 0, :disc, NULL, NULL, '0')",
        {
            "market": market_id,
            "addr": get_checksum_address(USER_ADDRESS),
            "disc": gho_discount_percent,
        },
    )


def dump_debt_position_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump `aave_v3_debt_positions` rows as comparable dicts (the GHO debt
    balance + last_index the ops parser writes — the byte-IDENTITY target for
    the flag #1 fixture)."""
    rows = session.execute(
        "SELECT id, user_id, asset_id, balance, last_index FROM aave_v3_debt_positions ORDER BY id"
    ).fetchall()
    return [dict(row) for row in rows]


def dump_collateral_position_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump `aave_v3_collateral_positions` rows as comparable dicts (the
    balance + last_index the ops parser writes for a Supply)."""
    rows = session.execute(
        "SELECT id, user_id, asset_id, balance, last_index "
        "FROM aave_v3_collateral_positions ORDER BY id"
    ).fetchall()
    return [dict(row) for row in rows]


@dataclass
class MockRpcRegistry:
    """Holds the canned RPC responses for the parity mock server.

    `logs` is the full list of canned `eth_getLogs` entries; the server filters
    by the requested `address` + `topics[0]` group. `block_number` is the tip
    served for `eth_blockNumber`. `eth_call_responses` maps a calldata selector
    (the first 4 bytes, e.g. `"0xc44b11f7"` for `getConfiguration(address)`)
    to the canned ABI return value — the server dispatches `eth_call`s by
    selector.
    """

    block_number: int
    logs: list[dict[str, Any]] = field(default_factory=list)
    eth_call_responses: dict[str, str] = field(default_factory=dict)


class _MockRpcHandler(BaseHTTPRequestHandler):
    """Serves the canned methods the writer issues; any other method is loud."""

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
        try:
            result = self._canned_result(method, req)
        except _UnservedMethod as unserved:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {
                    "code": -32601,
                    "message": f"mock RPC serves no {unserved} answer",
                },
            }
        return {"jsonrpc": "2.0", "id": req_id, "result": result}

    def _canned_result(self, method: str, req: dict[str, Any]) -> Any:
        if method == "eth_blockNumber":
            return _hex(self.registry.block_number)
        if method == "eth_getLogs":
            return self._filtered_logs(req)
        if method == "eth_call":
            return self._eth_call_result(req)
        if method == "eth_chainId":
            return _hex(1)
        if method == "eth_getBlockByNumber":
            return self._header_result(req)
        if method == "eth_getTransactionReceipt":
            return self._receipt_result(req)
        if method == "debug_traceTransaction":
            return self._trace_result(req)
        if method == "eth_getStorageAt":
            # The fixture market's addresses are not EIP-1967 proxies, so the
            # implementation slot reads as the zero word.
            return _u256(0)
        raise _UnservedMethod(method)

    def _header_result(self, req: dict[str, Any]) -> dict[str, str] | None:
        params = req.get("params", [])
        tag = params[0] if params else None
        number = _block_tag_number(tag)
        if number is None:
            # No height, no header: a null answer is the typed missing-header
            # refusal downstream, never a fabricated block.
            return None
        return terminal_header(number)

    def _receipt_result(self, req: dict[str, Any]) -> dict[str, Any] | None:
        """One transaction's canned logs as its COMPLETE receipt.

        The driver binds every candidate transaction to its complete receipt
        (ADR-105 D2), so the mock owes one per transaction its canned logs name.
        A hash no canned log carries is a transaction the fixture never executed:
        None is the typed missing-receipt refusal downstream.
        """
        params = req.get("params", [])
        asked = params[0].lower() if params and isinstance(params[0], str) else ""
        owned = [log for log in self.registry.logs if log["transactionHash"].lower() == asked]
        if not owned:
            return None
        owned.sort(key=lambda log: int(str(log["logIndex"]), 0))
        first = owned[0]
        return {
            "transactionHash": first["transactionHash"],
            "transactionIndex": first["transactionIndex"],
            "blockHash": first["blockHash"],
            "blockNumber": first["blockNumber"],
            "status": "0x1",
            "logs": owned,
        }

    def _trace_result(self, req: dict[str, Any]) -> dict[str, Any] | None:
        """One transaction's canned logs as a single-root call tree.

        The fixtures carry no sub-call topology, so the honest tracer answer is one
        frame whose OWN logs are the transaction's logs: the join still proves every
        receipt log has exactly one owner, and a liquidation transaction gets the
        call evidence its collateral leg needs (ADR-105 D2).
        """
        params = req.get("params", [])
        asked = params[0].lower() if params and isinstance(params[0], str) else ""
        owned = sorted(
            (log for log in self.registry.logs if log["transactionHash"].lower() == asked),
            key=lambda log: int(str(log["logIndex"]), 0),
        )
        if not owned:
            return None
        return {
            "type": "CALL",
            "to": owned[0]["address"],
            "logs": [
                {"address": log["address"], "topics": log["topics"], "data": log["data"]}
                for log in owned
            ],
            "calls": [],
        }

    def _eth_call_result(self, req: dict[str, Any]) -> str:
        # alloy/web3 send `eth_call` params as `[{to, data/input, ...}]`.
        # alloy's TransactionRequest serializes the calldata under `input`
        # (the newer field name); web3/legacy uses `data`. Handle both.
        params = req.get("params", [])
        filt = params[0] if params and isinstance(params[0], dict) else {}
        calldata = filt.get("data") or filt.get("input") or "0x"
        selector = calldata[:10].lower() if isinstance(calldata, str) else ""
        return self.registry.eth_call_responses.get(selector, "0x")

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
            addresses.update(a.lower() for a in raw_addr if isinstance(a, str))
        topic0_group: set[str] = set()
        topics = filt.get("topics") or []
        if topics:
            first = topics[0]
            if isinstance(first, list):
                # An OR-group: topic0 in {t0_a, t0_b, ...}.
                topic0_group = {t.lower() for t in first if isinstance(t, str)}
            elif isinstance(first, str):
                # A single topic0 value (the fetcher flattens a 1-element
                # OR-group to a bare hash). Treat it as a 1-element group.
                topic0_group = {first.lower()}
        # Match canned logs by address (+ topic0 if a group was requested).
        out: list[dict[str, Any]] = []
        for log in self.registry.logs:
            if addresses and log["address"].lower() not in addresses:
                continue
            if topic0_group and log["topics"][0].lower() not in topic0_group:
                continue
            out.append(log)
        return out

    def log_message(self, format: str, *args: object) -> None:  # ruff: ignore[builtin-argument-shadowing]
        return


@contextmanager
def mock_rpc_server(
    *,
    logs: list[dict[str, Any]],
    block_number: int,
    eth_call_responses: dict[str, str] | None = None,
) -> Generator[str, None, None]:
    """Start a local mock JSON-RPC server; yield its URL.

    `eth_call_responses` maps a calldata selector (e.g.
    `GET_CONFIGURATION_SELECTOR`) to the canned ABI return value — the server
    dispatches `eth_call`s by selector. `ERC20_METADATA_SELECTORS` is answered
    underneath every fixture (a `ReserveInitialized` dispatch reads it), and a
    caller's own responses win on conflict.
    """
    _MockRpcHandler.registry = MockRpcRegistry(
        block_number=block_number,
        logs=logs,
        eth_call_responses={**ERC20_METADATA_SELECTORS, **(eth_call_responses or {})},
    )
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockRpcHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        # ``shutdown()`` returns only once ``serve_forever``'s loop has exited,
        # so the join reaps a thread that is already returning.
        server.shutdown()
        server.server_close()
        thread.join()


@contextmanager
def seeded_db(
    tmp_path: Path,
    *,
    name: str,
    market_id: int = 1,
    chain_id: int = 1,
    last_update_block: int = BOOTSTRAP_BLOCK,
    with_price_oracle: bool = True,
) -> Generator[tuple[Path, sqlite3.Connection], None, None]:
    """Create a temp SQLite DB with the Rust schema + a seeded Aave market.

    Yields the DB path + an open sqlite connection over the Rust-created schema. The market
    + the POOL/POOL_CONFIGURATOR/POOL_ADDRESS_PROVIDER/PRICE_ORACLE contracts +
    a minimal GHO asset (token + aave_gho_tokens row) are seeded — the minimum
    both ``run_aave_update`` (the fetch-spec lookup) + ``update_aave_market``
    (the ``assert gho_asset is not None`` requirement) need.

    Closes the connection on exit; callers finish using it before the block ends.
    """
    db_path = tmp_path / f"{name}.db"
    # Create the full Rust-owned schema (the source of truth) on an empty file.
    db_upgrade_database(str(db_path))
    session = sqlite3.connect(db_path)
    session.row_factory = sqlite3.Row
    try:
        session.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, "
            "last_update_block) VALUES (:id, :chain, :name, 1, :block)",
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
        if with_price_oracle:
            _seed_contract(session, market_id, "PRICE_ORACLE", PRICE_ORACLE_ADDRESS)
        # The GHO token erc20 row + the aave_gho_tokens row (the Python oracle
        # asserts gho_asset is not None; the Rust treats it as optional).
        session.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, :chain, :addr)",
            {"chain": chain_id, "addr": GHO_TOKEN_ADDRESS},
        )
        session.execute(
            "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, "
            "v_gho_discount_rate_strategy, v_gho_discount_token) "
            "VALUES (1, 1, NULL, NULL, NULL)",
        )
        session.commit()
        yield db_path, session
    finally:
        session.close()


def _seed_contract(
    session: sqlite3.Connection,
    market_id: int,
    name: str,
    address: str,
    *,
    revision: int | None = None,
) -> None:
    """Insert an aave_v3_contracts row (read by the Rust fetch spec + get_contract)."""
    session.execute(
        "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
        "VALUES (:market, :name, :addr, :rev)",
        {"market": market_id, "name": name, "addr": address, "rev": revision},
    )


def dump_user_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump the ``aave_v3_users`` rows as comparable dicts (column-by-column)."""
    rows = session.execute(
        "SELECT id, market_id, address, e_mode, gho_discount, stk_aave_balance, "
        "isolation_mode_collateral_asset_id, isolation_mode_debt "
        "FROM aave_v3_users ORDER BY address"
    ).fetchall()
    return [dict(row) for row in rows]


def seed_asset_and_user(
    session: sqlite3.Connection, *, market_id: int = 1, chain_id: int = 1
) -> None:
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
        "INSERT INTO erc20_tokens (id, chain, address) VALUES (2, :c, :u)",
        {"c": chain_id, "u": uw},
    )
    session.execute(
        "INSERT INTO erc20_tokens (id, chain, address) VALUES (3, :c, :a)",
        {"c": chain_id, "a": aw},
    )
    session.execute(
        "INSERT INTO erc20_tokens (id, chain, address) VALUES (4, :c, :v)",
        {"c": chain_id, "v": vw},
    )
    session.execute(
        "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, "
        "a_token_id, a_token_revision, v_token_id, v_token_revision, "
        "e_mode_category_id, price_source, last_update_block, "
        "liquidity_index, liquidity_rate, borrow_index, borrow_rate) "
        "VALUES (1, :market, 2, 3, 1, 4, 1, NULL, NULL, NULL, 0, 0, 0, 0)",
        {"market": market_id},
    )
    # The pre-seeded user (checksummed — both paths store/lookup the EIP-55 form).
    session.execute(
        "INSERT INTO aave_v3_users (id, market_id, address, e_mode, "
        "gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, "
        "isolation_mode_debt) "
        "VALUES (1, :market, :addr, 0, 0, NULL, NULL, '0')",
        {"market": market_id, "addr": get_checksum_address(USER_ADDRESS)},
    )


def dump_collateral_config_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump ``aave_v3_user_collateral_configs`` rows as comparable dicts."""
    rows = session.execute(
        "SELECT id, user_id, asset_id, enabled "
        "FROM aave_v3_user_collateral_configs ORDER BY user_id, asset_id"
    ).fetchall()
    return [dict(row) for row in rows]


def dump_asset_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump the rate/index/block columns of ``aave_v3_assets`` as comparable dicts."""
    rows = session.execute(
        "SELECT id, market_id, price_source, liquidity_index, liquidity_rate, "
        "borrow_index, borrow_rate, last_update_block "
        "FROM aave_v3_assets ORDER BY id"
    ).fetchall()
    return [dict(row) for row in rows]


def dump_emode_category_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump ``aave_v3_emode_categories`` rows as comparable dicts."""
    rows = session.execute(
        "SELECT id, market_id, category_id, label, ltv, "
        "liquidation_threshold, liquidation_bonus, price_source "
        "FROM aave_v3_emode_categories ORDER BY category_id"
    ).fetchall()
    return [dict(row) for row in rows]


def dump_contract_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump ``aave_v3_contracts`` rows as comparable dicts (name-ordered)."""
    rows = session.execute(
        "SELECT id, market_id, name, address, revision FROM aave_v3_contracts ORDER BY name, id"
    ).fetchall()
    return [dict(row) for row in rows]


def dump_gho_token_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump ``aave_gho_tokens`` rows as comparable dicts."""
    rows = session.execute(
        "SELECT id, token_id, v_token_id, v_gho_discount_rate_strategy, "
        "v_gho_discount_token FROM aave_gho_tokens ORDER BY id"
    ).fetchall()
    return [dict(row) for row in rows]


def seed_gho_vtoken(session: sqlite3.Connection, *, chain_id: int = 1) -> None:
    """Seed the chain-unique GHO variable-debt token.

    Adds an ``erc20_tokens`` row for the GHO vToken (id 5) + sets
    ``aave_gho_tokens.v_token_id = 5`` so ``gho_asset.v_token`` resolves
    (the Python ``_process_discount_*_updated_event`` emitter check + the Rust
    ``AaveGhoAsset.v_token_address`` both need this non-NULL).
    """
    from degenbot.checksum_cache import get_checksum_address

    session.execute(
        "INSERT INTO erc20_tokens (id, chain, address) VALUES (5, :c, :a)",
        {"c": chain_id, "a": get_checksum_address(GHO_VTOKEN_ADDRESS)},
    )
    session.execute(
        "UPDATE aave_gho_tokens SET v_token_id = 5 WHERE id = 1",
    )


def dump_asset_config_rows(session: sqlite3.Connection) -> list[dict[str, Any]]:
    """Dump ``aave_v3_asset_configs`` rows as comparable dicts."""
    rows = session.execute(
        "SELECT id, asset_id, ltv, liquidation_threshold, liquidation_bonus, "
        "e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, "
        "flash_loan_enabled, isolation_mode, borrowable_in_isolation, "
        "debt_ceiling "
        "FROM aave_v3_asset_configs ORDER BY asset_id"
    ).fetchall()
    return [dict(row) for row in rows]


# Corpus for the ReserveDataUpdated parity. Each entry is a tuple of
# (label, liquidity_rate, stable_borrow_rate, variable_borrow_rate,
# liquidity_index, variable_borrow_index). The stable_borrow_rate is
# decoded-then-discarded (deprecated on Aave V3). The Ray-scale set exercises
# the U256 path (the values exceed 2**64 -> the Rust stores `U256.to_string()`
# + the Python stores the big int -> both must agree as decimal strings).
_RAY = 10**27


def create_resered_data_values() -> list[tuple[str, int, int, int, int, int]]:
    """Return the parametrized value sets for the ReserveDataUpdated parity."""
    return [
        (
            "small",
            111,
            222,
            333,
            444,
            555,
        ),
        (
            "zero",
            0,
            0,
            0,
            0,
            0,
        ),
        (
            "ray-scale",
            100_000_000_000_000_000_000_000_000,  # 1e26
            0,
            200_000_000_000_000_000_000_000_000,  # 2e26
            _RAY,  # 1e27 (exceeds 2**64)
            _RAY + 100_000_000_000_000_000_000_000_000,  # 1.1e27
        ),
    ]
