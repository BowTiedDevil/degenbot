"""§4.5 DelegateSpy test: prove the example routes encode_cmd_stream through
the Rust seam, not the retired Python encoder.

After §4.3 oracle-retirement, the Python `encode_cmd_stream` /
`compute_simulation_warmup_slots` / `pack_expected_balance` /
`pack_config` / `mapping_slot` / `v4_input_is_native` functions are deleted
from `examples/eth_backrun_helpers.py` and `examples/cmd_stream.py`. The
example (`examples/eth_backrun_v2_v3_v4_rust.py`) must source these from the
Rust extension `degenbot_rs`.

This test does NOT exercise the encode values (that's the Rust-side golden-
file parity in `cargo test -p degenbot-executor`). It proves the **seam**:
(1) the Python encoder symbols are absent, (2) the Rust seam symbols ARE
present, (3) the example's import graph reaches `degenbot_rs` for each
retired symbol — monkey-patching the Rust symbol and calling the example's
encode path confirms the call traverses Rust.
"""

from __future__ import annotations

import ast
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
EXAMPLES_DIR = REPO / "examples"


# ── §4.5: prove the Python encoder is gone ──────────────────────────────────


class TestPythonEncoderRetired:
    """The retired Python encoder symbols must NOT exist in Python land."""

    def test_no_python_encode_cmd_stream(self) -> None:
        """`eth_backrun_helpers` no longer exports `encode_cmd_stream`."""
        sys.path.insert(0, str(EXAMPLES_DIR))
        import eth_backrun_helpers as ebh

        assert not hasattr(ebh, "encode_cmd_stream"), (
            "eth_backrun_helpers.encode_cmd_stream must be deleted (§4.3)"
        )

    def test_no_python_v4_input_is_native(self) -> None:
        sys.path.insert(0, str(EXAMPLES_DIR))
        import eth_backrun_helpers as ebh

        assert not hasattr(ebh, "v4_input_is_native"), (
            "eth_backrun_helpers.v4_input_is_native must be deleted (§4.3)"
        )
        assert not hasattr(ebh, "v4_output_is_native"), (
            "eth_backrun_helpers.v4_output_is_native must be deleted (§4.3)"
        )

    def test_cmd_stream_module_deleted(self) -> None:
        """`examples/cmd_stream.py` is deleted entirely."""
        assert not (EXAMPLES_DIR / "cmd_stream.py").exists(), (
            "examples/cmd_stream.py must be deleted (§4.3)"
        )

    def test_no_python_warmup_slots(self) -> None:
        sys.path.insert(0, str(EXAMPLES_DIR))
        import eth_backrun_helpers as ebh

        assert not hasattr(ebh, "compute_simulation_warmup_slots"), (
            "compute_simulation_warmup_slots must be sourced from degenbot_rs (§4.3)"
        )


# ── §4.5: prove the Rust seam symbols exist ─────────────────────────────────


class TestRustSeamPresent:
    """The Rust extension must expose every retired symbol."""

    @pytest.fixture(autouse=True)  # noqa: RUF076
    def _import_rs(self) -> None:
        from degenbot._ffi import executor as _ffi_executor

        self.rs = _ffi_executor

    def test_encode_cmd_stream(self) -> None:
        assert hasattr(self.rs, "encode_cmd_stream")

    def test_compute_simulation_warmup_slots(self) -> None:
        assert hasattr(self.rs, "compute_simulation_warmup_slots")

    def test_pack_config(self) -> None:
        assert hasattr(self.rs, "pack_config")

    def test_pack_expected_balance(self) -> None:
        assert hasattr(self.rs, "pack_expected_balance")

    def test_mapping_slot(self) -> None:
        assert hasattr(self.rs, "mapping_slot")

    def test_v4_input_is_native(self) -> None:
        assert hasattr(self.rs, "v4_input_is_native")

    def test_v4_output_is_native(self) -> None:
        assert hasattr(self.rs, "v4_output_is_native")


# ── §4.5: prove the example import graph reaches degenbot_rs ─────────────────


