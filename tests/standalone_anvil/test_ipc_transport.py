"""End-to-end IPC transport proof (R2DALC).

The suite's only prior IPC coverage is ``test_ipc_kwargs`` in
``tests/test_anvil_fork.py``, which asserts nothing about a socket. This module
dials a real one: the Anvil IPC socket surfaced by ``AnvilFork.ipc_path`` (the
same socket the Rust core's ``DynProvider`` connects to).

The endpoint is resolved by the request-scope / subscription-scope resolver
from a temporary operator file (``[nodes.ipc]``) in a subprocess with every
``DEGENBOT_RPC_*`` name cleared, so the file layer is the only source. Over the
resolved socket the test asserts both capabilities:

- a one-shot request path: ``eth_chainId``, a token read (``decimals()``), and
  the sim client's ``eth_callMany`` framing;
- a live subscription: the pump's ``subscribe`` reaches block ``W``.

It also asserts the scope filter refuses rather than degrading: an ``http``-only
file cannot satisfy a subscription, and a chain with no entry at all refuses the
request scope instead of inventing a ``localhost`` default.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import threading
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.arbitrage.engine_registry import ArbitrageEngine
from degenbot.fork import AnvilFork, ForkLaunchConfig
from degenbot.provider import AlloyProvider
from tests.standalone_anvil import seed as seed_catalog

if TYPE_CHECKING:
    from pathlib import Path

# The node needs a live local anvil and a spawned subprocess, so it is a
# non-default ("slow") run; the default addopts filter (`not slow`) deselects it.
pytestmark = pytest.mark.slow

_CHAIN_ID = seed_catalog.CHAIN_ID
_OTHER_CHAIN_ID = 999

# `decimals()` on the seeded SimpleToken; the selector is the ERC-20 ABI entry.
_DECIMALS_SELECTOR = bytes.fromhex("313ce567")
_TOKEN_DECIMALS = 8

# The probe runs the PUBLIC resolver surface in a fresh interpreter, because the
# config is installed once at FFI import. It reports one JSON object per op and
# renders a refusal as `{"error": "Type: message"}` so the test asserts on the
# message an operator actually sees.
_PROBE = """\
import json
import sys

from degenbot.config import resolve_http_rpc_uri, resolve_node, resolve_ws_rpc_uri

results = []
for op in json.loads(sys.argv[1]):
    kind = op[0]
    try:
        if kind == "node":
            resolved = resolve_node(op[1], op[2])
            results.append({"uri": resolved.uri, "source": resolved.source})
        elif kind == "request":
            results.append({"uri": resolve_http_rpc_uri(op[1])})
        elif kind == "subscription":
            results.append({"uri": resolve_ws_rpc_uri(op[1])})
        else:
            raise AssertionError("unknown probe op: " + kind)
    except BaseException as exc:  # noqa: BLE001 - the message is the assertion
        results.append({"error": type(exc).__name__ + ": " + str(exc)})
