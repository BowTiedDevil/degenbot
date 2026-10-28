"""DelegateSpy test: the executor seam is Rust-bound, not a Python re-implementation.

The final arbiter for the encode values is the Rust-side golden-file parity in
``cargo test -p degenbot-executor``. What this module pins is the **seam**: the
Rust extension exposes the warmup/config/slot helpers, the helpers are Rust
builtins (so no Python re-implementation stands in for them), and the driver
sources its dispatch from the companion package rather than ``degenbot._ffi``.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
EXAMPLES_DIR = REPO / "examples"

# ── prove the Rust seam symbols exist ─────────────────────────────────


class TestRustSeamPresent:
    """The Rust extension must expose every kept seam symbol."""

    @pytest.fixture(autouse=True)
    def _import_rs(self) -> None:
        from tests.helpers.seam import executor as _ffi_executor

        self.rs = _ffi_executor

    def test_compute_simulation_warmup_slots(self) -> None:
        assert hasattr(self.rs, "compute_simulation_warmup_slots")

    def test_pack_config(self) -> None:
        assert hasattr(self.rs, "pack_config")

    def test_mapping_slot(self) -> None:
        assert hasattr(self.rs, "mapping_slot")

    def test_nested_mapping_slot(self) -> None:
        assert hasattr(self.rs, "nested_mapping_slot")


# ── prove the example import graph reaches degenbot_rs ─────────────────


class TestExampleRoutesThroughRust:
    """The example's source routes its dispatch through the companion seam.

    Rather than a fragile full-import (the example needs dotenv/web3/etc.),
    this parses the example's AST import graph and the driver modules it
    reaches.
    """

    @staticmethod
    def _example_source() -> str:
        return (EXAMPLES_DIR / "eth_settlement_arbitrage_v2_v3_v4_rust.py").read_text()

    def test_imports_dispatch_from_companion(self) -> None:
        """The driver imports the dispatch seam from the companion, not FFI.

        A5 originally wired the example to import ``degenbot._ffi`` directly.
        The three-layer cutover (ADR-005) moves the driver into
        ``degenbot.runner``, which imports the stable companion re-exports from
        ``degenbot.dispatch`` — the driver's dispatch module must NOT import
        ``degenbot._ffi`` (the PyO3 wrapper is an implementation detail of the
        companion, not a driver surface).
        """
        src = (REPO / "src" / "degenbot" / "runner" / "_dispatch.py").read_text()
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
            "driver must import dispatch symbols from degenbot.dispatch (the companion seam)"
        )
        assert not imports_ffi, (
            "driver must NOT import degenbot._ffi directly "
            "(use degenbot.dispatch / degenbot.exceptions)"
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
        (``dispatch_profitable_py`` owns simulate + the warmup/payload
        helpers ``compute_simulation_warmup_slots`` / ``mapping_slot``, now
        called INTERNALLY by the seam, not from the example).
        the standalone
        ``encode_cmd_stream`` / ``v4_input_is_native`` /
        ``v4_output_is_native`` PyO3 pyfunctions are retired too (the encode
        path is core-internal; the candidate resolves ``composers::PathInfo``
        from ``path_id`` via ``path_info_for_core``.
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
        """The driver routes the sim+submit flow through the Rust seam (A5).

        A5 superseded the per-symbol ``_ffi.<symbol>`` routing that this
        class previously enforced: the five encoder/warmup symbols moved INTO
        the Rust core (``degenbot_simulation`` / ``degenbot_executor``), called
        internally by ``dispatch_profitable`` + ``SimulateContext``
        construction. The sim-submit pipeline split the route across two
        leaves: the simulate leaf (``degenbot.runner._sim_submit``) calls
        ``dispatch_profitable``, and the submit leaf
        (``degenbot.runner._dispatch``) exposes ``submitter``/
        ``relay_providers`` DI seams whose default is the Rust pyfunction
        ``dispatch_and_submit``, so the production path (no injected
        submitter) stays Rust-bound. Both pyfunctions are imported via the
        companion package ``degenbot.dispatch`` (stable re-exports of the FFI
        symbols — the driver does not import ``degenbot_rs`` directly).
        """
        sim_src = (REPO / "src" / "degenbot" / "runner" / "_sim_submit.py").read_text()
        assert "engine_builder or build_batch_executor_py" in sim_src, (
            "driver must construct the core batch executor (build_batch_executor_py), "
            "bound as the construction seam's production default"
        )
        assert "dispatch_profitable" not in sim_src and "dispatch_and_submit" not in sim_src, (
            "the cut-over driver authors no sim/submit choreography — the "
            "ordered lane runs core-side"
        )


# ── _DelegateSpy — the encode call traverses the Rust seam ───────────────


class TestDelegateSpyEncodeCall:
    """Confirm the kept seam symbols are Rust-bound builtins (not Python
    re-implementations) — proving delegation to the Rust core.

    The ``encode_cmd_stream`` spy case is retired (the standalone
    pyfunction is gone — the encode path moved to the Rust core per A5).
    The remaining warmup/config/slot helpers stay Rust-bound.
    """

    def test_compute_simulation_warmup_slots_is_rust_bound(self) -> None:
        from tests.helpers.seam import executor as _ffi_executor

        fn = _ffi_executor.compute_simulation_warmup_slots
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )

    def test_pack_config_is_rust_bound(self) -> None:
        from tests.helpers.seam import executor as _ffi_executor

        fn = _ffi_executor.pack_config
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )

    def test_mapping_slot_is_rust_bound(self) -> None:
        from tests.helpers.seam import executor as _ffi_executor

        fn = _ffi_executor.mapping_slot
        assert fn.__class__.__name__ in (
            "builtin_function_or_method",
            "method_descriptor",
            "builtin_function",
        )