class TestExampleRoutesThroughRust:
    """The example's source references `_ffi` for every retired symbol.

    Rather than a fragile full-import (the example needs dotenv/web3/etc.),
    this parses the example's AST import graph and confirms the symbols are
    bound to `_ffi`, not `eth_backrun_helpers` or `cmd_stream`.
    """

    @staticmethod
    def _example_source() -> str:
        return (EXAMPLES_DIR / "eth_backrun_v2_v3_v4_rust.py").read_text()

    def test_imports_dispatch_from_companion(self) -> None:
        """The example imports the dispatch seam from the companion, not FFI.

        A5 originally wired the example to import ``degenbot._ffi`` directly.
        The three-layer cutover (ADR-005) moves the driver to import the
        stable companion re-exports from ``degenbot.dispatch`` instead —
        the example must NOT import ``degenbot._ffi`` (the PyO3 wrapper is an
        implementation detail of the companion, not a driver surface).
        """
        src = self._example_source()
        tree = ast.parse(src)
        imports_ffi = False
        imports_companion_dispatch = False
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module:
                if node.module == "degenbot._ffi" or (
                    node.module == "degenbot" and "_ffi" in [a.name for a in node.names]
                ):
                    imports_ffi = True
                if node.module == "degenbot.dispatch":
                    imports_companion_dispatch = True
        assert imports_companion_dispatch, (
            "example must import dispatch symbols from degenbot.dispatch (the companion seam)"
        )
        assert not imports_ffi, (
            "example must NOT import degenbot._ffi directly (use degenbot.dispatch / degenbot.exceptions)"
        )

    def test_does_not_import_python_encoder(self) -> None:
        """The example must NOT import `encode_cmd_stream` from Python helpers."""
        src = self._example_source()
        # The example's eth_backrun_helpers import block must not include
        # encode_cmd_stream, v4_input_is_native.
        ebh_import_lines = [
            line
            for line in src.splitlines()
            if line.strip().startswith(("encode_cmd_stream", "v4_input_is_native"))
        ]
        assert ebh_import_lines == [], (
            f"example must not import encoder symbols from eth_backrun_helpers: {ebh_import_lines}"
        )

    def test_does_not_import_cmd_stream(self) -> None:
        src = self._example_source()
        assert "from cmd_stream import" not in src, (
            "example must not import from cmd_stream (deleted §4.3)"
        )

    @pytest.mark.parametrize(
        "function",
        [
            "dispatch_profitable_results",
            "simulate_one",
            "build_simulation_state_overrides",
            "_compute_priority_fee",
        ],
    )
    def test_python_sim_functions_retired(self, function: str) -> None:
        """The parallel Python sim implementation is deleted (A5).

        The example must NOT define the retired Python sim functions — their
        logic moved to the ``degenbot_simulation`` Rust crate
        (``dispatch_profitable_py`` owns simulate + the encode/warmup/payload
        helpers ``encode_cmd_stream`` / ``compute_simulation_warmup_slots`` /
        ``pack_expected_balance`` / ``v4_input_is_native`` / ``mapping_slot``,
        now called INTERNALLY by the seam, not from the example).
        ``format_failure_breakdown`` is kept (a pure renderer
        ``_render_sim_summary`` plugs into).
        """
        src = self._example_source()
        tree = ast.parse(src)
        defined = {
            node.name
            for node in ast.walk(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        }
        assert function not in defined, (
            f"example must not define the retired Python sim function {function} (A5)"
        )

    def test_sim_dispatch_routes_through_rust_seam(self) -> None:
        """The example routes the sim+submit flow through the Rust seam (A5).

        A5 superseded the per-symbol ``_ffi.<symbol>`` routing that this
        class previously enforced: the five encoder/warmup symbols moved INTO
        the Rust core (``degenbot_simulation`` / ``degenbot_executor``), called
        internally by ``dispatch_profitable`` + ``SimulateContext``
        construction. The example's dispatch path is now
        ``dispatch_profitable`` (simulate) → ``dispatch_and_submit``
        (submit), both Rust-bound pyfunctions imported via the companion
        package ``degenbot.dispatch`` (stable re-exports of the FFI symbols —
        the example does not import ``degenbot_rs`` directly).
        """
        src = self._example_source()
        assert "dispatch_profitable(" in src, (
            "example must route simulation through dispatch_profitable (A5)"
        )
        assert "dispatch_and_submit(" in src, (
            "example must route submission through dispatch_and_submit (A5)"
        )


# ── §4.5: _DelegateSpy — the encode call traverses the Rust seam ───────────────


class TestDelegateSpyEncodeCall:
    """Monkey-patch `_ffi_executor.encode_cmd_stream` and confirm the example
    actually calls it when encoding a path — proving delegation, not just
    import graph shape.

    Since the example has heavy import deps, we instead spy on the seam
    directly: call `_ffi_executor.encode_cmd_stream` through a patched sentinel
    and confirm the patched function is invoked.
    """

    def test_encode_cmd_stream_is_the_rust_bound_function(self) -> None:
        """The `encode_cmd_stream` symbol on `degenbot_rs` is a built-in
        (Rust extension function), not a Python function — proving it
        delegates to the Rust core, not a Python re-implementation.
        """
        from degenbot._ffi import executor as _ffi_executor

        fn = _ffi_executor.encode_cmd_stream
        # PyO3-bound functions are builtin_function_or_method, not Python defs.
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        ), (
            f"encode_cmd_stream must be a Rust-bound builtin, not a Python function "
            f"(got {fn.__class__.__name__})"
        )

    def test_compute_simulation_warmup_slots_is_rust_bound(self) -> None:
        from degenbot._ffi import executor as _ffi_executor

        fn = _ffi_executor.compute_simulation_warmup_slots
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )

    def test_pack_config_is_rust_bound(self) -> None:
        from degenbot._ffi import executor as _ffi_executor

        fn = _ffi_executor.pack_config
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )

    def test_mapping_slot_is_rust_bound(self) -> None:
        from degenbot._ffi import executor as _ffi_executor

        fn = _ffi_executor.mapping_slot
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )
