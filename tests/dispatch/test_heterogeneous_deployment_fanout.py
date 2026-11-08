"""Heterogeneous execution-deployment fan-out through the Python seam (ADR-098 D4).

A Python driver declares foreign execution deployments — a deployment id, an
existing ``degenbot._ffi.execution.PayloadAdapter``, the owner, the execution
target, the gas limit, and an optional declared runtime — and hands them to the
batch executor's ONE composition boundary. Everything after that is the Rust
core's: the deployment registry, the explicit all-target fan-out, the
``CandidateId`` records, the adapters' own verdicts, and the per-target submit
join. These tests pin that the Python surface is a thin configuration
projection over it:

* ``deployments=None`` / ``deployments=[]`` is the one-entry built-in session
  with unchanged behavior;
* the core's composition refusals (a duplicate identity, an owner the session
  signer does not hold) arrive as ``ValueError``, never as a ``PanicException``;
* one solved path becomes one candidate per registered deployment — distinct
  ``CandidateId``s, independent per-target outcomes — driven through the real
  FFI batch path (``SessionConsumer.scripted`` + ``BatchExecutor.next_outcome``),
  not merely object construction.

The engine fixture is real (offline-registered V2 pools) and the sim runs for
real: ``_CannedChainNode`` answers the cold-state reads the in-process engine
issues (the same canned surface the Rust fan-out fixture's transport serves), so
nothing reaches a chain and no live endpoint is required. Every scripted batch
runs ``dry_run``, so the submit lane plans and skips without broadcasting.
"""

from __future__ import annotations

import json
import threading
from dataclasses import fields
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.dispatch import (
    AssemblyVerdict,
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
    builtin_deployment_id,
    typed_submit_record,
)
from degenbot.runner._sim_submit import ExecutorInputs
from tests.helpers.seam import (
    AlloyProvider,
    AsyncAlloyProvider,
    PayloadAdapter,
    build_batch_executor_py,
)

if TYPE_CHECKING:
    from collections.abc import Iterator

    from degenbot.arbitrage.engine_registry import ArbitrageEngine


# The operator account the fixture signer holds (key ``0x11..11``): the one
# authority every deployment's owner must name.
OWNER = "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"

# The built-in settlement executor (code-less in this fixture).
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"

# The two foreign execution targets.
TARGET_A = "0x00000000000000000000000000000000000000A1"
TARGET_B = "0x00000000000000000000000000000000000000b2"

#: A dead port — the transport never dials (the construction-only tests).
_RPC_URL = "http://127.0.0.1:1"

#: The fee the simulated block runs under (1 gwei base fee, 1 gwei priority).
_BASE_FEE = 1_000_000_000

#: The native value target A's adapter sends with its transaction (0.01 ETH).
_VALUE_A = 10**16

_GAS_LIMIT = 1_000_000

_ZERO_WORD = "0x" + "00" * 32


