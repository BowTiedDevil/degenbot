#!/usr/bin/env python3
"""Record construction cassettes for the offline construction suites.

Drives each construction replay suite's exact ``Bot.build_pool`` surface (plus
the recorded-quote calls the Camelot parity assertions compare against) through
a head-pinned recording proxy in front of a live endpoint, then projects the
captured JSON-RPC ledger onto the per-block ``OfflineProvider`` wire shape
(``degenbot.chain-data/v1``) under
``tests/fixtures/chain_data/<chain_id>/construction_<scenario>_block<N>.json``.

The recorded answers ARE the corpus: every value comes off the wire. An
``eth_call`` that reverted at the pin is stored as ``null`` (the offline
transport replays it as an execution revert); any other node error is a fixture
gap and fails the recording instead of being stored.

Head pinning. ``Bot.build_pool`` (and every un-``state_block`` call) resolves
"latest" through ``eth_blockNumber``, so the proxy answers ``eth_blockNumber``
with the scenario's pinned block and rewrites every block tag to that block
before forwarding. Each scenario records twice and requires the two passes
byte-identical; ``--check`` additionally requires byte-identity with the corpus
on disk (exit 1 on drift).

Usage::

    uv run --no-sync python scripts/record_construction_cassettes.py
    uv run --no-sync python scripts/record_construction_cassettes.py --check
    uv run --no-sync python scripts/record_construction_cassettes.py --scenario camelot_v2_mim_usdc

Requires endpoints serving pinned-block state: Base on
``https://mainnet.base.org`` (paced — it rate-limits bursts), Arbitrum on
``https://arbitrum-one.public.blastapi.io`` (serves the camelot pin keyless;
the official arb1 endpoint rejects historical state).
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import TYPE_CHECKING, Any
from urllib.error import URLError
from urllib.request import Request, urlopen

if TYPE_CHECKING:
    from collections.abc import Callable


# Script mode puts scripts/ on sys.path, not the repo root the tests.* helpers
# resolve from.
_REPO_ROOT = Path(__file__).resolve().parents[1]
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))


CHAIN_DATA_FORMAT_V1 = "degenbot.chain-data/v1"

# Base pin: the block the existing Base corpora serve.
BASE_BLOCK = 46_875_151
# Arbitrum pin: the block the existing camelot corpus serves.
ARBITRUM_BLOCK = 477_785_000

BASE_UPSTREAM = "https://mainnet.base.org"
ARBITRUM_UPSTREAM = "https://arbitrum-one.public.blastapi.io"

# Pin spellings the proxy rewrites to the scenario's block before forwarding.
_TAGS = {"latest", "pending", "safe", "finalized", "earliest"}
# `eth_call` / `eth_getCode` / `eth_getBalance` take `[target, block]`;
# `eth_getBlockByNumber` takes `[block, full]`.
_BLOCK_INDEX = {"eth_call": 1, "eth_getCode": 1, "eth_getBalance": 1, "eth_getBlockByNumber": 0}
# The projection understands exactly these; anything else would replay as an
# unrecorded method and is a fixture gap.
_PROJECTED_METHODS = {"eth_call", "eth_getCode", "eth_getBalance", "eth_getBlockByNumber"}
_PASSTHROUGH_METHODS = {"eth_blockNumber", "eth_chainId"}

_RETRIES = 8
# geth-family JSON-RPC error code for an execution revert.
_REVERT_ERROR_CODE = 3


@dataclass(frozen=True)
class Scenario:
    """One construction cassette: its pin, endpoint, and recorded call surface."""

    name: str
    chain_id: int
    block: int
    upstream: str
    drive: Callable[[str], None]
    out: str

    @property
    def output_path(self) -> str:
        """Corpus path relative to the repo root.

        Returns:
            The chain-data home path this scenario writes.

        """
        return f"tests/fixtures/chain_data/{self.chain_id}/{self.out}"


def _fetch(upstream: str, body: bytes, pace_s: float) -> bytes:
    last: Exception | None = None
    for attempt in range(_RETRIES):
        time.sleep(pace_s)
        try:
            req = Request(
                upstream,
                data=body,
                # The public gateways reject the urllib default agent.
                headers={"Content-Type": "application/json", "User-Agent": "degenbot-recorder/1"},
            )
            # The upstream is the operator-configured endpoint.
            with urlopen(req, timeout=60) as response:  # ruff: ignore[suspicious-url-open-usage]
                return response.read()
        except (URLError, TimeoutError, OSError) as exc:
            last = exc
            time.sleep(0.5 * (attempt + 1))
    assert last is not None
    raise last


def _classify_error(response: dict) -> None:
    """Fail loud on a node error that is not a contract revert.

    A revert is corpus truth (recorded as ``null``); every other error is a
    fixture gap (endpoint weather, missing archive state) and must not be
    stored as chain truth.

    Raises:
        RuntimeError: The node answered with a non-revert JSON-RPC error.

    """
    error = response.get("error")
    if error is None:
        return
    message = str(error.get("message", ""))
    if "revert" in message.lower() or error.get("code") == _REVERT_ERROR_CODE:
        return
    msg = f"node error that is not a contract revert: {error!r}"
    raise RuntimeError(msg)


class _Recorder:
    """Recording HTTP proxy: pin the head, forward paced, keep the ledger."""

    def __init__(self, upstream: str, pin: int, pace_s: float) -> None:
        self.upstream = upstream
        self.pin = pin
        self.pace_s = pace_s
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
        response = json.loads(_fetch(self.upstream, json.dumps(request).encode(), self.pace_s))
        _classify_error(response)
        with self._lock:
            self.entries.append((method, params, response))
        return response

    def _make_handler(self) -> type[BaseHTTPRequestHandler]:
        recorder = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args: object) -> None:
                pass

            def do_POST(self) -> None:
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

    def fetch_block_timestamp(self) -> None:
        """Record the pin's own block header (the corpus timestamp source)."""
        self._respond({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBlockByNumber",
            "params": [hex(self.pin), False],
        })


