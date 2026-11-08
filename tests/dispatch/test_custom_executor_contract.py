"""Certify a compiled custom executor through the generic adapter seam (ADR-098).

``contracts/vyper/custom_executor_probe.vy`` is a real Vyper contract, compiled
by the pinned ``vyper`` dependency inside this module and installed as a foreign
deployment's declared runtime. Nothing here is a hand-assembled EVM stub: the
engine executes the compiled runtime at the deployment's target address, and the
adapter's declared probe reads are the only observation surface.

The claim is functional, not textual. The positive case encodes
``record(uint256)`` with a unique marker and declares one read of the contract's
own ``last_value()`` getter; the gross figure the verdict reports IS that read's
post-minus-pre delta, so a selector that looked right over a run that stored
nothing cannot pass. The decoy case keeps the same read and encodes ``decoy()``
instead: the same engine runs the same contract, the decoy's own flag moves, and
the record evidence is absent — so neither a wrong selector nor an empty revert
can impersonate a certified run.

Every batch runs ``dry_run`` and answers its cold-state reads from a loopback
stub, so no live endpoint and no committed bytecode are involved.
"""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import TYPE_CHECKING, Any

import pytest
import vyper

from degenbot.crypto import function_selector
from degenbot.dispatch import (
    BatchExecutor,
    BatchOutcome,
    BatchOutcomeSet,
    Dispatcher,
    SessionConsumer,
    SimulateContext,
    SimulateVerdict,
    SimulationDeployment,
    SubmitSkipReason,
    TxSigner,
    typed_submit_record,
)
from tests.helpers.seam import (
    AlloyProvider,
    AsyncAlloyProvider,
    PayloadAdapter,
    abi_encode_call,
    build_batch_executor_py,
)

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

    from degenbot.arbitrage.engine_registry import ArbitrageEngine


CONTRACT_PATH = (
    Path(__file__).resolve().parents[2] / "contracts" / "vyper" / "custom_executor_probe.vy"
)

RECORD_SIGNATURE = "record(uint256)"
DECOY_SIGNATURE = "decoy()"
LAST_VALUE_SIGNATURE = "last_value()"
DECOY_FLAG_SIGNATURE = "decoy_flag()"

#: The value ``record(uint256)`` is asked to store, and the gross profit the
#: verdict must report: the stored word is the opportunity's whole evidence.
MARKER = 10**16

#: The one-bit movement ``decoy()`` leaves behind in ``decoy_flag()``.
DECOY_FLAG_DELTA = 1

#: The custom deployment's identity, target, and gas ceiling. The target holds no
#: chain code, so the only code the engine can run there is the declared runtime.
DEPLOYMENT_ID = 2
TARGET = "0x00000000000000000000000000000000000000c0"
GAS_LIMIT = 1_000_000

# The operator account the fixture signer holds (key ``0x11..11``): the one
# authority every deployment's owner must name.
OWNER = "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"

# The built-in settlement executor (code-less in this fixture).
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"

#: The fee the simulated block runs under (1 gwei base fee, 1 gwei priority).
_BASE_FEE = 1_000_000_000

_ZERO_WORD = "0x" + "00" * 32


class _CannedChainNode:
    """A loopback JSON-RPC stub answering the cold-state reads the sim issues.

    The engine executes in-process against the fixture's own state (the pools,
    tokens, and the deployment's declared runtime) and only calls out for a cold
    miss, so the stub serves that miss surface and nothing else.
    """

    def __init__(self) -> None:
        self.methods: list[str] = []
        node = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0"))
                body = json.loads(self.rfile.read(length) or b"{}")
                # alloy packs independent reads into one JSON-RPC batch.
                requests = body if isinstance(body, list) else [body]
                responses = [node._respond(request) for request in requests]
                payload = json.dumps(
                    responses if isinstance(body, list) else responses[0],
                ).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args: object) -> None:
                """Keep the stub off the test log."""

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def _respond(self, request: dict[str, Any]) -> dict[str, Any]:
        method = request.get("method")
        self.methods.append(method)
        return {
            "jsonrpc": "2.0",
            "id": request.get("id", 1),
            "result": _canned_result(method),
        }

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()


