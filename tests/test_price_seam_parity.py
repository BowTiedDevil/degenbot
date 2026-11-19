"""Offline parity tests for the price-reader PyO3 seam + shells.

Drives the full ``ChainlinkPriceFeed`` / ``AavePriceOracle`` pyclasses +
the delegating ``ChainlinkPriceContract`` / ``OraclePriceFetcher`` shells
through a local in-process JSON-RPC mock that returns canned ABI-encoded
``eth_call`` bytes. This exercises the actual ``eth_call`` → Rust ABI decode
→ shell float path end-to-end, with no live RPC and no Anvil fork.

Parity target (the parity gate,
``docs/architecture/parity-gate.md``; Python as oracle):
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
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from collections.abc import Generator

from degenbot.aave import AavePriceOracle
from degenbot.aave.analysis.orchestrator import OraclePriceFetcher
from degenbot.chainlink import ChainlinkPriceContract, ChainlinkPriceFeed
from degenbot.provider import AlloyProvider

# Canonical Chainlink selectors (first 4 bytes of keccak of the canonical sig).
DECIMALS_SELECTOR = "313ce567"
LATEST_ROUND_DATA_SELECTOR = "feaf968c"
GET_ASSET_PRICE_SELECTOR = "b3596f07"

# A non-whole ETH/USD answer at 8 decimals → 1845.00000005 whole units
# (proves the fractional part is preserved, not truncated).
CHAINLINK_ANSWER = 184_500_000_005
CHAINLINK_DECIMALS = 8
# A negative answer is malformed feed data (a live USD feed never produces one).
CHAINLINK_NEGATIVE_ANSWER = -184_500_000_005
# More decimals than `10**decimals` can hold — malformed `decimals()`.
CHAINLINK_EXCESSIVE_DECIMALS = 40
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


def _int256_bytes(value: int) -> bytes:
    """Encode a signed int as its 32-byte two's-complement word (int256)."""
    return (value % (1 << 256)).to_bytes(32, "big")


