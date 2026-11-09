"""The Python payload adapter's declared interface and validation cases (ADR-100).

`PayloadAdapter` carries an optional `interface=` declaration — standard ABI JSON
plus an optional selector allow-list, target, and native-value policy — and an
explicit `check_cases(...)` run over path/amount literals. This module pins the
Python end of that surface:

* **Construction is pure data validation.** A malformed declaration (bad ABI
  JSON, an empty surface, a duplicate selector, an unknown allow-listed
  signature, an invalid value policy, a target that disagrees with the binding's
  own, a probe to the adapter's own contract the declaration does not describe,
  or an unknown key at all) is a `ValueError` naming the problem — and the
  compose callable is never invoked to find out.
* **Validation runs on every encode.** The declaration is attached to the
  pyclass's own encode, so calldata the declaration does not describe (an
  undeclared selector, argument bytes that do not decode against the declared
  types, a native value the declared policy refuses) is a typed violation, and
  the case run reports it as a failure rather than an excused decline.
* **A case hands the encoder a real path and real amounts.** Each `hops` literal
  becomes a real `HopInfo` variant (V2/V3/V4, every per-family field) and each
  `amounts` literal a real `ComposerInputs`, so the callable receives the seam's
  own `SolveResult` view — not a `SimpleNamespace` stand-in — and no chain is
  involved: the case path takes no provider, and the counting provider below is
  never reached.
* **A malformed case is refused before anything is encoded**, so an arity error
  or an out-of-bound amount cannot masquerade as an adapter defect.

The production-path test drives a real batch executor over the offline engine
fixture with a dead-port provider: an interface violation is a hard pre-encode
failure, so no provider call can occur and no submit row exists for it.
"""

from __future__ import annotations

import ast
import asyncio
import inspect
import pathlib
from typing import Any

import pytest

from degenbot.abi import encode as abi_encode
from degenbot.arbitrage.engine_registry import ArbitrageEngine, EngineRegistry
from degenbot.constants import ZERO_ADDRESS
from degenbot.crypto import function_selector, keccak256
from degenbot.dispatch import (
    BatchExecutor,
    Dispatcher,
    SessionConsumer,
    SimulateContext,
    SimulateVerdict,
    SimulationDeployment,
    TxSigner,
    builtin_deployment_id,
    typed_submit_record,
)
from tests.helpers.adapter_interface import (
    BuilderComposer,
    CountingComposer,
    CountingProvider,
    DecliningComposer,
    FailingComposer,
)
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import (
    AlloyProvider,
    AsyncAlloyProvider,
    Bot,
    CaseReport,
    PayloadAdapter,
    UnsupportedPath,
    abi_encode_call,
    build_batch_executor_py,
)
from tests.helpers.v4_pool_factory import make_v4_pool

# ── The foreign contract the adapter binds ───────────────────────────────────

EXECUTE = "execute(uint256,uint256,uint256[])"
RESCUE_SIGNATURE = "rescue()"
DECOY = "decoy()"

#: The foreign executor's own address — the transaction target.
EXECUTOR = "0x7777777777777777777777777777777777777777"

#: A second contract, used to prove a declared target must match the binding.
OTHER_EXECUTOR = "0x00000000000000000000000000000000000000c0"

#: The operator account the fixture signer holds (key ``0x11..11``).
OWNER = "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"

#: The built-in settlement executor (code-less in this fixture).
BUILTIN_EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"

#: A dead port: the production-path sim fails before any provider call, so a
#: dial would fail loudly instead of reaching a chain.
_DEAD_RPC_URL = "http://127.0.0.1:1"

DEPLOYMENT_ID = 2

#: A second, healthy foreign target: one walk may examine both.
HEALTHY_DEPLOYMENT_ID = 3
HEALTHY_EXECUTOR = "0x00000000000000000000000000000000000000e1"

GAS_LIMIT = 1_000_000


def _function(name: str, inputs: tuple[tuple[str, str], ...] = ()) -> dict[str, Any]:
    """One standard ABI JSON function entry."""
    return {
        "type": "function",
        "name": name,
        "stateMutability": "nonpayable",
        "inputs": [{"name": param, "type": ty} for param, ty in inputs],
        "outputs": [{"name": "", "type": "bool"}],
    }


def _execute_function() -> dict[str, Any]:
    """The foreign contract's `execute(uint256,uint256,uint256[])` entry."""
    return _function(
        "execute",
        (
            ("optimal_input", "uint256"),
            ("final_output", "uint256"),
            ("hop_outputs", "uint256[]"),
        ),
    )


#: A standard ABI JSON array, with the non-function entries a real toolchain
#: emits (accepted and ignored by the declaration).
EXECUTOR_ABI: list[dict[str, Any]] = [
    _execute_function(),
    _function("rescue"),
    {"type": "constructor", "inputs": [], "stateMutability": "nonpayable"},
    {"type": "event", "name": "Executed", "inputs": [], "anonymous": False},
]


def _interface(**overrides: Any) -> dict[str, Any]:
    """A declaration over `EXECUTOR_ABI`, narrowed by the keyword overrides."""
    declaration: dict[str, Any] = {"abi": EXECUTOR_ABI, "selectors": [EXECUTE]}
    declaration.update(overrides)
    return declaration


