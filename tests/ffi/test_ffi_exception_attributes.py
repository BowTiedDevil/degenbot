"""The manifested island exceptions' stamped attributes: stubs against runtime.

Every ``create_exception!`` island that stamps typed attributes does it in one
builder (``bot::pool::swap_refused_err``, ``rpc::errors::chain_mismatch_to_pyerr``,
``simulation::session::abort_error``), and the stubgen declaration manifest
(``rust/tools/stubgen/src/exception_manifest.rs``) declares those attributes so
the generated class bodies spell ``name: <ty>``. This suite ties both ends: the
declarations parsed from the generated ``src/degenbot/_ffi/*.pyi`` against
instances raised through the real construction paths. A runtime rename or new
stamp, or a stub annotation that moved away from the stamped value's type,
fails here instead of drifting silently.

"""

from __future__ import annotations

import ast
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from degenbot.dispatch import (
    ConsumerErrorKind,
    Dispatcher,
    SessionConsumer,
    SessionConsumerError,
)
from degenbot.provider import AlloyProvider
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_types import UniswapV3PoolState
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot, ChainMismatchError, SwapRefusalKind, SwapRefused
from tests.helpers.session_drive import offline_executor
from tests.helpers.v3_pool_factory import make_v3_pool

_REPO_ROOT = Path(__file__).resolve().parents[2]
_STUB_DIR = _REPO_ROOT / "src" / "degenbot" / "_ffi"

# The audited contract: ``module.Class`` -> {attribute: stub annotation}. The
# specification side of the tie — the generated stub must spell exactly these
# annotations, and the raised instances must carry exactly these names with
# values of these types.
_DECLARED: dict[str, dict[str, str]] = {
    "degenbot._ffi.ChainMismatchError": {
        "expected": "int",
        "actual": "int",
        "endpoint": "str",
    },
    "degenbot._ffi.simulation.SessionConsumerError": {
        "kind": "ConsumerErrorKind | None",
        "batch_index": "int",
        "report": "dict[str, Any]",
    },
    "degenbot._ffi.SwapRefused": {
        "kind": "SwapRefusalKind",
        "word": "int | None",
        "amount": "int | None",
    },
}

# The enum classes a stub annotation can reference, resolved through the
# suite's doors (never through the private extension): an annotation naming a
# class outside this table fails the type check loudly.
_ENUM_TYPES: dict[str, type] = {
    "ConsumerErrorKind": ConsumerErrorKind,
    "SwapRefusalKind": SwapRefusalKind,
}


def _stub_module(stub: Path) -> str:
    """The dotted module path one generated stub file declares."""
    segments = list(stub.relative_to(_STUB_DIR).with_suffix("").parts)
    if segments[-1] == "__init__":
        segments.pop()
    return "degenbot._ffi." + ".".join(segments) if segments else "degenbot._ffi"


def _stub_declarations() -> dict[str, dict[str, str]]:
    """``module.Class`` -> {attribute: annotation} for every generated class body."""
    declarations: dict[str, dict[str, str]] = {}
    for stub in sorted(_STUB_DIR.glob("*.pyi")):
        module = _stub_module(stub)
        tree = ast.parse(stub.read_text(encoding="utf-8"))
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            attributes = {
                child.target.id: ast.unparse(child.annotation)
                for child in node.body
                if isinstance(child, ast.AnnAssign) and isinstance(child.target, ast.Name)
            }
            declarations[f"{module}.{node.name}"] = attributes
    return declarations


def _value_satisfies(value: object, annotation: str) -> bool:
    """Whether a raised instance's value satisfies the stub annotation spelling."""
    if annotation == "int":
        return type(value) is int
    if annotation == "str":
        return type(value) is str
    if annotation == "dict[str, Any]":
        return type(value) is dict
    if annotation.endswith(" | None"):
        return value is None or _value_satisfies(value, annotation.removesuffix(" | None"))
    expected = _ENUM_TYPES.get(annotation)
    return isinstance(expected, type) and isinstance(value, expected)


