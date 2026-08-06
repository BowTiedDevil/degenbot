"""Operator command channel: JSON-lines over a Unix domain socket (NWTUM3).

Lets an operator steer a live bot without touching its process. The host bot
runs an :class:`OperatorServer` (an asyncio task) bound to a Unix domain
socket; the ``degenbot path add`` / ``degenbot path discover`` CLI (or any
client) writes ONE JSON command line and reads ONE JSON response line. The wire
format is minimal and versioned so a client built earlier still talks to a
running host built later.

Wire protocol (one JSON object per line, newline-terminated). Request lines::

    {
        "op": "add_path",
        "steps": [
            {"family": "V2|V3|V4", "address": "0x..", "hash": "0x..(32-byte v4 pool_id, v4 only)"},
            ...,
        ],
        "directions": [true, false],
    }  # optional; auto-resolved if absent
    {"op": "discover", "bound": 5}  # bounded on-demand discovery

Response lines::

    {"ok": true, "detail": "..."}
    {"ok": false, "error": "..."}

The server maps a ``family`` string to the pool-table base class the
registration pipeline classifies steps with, so no Python class object crosses
the wire. The handler (supplied by the host) turns an ``(op, payload)`` pair
into a response dict; the server guards the wire (JSON decode, op dispatch,
exception -> ``{"ok": false}``) so a malformed or failing command never crashes
the host.

The protocol is a plain request/response: the host processes each command
concurrently with the pump and replies once the registration pipeline has
accepted it (add-path replies after the per-path registration body completes;
discover replies after the bounded sweep is consumed).
"""

from __future__ import annotations

import asyncio
import contextlib
import json
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTableBase,
)
from degenbot.logging import logger

#: Map a wire ``family`` string to the pool-table base class the registration
#: pipeline classifies `step.type` with (V2/V3/V4) -- the single source so the
#: operator wire cannot name a table class directly.
_FAMILY_TO_TABLE: dict[str, type] = {
    "V2": UniswapV2PoolTableBase,
    "V3": UniswapV3PoolTableBase,
    "V4": UniswapV4PoolTableBase,
}

#: Handler signature: ``async def (op: str, payload: dict) -> dict`` returning a
#: response with ``detail`` (ok) or ``error`` (failure).
OperatorHandler = Callable[[str, dict[str, Any]], Awaitable[dict[str, Any]]]


@dataclass
class StepSpec:
    """A hop descriptor in the discovery-item shape the pipeline consumes.

    Attributes:
        type: The pool-table base class for the hop's family (V2/V3/V4).
        address: The pool address (``0x`` + 40 hex).
        hash: The V4 pool id (``0x`` + 64 hex) when ``type`` is V4.

    """

    type: type
    address: str
    hash: object | None = None


def step_from_wire(step: dict[str, Any]) -> StepSpec:
    """Translate a wire ``steps`` entry into a :class:`StepSpec`.

    Args:
        step: ``{"family": "V2|V3|V4", "address": "0x..", "hash": "0x.."?}``.

    Returns:
        A :class:`StepSpec` mapping ``family`` to its pool-table base class.

    Raises:
        ValueError: if ``family`` is not V2/V3/V4 or ``address`` is absent.

    """
    family = step.get("family")
    table = _FAMILY_TO_TABLE.get(family)  # type: ignore[arg-type]
    if table is None:
        msg = f"unknown pool family {family!r} (expected V2|V3|V4)"
        raise ValueError(msg)
    address = step.get("address")
    if not address:
        msg = f"{family} step is missing an address"
        raise ValueError(msg)
    return StepSpec(type=table, address=address, hash=step.get("hash"))


def wrap_handler(
    handler: OperatorHandler,
) -> OperatorHandler:
    """Wrap a host ``(op, payload) -> response`` handler with wire hygiene.

    Turns any raised exception into a ``{"ok": false, "error": ...}`` response
    (never crashes the host), and provides a convenient success/error spelling:
    the handler returns ``{"detail": ...}`` on success or ``{"error": ...}`` on
    failure, and the wrapper normalizes both to the wire shape.

    Args:
        handler: The raw host ``(op, payload) -> response`` coroutine.

    Returns:
        The wrapped handler the :class:`OperatorServer` dispatches to.

    """

    async def wrapped(op: str, payload: dict[str, Any]) -> dict[str, Any]:
        try:
            resp = await handler(op, payload)
        except Exception as exc:  # ruff: ignore[blind-except] - wire boundary: never crash host
            logger.error(f"[operator] {op} failed: {exc}")
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        if resp.get("error"):
            return {"ok": False, "error": resp["error"]}
        return {"ok": True, "detail": resp.get("detail", "")}

    return wrapped


def _decode_request(line: bytes) -> tuple[str, dict[str, Any]]:
    """Decode one request line into an ``(op, payload)`` pair.

    Args:
        line: The raw request line from the wire.

    Returns:
        A tuple of the command ``op`` and its ``payload`` dict.

    Raises:
        ValueError: if the line is not a JSON object with an ``op`` key.
        TypeError: if the ``payload`` is present but not a dict.

    """
    try:
        req = json.loads(line.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError, TypeError) as exc:
        msg = f"invalid request JSON: {exc}"
        raise ValueError(msg) from None
    op = req.get("op")
    if op is None:
        msg = "request is missing 'op'"
        raise ValueError(msg)
    payload = req.get("payload", {})
    if not isinstance(payload, dict):
        msg = "'payload' must be a dict"
        raise TypeError(msg)
    return op, payload