def compose_execute(result: Any) -> bytes:
    """The documented Encode blob: one `execute` call for the foreign contract."""
    hop_outputs = list(result.hop_outputs)
    return abi_encode_call(EXECUTE, [result.optimal_input, hop_outputs[-1], hop_outputs])


def _v2_hop(
    pool: str,
    token0: str,
    token1: str,
    fee: int,
    zfo: bool,  # noqa: FBT001 - the seam's own direction field is a bool
) -> dict[str, Any]:
    """One V2 hop literal — the full `HopInfo` shape for the family."""
    return {
        "family": "v2",
        "pool_address": pool,
        "token0": token0,
        "token1": token1,
        "fee": fee,
        "zfo": zfo,
    }


def _v3_hop(
    pool: str,
    token0: str,
    token1: str,
    fee: int,
    zfo: bool,  # noqa: FBT001
) -> dict[str, Any]:
    """One V3 hop literal — the full `HopInfo` shape for the family."""
    return {
        "family": "v3",
        "pool_address": pool,
        "token0": token0,
        "token1": token1,
        "fee": fee,
        "zfo": zfo,
    }


def _v4_hop(
    pool_id_hex: str,
    currency0: str,
    currency1: str,
    fee: int,
    tick_spacing: int,
    hook: str,
    zfo: bool,  # noqa: FBT001
) -> dict[str, Any]:
    """One V4 hop literal — the full `HopInfo` shape for the family."""
    return {
        "family": "v4",
        "pool_manager_address": POOL_MANAGER,
        "pool_id_hex": pool_id_hex,
        "currency0": currency0,
        "currency1": currency1,
        "fee": fee,
        "tick_spacing": tick_spacing,
        "hook_address": hook,
        "zfo": zfo,
    }


POOL_A = "0x1111111111111111111111111111111111111111"
POOL_B = "0x2222222222222222222222222222222222222222"
V4_POOL_ID = "0x" + "ab" * 32
V4_HOOK = "0x00000000000000000000000000000000000000f0"

#: Two V2 hops whose amounts mirror the shipped sample's recorded corpus.
TWO_HOP_HOPS = [
    _v2_hop(POOL_A, WETH, USDC, 30, True),
    _v2_hop(POOL_B, USDC, WETH, 30, False),
]
TWO_HOP_AMOUNTS: dict[str, Any] = {
    "optimal_input": 10**18,
    "hop_outputs": [10**18, 121 * 10**16],
    "consumed_inputs": [10**18, 10**18],
}

#: The calldata the `execute` blob produces for `TWO_HOP_AMOUNTS`, built
#: independently of the case machinery (the same helper the adapter uses).
TWO_HOP_CALLDATA = abi_encode_call(
    EXECUTE,
    [10**18, 121 * 10**16, [10**18, 121 * 10**16]],
)

#: One V2/V3/V4 path: the heterogeneous shape a real solver hands the encoder.
MIXED_HOPS = [
    _v2_hop(POOL_A, WETH, USDC, 30, True),
    _v3_hop(POOL_B, USDC, WETH, 500, False),
    _v4_hop(V4_POOL_ID, WETH, USDC, 3000, 60, V4_HOOK, True),
]
MIXED_AMOUNTS: dict[str, Any] = {
    "optimal_input": 10**18,
    "hop_outputs": [10**18, 2 * 10**18, 4 * 10**18],
    "consumed_inputs": [10**18, 2 * 10**18, 4 * 10**18],
}
MIXED_CALLDATA = abi_encode_call(
    EXECUTE,
    [10**18, 4 * 10**18, [10**18, 2 * 10**18, 4 * 10**18]],
)


def transaction_expectation(calldata: bytes) -> dict[str, Any]:
    """A `transaction` expectation pinned to one exact calldata golden."""
    return {"kind": "transaction", "selector": EXECUTE, "calldata": "0x" + calldata.hex()}


