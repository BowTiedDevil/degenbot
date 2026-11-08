"""A thin Python user-defined `ExecutionAdapter` (foreign contract).

Mirror of the standalone-Rust sample (`degenbot-execution-sample`,
`SimpleExecutor` foreign strategy) at the PYTHON layer, using the ADR-025 PyO3
lift. The driver supplies the **Encode blob** (a Python callable
`SolveResult -> bytes`) via `degenbot._ffi.execution.PayloadComposer`, and binds
it to the complete transaction the production seam carries —
`{to, data, value}` plus declared probe reads and a minimum net profit — through
`degenbot._ffi.execution.PayloadAdapter`. Calldata is built with `degenbot.abi`.

This exercises the seam end-to-end at the Python layer for a **foreign**
contract (`SimpleExecutor`) whose `execute(uint256,uint256,uint256[])` calldata
is deliberately distinct from the default `cmd_executor` adapter — never a
re-derivation of the canonical settlement-arbitrage 7-call bundle (ADR-019 R / ADR-025 D3,
"driver shell, not a co-implementation").

Guardrails:
- The Python lift produces calldata and declares the binding, probe list, and
  minimum profit. The engine runs the simulation and the Rust seam interprets
  its typed observation; there is no Python simulation loop and no Python
  assessment engine.
- The canonical batch-executor path is used unchanged (the wall).
- Amounts stay integer fixed-point (u256); never floats.

Run: `uv run python examples/execution_strategy_foreign.py`
"""

from __future__ import annotations

import logging
from typing import Any

from degenbot._ffi.execution import PayloadAdapter, abi_encode_call

logger = logging.getLogger(__name__)

# The foreign contract's `execute` signature — the wire format for OUR own
# `SimpleExecutor`, not `cmd_executor`. Its selector is `keccak256(sig)[..4]`.
SIMPLE_EXECUTOR_SIGNATURE = "execute(uint256,uint256,uint256[])"

# The foreign contract's own address (the transaction target). The sample
# `SimpleExecutor` is flash-funded, so the call carries no native value.
SIMPLE_EXECUTOR = "0x7777777777777777777777777777777777777777"


def compose_simple_executor(result: Any) -> bytes:
    """**Encode blob** (ADR-025 D2) — solve result → calldata for OUR contract.

    Reads the typed `SolveResult` view (integer fixed-point u256 amounts) and
    ABI-encodes a `execute(optimal_input, final_output, hop_outputs[])` call via
    the `degenbot.abi`-backed helper. Distinct ABI shape from `cmd_executor`.

    Returns:
        The ABI-encoded ``execute`` calldata for the foreign contract.

    """
    hop_outputs = list(result.hop_outputs)
    final_output = hop_outputs[-1]
    return abi_encode_call(
        SIMPLE_EXECUTOR_SIGNATURE,
        [result.optimal_input, final_output, hop_outputs],
    )


# Probe — declared data, not code: which pre/post read-calls the engine runs
# (`label, address, selector`). Assess/Fee use the seam's defaults.
PROBES: list[tuple[str, str, bytes]] = [
    ("WETH", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", bytes([0x70, 0xA0, 0x82, 0x31])),
]


def build_strategy() -> PayloadAdapter:
    """Bind the Python Encode blob to the foreign contract's transaction.

    The constructed `PayloadAdapter` is a full core `ExecutionAdapter`: its
    Encode returns ONE complete transaction (`to` = the foreign contract,
    `data` = the ABI calldata, `value` = the declared native value), its Probe
    is `PROBES`, and its Assess is the seam's default sum-of-deltas gate with
    the declared minimum net profit. Nothing threads into the canonical
    dispatch fan-out.

    Returns:
        The bound ``PayloadAdapter`` strategy for the foreign contract.

    """
    return PayloadAdapter(
        compose_simple_executor,
        to=SIMPLE_EXECUTOR,
        value=0,
        probes=PROBES,
        min_net_profit=0,
    )


def demo() -> None:
    """Compose a sample solved path and print the foreign transaction binding."""
    from types import SimpleNamespace

    strategy = build_strategy()

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
        "foreign transaction: to=%s value=%s data (%s bytes): 0x%s… via %s (probes=%d)",
        strategy.to,
        strategy.value,
        len(payload),
        payload.hex()[:16],
        strategy.__class__.__name__,
        len(strategy.probes),
    )


if __name__ == "__main__":
    demo()