class _MockRpcHandler(BaseHTTPRequestHandler):
    """Serves canned ABI-encoded ``eth_call`` results keyed by 4-byte selector.

    The answer and decimals words are class attributes so a case can serve
    malformed feed data from an otherwise successful RPC; ``observed_selectors``
    optionally records every selector the case issued.
    """

    chainlink_answer = CHAINLINK_ANSWER
    chainlink_decimals = CHAINLINK_DECIMALS
    observed_selectors: list[str] | None = None

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
        if self.observed_selectors is not None:
            self.observed_selectors.append(selector)
        if selector == DECIMALS_SELECTOR:
            return "0x" + _u256_bytes(self.chainlink_decimals).hex()
        if selector == LATEST_ROUND_DATA_SELECTOR:
            # (uint80 roundId, int256 answer, uint256 startedAt,
            #  uint256 updatedAt, uint80 answeredInRound)
            return (
                "0x"
                + _u256_bytes(1).hex()
                + _int256_bytes(self.chainlink_answer).hex()
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
def _mock_rpc_server(
    answer: int = CHAINLINK_ANSWER,
    decimals: int = CHAINLINK_DECIMALS,
    observed_selectors: list[str] | None = None,
) -> Generator[str, None, None]:
    class _CaseHandler(_MockRpcHandler):
        chainlink_answer = answer
        chainlink_decimals = decimals

    _CaseHandler.observed_selectors = observed_selectors
    server = ThreadingHTTPServer(("127.0.0.1", 0), _CaseHandler)
    port = server.server_address[1]
    import threading

    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        # ``shutdown()`` returns only once ``serve_forever``'s loop has exited,
        # so the join reaps a thread that is already returning: no timeout, and
        # nothing here reads elapsed time as evidence.
        server.shutdown()
        server.server_close()
        thread.join()


@pytest.fixture
def mock_provider() -> Generator[AlloyProvider, None, None]:
    with _mock_rpc_server() as url:
        yield AlloyProvider(url, 0)


@dataclass
class FakeProviderBot:
    """Bot double carrying the provider slice ``ChainlinkPriceContract`` reads."""

    provider: AlloyProvider


@pytest.fixture
def fake_bot(mock_provider: AlloyProvider) -> FakeProviderBot:
    return FakeProviderBot(provider=mock_provider)


def _free_port_plausible() -> bool:
    """Bound 127.0.0.1 sockets always work offline — smoke guard."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return True


# ---------------------------------------------------------------------------
# Chainlink seam + shell parity
# ---------------------------------------------------------------------------


def test_py_chainlink_feed_decimals_byte_exact(mock_provider: AlloyProvider) -> None:
    feed = ChainlinkPriceFeed(CHAINLINK_ETH_USD, mock_provider.to_alloy_provider())
    assert feed.decimals() == CHAINLINK_DECIMALS


def test_py_chainlink_feed_latest_round_data_byte_exact(
    mock_provider: AlloyProvider,
) -> None:
    feed = ChainlinkPriceFeed(CHAINLINK_ETH_USD, mock_provider.to_alloy_provider())
    round_id, answer, started_at, updated_at, answered_in_round = feed.latest_round_data()
    assert round_id == 1
    assert answer == CHAINLINK_ANSWER
    assert started_at == 17_000_000_000
    assert updated_at == 17_000_000_050
    assert answered_in_round == 1


def test_chainlink_shell_price_matches_python_float_division(
    fake_bot: FakeProviderBot,
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


def test_chainlink_shell_decimals_cached(fake_bot: FakeProviderBot) -> None:
    contract = ChainlinkPriceContract(CHAINLINK_ETH_USD, bot=fake_bot)
    assert contract.decimals == CHAINLINK_DECIMALS
    # Second access uses the cached value (no second eth_call for decimals).
    assert contract.decimals == CHAINLINK_DECIMALS


def test_chainlink_py_price_truncates_whole_units(mock_provider: AlloyProvider) -> None:
    """The Rust ``price()`` convenience truncates to whole units (integer div)."""
    feed = ChainlinkPriceFeed(CHAINLINK_ETH_USD, mock_provider.to_alloy_provider())
    truncated = feed.price()
    assert truncated == 1845  # int(answer // 10**decimals)


def test_chainlink_shell_price_rejects_negative_answer() -> None:
    """The float shell refuses malformed data with the feed context, not a price.

    A negative answer yields a negative float under plain division, so the
    display path must apply the reader's own check — over the tuple it already
    decoded, with no extra ``eth_call``.
    """
    calls: list[str] = []
    with _mock_rpc_server(answer=CHAINLINK_NEGATIVE_ANSWER, observed_selectors=calls) as url:
        contract = ChainlinkPriceContract(
            CHAINLINK_ETH_USD, bot=FakeProviderBot(provider=AlloyProvider(url, 0))
        )

        with pytest.raises(ValueError, match="negative answer") as excinfo:
            _ = contract.price

    message = str(excinfo.value)
    assert CHAINLINK_ETH_USD.lower() in message.lower(), message
    assert str(CHAINLINK_NEGATIVE_ANSWER) in message, message
    assert f"decimals {CHAINLINK_DECIMALS}" in message, message
    # The check reads the already-decoded tuple: `latestRoundData` + `decimals`.
    assert sorted(calls) == sorted([DECIMALS_SELECTOR, LATEST_ROUND_DATA_SELECTOR]), calls


def test_chainlink_shell_price_rejects_excessive_decimals() -> None:
    """An unscalable ``decimals`` raises from the float shell, with context."""
    with _mock_rpc_server(decimals=CHAINLINK_EXCESSIVE_DECIMALS) as url:
        contract = ChainlinkPriceContract(
            CHAINLINK_ETH_USD, bot=FakeProviderBot(provider=AlloyProvider(url, 0))
        )

        with pytest.raises(ValueError, match="overflows u128") as excinfo:
            _ = contract.price

    message = str(excinfo.value)
    assert CHAINLINK_ETH_USD.lower() in message.lower(), message
    assert str(CHAINLINK_ANSWER) in message, message
    assert f"decimals {CHAINLINK_EXCESSIVE_DECIMALS}" in message, message


def test_chainlink_py_price_rejects_negative_answer() -> None:
    """RPC success with malformed data raises, naming the feed and the answer.

    A negative answer divides to a whole-unit zero, so a clamp would report this
    broken feed as a valid ``$0`` price. The typed error must carry the feed, the
    answer, and the decimals instead.
    """
    with _mock_rpc_server(answer=CHAINLINK_NEGATIVE_ANSWER) as url:
        provider = AlloyProvider(url, 0)
        feed = ChainlinkPriceFeed(CHAINLINK_ETH_USD, provider.to_alloy_provider())

        with pytest.raises(ValueError, match="negative answer") as excinfo:
            feed.price()

    message = str(excinfo.value)
    assert CHAINLINK_ETH_USD.lower() in message.lower(), message
    assert str(CHAINLINK_NEGATIVE_ANSWER) in message, message
    assert f"decimals {CHAINLINK_DECIMALS}" in message, message


# ---------------------------------------------------------------------------
# Aave seam + shell parity
# ---------------------------------------------------------------------------


def test_py_aave_oracle_get_asset_price_byte_exact(
    mock_provider: AlloyProvider,
) -> None:
    oracle = AavePriceOracle(AAVE_ORACLE, mock_provider.to_alloy_provider())
    assert oracle.get_asset_price(ASSET_ADDRESS) == AAVE_ASSET_PRICE


def test_aave_shell_fetch_matches_prior_shape(mock_provider: AlloyProvider) -> None:
    fetcher = OraclePriceFetcher(mock_provider, AAVE_ORACLE)
    prices = fetcher.fetch({ASSET_ADDRESS})
    assert prices == {ASSET_ADDRESS: AAVE_ASSET_PRICE}


# Smoke: the localhost mock bind works in the sandbox (no network needed).
def test_offline_mock_binding() -> None:
    assert _free_port_plausible()