class OperatorServer:
    """A Unix-domain-socket command server for a live bot (NWTUM3).

    Run :meth:`serve` as an asyncio task (e.g. a background task on the
    registration loop). It accepts one or more concurrent client connections,
    reads one JSON request line, routes it to the wrapped handler, and writes
    one JSON response line. The handler is wrapped by :func:`wrap_handler` so a
    failing command replies with ``{"ok": false}`` instead of raising into the
    host.

    Args:
        handler: async ``(op, payload) -> dict`` (see :data:`OperatorHandler`).
        socket_path: filesystem path for the Unix domain socket.
        request_timeout: seconds to wait for a request line before dropping the
            connection (default 60).

    """

    def __init__(
        self,
        handler: OperatorHandler,
        *,
        socket_path: str,
        request_timeout: float = 60.0,
    ) -> None:
        """Bind an :class:`OperatorServer` to ``socket_path`` with ``handler``.

        Args:
            handler: async ``(op, payload) -> dict`` command dispatcher.
            socket_path: filesystem path for the Unix domain socket.
            request_timeout: seconds to wait for a request line (default 60).

        """
        self._handler = wrap_handler(handler)
        self._socket_path = socket_path
        self._request_timeout = request_timeout
        self._server: asyncio.AbstractServer | None = None
        self._serving: asyncio.Future[None] | None = None

    async def serve(self) -> None:
        """Start the socket server and accept connections until closed.

        Runs forever (cancellable); the caller starts it with
        ``asyncio.create_task`` and cancels it on shutdown. The ``serve_forever``
        loop runs as its own future so :meth:`close` can cancel it (without
        that, ``wait_closed`` in ``close`` would block on the still-pending
        loop).
        """
        self._server = await asyncio.start_unix_server(self._on_client, self._socket_path)
        self._serving = asyncio.ensure_future(self._server.serve_forever())
        async with self._server:
            await self._serving

    async def _handle_line(self, line: bytes) -> dict[str, Any]:
        """Decode + dispatch one request line to the wrapped handler.

        Args:
            line: The raw request line from the wire.

        Returns:
            The wire response dict (``{"ok": bool, ...}``); a decode or
            dispatch error becomes an ``{"ok": false, "error": ...}`` response.

        """
        try:
            op, payload = _decode_request(line)
        except ValueError as exc:
            return {"ok": False, "error": str(exc)}
        return await self._handler(op, payload)

    async def _on_client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        """Handle a single client connection: one request in, one response out.

        Args:
            reader: The client's stream reader.
            writer: The client's stream writer.

        Raises:
            asyncio.CancelledError: if the server task is cancelled mid-connection
                (propagated so task teardown observes the cancellation).

        """
        peer = writer.get_extra_info("peername")
        try:
            line = await asyncio.wait_for(reader.readline(), timeout=self._request_timeout)
            if not line:
                resp = {"ok": False, "error": "empty request"}
            else:
                resp = await self._handle_line(line)
        except TimeoutError:
            logger.warning(f"[operator] request timeout from {peer}; dropping")
            return
        except asyncio.CancelledError:
            raise
        except Exception as exc:  # ruff: ignore[blind-except] - never leak a connection error
            logger.error(f"[operator] connection error: {exc}")
            return
        writer.write((json.dumps(resp) + "\n").encode("utf-8"))
        await writer.drain()
        writer.close()
        await writer.wait_closed()

    async def close(self) -> None:
        """Stop accepting connections and remove the socket file.

        Self-sufficient: cancels the in-flight ``serve_forever`` loop (so
        ``wait_closed()`` cannot block on it) then closes the server and unlinks
        the socket. Safe to call whether ``serve()`` is still running, was
        cancelled by the host, or is running on another thread's event loop.
        """
        if self._serving is not None and not self._serving.done():
            self._serving.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._serving
        if self._server is not None:
            self._server.close()
            await self._server.wait_closed()
        await asyncio.to_thread(Path(self._socket_path).unlink, missing_ok=True)


async def send_command(socket_path: str, op: str, payload: dict[str, Any]) -> dict[str, Any]:
    """Connect to a running :class:`OperatorServer` and send one command.

    Args:
        socket_path: the Unix domain socket path the server listens on.
        op: command op (``add_path`` / ``discover``).
        payload: the command payload.

    Returns:
        The server's response dict (``{"ok": bool, "detail"/"error": ...}``).

    Raises:
        RuntimeError: if the server sends no response line.

    """
    reader, writer = await asyncio.open_unix_connection(socket_path)
    try:
        req = json.dumps({"op": op, "payload": payload})
        writer.write((req + "\n").encode("utf-8"))
        await writer.drain()
        line = await reader.readline()
        if not line:
            msg = f"no response from operator server at {socket_path}"
            raise RuntimeError(msg)
        return json.loads(line.decode("utf-8"))
    finally:
        writer.close()
        await writer.wait_closed()