class _CannedChainNode:
    """A loopback JSON-RPC stub answering the cold-state reads the sim issues.

    The engine executes the sim in-process against the fixture's own state (the
    pools, tokens, and the deployment's overridden accounts) and only calls out
    for a cold miss, so the stub serves that miss surface and nothing else. It
    takes the same role as the Rust fan-out fixture's transport.
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
    """A loopback stub RPC endpoint for the sim fan-out (see `_CannedChainNode`)."""
    node = _CannedChainNode()
    try:
        yield node
    finally:
        node.close()


def _provider(rpc_url: str = _RPC_URL) -> AsyncAlloyProvider:
    return AsyncAlloyProvider(AlloyProvider(rpc_url))


def _ctx(rpc_url: str = _RPC_URL) -> SimulateContext:
    return SimulateContext(
        provider=_provider(rpc_url),
        executor_owner=OWNER,
        executor_address=EXECUTOR,
        weth_address=WETH,
        pool_manager_address=POOL_MANAGER,
        inject_code=False,
    )


def _executor(
    engine: ArbitrageEngine,
    *,
    deployments: list[SimulationDeployment] | None = None,
    dry_run: bool = True,
    rpc_url: str = _RPC_URL,
    target_selection: str | None = None,
) -> BatchExecutor:
    """Build the session executor over the offline fixture engine."""
    return build_batch_executor_py(
        context=_ctx(rpc_url),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=_provider(rpc_url),
        operator_nonce=5,
        dry_run=dry_run,
        broadcast_providers=None,
        deployments=deployments,
        target_selection=target_selection,
    )


def _adapter(*, to: str, value: int = 0) -> PayloadAdapter:
    """A Python execution adapter that encodes a fixed calldata blob."""

    def compose(_result: object) -> bytes:
        return b"\xde\xad\xbe\xef"

    return PayloadAdapter(compose, to=to, value=value)


def _deployment(
    deployment_id: int,
    *,
    target: str,
    value: int = 0,
    code: bytes | None = None,
    owner: str = OWNER,
) -> SimulationDeployment:
    return SimulationDeployment(
        deployment_id,
        _adapter(to=target, value=value),
        owner,
        target,
        _GAS_LIMIT,
        code=code,
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


def _solver_row(
    path_id: int,
    *,
    optimal_input: int = 10**15,
    hop_outputs: tuple[int, int] = (10**15, 10**15),
    consumed_inputs: tuple[int, int] = (10**15, 10**15),
) -> tuple[Any, ...]:
    """One engine solve row — the stream's six-tuple."""
    return (path_id, optimal_input, 1, hop_outputs, consumed_inputs, (0, 0))


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


def _by_deployment(records: list[BatchOutcome]) -> dict[int, BatchOutcome]:
    return {rec.candidate_id.deployment_id: rec for rec in records}