def _assert_stamps_the_declarations(exc: BaseException, expected: type, class_path: str) -> None:
    """The raised instance carries exactly the attributes its generated class declares."""
    assert type(exc) is expected, f"the refusal must raise {expected}, got {type(exc)}"
    declared = _stub_declarations().get(class_path, {})
    assert set(vars(exc)) == set(declared), (
        f"{class_path} stamps {sorted(vars(exc))} but the stub declares {sorted(declared)}"
    )
    for name, annotation in declared.items():
        assert _value_satisfies(vars(exc)[name], annotation), (
            f"{class_path}.{name} = {vars(exc)[name]!r} does not satisfy the stub's `{annotation}`"
        )


def test_generated_stubs_declare_the_stamped_attributes() -> None:
    """The generated class bodies spell exactly the audited attribute contract."""
    declarations = _stub_declarations()
    for class_path, attributes in _DECLARED.items():
        assert declarations.get(class_path) == attributes, (
            f"{class_path} must declare exactly {attributes}, got {declarations.get(class_path)}"
        )


def test_swap_refusal_stamps_the_declared_attributes() -> None:
    """A real swap-simulation refusal carries kind/word/amount as the stub declares."""
    py_bot = Bot()
    liquidity = 10_000_000_000_000
    token0 = make_erc20(py_bot, address="0x" + "11" * 20, name="T0", symbol="T0", decimals=18)
    token1 = make_erc20(py_bot, address="0x" + "22" * 20, name="T1", symbol="T1", decimals=18)
    pool = make_v3_pool(
        address="0x" + "33" * 20,
        token0=token0,
        token1=token1,
        factory="0x" + "44" * 20,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=liquidity,
        tick_data={
            -60: LiquidityAtTick(liquidity_net=liquidity, liquidity_gross=liquidity, block=0),
            60: LiquidityAtTick(liquidity_net=-liquidity, liquidity_gross=liquidity, block=0),
        },
        py_bot=py_bot,
        tick_data_fetcher=None,
    )
    empty_state = UniswapV3PoolState(
        address=pool.address,
        block=None,
        liquidity=liquidity,
        sqrt_price_x96=1 << 96,
        tick=0,
        tick_bitmap=set(),
        tick_data={},
    )
    with pytest.raises(SwapRefused) as refusal:
        pool._py_pool.simulate(
            zero_for_one=True,
            amount_specified=-1_000,
            override_state=empty_state,
        )
    _assert_stamps_the_declarations(refusal.value, SwapRefused, "degenbot._ffi.SwapRefused")


async def test_session_consumer_error_stamps_the_declared_attributes() -> None:
    """A real consume-loop refusal carries kind/batch_index/report as the stub declares."""
    executor = offline_executor(Dispatcher.for_block(12_345))
    consumer = SessionConsumer(executor, allow_quiet_end=True)
    with pytest.raises(SessionConsumerError) as refusal:
        await consumer.resume()
    _assert_stamps_the_declarations(
        refusal.value,
        SessionConsumerError,
        "degenbot._ffi.simulation.SessionConsumerError",
    )


class _FakeChainNode:
    """An HTTP node whose only interesting answer is ``eth_chainId``."""

    def __init__(self, chain_id: int) -> None:
        self._chain_id = chain_id
        node = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0"))
                request = json.loads(self.rfile.read(length) or b"{}")
                payload = json.dumps(
                    {"jsonrpc": "2.0", "id": request.get("id", 1), "result": hex(node._chain_id)},
                ).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args: object) -> None:
                """Keep the fake node off the test log."""

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()


@pytest.fixture
def chain_node():
    node = _FakeChainNode(8453)
    try:
        yield node
    finally:
        node.close()


def test_chain_mismatch_stamps_the_declared_attributes(chain_node: _FakeChainNode) -> None:
    """A real binding refusal carries expected/actual/endpoint as the stub declares."""
    with pytest.raises(ChainMismatchError) as refusal:
        AlloyProvider(chain_node.url, chain_id=1)
    _assert_stamps_the_declarations(
        refusal.value, ChainMismatchError, "degenbot._ffi.ChainMismatchError"
    )