def _case(
    *,
    name: str = "two_hop_v2_execute",
    hops: list[dict[str, Any]] | None = None,
    amounts: dict[str, Any] | None = None,
    expect: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """One case document over the two-V2-hop defaults."""
    return {
        "name": name,
        "hops": TWO_HOP_HOPS if hops is None else hops,
        "amounts": TWO_HOP_AMOUNTS if amounts is None else amounts,
        "expect": transaction_expectation(TWO_HOP_CALLDATA) if expect is None else expect,
    }


# ── Construction: pure data validation, no callback, no chain ────────────────


class TestConstruction:
    """`interface=` is validated as data; nothing about it runs a callback."""

    def test_payload_adapter_accepts_an_interface_declaration(self) -> None:
        """A conforming declaration attaches, and the adapter encodes as before."""
        composer = CountingComposer(TWO_HOP_CALLDATA)
        adapter = PayloadAdapter(
            composer,
            to=EXECUTOR,
            value=0,
            probes=[],
            min_net_profit=0,
            interface=_interface(value={"kind": "zero"}),
        )

        assert composer.called is False, "construction invoked the compose callable"
        assert adapter.to == EXECUTOR
        assert adapter.value == 0
        assert adapter.probes == []
        assert adapter.min_net_profit == 0
        # The declaration is checked, never republished: the binding's surface is
        # the transaction data it was constructed with, and the declaration is
        # reached only through the behaviour it guards.
        with pytest.raises(AttributeError):
            adapter.interface

        # The declared adapter's encode is unchanged: the case's golden is the
        # callable's own payload, built without the case machinery.
        reports = adapter.check_cases([_case()])

        assert [report.name for report in reports] == ["two_hop_v2_execute"]
        assert all(isinstance(report, CaseReport) for report in reports)
        assert [report.passed for report in reports] == [True], reports
        assert reports[0].violations == []
        assert composer.calls == 1, "a case run encodes exactly once per case"

    @pytest.mark.parametrize(
        ("declaration", "message"),
        [
            ({"abi": "not an abi"}, "is not a standard ABI array"),
            (
                {"abi": [{"type": "constructor", "inputs": [], "stateMutability": "nonpayable"}]},
                "contains no function",
            ),
            ({"abi": [_execute_function(), _execute_function()]}, "share selector"),
            (
                {"abi": [_execute_function()], "selectors": [RESCUE_SIGNATURE]},
                "is not declared by the ABI",
            ),
            (
                {"abi": [_execute_function()], "value": {"kind": "sometimes"}},
                "is not a value-policy kind",
            ),
            ({"abi": [_execute_function()], "value": {"kind": "max"}}, "needs a `max_wei`"),
            (
                {"abi": [_execute_function()], "value": {"kind": "zero", "maximum": "1"}},
                "declares no `maximum` key",
            ),
            (
                {"abi": [_execute_function()], "value": {"kind": "exact", "exact_wei": 5}},
                "must be a decimal string",
            ),
            (
                {"abi": [_execute_function()], "target": OTHER_EXECUTOR},
                "one binding encodes to one address",
            ),
            ({"abi": [_execute_function()], "schema": "v1"}, "declares no `schema` key"),
        ],
    )
    def test_payload_adapter_refuses_a_malformed_interface(
        self,
        declaration: dict[str, Any],
        message: str,
    ) -> None:
        """Every unusable declaration is a `ValueError` naming the problem."""
        composer = CountingComposer(TWO_HOP_CALLDATA)

        with pytest.raises(ValueError, match=message):
            PayloadAdapter(composer, to=EXECUTOR, interface=declaration)

        assert composer.called is False, "a refused declaration must not run the callback"

    def test_a_declared_probe_to_the_adapters_own_contract_must_be_declared(self) -> None:
        """A self-read probe is checked against the declaration; WETH is left alone."""
        undeclared_self_read = {
            "label": "SELF",
            "to": EXECUTOR,
            "data": abi_encode_call(DECOY, []),
        }
        with pytest.raises(ValueError, match="declared probe"):
            PayloadAdapter(
                CountingComposer(TWO_HOP_CALLDATA),
                to=EXECUTOR,
                probes=[undeclared_self_read],
                interface=_interface(),
            )

        # A probe to a *different* contract is not this interface's business: the
        # same declaration accepts the WETH read untouched.
        weth_read = {
            "label": "WETH",
            "to": WETH,
            "data": abi_encode_call("balanceOf(address)", [EXECUTOR]),
        }
        adapter = PayloadAdapter(
            CountingComposer(TWO_HOP_CALLDATA),
            to=EXECUTOR,
            probes=[weth_read],
            interface=_interface(),
        )
        assert len(adapter.probes) == 1

    def test_registering_a_validated_adapter_invokes_no_callback(self) -> None:
        """A declared adapter inside a deployment is still pure until it encodes."""
        composer = CountingComposer(TWO_HOP_CALLDATA)
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        deployment = SimulationDeployment(
            DEPLOYMENT_ID,
            adapter,
            OWNER,
            EXECUTOR,
            GAS_LIMIT,
        )

        assert composer.called is False, (
            "registering a validated adapter is data validation: it never calls the encoder"
        )
        assert deployment is not None

    def test_check_cases_requires_a_declared_interface(self) -> None:
        """Tier 2 sits above tier 1: an undeclared adapter refuses the run."""
        composer = CountingComposer(TWO_HOP_CALLDATA)
        adapter = PayloadAdapter(composer, to=EXECUTOR)

        with pytest.raises(ValueError, match="needs a declared `interface`"):
            adapter.check_cases([_case()])

        assert composer.called is False, "the refusal happens before any encode"


# ── Tier 1: the declaration checks every encode ──────────────────────────────


class TestPerEncodeValidation:
    """Calldata the declaration does not describe is a typed failure."""

    def _adapter(self, payload: bytes, **interface: Any) -> PayloadAdapter:
        """A declared adapter over one fixed payload."""
        return PayloadAdapter(
            CountingComposer(payload),
            to=EXECUTOR,
            interface=_interface(**interface),
        )

    def test_an_undeclared_selector_is_refused(self) -> None:
        """A payload for another function never reaches the seam as a transaction."""
        payload = abi_encode_call(DECOY, [])
        reports = self._adapter(payload).check_cases([
            _case(expect=transaction_expectation(payload))
        ])

        assert [report.passed for report in reports] == [False]
        assert any(
            "is not declared by the interface" in violation for violation in reports[0].violations
        ), reports[0].violations

    def test_the_selector_allow_list_narrows_the_declared_surface(self) -> None:
        """A function the ABI declares but the allow-list omits is still refused."""
        payload = abi_encode_call(RESCUE_SIGNATURE, [])
        reports = self._adapter(payload).check_cases([
            _case(expect=transaction_expectation(payload))
        ])

        assert [report.passed for report in reports] == [False]
        assert any(
            "is not declared by the interface" in violation for violation in reports[0].violations
        ), reports[0].violations

    def test_argument_bytes_that_do_not_decode_are_refused(self) -> None:
        """A truncated dynamic-array payload is not the declared call."""
        payload = TWO_HOP_CALLDATA[:-32]
        assert payload[:4] == function_selector(EXECUTE), "the selector stays the declared one"

        reports = self._adapter(payload).check_cases([
            _case(expect=transaction_expectation(payload))
        ])

        assert [report.passed for report in reports] == [False]
        assert any("arguments do not decode" in violation for violation in reports[0].violations), (
            reports[0].violations
        )

    @pytest.mark.parametrize(
        ("policy", "passes", "message"),
        [
            pytest.param(None, True, None, id="omitted-is-any"),
            ({"kind": "any"}, True, None),
            ({"kind": "exact", "exact_wei": "5"}, True, None),
            ({"kind": "zero"}, False, "violates the `zero` policy"),
            ({"kind": "max", "max_wei": "4"}, False, "violates the `max` policy"),
        ],
    )
    def test_the_declared_native_value_policy_checks_the_bound_value(
        self,
        policy: dict[str, Any] | None,
        passes: bool,  # noqa: FBT001
        message: str | None,
    ) -> None:
        """The three value arms, against the value the binding stamps."""
        overrides: dict[str, Any] = {} if policy is None else {"value": policy}
        adapter = PayloadAdapter(
            CountingComposer(TWO_HOP_CALLDATA),
            to=EXECUTOR,
            value=5,
            interface=_interface(**overrides),
        )

        reports = adapter.check_cases([_case()])

        assert [report.passed for report in reports] == [passes]
        if message is not None:
            assert any(message in violation for violation in reports[0].violations), reports[
                0
            ].violations


# ── Tier 2: explicit cases over real paths and real amounts ──────────────────


class TestValidationCases:
    """A case is a real `PathInfo` + `ComposerInputs`, run offline."""

    def test_a_case_hands_the_encoder_a_real_path_and_real_amounts(self) -> None:
        """Every family reaches the callable as its real `HopInfo` variant."""
        composer = BuilderComposer(compose_execute)
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([
            _case(
                name="mixed_family_execute",
                hops=MIXED_HOPS,
                amounts=MIXED_AMOUNTS,
                expect=transaction_expectation(MIXED_CALLDATA),
            )
        ])

        assert [report.passed for report in reports] == [True], [
            report.violations for report in reports
        ]
        assert composer.calls == 1

        view = composer.views[0]
        descriptors = view.hop_descriptors
        assert [descriptor["family"] for descriptor in descriptors] == ["v2", "v3", "v4"]
        assert [descriptor["pool_address"].lower() for descriptor in descriptors] == [
            POOL_A,
            POOL_B,
            POOL_MANAGER.lower(),
        ]
        assert [descriptor["zfo"] for descriptor in descriptors] == [True, False, True]
        assert descriptors[2]["v4_pool_id"] == V4_POOL_ID
        assert set(descriptors[2]) == {
            "family",
            "pool_address",
            "token0",
            "token1",
            "zfo",
            "v4_pool_id",
        }, "the view carries the seam's own report-descriptor field set"
        assert [descriptor["v4_pool_id"] for descriptor in descriptors[:2]] == [None, None]
        assert view.optimal_input == MIXED_AMOUNTS["optimal_input"]
        assert list(view.hop_outputs) == MIXED_AMOUNTS["hop_outputs"]
        assert list(view.consumed_inputs) == MIXED_AMOUNTS["consumed_inputs"]
        assert view.hop_count == len(MIXED_HOPS)

    def test_a_mismatched_expectation_names_the_case_and_the_diff(self) -> None:
        """A decodable but different call is a named case mismatch, not a pass."""
        reports = PayloadAdapter(
            CountingComposer(TWO_HOP_CALLDATA),
            to=EXECUTOR,
            interface=_interface(),
        ).check_cases([
            _case(
                expect={
                    "kind": "transaction",
                    "selector": EXECUTE,
                    "arguments": [10**18, 121 * 10**16, [10**18, 1]],
                }
            )
        ])

        assert [report.passed for report in reports] == [False]
        rendered = reports[0].violations
        assert any("`two_hop_v2_execute`" in violation for violation in rendered), rendered
        assert any("arguments" in violation for violation in rendered), rendered

    def test_a_decline_expectation_rejects_a_produced_transaction(self) -> None:
        """A producing adapter cannot satisfy `expect.kind == "decline"`."""
        reports = PayloadAdapter(
            CountingComposer(TWO_HOP_CALLDATA),
            to=EXECUTOR,
            interface=_interface(),
        ).check_cases([_case(expect={"kind": "decline"})])

        assert [report.passed for report in reports] == [False]
        assert any("expected a decline" in violation for violation in reports[0].violations), (
            reports[0].violations
        )

    def test_a_case_that_raises_is_reported_never_excused(self) -> None:
        """A broken encoder is a reported failure, whatever the case expected."""
        composer = FailingComposer("the adapter refuses this path")
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([_case(), _case(name="second", expect={"kind": "decline"})])

        assert [report.name for report in reports] == ["two_hop_v2_execute", "second"]
        assert [report.passed for report in reports] == [False, False]
        assert all(report.violations for report in reports)
        assert composer.calls == 2

    @pytest.mark.parametrize(
        ("case", "message"),
        [
            pytest.param(
                _case(hops=[{**_v2_hop(POOL_A, WETH, USDC, 30, True), "family": "v5"}]),
                "names no pool family",
                id="unknown-family",
            ),
            pytest.param(
                _case(
                    hops=MIXED_HOPS,
                    amounts={
                        "optimal_input": 1,
                        "hop_outputs": [1, 2],
                        "consumed_inputs": [1, 2, 3],
                    },
                ),
                "entries for 3 hops",
                id="amount-arity",
            ),
            pytest.param(
                _case(
                    amounts={
                        "optimal_input": 2**127,
                        "hop_outputs": [1, 2],
                        "consumed_inputs": [1, 2],
                    }
                ),
                "int128 bound",
                id="amount-bound",
            ),
            pytest.param(
                _case(expect={"kind": "sometimes"}),
                "names no expectation",
                id="unknown-expect-kind",
            ),
            pytest.param(
                _case(
                    expect={
                        "kind": "transaction",
                        "selector": EXECUTE,
                        "arguments": [],
                        "calldata": "0x",
                    }
                ),
                "mutually",
                id="both-call-forms",
            ),
            pytest.param(
                {"hops": TWO_HOP_HOPS, "amounts": TWO_HOP_AMOUNTS, "expect": {"kind": "decline"}},
                "needs a `name`",
                id="missing-name",
            ),
            pytest.param(
                _case(
                    hops=[{**_v2_hop(POOL_A, WETH, USDC, 30, True), "token_0": WETH}],
                ),
                "must declare exactly",
                id="misspelled-hop-field",
            ),
            pytest.param(
                _case(
                    expect={
                        "kind": "transaction",
                        "selector": EXECUTE,
                        "arguments": [10**18, 1],
                    }
                ),
                "values for the 3 parameters",
                id="argument-arity",
            ),
            pytest.param(
                _case(
                    expect={
                        "kind": "transaction",
                        "selector": "execute(nope)",
                        "calldata": "0x",
                    }
                ),
                "declares an unresolvable parameter type",
                id="unresolvable-parameter-type",
            ),
        ],
    )
    def test_a_malformed_case_is_refused_before_any_encode_runs(
        self,
        case: dict[str, Any],
        message: str,
    ) -> None:
        """A malformed document is data, refused with zero encodes."""
        composer = CountingComposer(TWO_HOP_CALLDATA)
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        with pytest.raises(ValueError, match=message):
            adapter.check_cases([_case(name="good"), case])

        assert composer.called is False, "the whole document is checked before the first encode"

    def test_case_validation_takes_no_provider_and_reads_no_chain(self) -> None:
        """The case path has no provider parameter, and no provider is reached."""
        parameters = inspect.signature(PayloadAdapter.check_cases).parameters
        assert set(parameters) == {"self", "cases"}, parameters
        assert "provider" not in parameters

        provider = CountingProvider()
        composer = BuilderComposer(compose_execute)
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([_case()])

        assert [report.passed for report in reports] == [True], reports[0].violations
        assert provider.calls == 0, "the case path reached a provider"
        assert composer.calls == 1


# ── The routine decline signal ───────────────────────────────────────────────


class NoSuchShape(UnsupportedPath):
    """A consumer's own narrower decline: the same routine capability refusal."""


class TestUnsupportedPathDecline:
    """`UnsupportedPath` is the callback's routine decline, never a validation failure.

    The seam's `ComposeError::Unsupported` is the adapter's own capability
    decision, and `ComposeError::Encode` is its unrecoverable failure. Python
    spells the first by raising `UnsupportedPath` and the second by raising
    anything else, so `expect.kind == "decline"` is a case a Python adapter can
    satisfy — and a broken encoder is still never excused.
    """

    def test_a_declining_callback_satisfies_a_decline_expectation(self) -> None:
        """`expect.kind == "decline"` is expressible from Python, and it passes."""
        composer = DecliningComposer("this contract cannot encode a 3-hop path")
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([_case(expect={"kind": "decline"})])

        assert [report.passed for report in reports] == [True], [
            report.violations for report in reports
        ]
        assert reports[0].violations == []
        assert composer.calls == 1, "the decline is the callable's own answer"

    def test_a_decline_subclass_is_still_a_decline(self) -> None:
        """The classification is `isinstance`, exactly like `except UnsupportedPath`."""

        def decline(_result: Any) -> bytes:
            message = "no producer for this shape"
            raise NoSuchShape(message)

        adapter = PayloadAdapter(BuilderComposer(decline), to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([_case(expect={"kind": "decline"})])

        assert [report.passed for report in reports] == [True], reports[0].violations

    def test_a_declining_callback_fails_a_transaction_expectation(self) -> None:
        """A decline is not a pass: the case expected a transaction."""
        composer = DecliningComposer("this contract cannot encode a 3-hop path")
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([_case()])

        assert [report.passed for report in reports] == [False]
        assert any(
            "this contract cannot encode a 3-hop path" in violation
            for violation in reports[0].violations
        ), reports[0].violations

    def test_an_ordinary_exception_is_a_hard_encode_failure(self) -> None:
        """Only `UnsupportedPath` declines; every other exception keeps the hard arm."""
        composer = FailingComposer("the adapter is broken")
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        reports = adapter.check_cases([_case(), _case(name="declined", expect={"kind": "decline"})])

        assert [report.passed for report in reports] == [False, False]
        assert all(
            any("encode failure" in violation for violation in report.violations)
            for report in reports
        ), [report.violations for report in reports]


# ── The generated stub carries the surface ───────────────────────────────────

_STUB = pathlib.Path(__file__).resolve().parents[2] / "src" / "degenbot" / "_ffi" / "execution.pyi"


class TestGeneratedStub:
    """The declared-interface surface reaches the committed stub."""

    def test_the_interface_surface_is_in_the_generated_stub(self) -> None:
        """`just gen-stubs` output carries the keyword, the method, and the report."""
        stub = _STUB.read_text(encoding="utf-8")
        classes = {
            node.name: node for node in ast.parse(stub).body if isinstance(node, ast.ClassDef)
        }

        assert "CaseReport" in classes, sorted(classes)
        report = classes["CaseReport"]
        assert {"name", "passed", "violations"} <= {
            node.name for node in report.body if isinstance(node, ast.FunctionDef)
        }

        assert "PayloadAdapter" in classes
        adapter = classes["PayloadAdapter"]
        assert "check_cases" in {
            node.name for node in adapter.body if isinstance(node, ast.FunctionDef)
        }

        constructor = next(
            node
            for node in adapter.body
            if isinstance(node, ast.FunctionDef) and node.name == "__new__"
        )
        arguments = [argument.arg for argument in constructor.args.args]
        assert arguments[-1] == "interface", arguments
        assert [ast.literal_eval(default) for default in constructor.args.defaults][-1] is None, (
            "the `interface` declaration is an optional keyword"
        )

    def test_the_decline_exception_is_in_the_generated_stub(self) -> None:
        """The typed decline is a public class of the module, exported with its peers."""
        module = ast.parse(_STUB.read_text(encoding="utf-8"))
        classes = {node.name: node for node in module.body if isinstance(node, ast.ClassDef)}

        assert "UnsupportedPath" in classes, sorted(classes)
        decline = classes["UnsupportedPath"]
        assert [ast.unparse(base) for base in decline.bases] == ["Exception"], (
            "the decline subclasses nothing narrower: a routine signal, not a refusal class"
        )

        exported: set[str] = set()
        for node in module.body:
            if isinstance(node, ast.Assign) and any(
                getattr(target, "id", None) == "__all__" for target in node.targets
            ):
                exported = {ast.literal_eval(element) for element in node.value.elts}
        assert "UnsupportedPath" in exported, sorted(exported)


# ── Classification through the production path ───────────────────────────────


def _block_event(number: int, *, base_fee_per_gas: int = 1_000_000_000) -> dict[str, Any]:
    """One engine stream header (the `block` arm)."""
    return {
        "kind": "block",
        "number": number,
        "timestamp": 1_700_000_000,
        "base_fee_per_gas": base_fee_per_gas,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _batch_event(solve_block: int, *, rows: tuple[tuple[Any, ...], ...]) -> dict[str, Any]:
    """One engine stream batch (the `batch` arm) carrying the solver rows."""
    return {
        "kind": "batch",
        "solve_block": solve_block,
        "timestamp": 0,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 0,
        "gas_limit": 0,
        "fresh": list(rows),
        "updated": [],
        "expired": [],
        "removed": [],
        "payloads": {},
    }


def _solver_row(path_id: int, *, hops: int = 2) -> tuple[Any, ...]:
    """One engine solve row — the stream's six-tuple, one step per hop."""
    amounts = (10**15,) * hops
    return (path_id, 10**15, 1, amounts, amounts, (0,) * hops)


def _executor(
    engine: ArbitrageEngine,
    deployments: list[SimulationDeployment],
    *,
    target_selection: str = "all_targets",
) -> BatchExecutor:
    """The session executor over the declared foreign deployments, offline."""
    provider = AsyncAlloyProvider(AlloyProvider(_DEAD_RPC_URL))
    return build_batch_executor_py(
        context=SimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=BUILTIN_EXECUTOR,
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
        deployments=deployments,
        target_selection=target_selection,
    )


async def _one_batch(
    executor: BatchExecutor, *, path_id: int, block: int = 100, hops: int = 2
) -> Any:
    """Drive one scripted batch (one solved path) through the real lane."""
    consumer = SessionConsumer.scripted(
        executor,
        [_block_event(block), _batch_event(block, rows=(_solver_row(path_id, hops=hops),))],
    )
    await consumer.run()
    batch = await executor.next_outcome()
    assert batch is not None, "the session consumed its one batch"
    assert await executor.next_outcome() is None
    return batch


def _bucket(record: Any) -> str | None:
    """The failure bucket a record's verdict carries, when it carries one."""
    verdict = record.simulate
    if isinstance(verdict, SimulateVerdict.Failed):
        return verdict.detail.bucket
    return None


def _v4_pool_id(currency0: str, currency1: str, fee: int, tick_spacing: int, hook: str) -> str:
    """A V4 pool's identity: keccak of its ABI-encoded pool key."""
    key = abi_encode(
        ["address", "address", "uint24", "int24", "address"],
        [currency0, currency1, fee, tick_spacing, hook],
    )
    return "0x" + keccak256(key).hex()


@pytest.fixture
def unsupported_shape_engine_and_path() -> tuple[ArbitrageEngine, int]:
    """A real engine + a registered four-hop all-V4 path the built-in declines.

    The built-in `cmd_executor` grammar produces the any-N all-V2 family and the
    2/3-hop mixes; a four-hop all-V4 path has no producer, so the built-in
    deployment answers `ComposeError::Unsupported` — a routine capability
    decline. That is what makes the walk's decline arm observable from Python:
    over the two-hop V2 fixture the built-in accepts composition and the
    first-compatible walk stops before any foreign adapter is examined.

    Offline: `make_erc20` + `make_v4_pool` build the pools in process, and the
    scripted solver row supplies the amounts, so the path never has to solve.
    """
    py_bot = Bot()
    weth = make_erc20(py_bot, WETH, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18)
    usdc = make_erc20(py_bot, USDC, chain_id=1, name="USD Coin", symbol="USDC", decimals=6)
    registry = EngineRegistry(bot=None, engine=ArbitrageEngine(py_bot=py_bot))
    pools = []
    for token0, token1, fee, tick_spacing in (
        (weth, usdc, 500, 10),
        (usdc, weth, 3000, 60),
        (weth, usdc, 100, 1),
        (usdc, weth, 10_000, 200),
    ):
        pool = make_v4_pool(
            pool_id=_v4_pool_id(token0.address, token1.address, fee, tick_spacing, ZERO_ADDRESS),
            pool_manager_address=POOL_MANAGER,
            token0=token0,
            token1=token1,
            fee=fee,
            tick_spacing=tick_spacing,
            hook_address=ZERO_ADDRESS,
            sqrt_price_x96=2_198_666_895_605_149_686_863,
            tick=-76020,
            liquidity=9_876_543_210,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500_000,
            state_block=18_000_000,
            py_bot=py_bot,
        )
        asyncio.run(registry.register_v4_pool(pool))
        pools.append(pool)
    path_id, _created = registry.register_path([(pool, True) for pool in pools])
    return registry.engine, path_id


class TestProductionPathClassification:
    """A violation is the candidate's own hard failure, never a decline."""

    async def test_an_interface_violation_reaches_the_verdict_as_an_encode_error(
        self,
        nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
    ) -> None:
        """The batch records the pre-encode failure and plans no submission."""
        engine, path_id = nxm2bf_v2_engine_and_path
        composer = CountingComposer(abi_encode_call(DECOY, []))
        adapter = PayloadAdapter(composer, to=EXECUTOR, interface=_interface())

        deployment = SimulationDeployment(DEPLOYMENT_ID, adapter, OWNER, EXECUTOR, GAS_LIMIT)
        batch = await _one_batch(_executor(engine, [deployment]), path_id=path_id)

        assert composer.calls == 1, "the adapter encoded once for the candidate"
        record = {rec.candidate_id.deployment_id: rec for rec in batch.records}[DEPLOYMENT_ID]
        assert isinstance(record.simulate, SimulateVerdict.Exception), (
            "a declared-interface violation is the candidate's own unrecoverable failure, "
            f"never a simulated verdict: {record.simulate!r}"
        )
        assert record.submit is None, "a violating candidate never reaches the submit lane"

        submitted = [typed_submit_record(raw) for raw in batch.submit_records]
        assert [rec for rec in submitted if rec.candidate.deployment_id == DEPLOYMENT_ID] == [], (
            "no submit row exists for a violating candidate"
        )

    async def test_a_violation_does_not_retire_the_sibling_deployment(
        self,
        nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
    ) -> None:
        """Each declared deployment is classified on its own candidate."""
        engine, path_id = nxm2bf_v2_engine_and_path
        violating = CountingComposer(abi_encode_call(DECOY, []))
        healthy = BuilderComposer(compose_execute)

        executor = _executor(
            engine,
            [
                SimulationDeployment(
                    DEPLOYMENT_ID,
                    PayloadAdapter(
                        violating,
                        to=EXECUTOR,
                        interface=_interface(),
                    ),
                    OWNER,
                    EXECUTOR,
                    GAS_LIMIT,
                ),
                SimulationDeployment(
                    HEALTHY_DEPLOYMENT_ID,
                    PayloadAdapter(
                        healthy,
                        to=HEALTHY_EXECUTOR,
                        interface=_interface(),
                    ),
                    OWNER,
                    HEALTHY_EXECUTOR,
                    GAS_LIMIT,
                ),
            ],
        )

        batch = await _one_batch(executor, path_id=path_id)
        by_deployment = {record.candidate_id.deployment_id: record for record in batch.records}

        assert violating.calls == 1, "the violating deployment encoded once"
        assert healthy.calls == 1, (
            "a declared-interface violation is the violating candidate's own hard failure: "
            "the sibling deployment still gets its own encode"
        )
        assert {DEPLOYMENT_ID, HEALTHY_DEPLOYMENT_ID} <= set(by_deployment), sorted(by_deployment)
        assert isinstance(by_deployment[DEPLOYMENT_ID].simulate, SimulateVerdict.Exception)
        assert by_deployment[DEPLOYMENT_ID].submit is None
        assert by_deployment[HEALTHY_DEPLOYMENT_ID].candidate_id.path_id == path_id

        submitted = [typed_submit_record(raw) for raw in batch.submit_records]
        assert DEPLOYMENT_ID not in {rec.candidate.deployment_id for rec in submitted}


# ── Classification across the first-compatible walk ──────────────────────────


class TestWalkClassification:
    """A decline advances the walk to the next target; a violation ends it.

    Both tests drive the same walk — the built-in deployment (which declines this
    shape), then `DEPLOYMENT_ID`, then `HEALTHY_DEPLOYMENT_ID` — so the two
    verdicts are distinguished by the production lane alone: the routine
    `ComposeError::Unsupported` (a Python `UnsupportedPath`) lets the path move
    on, and `ComposeError::Validation` stops it where it stands.
    """

    async def test_a_declined_path_leaves_the_walk_to_the_next_deployment(
        self,
        unsupported_shape_engine_and_path: tuple[ArbitrageEngine, int],
    ) -> None:
        """`UnsupportedPath` declines the target; the walk examines the next one."""
        engine, path_id = unsupported_shape_engine_and_path
        declining = DecliningComposer("this contract cannot encode a four-hop V4 path")
        healthy = BuilderComposer(compose_execute)

        executor = _executor(
            engine,
            [
                SimulationDeployment(
                    DEPLOYMENT_ID,
                    PayloadAdapter(declining, to=EXECUTOR, interface=_interface()),
                    OWNER,
                    EXECUTOR,
                    GAS_LIMIT,
                ),
                SimulationDeployment(
                    HEALTHY_DEPLOYMENT_ID,
                    PayloadAdapter(healthy, to=HEALTHY_EXECUTOR, interface=_interface()),
                    OWNER,
                    HEALTHY_EXECUTOR,
                    GAS_LIMIT,
                ),
            ],
            target_selection="first_compatible",
        )

        batch = await _one_batch(executor, path_id=path_id, hops=4)
        by_deployment = {record.candidate_id.deployment_id: record for record in batch.records}

        assert declining.calls == 1, "the declining deployment was examined"
        assert healthy.calls == 1, (
            "a routine decline is not this candidate's failure: the walk advanced past it to "
            "the next target"
        )

        # The built-in leads the registry and answers its own routine refusal for
        # this shape, so the walk had two declines to pass before the producer.
        builtin = by_deployment[builtin_deployment_id()]
        assert _bucket(builtin) == "encode-failed", (
            f"a capability refusal is the routine bucket, never an exception: {builtin.simulate!r}"
        )
        assert _bucket(by_deployment[DEPLOYMENT_ID]) == "encode-failed", (
            f"{by_deployment[DEPLOYMENT_ID].simulate!r}"
        )
        assert by_deployment[HEALTHY_DEPLOYMENT_ID].candidate_id.path_id == path_id

    async def test_a_validation_violation_ends_the_walk(
        self,
        unsupported_shape_engine_and_path: tuple[ArbitrageEngine, int],
    ) -> None:
        """`ComposeError::Validation` is hard: the walk stops at the violating target."""
        engine, path_id = unsupported_shape_engine_and_path
        violating = CountingComposer(abi_encode_call(DECOY, []))
        healthy = BuilderComposer(compose_execute)

        executor = _executor(
            engine,
            [
                SimulationDeployment(
                    DEPLOYMENT_ID,
                    PayloadAdapter(violating, to=EXECUTOR, interface=_interface()),
                    OWNER,
                    EXECUTOR,
                    GAS_LIMIT,
                ),
                SimulationDeployment(
                    HEALTHY_DEPLOYMENT_ID,
                    PayloadAdapter(healthy, to=HEALTHY_EXECUTOR, interface=_interface()),
                    OWNER,
                    HEALTHY_EXECUTOR,
                    GAS_LIMIT,
                ),
            ],
            target_selection="first_compatible",
        )

        batch = await _one_batch(executor, path_id=path_id, hops=4)
        by_deployment = {record.candidate_id.deployment_id: record for record in batch.records}

        assert violating.calls == 1, "the violating deployment was examined once"
        assert healthy.calls == 0, (
            "a declared-interface violation ends the walk: the path is not offered to the "
            "next target"
        )
        assert isinstance(by_deployment[DEPLOYMENT_ID].simulate, SimulateVerdict.Exception), (
            "the violation is the candidate's own unrecoverable failure, never the routine "
            f"decline: {by_deployment[DEPLOYMENT_ID].simulate!r}"
        )
        assert HEALTHY_DEPLOYMENT_ID not in by_deployment, sorted(by_deployment)
