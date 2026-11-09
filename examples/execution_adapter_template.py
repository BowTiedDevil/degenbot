"""Template: a validated custom-executor adapter (ADR-100).

A custom executor needs two things from you: calldata for your own contract, and
the transaction binding calldata cannot carry (`to` + native `value`). This file
adds the part a maintained adapter wants on top — a declared interface degenbot
checks every encode against, and explicit cases that pin what the encoder
produces for real solved paths.

It runs offline and asserts itself: no provider, no engine, no RPC endpoint, no
fixture, no chain. Run it with:

    uv run python examples/execution_adapter_template.py

The progressive tiers, weakest first (`docs/execution-adapters.md`, ADR-100):

- **tier 1 — the `interface=` declaration** checks every encode: is this a
  declared call to *this* contract (declared selector, arguments that decode
  against the declared types, declared target and native value)?
- **tier 2 — `check_cases(...)`** compares explicit `PathInfo` + amount
  literals against the transaction the adapter must produce for them, and
  accepts a declared decline for a path the contract cannot encode.
- **tier 3 — execution-backed certification** is the only *behaviour* proof,
  and it needs a chain: it lives in
  `tests/dispatch/test_custom_executor_contract.py`, not here. A violation is
  dispositive; a pass is a screen.

To make this yours:

1. Replace `MY_EXECUTOR` with the address your adapter targets.
2. Replace `EXECUTOR_ABI` with your contract's own ABI JSON array. Every
   toolchain emits it: `solc --abi MyContract.sol`, `forge inspect MyContract
   abi`, `vyper -f abi my_contract.vy`. Paste the array verbatim (a whole
   artifact's `abi` member is the same array). The declaration is a checker —
   it never encodes anything for you.
3. Replace `compose_execute` with your calldata. Raise `UnsupportedPath` for a
   path your contract cannot encode: that is the routine capability decline the
   target-selection walk advances past, not a validation failure.
4. Replace `CASES` with one case per behaviour you promise, and keep the two
   assertions in `main()`: they are what makes the template self-checking.

"""

from __future__ import annotations

import logging
from typing import Any

from degenbot._ffi.execution import PayloadAdapter, UnsupportedPath, abi_encode_call

logger = logging.getLogger(__name__)


# ── Your contract ────────────────────────────────────────────────────────────

#: The contract your adapter encodes for. A binding encodes to ONE address.
MY_EXECUTOR = "0x7777777777777777777777777777777777777777"

#: The function this adapter emits, as a canonical signature string.
EXECUTE = "execute(uint256,uint256,uint256[])"

#: Your contract's ABI, as a standard ABI JSON array. The declaration reads the
#: callable surface from `type: "function"` entries; `constructor`, `event`,
#: `error`, `fallback`, and `receive` entries are accepted and ignored, so a
#: whole generated artifact can be pasted unchanged.
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

#: The largest path this contract can settle — the encoder's own capability
#: limit, declared in ONE place so the decline case has a fact to point at.
MAX_HOPS = 2


# ── Encode: solved path -> calldata for YOUR contract ────────────────────────


def compose_execute(result: Any) -> bytes:
    """Encode one solved path into an ``execute`` call.

    Args:
        result: The typed ``SolveResult`` view the seam hands a compose
            callable — integer fixed-point amounts (``optimal_input``,
            ``hop_outputs``, ``consumed_inputs``), never floats.

    Returns:
        The ABI-encoded ``execute`` calldata for this contract.

    Raises:
        UnsupportedPath: When the path has more hops than this contract can
            settle. That is this adapter's routine capability refusal: the
            target-selection walk advances to the next target, and a validation
            case that declares the decline accepts it.

    """
    hop_outputs = list(result.hop_outputs)
    if len(hop_outputs) != MAX_HOPS:
        message = f"this contract settles exactly {MAX_HOPS} hops, the path has {len(hop_outputs)}"
        raise UnsupportedPath(message)
    return abi_encode_call(
        EXECUTE,
        [result.optimal_input, hop_outputs[-1], hop_outputs],
    )


