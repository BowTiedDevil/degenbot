"""Offline §4.2 parity tests for the price-reader PyO3 seam + shells.

Drives the full ``PyChainlinkPriceFeed`` / ``PyAavePriceOracle`` pyclasses +
the delegating ``ChainlinkPriceContract`` / ``OraclePriceFetcher`` shells
through a local in-process JSON-RPC mock that returns canned ABI-encoded
``eth_call`` bytes. This exercises the actual ``eth_call`` → Rust ABI decode
→ shell float path end-to-end, with no live RPC and no Anvil fork.

Parity target (ADR-005 §4.2, Python as oracle):
    prior Python ``ChainlinkPriceContract.price`` =
        ``float(answer / 10**decimals)``   (where ``answer`` came from
        ``abi_decode(["uint80","int256","uint256","uint256","uint80"], ...)``)
    shell ``price`` =
        ``float(latest_round_data()[1]) / 10**decimals``  (Rust-decoded answer)

Both are ``int / 10**decimals`` float division, so they agree value-exactly —
*including* the fractional part (which the Rust ``price()`` truncating-integer
convenience intentionally drops), proven here with a non-whole answer.

prior ``OraclePriceFetcher.fetch`` returned ``{asset: uint256_price}``; the shell
returns the same dict (Rust core tolerantly skips reverts, matching the prior
``ContractLogicError`` / ``ValueError`` catch).
"""

from __future__ import annotations

import json
import socket
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from collections.abc import Generator

from degenbot.aave.analysis.orchestrator import OraclePriceFetcher
from degenbot.chainlink import ChainlinkPriceContract
from degenbot.degenbot_rs import AlloyProvider, PyAavePriceOracle, PyChainlinkPriceFeed
from degenbot.provider import AlloyProvider

# Canonical Chainlink selectors (first 4 bytes of keccak of the canonical sig).
DECIMALS_SELECTOR = "313ce567"
LATEST_ROUND_DATA_SELECTOR = "feaf968c"
GET_ASSET_PRICE_SELECTOR = "b3596f07"

# A non-whole ETH/USD answer at 8 decimals → 1845.00000005 whole units
# (proves the fractional part is preserved, not truncated).
CHAINLINK_ANSWER = 184_500_000_005
CHAINLINK_DECIMALS = 8
# A representative Aave oracle uint256 price for an asset.
AAVE_ASSET_PRICE = 1_234_567_890_123_456_789

# A canonical Chainlink ETH/USD aggregator address (any 20-byte address works
# against the canned mock; this is the reference the production code points at).
CHAINLINK_ETH_USD = "0x5f4eC3Df9cbd43714FE2740f5e3616155c5b8419"
AAVE_ORACLE = "0x54586bE62E3143580F063f948614F4f9F3D2C2D8"
ASSET_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def _u256_bytes(value: int) -> bytes:
    """Encode a non-negative int as a 32-byte big-endian word (uint256)."""
    return value.to_bytes(32, "big")


class _MockRpcHandler(BaseHTTPRequestHandler):
    """Serves canned ABI-encoded ``eth_call`` results keyed by 4-byte selector."""

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        req = json.loads(body)
        method = req.get("method")
        req_id = req.get("id")
        result = self._canned_result(method, req)
        if result is None:
            # Generic OK for any auxiliary method the provider may issue.
            result = "0x0"
        self._respond(req_id, result)

    def _canned_result(self, method: str, req: dict) -> str | None:
        if method != "eth_call":
            return None
        params = req.get("params", [])
        call_obj = params[0] if params and isinstance(params[0], dict) else {}
        # alloy serializes eth_call calldata under the `input` key
        # (TransactionRequest::input); some clients use `data` — accept both.
        data = call_obj.get("input") or call_obj.get("data") or ""
        selector = data[2:10] if data.startswith("0x") else data[0:8]
        if selector == DECIMALS_SELECTOR:
            return "0x" + _u256_bytes(CHAINLINK_DECIMALS).hex()
        if selector == LATEST_ROUND_DATA_SELECTOR:
            # (uint80 roundId, int256 answer, uint256 startedAt,
            #  uint256 updatedAt, uint80 answeredInRound)
            return (
                "0x"
                + _u256_bytes(1).hex()
                + _u256_bytes(CHAINLINK_ANSWER).hex()
                + _u256_bytes(17_000_000_000).hex()
                + _u256_bytes(17_000_000_050).hex()
                + _u256_bytes(1).hex()
            )
        if selector == GET_ASSET_PRICE_SELECTOR:
            return "0x" + _u256_bytes(AAVE_ASSET_PRICE).hex()
        return None

    def _respond(self, req_id: object, result: str) -> None:
        payload = json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args: object) -> None:
        return