print(json.dumps(results))
"""


def _resolve_from_operator_file(tmp_path: Path, body: str, *ops: list[Any]) -> list[dict]:
    """Resolve ``ops`` in a fresh interpreter with a pinned operator file.

    Every ``DEGENBOT_RPC_*`` / ``DEGENBOT_DEFAULT_CHAIN_ID`` name is cleared and
    the XDG homes point into ``tmp_path``, so neither the developer's shell nor
    their ``~/.config`` file can decide a layer. ``DEGENBOT_CONFIG`` only points
    at the written file (the documented file selection), so the file layer is
    the sole endpoint source. This mirrors the subprocess pattern of
    ``tests/test_config_rpc.py`` and the Rust ``operator_file_resolution`` test.

    Args:
        tmp_path: Per-test scratch directory.
        body: The operator file's TOML.
        ops: Probe operations, in order.

    Returns:
        One result dict per op.
    """
    config_file = tmp_path / "operator.toml"
    config_file.write_text(body, encoding="utf-8")
    xdg_config = tmp_path / "xdg-config"
    xdg_state = tmp_path / "xdg-state"
    xdg_config.mkdir(exist_ok=True)
    xdg_state.mkdir(exist_ok=True)

    env = dict(os.environ)
    for name in list(env):
        if name.startswith("DEGENBOT_RPC_") or name == "DEGENBOT_DEFAULT_CHAIN_ID":
            del env[name]
    env.update({
        "DEGENBOT_CONFIG": str(config_file),
        "XDG_CONFIG_HOME": str(xdg_config),
        "XDG_STATE_HOME": str(xdg_state),
    })

    completed = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - fixed argv, no shell
        [sys.executable, "-c", _PROBE, json.dumps(ops)],
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    assert completed.returncode == 0, completed.stderr
    parsed: list[dict] = json.loads(completed.stdout)
    assert len(parsed) == len(ops)
    return parsed


def _nodes_file(*, http: dict[int, str] | None = None, ipc: dict[int, str] | None = None) -> str:
    """Render an operator file carrying the declared ``[nodes.*]`` tables."""
    lines: list[str] = []
    for table, entries in (("http", http), ("ipc", ipc)):
        if not entries:
            continue
        lines.append(f"[nodes.{table}]")
        lines.extend(f"{chain_id} = {json.dumps(uri)}" for chain_id, uri in entries.items())
    return "\n".join(lines) + "\n"


def _spawn_anvil() -> AnvilFork:
    """Spawn the standalone anvil over an interval-mined chain with IPC enabled.

    The default ``standalone_anvil`` fixture automines only on a transaction; the
    pump's ``subscribe`` handshake needs a stream of headers, so this boot uses
    a one-second block interval. The IPC socket is surfaced at
    ``AnvilFork.ipc_path``.
    """
    fork = AnvilFork(
        launch=ForkLaunchConfig(
            chain_id=_CHAIN_ID,
            mining_mode="interval",
            mining_interval=1,
        )
    )
    seed_catalog.seed(fork)
    fork.mine()
    return fork


def test_ipc_request_and_subscription_resolve_from_the_operator_file(tmp_path: Path) -> None:
    """The operator-file ``ipc`` entry serves both scopes and dials the node.

    The endpoint comes from the resolver reading a temporary operator file with
    no ``DEGENBOT_RPC_*`` environment. Over that endpoint: ``eth_chainId`` and a
    token read (request scope) and the pump's ``subscribe`` (subscription
    scope).
    """
    fork = _spawn_anvil()
    try:
        socket_path = fork.ipc_path
        body = _nodes_file(ipc={1: socket_path})

        request, subscription, absent = _resolve_from_operator_file(
            tmp_path,
            body,
            ["node", 1, "request"],
            ["node", 1, "subscription"],
            ["request", _OTHER_CHAIN_ID],
        )

        assert request == {"uri": socket_path, "source": "file"}, (
            "the request scope must resolve the operator-file ipc entry"
        )
        assert subscription == {"uri": socket_path, "source": "file"}, (
            "the subscription scope must resolve the operator-file ipc entry"
        )
        assert "error" in absent, "an ipc-only file must not satisfy a chain with no entry"

        request_uri = request["uri"]
        # The resolver handed back the same socket the node is actually bound
        # to; dial THAT, not a path re-derived from the fixture.
        assert request_uri == socket_path, "the resolved endpoint is the live socket"

        provider = AlloyProvider(request_uri)
        try:
            assert provider.get_chain_id() == _CHAIN_ID, (
                "eth_chainId over the ipc socket returns the node's chain"
            )
            decimals = provider.call(seed_catalog.TOKEN, _DECIMALS_SELECTOR)
            assert int.from_bytes(decimals, "big") == _TOKEN_DECIMALS, (
                "a token read (decimals()) over the ipc socket returns the seeded value"
            )
        finally:
            provider.close()

        engine = ArbitrageEngine()
        try:
            block_before = fork.provider.block_number
            boundary = engine.subscribe(subscription["uri"])
            assert boundary >= block_before, (
                "the pump's subscribe reaches a live block over the ipc socket"
            )
            assert boundary <= fork.provider.block_number, (
                "the subscription boundary is a real observed block"
            )
        finally:
            engine.stop()
    finally:
        fork.close()


class _IpcCallManyNode:
    """A real Unix-socket JSON-RPC node that serves ``eth_callMany``.

    Anvil's Tempo build rejects every ``eth_callMany`` parameter shape, so the
    sim client's bundle request is proven against a real (local) IPC socket that
    answers it instead. The framing matches alloy's IPC client: bare JSON values
    with no delimiter, so a buffer is parsed as soon as it holds one complete
    value.
    """

    def __init__(self, socket_path: Path, chain_id: int) -> None:
        self.socket_path = socket_path
        self.chain_id = chain_id
        self.requests: list[dict] = []
        self._listener: socket.socket | None = None
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(self.socket_path))
        listener.listen(4)
        self._listener = listener
        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()

    def _accept_loop(self) -> None:
        assert self._listener is not None
        try:
            while True:
                conn, _ = self._listener.accept()
                threading.Thread(target=self._serve, args=(conn,), daemon=True).start()
        except OSError:
            return

    def _serve(self, conn: socket.socket) -> None:
        pending = ""
        try:
            while True:
                chunk = conn.recv(4096)
                if not chunk:
                    return
                pending += chunk.decode()
                while pending.strip():
                    try:
                        request = json.loads(pending.strip())
                    except json.JSONDecodeError:
                        break
                    pending = ""
                    response = self._respond(request)
                    conn.sendall(json.dumps(response).encode())
        finally:
            conn.close()

    def _respond(self, request: dict) -> dict:
        self.requests.append(request)
        method = request.get("method")
        if method == "eth_chainId":
            result: Any = hex(self.chain_id)
        elif method == "eth_callMany":
            result = [{"success": True, "returnData": "0x"}]
        else:
            result = None
        return {"jsonrpc": "2.0", "id": request.get("id"), "result": result}

    def close(self) -> None:
        if self._listener is not None:
            self._listener.close()
            self._listener = None


def test_sim_client_eth_call_many_over_a_real_ipc_socket(tmp_path: Path) -> None:
    """The sim client's ``eth_callMany`` bundle frame crosses a real IPC socket.

    The bundle params mirror ``degenbot_submission::bundle::eth_call_many_bundle_sim_params``
    (a target call followed by the backrun), which is the request the frame-sim
    gate sends. The endpoint is a real Unix socket; the assertion is that the
    node parsed the bundle and answered it, not that a mock transport was called.
    """
    socket_path = tmp_path / "sim.ipc"
    node = _IpcCallManyNode(socket_path, _CHAIN_ID)
    node.start()
    try:
        provider = AlloyProvider(str(socket_path))
        try:
            assert provider.get_chain_id() == _CHAIN_ID, "the sim socket dialect serves eth_chainId"
            bundle_params = [
                {
                    "transactions": [
                        {
                            "to": "0x0000000000000000000000000000000000000000",
                            "data": "0x",
                        }
                    ]
                },
                {"blockNumber": "latest", "transactionIndex": 0},
            ]
            result = provider.make_request("eth_callMany", bundle_params)
            assert result[0]["success"] is True, (
                "the sim client's eth_callMany bundle crossed the ipc socket"
            )
        finally:
            provider.close()

        methods = [request.get("method") for request in node.requests]
        assert "eth_callMany" in methods, "the ipc socket saw the sim request"
        call_many = next(r for r in node.requests if r.get("method") == "eth_callMany")
        bundle, context = call_many["params"]
        assert bundle["transactions"][0]["to"] == ("0x0000000000000000000000000000000000000000"), (
            "the sim bundle carried the target transaction"
        )
        assert context["blockNumber"] == "latest", "the sim bundle pinned the state block"
    finally:
        node.close()


def test_scope_filter_refuses_instead_of_degrading(tmp_path: Path) -> None:
    """An ``http``-only file cannot satisfy a subscription; no entry means refuse.

    The subscription scope accepts only feed-capable transports (``ipc``/``ws``),
    so an ``http`` entry is refused loudly rather than silently degraded to
    polling. A chain with no entry in any layer refuses the request scope
    instead of falling back to a ``localhost`` default.
    """
    http_only = _nodes_file(http={1: "http://file-http:8545"})
    refused = _resolve_from_operator_file(
        tmp_path,
        http_only,
        ["subscription", 1],
    )[0]
    assert "error" in refused, "an http-only file must not satisfy a subscription"
    message = refused["error"]
    assert "subscription" in message, "the refusal names the subscription scope"
    assert "ipc" in message, "the refusal names ipc as an accepted feed transport"
    assert "ws" in message, "the refusal names ws as an accepted feed transport"

    ipc_only = _nodes_file(ipc={1: str(tmp_path / "never-dialled.ipc")})
    no_layer = _resolve_from_operator_file(
        tmp_path,
        ipc_only,
        ["request", _OTHER_CHAIN_ID],
    )[0]
    assert "error" in no_layer, "a chain with no entry must refuse the request scope"
    no_layer_message = no_layer["error"]
    assert str(_OTHER_CHAIN_ID) in no_layer_message, "the refusal names the chain"
    assert "there is no localhost default" in no_layer_message, (
        "the refusal must not fall back to a localhost default"
    )
