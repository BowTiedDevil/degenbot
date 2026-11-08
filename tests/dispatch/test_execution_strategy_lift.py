"""DelegateSpy test (ADR-025): prove the execution-strategy PyO3 lift
(``PayloadComposer`` / ``PayloadAdapter`` / ``SolveResult`` / ``abi_encode_call``)
is exposed on the Rust seam ``degenbot._ffi.execution``.

The lift adapts an arbitrary Python callable (``SolveResult -> bytes``) into the
core ``PayloadComposer`` / ``ExecutionAdapter`` seam (Polars ``map_elements``
model — Rust holds the ``Py<PyAny>`` and calls back under the GIL). It is the
foreign-contract path: nothing here is wired into the canonical
``dispatch_profitable_*`` fan-out (ADR-025 D3) — this test only pins the seam
surface + the thin-translate contract, not any strategy logic.

``PayloadAdapter`` is the production-intended route: the Encode callable bound to
the complete transaction the seam carries (``to`` + native ``value``) with its
declared probe reads and minimum net profit. The functional encode round-trip
(compose → Python callable → complete transaction) is proven Rust-side in
``crates/shells/degenbot-python/src/execution/mod.rs`` unit tests.
"""

from __future__ import annotations

from dataclasses import dataclass
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from types import ModuleType

import pytest

from degenbot.crypto import function_selector

EXAMPLE_PATH = Path(__file__).resolve().parents[2] / "examples" / "execution_strategy_foreign.py"


@dataclass(frozen=True)
class _StrategyResult:
    """The SolveResult slice the foreign example's composer reads."""

    optimal_input: int
    hop_outputs: list[int]
    consumed_inputs: list[int]


@pytest.fixture(scope="module")
def foreign_strategy_example() -> ModuleType:
    spec = spec_from_file_location("execution_strategy_foreign_example", EXAMPLE_PATH)
    if spec is None or spec.loader is None:
        message = f"cannot load public example: {EXAMPLE_PATH}"
        raise RuntimeError(message)
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _ffi_execution() -> Any:
    from tests.helpers.seam import execution

    return execution


class TestRustSeamPresent:
    """The Rust seam exposes every lift symbol as a Rust-bound builtin."""

    def test_symbols_present(self) -> None:
        execution = _ffi_execution()
        assert hasattr(execution, "PayloadComposer")
        assert hasattr(execution, "PayloadAdapter")
        assert hasattr(execution, "SolveResult")
        assert hasattr(execution, "abi_encode_call")

    def test_abi_encode_call_is_rust_bound(self) -> None:
        fn = _ffi_execution().abi_encode_call
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )


class TestPyPayloadComposerConstruction:
    """The encoding-only lift wraps a Python callable; non-callables are rejected."""

    def test_accepts_callable(self) -> None:
        composer = _ffi_execution().PayloadComposer(lambda result: b"\x00")
        assert composer is not None

    def test_rejects_non_callable(self) -> None:
        with pytest.raises(TypeError, match="callable"):
            _ffi_execution().PayloadComposer(42)


class TestPyPayloadAdapterBinding:
    """The production lift binds calldata to a complete transaction."""

    TO = "0x7777777777777777777777777777777777777777"
    WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    BALANCE_OF = bytes([0x70, 0xA0, 0x82, 0x31])

    def _balance_of(self, account: str) -> bytes:
        """`WETH.balanceOf(account)` — a COMPLETE calldata read."""
        return _ffi_execution().abi_encode_call("balanceOf(address)", [account])

    def test_binds_target_and_preserves_nonzero_value(self) -> None:
        adapter = _ffi_execution().PayloadAdapter(
            lambda result: b"\x01\x02",
            to=self.TO,
            value=5_000_000_000_000_000,
        )
        assert adapter.to.lower() == self.TO
        assert adapter.value == 5_000_000_000_000_000, "native value survives the lift"
        assert adapter.min_net_profit == 0

    def test_declares_a_complete_calldata_read(self) -> None:
        data = self._balance_of(self.TO)
        assert len(data) == 4 + 32, "selector + the ABI address argument"
        adapter = _ffi_execution().PayloadAdapter(
            lambda result: b"",
            to=self.TO,
            probes=[{"label": "WETH", "to": self.WETH, "data": data}],
            min_net_profit=1_000_000_000_000_000,
        )
        assert adapter.probes == [{"label": "WETH", "to": self.WETH, "data": data}]
        assert adapter.min_net_profit == 1_000_000_000_000_000

    def test_declares_a_native_account_read(self) -> None:
        adapter = _ffi_execution().PayloadAdapter(
            lambda result: b"",
            to=self.TO,
            probes=[{"label": "ETH", "account": self.TO}],
        )
        assert adapter.probes == [{"label": "ETH", "account": self.TO}]

    def test_declares_both_forms_in_order(self) -> None:
        data = self._balance_of(self.TO)
        declared = [
            {"label": "WETH", "to": self.WETH, "data": data},
            {"label": "ETH", "account": self.TO},
        ]
        adapter = _ffi_execution().PayloadAdapter(
            lambda result: b"",
            to=self.TO,
            probes=declared,
        )
        assert adapter.probes == declared, "declared order is preserved"

    def test_rejects_non_callable(self) -> None:
        with pytest.raises(TypeError, match="callable"):
            _ffi_execution().PayloadAdapter(42, to=self.TO)

    def test_rejects_a_short_call_data(self) -> None:
        with pytest.raises(ValueError, match="4-byte selector"):
            _ffi_execution().PayloadAdapter(
                lambda result: b"",
                to=self.TO,
                probes=[{"label": "WETH", "to": self.WETH, "data": b"\x01\x02\x03"}],
            )

    def test_rejects_the_retired_probe_triple(self) -> None:
        """The selector-only triple cannot carry a call's arguments."""
        with pytest.raises(ValueError, match="dict"):
            _ffi_execution().PayloadAdapter(
                lambda result: b"",
                to=self.TO,
                probes=[("WETH", self.WETH, self.BALANCE_OF)],
            )

    def test_rejects_a_probe_with_unknown_keys(self) -> None:
        with pytest.raises(ValueError, match="exactly one form"):
            _ffi_execution().PayloadAdapter(
                lambda result: b"",
                to=self.TO,
                probes=[{"label": "WETH", "to": self.WETH, "account": self.TO}],
            )


