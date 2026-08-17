"""§4.6 / ADR-025 DelegateSpy test: prove the execution-strategy PyO3 lift
(``PayloadComposer`` / ``SolveResult`` / ``abi_encode_call``) is exposed on
the Rust seam ``degenbot._ffi.execution``.

The lift adapts an arbitrary Python callable (``SolveResult -> bytes``) into the
core ``PayloadComposer`` / ``ExecutionStrategy`` seam (Polars ``map_elements``
model — Rust holds the ``Py<PyAny>`` and calls back under the GIL). It is the
foreign-contract path: nothing here is wired into the canonical
``dispatch_profitable_*`` fan-out (ADR-025 D3) — this test only pins the seam
surface + the thin-translate contract, not any strategy logic.

The functional encode round-trip (compose → Python callable → payload bytes) is
proven Rust-side in ``crates/degenbot-python/src/execution/mod.rs`` unit tests
(``cargo test -p degenbot_rs``).
"""

from __future__ import annotations

from typing import Any

import pytest


def _ffi_execution() -> Any:
    from degenbot._ffi import execution

    return execution


class TestRustSeamPresent:
    """The Rust seam exposes every lift symbol as a Rust-bound builtin."""

    def test_symbols_present(self) -> None:
        execution = _ffi_execution()
        assert hasattr(execution, "PayloadComposer")
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
    """The lift wraps a Python callable; non-callables are rejected early."""

    def test_accepts_callable(self) -> None:
        composer = _ffi_execution().PayloadComposer(lambda result: b"\x00")
        assert composer is not None

    def test_rejects_non_callable(self) -> None:
        with pytest.raises(TypeError, match="callable"):
            _ffi_execution().PayloadComposer(42)


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
        assert payload[:4] == bytes.fromhex("a9059cbb"), "ERC20 transfer selector"

    def test_rejects_returns_clause(self) -> None:
        with pytest.raises(ValueError, match="returns"):
            _ffi_execution().abi_encode_call(
                "balanceOf(address) returns (uint256)",
                ["0x" + "00" * 20],
            )


class TestPythonForeignStrategySample:
    """OULU5O — the Python driver's foreign Encode blob, exercised end-to-end."""

    def test_foreign_encode_via_abi_helper(self) -> None:
        from types import SimpleNamespace

        from examples.execution_strategy_foreign import (
            SIMPLE_EXECUTOR_SIGNATURE,
            compose_simple_executor,
        )

        result = SimpleNamespace(
            optimal_input=1_000_000_000_000_000_000,
            hop_outputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
            consumed_inputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
        )
        payload = compose_simple_executor(result)
        # ABI shape distinct from cmd_executor: `execute(uint256,uint256,uint256[])`.
        assert payload[:4] == bytes.fromhex("ead35cae"), "foreign execute() selector"
        assert len(payload) == 4 + 6 * 32, "selector + ABI head + 2 hop words"
        # dynamic uint256[] head — offset word after (selector, opt, final).
        offset = int.from_bytes(payload[68:100], "big")
        assert offset == 0x60, "ABI array offset"
        assert SIMPLE_EXECUTOR_SIGNATURE == "execute(uint256,uint256,uint256[])"

    def test_python_strategy_wraps_in_payload_composer(self) -> None:
        from examples.execution_strategy_foreign import build_strategy

        composer = build_strategy()
        assert composer is not None

    def test_cross_layer_oracle_matches_recorded_corpus(self) -> None:
        """UQ6WOG — the Python foreign path reproduces the SAME recorded corpus
        the Rust sample pins (byte-identical across layers), and that corpus is
        distinct from `cmd_executor`."""
        from types import SimpleNamespace

        from examples.execution_strategy_foreign import compose_simple_executor

        corpus = bytes.fromhex(
            "ead35cae"
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
            "00000000000000000000000000000000000000000000000010cac896d2390000"
            "0000000000000000000000000000000000000000000000000000000000000060"
            "0000000000000000000000000000000000000000000000000000000000000002"
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
            "00000000000000000000000000000000000000000000000010cac896d2390000"
        )
        result = SimpleNamespace(
            optimal_input=999_999_999_999_999_999 + 1,
            hop_outputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
            consumed_inputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
        )
        payload = compose_simple_executor(result)
        assert payload == corpus, "Python foreign payload must match the Rust-recorded corpus"