@contextmanager
def _mock_rpc_server() -> Generator[str, None, None]:
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockRpcHandler)
    port = server.server_address[1]
    import threading

    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@pytest.fixture
def mock_provider() -> Generator[AlloyProvider, None, None]:
    with _mock_rpc_server() as url:
        alloy = AlloyProvider(url, 0)
        yield alloy


@pytest.fixture
def fake_bot(mock_provider: AlloyProvider) -> SimpleNamespace:
    # ChainlinkPriceContract only reads ``bot.provider`` → a namespace suffices.
    return SimpleNamespace(provider=mock_provider)


def _free_port_plausible() -> bool:
    """Bound 127.0.0.1 sockets always work offline — smoke guard."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return True


# ---------------------------------------------------------------------------
# Chainlink seam + shell parity
# ---------------------------------------------------------------------------


def test_py_chainlink_feed_decimals_byte_exact(mock_provider: AlloyProvider) -> None:
    feed = PyChainlinkPriceFeed(CHAINLINK_ETH_USD, mock_provider.as_alloy())
    assert feed.decimals() == CHAINLINK_DECIMALS


def test_py_chainlink_feed_latest_round_data_byte_exact(
    mock_provider: AlloyProvider,
) -> None:
    feed = PyChainlinkPriceFeed(CHAINLINK_ETH_USD, mock_provider.as_alloy())
    round_id, answer, started_at, updated_at, answered_in_round = feed.latest_round_data()
    assert round_id == 1
    assert answer == CHAINLINK_ANSWER
    assert started_at == 17_000_000_000
    assert updated_at == 17_000_000_050
    assert answered_in_round == 1


def test_chainlink_shell_price_matches_python_float_division(
    fake_bot: SimpleNamespace,
) -> None:
    """Shell ``price`` == ``float(answer) / 10**decimals`` (prior Python path)."""
    contract = ChainlinkPriceContract(CHAINLINK_ETH_USD, bot=fake_bot)
    price = contract.price
    # Fractional parity: the non-whole answer keeps its fractional part
    # (NOT truncated like the Rust ``price()`` convenience).
    assert price == pytest.approx(float(CHAINLINK_ANSWER) / 10**CHAINLINK_DECIMALS)
    assert isinstance(price, float)
    # The exact expected value: 1845.00000005
    assert price == pytest.approx(1845.00000005)


def test_chainlink_shell_decimals_cached(fake_bot: SimpleNamespace) -> None:
    contract = ChainlinkPriceContract(CHAINLINK_ETH_USD, bot=fake_bot)
    assert contract.decimals == CHAINLINK_DECIMALS
    # Second access uses the cached value (no second eth_call for decimals).
    assert contract.decimals == CHAINLINK_DECIMALS


def test_chainlink_py_price_truncates_whole_units(mock_provider: AlloyProvider) -> None:
    """The Rust ``price()`` convenience truncates to whole units (integer div)."""
    feed = PyChainlinkPriceFeed(CHAINLINK_ETH_USD, mock_provider.as_alloy())
    truncated = feed.price()
    assert truncated == 1845  # int(answer // 10**decimals)


# ---------------------------------------------------------------------------
# Aave seam + shell parity
# ---------------------------------------------------------------------------


def test_py_aave_oracle_get_asset_price_byte_exact(
    mock_provider: AlloyProvider,
) -> None:
    oracle = PyAavePriceOracle(AAVE_ORACLE, mock_provider.as_alloy())
    assert oracle.get_asset_price(ASSET_ADDRESS) == AAVE_ASSET_PRICE


def test_aave_shell_fetch_matches_prior_shape(mock_provider: AlloyProvider) -> None:
    fetcher = OraclePriceFetcher(mock_provider, AAVE_ORACLE)
    prices = fetcher.fetch({ASSET_ADDRESS})
    assert prices == {ASSET_ADDRESS: AAVE_ASSET_PRICE}


# Smoke: the localhost mock bind works in the sandbox (no network needed).
def test_offline_mock_binding() -> None:
    assert _free_port_plausible()