class TestSingleEntryParity:
    """The built-in session is unchanged: one deployment, one candidate."""

    def test_the_builtin_deployment_id_is_the_cores(self) -> None:
        assert builtin_deployment_id() == 1

    @pytest.mark.parametrize("deployments", [None, []])
    def test_an_absent_or_empty_deployment_list_keeps_the_one_entry_session(
        self,
        nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
        deployments: list[SimulationDeployment] | None,
    ) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine, deployments=deployments)
        assert isinstance(executor, BatchExecutor)
        assert executor.enqueued == 0

    async def test_a_registered_foreign_deployment_does_not_displace_the_builtin(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        executor = _executor(
            engine,
            deployments=[_deployment(2, target=TARGET_A, code=b"\x00")],
            # The claim is about registry order across every candidate, so the
            # session asks for the explicit fan-out.
            target_selection="all_targets",
        )
        batch = await _one_batch(executor, path_id=path_id)
        assert [rec.candidate_id.deployment_id for rec in batch.records] == [
            builtin_deployment_id(),
            2,
        ], "the built-in deployment leads the registry, then the declared order"
        assert {rec.candidate_id.path_id for rec in batch.records} == {path_id}


class TestCompositionRefusals:
    """The core's registry rules surface as ordinary Python exceptions."""

    def test_a_duplicate_identity_is_a_value_error(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        deployments = [
            _deployment(2, target=TARGET_A),
            _deployment(2, target=TARGET_B),
        ]
        with pytest.raises(ValueError, match="registered twice"):
            _executor(engine, deployments=deployments)

    def test_reusing_the_builtin_identity_is_a_value_error(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        with pytest.raises(ValueError, match="registered twice"):
            _executor(
                engine,
                deployments=[
                    _deployment(builtin_deployment_id(), target=TARGET_A),
                ],
            )

    def test_an_owner_the_signer_does_not_hold_is_a_value_error(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        stranger = "0x" + "22" * 20
        with pytest.raises(ValueError, match="cannot sign for"):
            _executor(
                engine,
                deployments=[_deployment(2, target=TARGET_A, owner=stranger)],
            )


class TestMalformedDeclarations:
    """Malformed configuration fails loudly at the declaration boundary."""

    def test_an_invalid_owner_address_is_a_value_error(self) -> None:
        with pytest.raises(ValueError, match="Invalid deployment owner"):
            _deployment(2, target=TARGET_A, owner="not-an-address")

    def test_an_invalid_target_address_is_a_value_error(self) -> None:
        with pytest.raises(ValueError, match="Invalid deployment target"):
            SimulationDeployment(
                2,
                _adapter(to=TARGET_A),
                OWNER,
                "not-an-address",
                _GAS_LIMIT,
            )

    def test_an_empty_declared_runtime_is_a_value_error(self) -> None:
        with pytest.raises(ValueError, match="non-empty runtime"):
            _deployment(2, target=TARGET_A, code=b"")

    def test_a_zero_gas_limit_is_a_value_error(self) -> None:
        with pytest.raises(ValueError, match="gas_limit must be non-zero"):
            SimulationDeployment(
                2,
                _adapter(to=TARGET_A),
                OWNER,
                TARGET_A,
                0,
            )

    def test_a_non_adapter_is_a_type_error(self) -> None:
        with pytest.raises(TypeError):
            SimulationDeployment(  # type: ignore[arg-type]
                2,
                "not-an-adapter",
                OWNER,
                TARGET_A,
                _GAS_LIMIT,
            )

    def test_the_declaration_is_readable_from_python(self) -> None:
        deployment = _deployment(3, target=TARGET_A, code=b"\x60\x00", value=7)
        assert deployment.deployment_id == 3
        assert deployment.target == TARGET_A
        assert deployment.owner == OWNER
        assert deployment.gas_limit == _GAS_LIMIT
        assert deployment.declared_code == b"\x60\x00"


class TestHeterogeneousFanOut:
    """One solved path, two foreign targets: independent records and verdicts."""

    async def test_one_solved_path_fans_out_to_each_registered_target(
        self,
        nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
        canned_chain: _CannedChainNode,
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        composed: list[bytes] = []

        def compose_a(_result: object) -> bytes:
            composed.append(b"\xa1\xa2\xa3\xa4")
            return b"\xa1\xa2\xa3\xa4"

        # Target A SUCCEEDS: its deployed runtime is declared (an undeployed
        # build), and its adapter's native-balance probe observes the value its
        # own transaction carries — so the gross figure the record reports is
        # the adapter's own assessment of target A's own transaction.
        deployment_a = SimulationDeployment(
            2,
            PayloadAdapter(
                compose_a,
                to=TARGET_A,
                value=_VALUE_A,
                probes=[{"label": "ETH", "account": TARGET_A}],
            ),
            OWNER,
            TARGET_A,
            _GAS_LIMIT,
            code=b"\x00",
        )
        # Target B REFUSES: its adapter encodes a different contract than the
        # deployment's declared target, which the engine refuses per candidate
        # (never simulated, never submitted) while target A's lane runs on.
        deployment_b = SimulationDeployment(
            3,
            PayloadAdapter(
                lambda _result: b"\xb1\xb2\xb3\xb4",
                to=TARGET_B,
            ),
            OWNER,
            "0x00000000000000000000000000000000000000c3",
            _GAS_LIMIT,
        )

        executor = _executor(
            engine,
            deployments=[deployment_a, deployment_b],
            rpc_url=canned_chain.url,
            # The explicit fan-out: every registered target is offered the path.
            target_selection="all_targets",
        )
        batch = await _one_batch(executor, path_id=path_id)

        assert [rec.candidate_id.deployment_id for rec in batch.records] == [
            builtin_deployment_id(),
            2,
            3,
        ], "one candidate per registered deployment, in registry order"
        assert {rec.candidate_id.path_id for rec in batch.records} == {path_id}, (
            "the solver path id is preserved verbatim on every target's candidate"
        )

        by_deployment = _by_deployment(batch.records)

        # Target A: its own adapter composed its own calldata for its own
        # target, its own transaction carried its own native value, and its own
        # assessment passed.
        assert composed == [b"\xa1\xa2\xa3\xa4"]
        verdict_a = by_deployment[2].simulate
        assert isinstance(verdict_a, SimulateVerdict.Profitable), (
            verdict_a.detail.record()
            if isinstance(verdict_a, SimulateVerdict.Failed)
            else verdict_a
        )
        assert verdict_a.receipt.gross_profit == _VALUE_A, (
            "the record reports the gross target A's own transaction produced"
        )

        # Target B: refused on its own record, and the refusal stayed there.
        verdict_b = by_deployment[3].simulate
        assert isinstance(verdict_b, SimulateVerdict.Failed), verdict_b
        assert verdict_b.detail.bucket == "target-mismatch"

        # The built-in settlement target still ran its own path; its outcome is
        # its own (the fixture's executor is code-less, so it has no receipt).
        builtin = by_deployment[builtin_deployment_id()]
        assert builtin.assembly is AssemblyVerdict.Assembled

        # The submit lane planned exactly ONE candidate — target A's. Target B's
        # refusal suppressed neither target A nor itself-only.
        submitted = [typed_submit_record(raw) for raw in batch.submit_records]
        assert [record.candidate.deployment_id for record in submitted] == [2]
        assert submitted[0].candidate.path_id == path_id
        assert submitted[0].reason is SubmitSkipReason.DRY_RUN


class TestTargetSelection:
    """The target-selection policy: ordered first-compatible by default, and the
    explicit all-target fan-out on request.

    The walk is the Rust core's; Python names the policy and nothing else. A
    Python adapter that a policy never examines is never called — the compose
    callable is the observable side of that claim.
    """

    def test_the_default_policy_is_ordered_first_compatible(self) -> None:
        default = {field.name: field for field in fields(ExecutorInputs)}[
            "target_selection"
        ].default
        assert default == "first_compatible", "a driver that names no policy gets the ordered walk"

    def test_an_unknown_policy_is_a_value_error(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        with pytest.raises(ValueError, match="unknown target_selection"):
            _executor(
                engine,
                deployments=[_deployment(2, target=TARGET_A)],
                target_selection="first_compat",
            )

    async def test_a_named_all_targets_policy_reaches_the_core(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        composed: list[int] = []

        def compose(_result: object) -> bytes:
            composed.append(len(composed) + 1)
            return b"\xa1\xa2\xa3\xa4"

        deployment = SimulationDeployment(
            2,
            PayloadAdapter(compose, to=TARGET_A),
            OWNER,
            TARGET_A,
            _GAS_LIMIT,
            code=b"\x00",
        )
        executor = _executor(
            engine,
            deployments=[deployment],
            target_selection="all_targets",
        )
        batch = await _one_batch(executor, path_id=path_id)

        assert composed == [1], "the explicit fan-out composes every target"
        assert [rec.candidate_id.deployment_id for rec in batch.records] == [
            builtin_deployment_id(),
            2,
        ], "one candidate per registered deployment, in registry order"

    async def test_the_default_policy_never_calls_a_later_deployment(
        self, nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        composed: list[int] = []

        def compose(_result: object) -> bytes:
            composed.append(len(composed) + 1)
            return b"\xa1\xa2\xa3\xa4"

        deployment = SimulationDeployment(
            2,
            PayloadAdapter(compose, to=TARGET_A),
            OWNER,
            TARGET_A,
            _GAS_LIMIT,
            code=b"\x00",
        )
        executor = _executor(engine, deployments=[deployment])
        batch = await _one_batch(executor, path_id=path_id)

        assert composed == [], (
            "the built-in deployment accepts composition first, so the foreign "
            "adapter is never invoked"
        )
        assert [rec.candidate_id.deployment_id for rec in batch.records] == [
            builtin_deployment_id()
        ], "only the first-compatible deployment produced a record"
        assert path_id in {rec.candidate_id.path_id for rec in batch.records}
