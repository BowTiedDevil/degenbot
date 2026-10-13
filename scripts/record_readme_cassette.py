#!/usr/bin/env python3
"""Record the README examples' chain surface into an offline cassette.

The README's Bot examples run against a live archive node by default. This
recorder drives the SAME ``Bot.build_pool`` / token-utility surface the README
examples exercise through a recording HTTP proxy in front of the archive node,
then projects the captured JSON-RPC ledger onto the multi-block
``OfflineProvider`` wire shape (``degenbot.chain-data/v1``) that
``degenbot.provider.OfflineProvider`` replays.

The README's examples then run with ZERO RPC: ``Bot(provider=OfflineProvider(...))``
answers every ``eth_call`` / ``eth_getCode`` / ``eth_getBalance`` /
``eth_getBlockByNumber`` / ``eth_blockNumber`` from the cassette.

**Head pinning.** ``build_erc20token`` (and every un-``state_block`` call)
resolves "latest" through ``eth_blockNumber``, so the proxy pins the head: it
answers ``eth_blockNumber`` with ``--pin`` and rewrites every "latest" block tag
to that block before forwarding. That makes the recording a pure function of the
pinned block (and the two explicit README blocks), so regeneration is
byte-identical — the drift-gate precondition.

Usage::

    uv run --no-sync python scripts/record_readme_cassette.py            # write
    uv run --no-sync python scripts/record_readme_cassette.py --check    # drift gate

Requires a reachable archive node (``TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI``).
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import URLError
from urllib.request import Request, urlopen

# The pinned "latest" head, and the README's two explicit blocks.
PIN = 24407242
HIST = 18900000
# The README's `block_identifier=10000000` token-balance example.
OLD = 10_000_000

V3 = "0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8"
V2 = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
CURVE = "0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
OWNER = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
SPENDER = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"

OUT = Path("tests/fixtures/chain_data/1/readme_examples.json")
CHAIN_DATA_FORMAT_V1 = "degenbot.chain-data/v1"
_TAGS = {"latest", "pending", "safe", "finalized", "earliest"}
_RETRIES = 8
# `eth_call` / `eth_getCode` / `eth_getBalance` take `[target, block]`.
_BLOCK_INDEX = {"eth_call": 1, "eth_getCode": 1, "eth_getBalance": 1, "eth_getBlockByNumber": 0}


def _fetch(upstream: str, body: bytes) -> bytes:
    last: Exception | None = None
    for attempt in range(_RETRIES):
        try:
            req = Request(upstream, data=body, headers={"Content-Type": "application/json"})
            with urlopen(req, timeout=60) as response:  # noqa: S310 - the configured archive node
                return response.read()
        except (URLError, TimeoutError, OSError) as exc:
            last = exc
            time.sleep(0.5 * (attempt + 1))
    assert last is not None
    raise last


class _Recorder:
    def __init__(self, upstream: str, pin: int) -> None:
        self.upstream = upstream
        self.pin = pin
        self.entries: list[tuple[str, list, dict]] = []
        self._lock = threading.Lock()
        self._server: ThreadingHTTPServer | None = None

    def _respond(self, request: dict) -> dict:
        method = request.get("method")
        if method == "eth_blockNumber":
            return {"jsonrpc": "2.0", "id": request.get("id"), "result": hex(self.pin)}
        params = request.get("params")
        if isinstance(params, list) and method in _BLOCK_INDEX:
            idx = _BLOCK_INDEX[method]
            if len(params) > idx and (params[idx] is None or params[idx] in _TAGS):
                params[idx] = hex(self.pin)
        response = json.loads(_fetch(self.upstream, json.dumps(request).encode()))
        with self._lock:
            self.entries.append((method, params, response))
        return response

    def _make_handler(self) -> type[BaseHTTPRequestHandler]:
        recorder = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args: object) -> None:
                pass

            def do_POST(self) -> None:  # noqa: N802 - http.server API
                length = int(self.headers.get("Content-Length", 0))
                sent = json.loads(self.rfile.read(length))
                batch = isinstance(sent, list)
                responses = [recorder._respond(rq) for rq in (sent if batch else [sent])]
                out = json.dumps(responses if batch else responses[0]).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(out)))
                self.end_headers()
                self.wfile.write(out)

        return Handler

    def __enter__(self) -> str:
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._make_handler())
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        return f"http://127.0.0.1:{self._server.server_address[1]}"

    def __exit__(self, *_exc: object) -> None:
        assert self._server is not None
        self._server.shutdown()


def _resolve_block(tag: object) -> int:
    if tag is None or tag in _TAGS:
        return PIN
    if isinstance(tag, str) and tag.startswith("0x"):
        return int(tag, 16)
    return int(tag)  # type: ignore[arg-type]


def _drive(url: str) -> None:
    """Drive every chain-touching operation the README's Bot examples issue.

    Fresh Bots per group defeat the registry cache so every call lands on the
    wire; a call the README only ever makes as a cache hit is never replayed.
    """
    import degenbot

    pools = degenbot.Bot(chain_id=1, node=url, database=":memory:")
    pools.build_pool(V3)
    pools.build_pool(V2)
    pools.build_pool(CURVE)

    # The historical builds run in a fresh Bot (the README's API-reference
    # section builds them in its own session), so the registry cache cannot
    # swallow the block-18900000 fetches.
    historical = degenbot.Bot(chain_id=1, node=url, database=":memory:")
    historical.build_pool(V3, state_block=HIST)
    historical.build_pool(V2, state_block=HIST)

    tokens = degenbot.Bot(chain_id=1, node=url, database=":memory:")
    weth = tokens.build_erc20token(WETH)
    usdc = tokens.build_erc20token(USDC)
    tokens.get_token_balance(weth, OWNER)
    tokens.get_token_balance(weth, OWNER, block_identifier=OLD)
    tokens.get_token_balance(usdc, OWNER)
    tokens.get_token_approval(weth, owner=OWNER, spender=SPENDER)
    tokens.get_token_approval(usdc, owner=OWNER, spender=SPENDER)
    tokens.get_token_total_supply(weth)
    tokens.get_ether_balance(address=OWNER)


def _project(entries: list[tuple[str, list, dict]]) -> dict:
    blocks: dict[int, dict] = {}

    def block(n: int) -> dict:
        return blocks.setdefault(n, {"timestamp": 0, "calls": {}, "code": {}, "balances": {}})

    for method, params, response in entries:
        if method == "eth_call":
            tx = params[0]
            data = tx.get("input") or tx.get("data") or "0x"
            key = f"{tx['to'].lower()}:0x{data.lower().removeprefix('0x')}"
            block(_resolve_block(params[1]))["calls"][key] = (
                response["result"].removeprefix("0x")
                if response.get("result") is not None
                else None
            )
        elif method == "eth_getCode":
            block(_resolve_block(params[1]))["code"][params[0].lower()] = response[
                "result"
            ].removeprefix("0x")
        elif method == "eth_getBalance":
            block(_resolve_block(params[1]))["balances"][params[0].lower()] = response[
                "result"
            ].removeprefix("0x")
        elif method == "eth_getBlockByNumber":
            block(_resolve_block(params[0]))["timestamp"] = int(response["result"]["timestamp"], 16)
    return {
        "format": CHAIN_DATA_FORMAT_V1,
        "chain_id": 1,
        "blocks": {str(n): blocks[n] for n in sorted(blocks)},
    }


def _render(cassette: dict) -> bytes:
    return (json.dumps(cassette, indent=2, sort_keys=True) + "\n").encode()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail on drift instead of writing")
    parser.add_argument("--out", type=Path, default=OUT)
    args = parser.parse_args()

    from tests.conftest import TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI

    recorder = _Recorder(TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI, PIN)
    with recorder as url:
        _drive(url)
    rendered = _render(_project(recorder.entries))

    if args.check:
        if not args.out.exists() or args.out.read_bytes() != rendered:
            print(
                f"ERROR: {args.out} has drifted; re-record with {Path(__file__).name}",
                file=sys.stderr,
            )
            return 1
        print(f"✓ {args.out} matches the recorded chain surface")
        return 0

    args.out.write_bytes(rendered)
    print(f"✓ wrote {args.out} ({len(rendered)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