def _project(entries: list[tuple[str, list, dict]], pin: int) -> dict:
    """Project the ledger onto the per-block OfflineProvider wire shape.

    Returns:
        The per-block corpus body (format marker, chain, pin, timestamp,
        calls, code, and balances when recorded).

    Raises:
        RuntimeError: A call landed off the pin, an unprojected method was
            recorded, or the block timestamp is not exactly one value.

    """
    calls: dict[str, str | None] = {}
    code: dict[str, str] = {}
    balances: dict[str, str] = {}
    timestamps: set[int] = set()

    def _block_of(params: list, method: str) -> int:
        raw = params[_BLOCK_INDEX[method]]
        block = int(raw, 16) if isinstance(raw, str) and raw.startswith("0x") else int(raw)
        if block != pin:
            msg = f"{method} recorded at block {block}, expected the pin {pin}"
            raise RuntimeError(msg)
        return block

    for method, params, response in entries:
        if method not in _PROJECTED_METHODS:
            if method not in _PASSTHROUGH_METHODS:
                msg = f"unprojected method {method!r} would replay as an unrecorded call"
                raise RuntimeError(msg)
            continue
        if method == "eth_getBlockByNumber":
            _block_of(params, method)
            timestamps.add(int(response["result"]["timestamp"], 16))
            continue
        _block_of(params, method)
        result = response.get("result")
        if method == "eth_call":
            tx = params[0]
            data = tx.get("input") or tx.get("data") or "0x"
            key = f"{tx['to'].lower()}:0x{data.lower().removeprefix('0x')}"
            calls[key] = result.removeprefix("0x").lower() if result is not None else None
        elif method == "eth_getCode":
            code[params[0].lower()] = result.removeprefix("0x").lower()
        elif method == "eth_getBalance":
            balances[params[0].lower()] = result.removeprefix("0x").lower()

    if len(timestamps) != 1:
        msg = f"expected exactly one block timestamp at the pin, got {sorted(timestamps)}"
        raise RuntimeError(msg)

    corpus: dict[str, Any] = {
        "format": CHAIN_DATA_FORMAT_V1,
        "chain_id": None,  # filled by the caller (scenario-owned)
        "block_number": pin,
        "timestamp": timestamps.pop(),
        "calls": calls,
        "code": code,
    }
    if balances:
        corpus["balances"] = balances
    return corpus


def _render(corpus: dict) -> bytes:
    return (json.dumps(corpus, indent=2, sort_keys=True) + "\n").encode()


# --- scenario drives: the exact construction-replay suites' surfaces ---------


_AERODROME_V2_TBTC_USDBC = "0x723AEf6543aecE026a15662Be4D3fb3424D502A9"
_AERODROME_V3_CBETH_WETH = "0x47cA96Ea59C13F72745928887f84C9F52C3D7348"
_PANCAKE_V3_WETH_CBETH = "0x257FCbAE4Ac6B26A02E4FC5e1a11e4174B5ce395"
_CAMELOT_WETH_USDC = "0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"
_CAMELOT_MIM_USDC = "0x68A0859de50B4Dfc6EFEbE981cA906D38Cdb0D1F"


def _build_and_touch_state(url: str, pool_address: str) -> tuple[Any, Any]:
    """Drive one construction replay suite's surface: build + state read.

    Returns:
        The recording provider and the built pool companion.

    """
    from degenbot.provider import AlloyProvider
    from tests.helpers.bot_factory import make_bot_with_provider

    provider = AlloyProvider(url)
    bot = make_bot_with_provider(provider)
    lp = bot.build_pool(pool_address)
    _ = lp.state
    return provider, lp


