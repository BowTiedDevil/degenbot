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

It is also the worked tier-1/tier-2 pair of ADR-100: the adapter declares its
interface (standard ABI JSON, a selector allow-list, and a zero native-value
policy), so every encode is checked against it, and `demo()` runs the recorded
corpus as a validation case over a real `PathInfo` — the seam's own intake, not
a stand-in shape.

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

# The same surface as standard ABI JSON — the tier-1 declaration's one required
# part, written the way a toolchain emits it. The `constructor` entry is here on
# purpose: non-function entries are accepted and ignored.
EXECUTOR_ABI: list[dict[str, Any]] = [
    {
        "type": "function",
        "name": "execute",
        "stateMutability": "nonpayable",
        "inputs": [
            {"name": "optimal_input", "type": "uint256"},
            {"name": "final_output", "type": "uint256"},
            {"name": "hop_outputs", "type": "uint256[]"},
        ],
        "outputs": [{"name": "", "type": "bool"}],
    },
    {"type": "constructor", "inputs": [], "stateMutability": "nonpayable"},
]


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


# Probe — declared data, not code: which pre/post reads the engine runs, in
# declared order. Two neutral forms, neither naming a protocol:
#
# - a contract CALL carries its COMPLETE calldata (selector + ABI arguments),
#   so `WETH.balanceOf(executor)` observes the executor's own balance;
# - a NATIVE-ACCOUNT read observes the declared account's own ETH balance.
# Assess/Fee use the seam's defaults.
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"


def weth_balance_of(account: str) -> bytes:
    """`WETH.balanceOf(account)` — the complete calldata the probe declares.

    Returns:
        The 4-byte selector followed by the ABI-encoded account word.

    """
    return abi_encode_call("balanceOf(address)", [account])


PROBES: list[dict[str, Any]] = [
    {"label": "WETH", "to": WETH, "data": weth_balance_of(SIMPLE_EXECUTOR)},
    {"label": "ETH", "account": SIMPLE_EXECUTOR},
]


# A solved two-hop path — WETH -> USDC -> WETH — declared as data, so the case
# below hands the encoder the seam's own `PathInfo` and `ComposerInputs` instead
# of a stand-in object.
TWO_HOP_HOPS: list[dict[str, Any]] = [
    {
        "family": "v2",
        "pool_address": "0x1111111111111111111111111111111111111111",
        "token0": WETH,
        "token1": USDC,
        "fee": 30,
        "zfo": True,
    },
    {
        "family": "v2",
        "pool_address": "0x2222222222222222222222222222222222222222",
        "token0": USDC,
        "token1": WETH,
        "fee": 30,
        "zfo": False,
    },
]

TWO_HOP_AMOUNTS: dict[str, Any] = {
    "optimal_input": 1_000_000_000_000_000_000,
    "hop_outputs": [1_000_000_000_000_000_000, 1_210_000_000_000_000_000],
    "consumed_inputs": [1_000_000_000_000_000_000, 1_000_000_000_000_000_000],
}

# The Rust sample's recorded `execute` payload for those amounts, pinned here
# byte-for-byte: the case compares against bytes neither layer derives in this
# file. Its first four bytes are the foreign selector `0xead35cae`, distinct
# from `cmd_executor`'s `execute(bytes,uint256)`.
RECORDED_CORPUS = bytes.fromhex(
    "ead35cae"
    "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
    "00000000000000000000000000000000000000000000000010cac896d2390000"
    "0000000000000000000000000000000000000000000000000000000000000060"
    "0000000000000000000000000000000000000000000000000000000000000002"
    "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
    "00000000000000000000000000000000000000000000000010cac896d2390000"
)


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
        interface={
            "abi": EXECUTOR_ABI,
            "selectors": [SIMPLE_EXECUTOR_SIGNATURE],
            "value": {"kind": "zero"},
        },
    )


def demo() -> None:
    """Run the recorded corpus as a validation case over a real `PathInfo`."""
    strategy = build_strategy()

    reports = strategy.check_cases([
        {
            "name": "two_hop_v2_execute",
            "hops": TWO_HOP_HOPS,
            "amounts": TWO_HOP_AMOUNTS,
            "expect": {
                "kind": "transaction",
                "selector": SIMPLE_EXECUTOR_SIGNATURE,
                "calldata": "0x" + RECORDED_CORPUS.hex(),
            },
        },
    ])

    assert [report.name for report in reports] == ["two_hop_v2_execute"]
    assert [report.passed for report in reports] == [True], [
        (report.name, report.violations) for report in reports if not report.passed
    ]

    logger.info(
        "foreign transaction: to=%s value=%s data (%s bytes) matches the recorded "
        "corpus via %s (probes=%d)",
        strategy.to,
        strategy.value,
        len(RECORDED_CORPUS),
        strategy.__class__.__name__,
        len(strategy.probes),
    )


if __name__ == "__main__":
    demo()