# ── Probe: declared reads, run by the engine, never by this file ─────────────

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def weth_balance_of(account: str) -> bytes:
    """``WETH.balanceOf(account)`` — a probe's complete calldata.

    Args:
        account: The account whose WETH balance the read observes.

    Returns:
        The 4-byte selector followed by the ABI-encoded account word.

    """
    return abi_encode_call("balanceOf(address)", [account])


PROBES: list[dict[str, Any]] = [
    {"label": "WETH", "to": WETH, "data": weth_balance_of(MY_EXECUTOR)},
    {"label": "ETH", "account": MY_EXECUTOR},
]


# ── The declaration: tier 1 ──────────────────────────────────────────────────


def build_adapter() -> PayloadAdapter:
    """Bind the Encode callable, its transaction, and its declared interface.

    ``interface`` is decomposed by the binding, never by you: the binding
    serializes the ABI array, parses the addresses, resolves the value policy,
    and stamps the target from its own ``to``. Construction is pure data
    validation — it never invokes ``compose_execute`` and never reads a chain.

    Returns:
        The validated adapter: every encode is checked against the declaration,
        and ``check_cases`` can compare explicit cases against it.

    """
    return PayloadAdapter(
        compose_execute,
        to=MY_EXECUTOR,
        value=0,  # this contract is flash-funded
        probes=PROBES,
        min_net_profit=0,
        interface={
            "abi": EXECUTOR_ABI,
            "selectors": [EXECUTE],
            "value": {"kind": "zero"},
        },
    )


# ── Cases: tier 2 ────────────────────────────────────────────────────────────

#: Two V2 hops, WETH -> USDC -> WETH: the shape a two-hop solution arrives in.
TWO_HOPS: list[dict[str, Any]] = [
    {
        "family": "v2",
        "pool_address": "0x1111111111111111111111111111111111111111",
        "token0": WETH,
        "token1": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "fee": 30,
        "zfo": True,
    },
    {
        "family": "v2",
        "pool_address": "0x2222222222222222222222222222222222222222",
        "token0": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "token1": WETH,
        "fee": 30,
        "zfo": False,
    },
]

#: Three hops, for the path this contract declines.
THREE_HOPS: list[dict[str, Any]] = [
    *TWO_HOPS,
    {
        "family": "v2",
        "pool_address": "0x3333333333333333333333333333333333333333",
        "token0": WETH,
        "token1": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "fee": 30,
        "zfo": True,
    },
]

CASES: list[dict[str, Any]] = [
    {
        "name": "two_hop_v2_execute",
        "hops": TWO_HOPS,
        "amounts": {
            "optimal_input": 10**18,
            "hop_outputs": [10**18, 121 * 10**16],
            "consumed_inputs": [10**18, 10**18],
        },
        # Decoded argument expectations, in declaration order: the produced
        # calldata is decoded against the interface's declared types and
        # compared, so the case speaks the call's vocabulary, not its bytes.
        "expect": {
            "kind": "transaction",
            "selector": EXECUTE,
            "arguments": [10**18, 121 * 10**16, [10**18, 121 * 10**16]],
        },
    },
    {
        "name": "three_hop_path_declines",
        "hops": THREE_HOPS,
        "amounts": {
            "optimal_input": 10**18,
            "hop_outputs": [10**18, 11 * 10**17, 121 * 10**16],
            "consumed_inputs": [10**18, 10**18, 11 * 10**17],
        },
        # The routine capability decline: `UnsupportedPath` from the callable.
        "expect": {"kind": "decline"},
    },
]


# ── The self-check ───────────────────────────────────────────────────────────


def main() -> None:
    """Run both cases and assert what the adapter must produce for each."""
    adapter = build_adapter()

    reports = adapter.check_cases(CASES)

    assert [report.name for report in reports] == ["two_hop_v2_execute", "three_hop_path_declines"]
    assert [report.passed for report in reports] == [True, True], [
        (report.name, report.violations) for report in reports if not report.passed
    ]
    assert all(report.violations == [] for report in reports)

    for report in reports:
        logger.info("case %s: passed", report.name)


if __name__ == "__main__":
    main()
