"""OULU5O — a thin Python user-defined `ExecutionStrategy` (foreign contract).

Mirror of the standalone-Rust sample (`degenbot-execution-sample`,
`SimpleExecutor` foreign strategy) at the PYTHON layer, using the ADR-025 PyO3
lift. The driver supplies the **Encode blob** (a Python callable
`SolveResult -> bytes`) via `degenbot._ffi.execution.PayloadComposer`
(Polars `map_elements` model — Rust holds the callable and invokes it under the
GIL), and builds the foreign payload through `degenbot.abi`.

This exercises the seam end-to-end at the Python layer for a **foreign**
contract (`SimpleExecutor`) whose `execute(uint256,uint256,uint256[])` calldata
is deliberately distinct from the default `cmd_executor` adapter — never a
re-derivation of the canonical settlement-arbitrage 7-call bundle (ADR-019 R / ADR-025 D3,
"driver shell, not a co-implementation").

Guardrails:
- Probe (declared read-calls) and Assess/Fee (gate rule + pricing) are declared
  data / built-in defaults — NOT Python re-implementations of the canonical
  bundle. The canonical `dispatch_profitable_py` path is used unchanged (the
  wall).
- Amounts stay integer fixed-point (u256); never floats.

Run: `uv run python examples/execution_strategy_foreign.py`
"""

from __future__ import annotations

import logging
from typing import Any

from degenbot._ffi.execution import PayloadComposer, abi_encode_call

logger = logging.getLogger(__name__)

# The foreign contract's `execute` signature — the wire format for OUR own
# `SimpleExecutor`, not `cmd_executor`. Its selector is `keccak256(sig)[..4]`.
SIMPLE_EXECUTOR_SIGNATURE = "execute(uint256,uint256,uint256[])"


def compose_simple_executor(result: Any) -> bytes:
    """**Encode blob** (ADR-025 D2) — solve result → payload for OUR contract.

    Reads the typed `SolveResult` view (integer fixed-point u256 amounts) and
    ABI-encodes a `execute(optimal_input, final_output, hop_outputs[])` call via
    the `degenbot.abi`-backed helper. Distinct ABI shape from `cmd_executor`.
    """
    hop_outputs = list(result.hop_outputs)
    final_output = hop_outputs[-1]
    return abi_encode_call(
        SIMPLE_EXECUTOR_SIGNATURE,
        [result.optimal_input, final_output, hop_outputs],
    )


# Probe — declared data, not code: which pre/post read-calls the engine runs
# (`label, address, selector`). Assess/Fee use the built-in defaults.
PROBES: list[tuple[str, str, bytes]] = [
    ("WETH", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", bytes([0x70, 0xA0, 0x82, 0x31])),
]


def build_strategy() -> PayloadComposer:
    """Wrap the Python Encode blob into the core `PayloadComposer` seam.

    The constructed `PayloadComposer` is a valid core `PayloadComposer` (and,
    via the seam's blanket impl, a full `ExecutionStrategy` with the built-in
    Probe/Assess/Fee defaults) — the foreign-contract path a Python user adopts.
    `PROBES` here is the declared-probe data a full driver hands the engine;
    nothing threads into the canonical dispatch fan-out.
    """
    return PayloadComposer(compose_simple_executor)


def demo() -> None:
    """Compose a sample solved path and print the foreign payload."""
    from types import SimpleNamespace

    composer = build_strategy()

    # A solved path's per-hop amounts, sealed into the Python `SolveResult`
    # shape (here a stand-in with the same integer attributes the view exposes).
    result = SimpleNamespace(
        optimal_input=1_000_000_000_000_000_000,
        hop_outputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
        consumed_inputs=[1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
    )
    payload = compose_simple_executor(result)
    # The foreign `execute(uint256,uint256,uint256[])` selector — distinct from
    # `cmd_executor`'s `execute(bytes,uint256)` (verified in the Rust sample).
    assert payload[:4] == bytes.fromhex("ead35cae")
    logger.info(
        "foreign calldata (%s bytes): 0x%s… via %s (probes=%d)",
        len(payload),
        payload.hex()[:16],
        composer.__class__.__name__,
        len(PROBES),
    )


if __name__ == "__main__":
    demo()
