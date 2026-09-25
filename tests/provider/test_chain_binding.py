"""The chain guard belongs to the core, not to a Python caller (ADR-006 D5).

These tests drive the FFI pyclasses against fake nodes that answer
``eth_chainId``, so what they pin is the CORE's behavior: binding an endpoint
to a chain refuses a misconfigured endpoint with both chain ids named, the
check costs one round-trip per BINDING, and the IPC transport is guarded the
same way HTTP is. A Python-side check would pass none of these.
"""

from __future__ import annotations

import json
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest

from degenbot.provider import AlloyProvider, AsyncAlloyProvider
from degenbot.provider.factory import get_provider_from_config


class _FakeHttpNode:
    """An HTTP node whose only interesting answer is ``eth_chainId``."""

    def __init__(self, chain_id: int) -> None:
        self.chain_id = chain_id
        self.chain_id_reads = 0
        node = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0"))
                request = json.loads(self.rfile.read(length) or b"{}")
                node._respond(self, request)

            def log_message(self, *args: Any) -> None:
                """Keep the fake node off the test log."""

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def _respond(self, handler: BaseHTTPRequestHandler, request: dict[str, Any]) -> None:
        method = request.get("method")
        result = "0x0"
        if method == "eth_chainId":
            self.chain_id_reads += 1
            result = hex(self.chain_id)
        elif method == "eth_blockNumber":
            result = "0x1"
        payload = json.dumps(
            {"jsonrpc": "2.0", "id": request.get("id", 1), "result": result},
        ).encode()
        handler.send_response(200)
        handler.send_header("Content-Type", "application/json")
        handler.send_header("Content-Length", str(len(payload)))
        handler.end_headers()
        handler.wfile.write(payload)

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)


class _FakeIpcNode:
    """A Unix-socket node serving the same answers as the HTTP one."""

    def __init__(self, chain_id: int, directory: Path) -> None:
        self.chain_id = chain_id
        self.chain_id_reads = 0
        self.path = directory / "node.ipc"
        self._server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server.bind(str(self.path))
        self._server.listen(4)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        """One thread per accepted socket.

        The IPC transport opens connections it does not use for a request
        (an auth handshake answers EOF), so a serial accept loop would block
        on a dead connection while the real one waits in the queue.
        """
        while True:
            try:
                connection, _ = self._server.accept()
            except OSError:
                return
            threading.Thread(target=self._answer, args=(connection,), daemon=True).start()

    def _answer(self, connection: socket.socket) -> None:
        with connection:
            buffer = b""
            while not buffer.endswith(b"}"):
                chunk = connection.recv(1)
                if not chunk:
                    return
                buffer += chunk
            request = json.loads(buffer)
            method = request.get("method")
            if method == "eth_chainId":
                self.chain_id_reads += 1
            response = json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": request.get("id", 1),
                    "result": hex(self.chain_id),
                },
            )
            try:
                connection.sendall(response.encode() + b"\n")
            except OSError:
                # The client hung up on a connection it never asked on.
                return

    @property
    def url(self) -> str:
        return f"ipc://{self.path}"

    def close(self) -> None:
        self._server.close()


@pytest.fixture
def http_node():
    node = _FakeHttpNode(chain_id=8453)
    try:
        yield node
    finally:
        node.close()


class TestBindingRefusesAnotherChain:
    def test_the_pyclass_refuses_a_mismatched_endpoint(self, http_node: _FakeHttpNode) -> None:
        with pytest.raises(ValueError, match="8453") as refusal:
            AlloyProvider(http_node.url, chain_id=1)

        message = str(refusal.value)
        assert "1" in message
        assert "8453" in message, "the refusal names the chain the endpoint serves"

    async def test_the_async_pyclass_refuses_a_mismatched_endpoint(
        self, http_node: _FakeHttpNode
    ) -> None:
        with pytest.raises(ValueError, match="8453") as refusal:
            await AsyncAlloyProvider.create(http_node.url, chain_id=1)

        assert "8453" in str(refusal.value)

    def test_the_factory_binds_the_chain_it_was_configured_for(
        self, http_node: _FakeHttpNode
    ) -> None:
        # The endpoint answers chain 8453 while the session declares 137 — a
        # chain no configured layer supplies, so the explicit override is the
        # only endpoint in play and the factory must fail fast at binding.
        with pytest.raises(ValueError, match="8453"):
            get_provider_from_config(chain_id=137, node=http_node.url)


class TestBindingTheMatchingChain:
    def test_a_matching_chain_binds_and_never_re_verifies(self, http_node: _FakeHttpNode) -> None:
        provider = AlloyProvider(http_node.url, chain_id=8453)

        assert http_node.chain_id_reads == 1, "the binding is the one round-trip"

        for _ in range(3):
            assert provider.get_chain_id() == 8453

        assert http_node.chain_id_reads == 1, "a hot-path chain read never re-verifies the binding"

    def test_the_factory_binds_a_matching_endpoint(self, http_node: _FakeHttpNode) -> None:
        provider = get_provider_from_config(chain_id=8453, node=http_node.url)

        assert provider.chain_id == 8453
        assert http_node.chain_id_reads == 1


class TestIpcTransportIsGuardedToo:
    def test_the_ipc_transport_is_refused_and_binds_the_same_way(self, tmp_path: Path) -> None:
        node = _FakeIpcNode(chain_id=8453, directory=tmp_path)
        try:
            with pytest.raises(ValueError, match="8453") as refusal:
                AlloyProvider(node.url, chain_id=1)
            assert "8453" in str(refusal.value)

            provider = AlloyProvider(node.url, chain_id=8453)
            assert provider.get_chain_id() == 8453
            assert node.chain_id_reads == 2, "one read per binding, none for the hot-path read"
        finally:
            node.close()