def _drive_aerodrome_v2_tbtc_usdbc(url: str) -> None:
    _build_and_touch_state(url, _AERODROME_V2_TBTC_USDBC)


def _drive_aerodrome_v3_cbeth_weth(url: str) -> None:
    _build_and_touch_state(url, _AERODROME_V3_CBETH_WETH)


def _drive_pancake_v3_weth_cbeth(url: str) -> None:
    _build_and_touch_state(url, _PANCAKE_V3_WETH_CBETH)


def _drive_camelot_with_quote(url: str, pool_address: str, token_in_index: int) -> None:
    """Build the pool, then record the contract quote the replay asserts against."""
    from degenbot.provider.call_helpers import encode_function_calldata

    provider, lp = _build_and_touch_state(url, pool_address)
    token_in = (lp.token0, lp.token1)[token_in_index]
    amount_in = 1000 * 10**token_in.decimals
    calldata = encode_function_calldata(
        "getAmountOut(uint256,address)", [amount_in, token_in.address]
    )
    provider.call(lp.address, calldata)


def _drive_camelot_v2_weth_usdc(url: str) -> None:
    _drive_camelot_with_quote(url, _CAMELOT_WETH_USDC, 1)


def _drive_camelot_v2_mim_usdc(url: str) -> None:
    _drive_camelot_with_quote(url, _CAMELOT_MIM_USDC, 0)


SCENARIOS = [
    Scenario(
        name="aerodrome_v2_tbtc_usdbc",
        chain_id=8453,
        block=BASE_BLOCK,
        upstream=BASE_UPSTREAM,
        drive=_drive_aerodrome_v2_tbtc_usdbc,
        out=f"construction_aerodrome_v2_tbtc_usdbc_block{BASE_BLOCK}.json",
    ),
    Scenario(
        name="aerodrome_v3_cbeth_weth",
        chain_id=8453,
        block=BASE_BLOCK,
        upstream=BASE_UPSTREAM,
        drive=_drive_aerodrome_v3_cbeth_weth,
        out=f"construction_aerodrome_v3_cbeth_weth_block{BASE_BLOCK}.json",
    ),
    Scenario(
        name="pancake_v3_weth_cbeth",
        chain_id=8453,
        block=BASE_BLOCK,
        upstream=BASE_UPSTREAM,
        drive=_drive_pancake_v3_weth_cbeth,
        out=f"construction_pancake_v3_weth_cbeth_block{BASE_BLOCK}.json",
    ),
    Scenario(
        name="camelot_v2_weth_usdc",
        chain_id=42161,
        block=ARBITRUM_BLOCK,
        upstream=ARBITRUM_UPSTREAM,
        drive=_drive_camelot_v2_weth_usdc,
        out=f"construction_camelot_v2_weth_usdc_block{ARBITRUM_BLOCK}.json",
    ),
    Scenario(
        name="camelot_v2_mim_usdc",
        chain_id=42161,
        block=ARBITRUM_BLOCK,
        upstream=ARBITRUM_UPSTREAM,
        drive=_drive_camelot_v2_mim_usdc,
        out=f"construction_camelot_v2_mim_usdc_block{ARBITRUM_BLOCK}.json",
    ),
]


def _record_pass(scenario: Scenario, pace_s: float) -> dict:
    recorder = _Recorder(scenario.upstream, scenario.block, pace_s)
    with recorder as url:
        scenario.drive(url)
        recorder.fetch_block_timestamp()
    corpus = _project(recorder.entries, scenario.block)
    corpus["chain_id"] = scenario.chain_id
    return corpus


def main() -> int:
    """Record (or drift-check) the selected construction corpora.

    Returns:
        Zero on success, 1 when two passes disagree or a corpus drifted.

    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail on drift instead of writing")
    parser.add_argument("--scenario", action="append", choices=[s.name for s in SCENARIOS])
    parser.add_argument("--pace-ms", type=int, default=250, help="delay between upstream requests")
    args = parser.parse_args()

    selected = [s for s in SCENARIOS if not args.scenario or s.name in args.scenario]
    pace_s = args.pace_ms / 1000

    for scenario in selected:
        first = _record_pass(scenario, pace_s)
        second = _record_pass(scenario, pace_s)
        if _render(first) != _render(second):
            print(f"ERROR: {scenario.name}: the two record passes disagree", file=sys.stderr)
            return 1
        rendered = _render(first)
        path = scenario.output_path
        out_path = Path(path)
        if args.check:
            on_disk = out_path.read_bytes() if out_path.exists() else b""
            if on_disk != rendered:
                print(
                    f"ERROR: {path} has drifted; re-record with {Path(__file__).name}",
                    file=sys.stderr,
                )
                return 1
            print(f"✓ {path} matches the recorded chain surface")
            continue
        out_path.write_bytes(rendered)
        print(f"✓ wrote {path} ({len(rendered)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