def _canned_result(method: str | None) -> object:
    """The canned answer for one cold-state read."""
    match method:
        case "eth_chainId":
            return "0x1"
        case "eth_blockNumber":
            return "0x64"
        case "eth_getCode" | "eth_call":
            return "0x"
        case "eth_getBalance" | "eth_getTransactionCount":
            return "0x0"
        case "eth_getStorageAt":
            return _ZERO_WORD
        case "eth_getBlockByNumber" | "eth_getBlockByHash":
            # The read-anchor resolution wants the block identity only; the
            # state itself comes from the fixture's own pools.
            return {"hash": "0x" + "11" * 32, "number": "0x64"}
        case "eth_gasPrice":
            return "0x" + format(_BASE_FEE, "x")
        case "eth_estimateGas":
            return "0x5208"
        case _:
            return "0x0"


@pytest.fixture
def canned_chain() -> Iterator[_CannedChainNode]:
    """A loopback stub RPC endpoint for the certified sim (see `_CannedChainNode`)."""
    node = _CannedChainNode()
    try:
        yield node
    finally:
        node.close()


@pytest.fixture(scope="module")
def probe_runtime() -> bytes:
    """The probe contract's runtime bytecode, compiled by the pinned Vyper."""
    compiled = vyper.compile_code(
        CONTRACT_PATH.read_text(encoding="utf-8"),
        output_formats=["bytecode_runtime"],
    )
    return bytes.fromhex(compiled["bytecode_runtime"].removeprefix("0x"))


def _executor(
    engine: ArbitrageEngine,
    *,
    rpc_url: str,
    runtime: bytes,
    compose: Callable[[object], bytes],
    probes: list[dict[str, object]],
) -> BatchExecutor:
    """Build the session executor over one foreign deployment carrying `runtime`."""
    provider = AsyncAlloyProvider(AlloyProvider(rpc_url))
    return build_batch_executor_py(
        context=SimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=EXECUTOR,
            weth_address=WETH,
            pool_manager_address=POOL_MANAGER,
            inject_code=False,
        ),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=provider,
        operator_nonce=5,
        dry_run=True,
        broadcast_providers=None,
        deployments=[
            SimulationDeployment(
                DEPLOYMENT_ID,
                PayloadAdapter(compose, to=TARGET, probes=probes),
                OWNER,
                TARGET,
                GAS_LIMIT,
                code=runtime,
            )
        ],
        # The explicit fan-out: the built-in target and the compiled one both run.
        target_selection="all_targets",
    )