class TestAbiEncodeCallHelper:
    """`abi_encode_call` — the degenbot.abi-backed calldata builder."""

    def test_full_function_call_calldata(self) -> None:
        # `transfer(address,uint256)` — selector 0xa9059cbb + ABI args.
        payload = _ffi_execution().abi_encode_call(
            "transfer(address,uint256)",
            ["0x" + "00" * 20, 1],
        )
        assert isinstance(payload, bytes)
        assert len(payload) == 4 + 64, "selector + two ABI words"
        assert payload[:4] == function_selector("transfer(address,uint256)")

    def test_rejects_returns_clause(self) -> None:
        with pytest.raises(ValueError, match="returns"):
            _ffi_execution().abi_encode_call(
                "balanceOf(address) returns (uint256)",
                ["0x" + "00" * 20],
            )


class TestPythonForeignStrategySample:
    """The Python driver's foreign Encode blob, exercised end-to-end."""

    def test_foreign_encode_via_abi_helper(self, foreign_strategy_example: ModuleType) -> None:
        result = _StrategyResult(
            optimal_input=1_000_000_000_000_000_000,
            hop_outputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
            consumed_inputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
        )
        payload = foreign_strategy_example.compose_simple_executor(result)
        # ABI shape distinct from cmd_executor: `execute(uint256,uint256,uint256[])`.
        assert payload[:4] == function_selector("execute(uint256,uint256,uint256[])")
        assert len(payload) == 4 + 6 * 32, "selector + ABI head + 2 hop words"
        # dynamic uint256[] head — offset word after (selector, opt, final).
        offset = int.from_bytes(payload[68:100], "big")
        assert offset == 0x60, "ABI array offset"
        assert (
            foreign_strategy_example.SIMPLE_EXECUTOR_SIGNATURE
            == "execute(uint256,uint256,uint256[])"
        )

    def test_python_strategy_binds_a_complete_transaction(
        self, foreign_strategy_example: ModuleType
    ) -> None:
        strategy = foreign_strategy_example.build_strategy()
        executor = foreign_strategy_example.SIMPLE_EXECUTOR
        assert strategy.to.lower() == executor.lower()
        assert strategy.value == 0, "the sample executor is flash-funded"
        # The declared reads carry the executor argument / name the account, so
        # the engine observes the executor's own balances rather than an
        # argument-less selector.
        assert strategy.probes == [
            {
                "label": "WETH",
                "to": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                "data": foreign_strategy_example.weth_balance_of(executor),
            },
            {"label": "ETH", "account": executor},
        ]
        (weth_probe, native_probe) = strategy.probes
        assert len(weth_probe["data"]) == 4 + 32, "selector + the address argument"
        assert weth_probe["data"][:4] == bytes([0x70, 0xA0, 0x82, 0x31]), "balanceOf(address)"
        assert native_probe["account"] == executor

    def test_cross_layer_oracle_matches_recorded_corpus(
        self, foreign_strategy_example: ModuleType
    ) -> None:
        """The Python foreign path reproduces the SAME recorded corpus
        the Rust sample pins (byte-identical across layers), and that corpus is
        distinct from `cmd_executor`."""
        corpus = bytes.fromhex(
            "ead35cae"
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
            "00000000000000000000000000000000000000000000000010cac896d2390000"
            "0000000000000000000000000000000000000000000000000000000000000060"
            "0000000000000000000000000000000000000000000000000000000000000002"
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
            "00000000000000000000000000000000000000000000000010cac896d2390000"
        )
        result = _StrategyResult(
            optimal_input=999_999_999_999_999_999 + 1,
            hop_outputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
            consumed_inputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
        )
        payload = foreign_strategy_example.compose_simple_executor(result)
        assert payload == corpus, "Python foreign payload must match the Rust-recorded corpus"