def _block_event(number: int) -> dict[str, Any]:
    return {
        "kind": "block",
        "number": number,
        "timestamp": 1_700_000_000,
        "base_fee_per_gas": _BASE_FEE,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _batch_event(solve_block: int, *, rows: tuple[tuple[Any, ...], ...] = ()) -> dict[str, Any]:
    return {
        "kind": "batch",
        "solve_block": solve_block,
        "timestamp": 0,
        "base_fee_per_gas": _BASE_FEE,
        "gas_used": 0,
        "gas_limit": 0,
        "fresh": list(rows),
        "updated": [],
        "expired": [],
        "removed": [],
        "payloads": {},
    }


def _solver_row(path_id: int) -> tuple[Any, ...]:
    """One engine solve row — the stream's six-tuple."""
    return (path_id, 10**15, 1, (10**15, 10**15), (10**15, 10**15), (0, 0))


async def _one_batch(
    executor: BatchExecutor,
    *,
    path_id: int,
    block: int = 100,
) -> BatchOutcomeSet:
    """Drive one scripted batch (one solved path) through the real lane."""
    consumer = SessionConsumer.scripted(
        executor,
        [
            _block_event(block),
            _batch_event(block, rows=(_solver_row(path_id),)),
        ],
    )
    await consumer.run()
    batch = await executor.next_outcome()
    assert batch is not None, "the session consumed its one batch"
    assert await executor.next_outcome() is None
    return batch


def _custom(records: list[BatchOutcome]) -> BatchOutcome:
    """The compiled deployment's own record, out of the fan-out's record set."""
    by_deployment = {rec.candidate_id.deployment_id: rec for rec in records}
    return by_deployment[DEPLOYMENT_ID]


def _custom_submits(batch: BatchOutcomeSet) -> list[Any]:
    """The submit-lane records naming the compiled deployment."""
    submitted = [typed_submit_record(raw) for raw in batch.submit_records]
    return [rec for rec in submitted if rec.candidate.deployment_id == DEPLOYMENT_ID]


def _getter_read(label: str, signature: str) -> dict[str, object]:
    """A contract-call probe: the getter's COMPLETE calldata to the deployment."""
    return {"label": label, "to": TARGET, "data": abi_encode_call(signature, [])}


class TestCompiledRecordFunction:
    """The engine executes the compiled ``record(uint256)`` the adapter encoded."""

    async def test_record_stores_the_encoded_marker_through_the_generic_engine(
        self,
        nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
        canned_chain: _CannedChainNode,
        probe_runtime: bytes,
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        assert probe_runtime, "the pinned Vyper compiler produced runtime bytecode"

        payload = abi_encode_call(RECORD_SIGNATURE, [MARKER])
        assert payload[:4] == function_selector(RECORD_SIGNATURE), "record(uint256)"
        assert int.from_bytes(payload[4:], "big") == MARKER, "selector + the ABI argument"

        composed: list[bytes] = []

        def compose(_result: object) -> bytes:
            composed.append(payload)
            return payload

        # A zero-argument getter's complete calldata IS its 4-byte selector.
        last_value = _getter_read("LAST_VALUE", LAST_VALUE_SIGNATURE)
        assert last_value["data"] == function_selector(LAST_VALUE_SIGNATURE)

        executor = _executor(
            engine,
            rpc_url=canned_chain.url,
            runtime=probe_runtime,
            compose=compose,
            probes=[last_value],
        )
        batch = await _one_batch(executor, path_id=path_id)

        assert composed == [payload], "the adapter encoded once, for the deployment it names"

        custom = _custom(batch.records)
        assert custom.candidate_id.path_id == path_id

        verdict = custom.simulate
        assert isinstance(verdict, SimulateVerdict.Profitable), (
            verdict.detail.record() if isinstance(verdict, SimulateVerdict.Failed) else verdict
        )
        assert verdict.receipt.gross_profit == MARKER, (
            "the verdict's gross figure is the declared last_value() read's "
            "post-minus-pre delta: the compiled function stored the argument"
        )
        assert verdict.receipt.net_profit > 0
        gas_cost = verdict.receipt.gas_used * (_BASE_FEE + verdict.receipt.priority_fee)
        assert gas_cost * 20 < MARKER, "the marker dwells far above the gas cost"

        submits = _custom_submits(batch)
        assert len(submits) == 1, "the compiled deployment reached the submit plan"
        assert submits[0].candidate.path_id == path_id
        assert submits[0].reason is SubmitSkipReason.DRY_RUN


class TestWrongSelectorNegativeControl:
    """``decoy()`` runs the same contract and produces none of the record evidence."""

    async def test_the_decoy_selector_runs_without_storing_the_marker(
        self,
        nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
        canned_chain: _CannedChainNode,
        probe_runtime: bytes,
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path

        payload = abi_encode_call(DECOY_SIGNATURE, [])
        assert payload == function_selector(DECOY_SIGNATURE), "decoy()"

        composed: list[bytes] = []

        def compose(_result: object) -> bytes:
            composed.append(payload)
            return payload

        executor = _executor(
            engine,
            rpc_url=canned_chain.url,
            runtime=probe_runtime,
            compose=compose,
            probes=[
                _getter_read("LAST_VALUE", LAST_VALUE_SIGNATURE),
                _getter_read("DECOY_FLAG", DECOY_FLAG_SIGNATURE),
            ],
        )
        batch = await _one_batch(executor, path_id=path_id)

        assert composed == [payload], "the adapter encoded the decoy, not record()"

        verdict = _custom(batch.records).simulate
        assert isinstance(verdict, SimulateVerdict.GasUnprofitable), (
            "the decoy committed, and its evidence is the one bit its own flag moved"
        )
        assert verdict.receipt.gross_profit == DECOY_FLAG_DELTA, (
            "the only movement is decoy_flag(): last_value() did not move"
        )
        assert verdict.receipt.gross_profit != MARKER

        assert _custom_submits(batch) == [], "a wrong selector never reaches the submit plan"
