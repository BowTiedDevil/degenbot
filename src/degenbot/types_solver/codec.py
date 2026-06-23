"""ABI calldata encoders for Executor strategy entry points.

Each encoder produces 0x-prefixed lowercase-hex calldata that is byte-
identical to viem's ``encodeFunctionData`` against
``coordinator/src/types/abi.ts``. The cross-language lock test
(``test_fixtures_lock.py``) is the regression guard.

Implementation notes
--------------------
- Selectors are hard-coded to the locked 4-byte values from the project
  brief. ``selectors_match_signatures()`` independently re-derives them
  via ``keccak256`` of the canonical Solidity signatures so a CI run
  catches drift between the table here and the contract source.
- ``eth_abi.encode`` takes parallel ``[type_str, ...]`` and ``[value, ...]``
  lists. Each Executor entry point takes one tuple parameter, so we pass
  a single-element type list and a single tuple value.
- Tuples are encoded as Python ``tuple``\\s. ``bytes`` fields take native
  ``bytes``, with a leading ``0x`` stripped from the hex string.
- ``IntEnum`` values are subclasses of ``int`` and pass through ``uint8``
  encoding directly.

Spec
----
- ``coordinator/src/types/abi.ts``  (canonical viem ABI fragment)
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from eth_abi.abi import encode
from eth_utils.crypto import keccak

if TYPE_CHECKING:
    from degenbot.types_solver.executor import (
        ComposeParams,
        MatchParams,
        NativeArbParams,
        SwapStep,
    )

# ---------------------------------------------------------------------------
# Locked function selectors. Independently verified by
# ``selectors_match_signatures()`` against the canonical signatures.
# ---------------------------------------------------------------------------

#: ``executeNativeArb(NativeArbParams)`` selector.
EXECUTE_NATIVE_ARB_SELECTOR: bytes = bytes.fromhex("f6f6add1")
#: ``matchInternal(MatchParams)`` selector (12-field MatchParams, incl. ADR-029 settlement-funding).
MATCH_INTERNAL_SELECTOR: bytes = bytes.fromhex("210ea473")
#: ``composeFourLeg(ComposeParams)`` selector (12-field ComposeParams, incl. ADR-029 settlement-funding).
COMPOSE_FOUR_LEG_SELECTOR: bytes = bytes.fromhex("2e1977d2")

# ---------------------------------------------------------------------------
# Canonical Solidity signatures — fully unfolded tuple shapes. Pulled
# verbatim from ``coordinator/src/types/abi.ts`` so a divergence between
# the TS source-of-truth and the Python mirror is caught at test time.
# ---------------------------------------------------------------------------

EXECUTE_NATIVE_ARB_SIGNATURE: str = (
    "executeNativeArb("
    "(address,uint8,address,uint256,"
    "(uint8,address,bytes,address,address,uint256,uint256)[],"
    "uint256,uint256))"
)
MATCH_INTERNAL_SIGNATURE: str = (
    "matchInternal((bytes,bytes,address[],uint256[],address,uint8,address,"
    "uint256,uint256,uint256,address,uint256))"
)
COMPOSE_FOUR_LEG_SIGNATURE: str = (
    "composeFourLeg("
    "(bytes,(uint8,address,bytes,address,address,uint256,uint256)[],"
    "bytes,bytes,address,uint8,address,uint256,uint256,uint256,address,uint256))"
)

# ---------------------------------------------------------------------------
# Tuple-type strings consumed by ``eth_abi.encode``. These are the inner
# Solidity tuple shapes — i.e. the function parameter type with its outer
# function name stripped. Field order matches the canonical signatures.
# ---------------------------------------------------------------------------

_NATIVE_ARB_TUPLE_TYPE: str = "(address,uint8,address,uint256,(uint8,address,bytes,address,address,uint256,uint256)[],uint256,uint256)"
_MATCH_PARAMS_TUPLE_TYPE: str = (
    "(bytes,bytes,address[],uint256[],address,uint8,address,uint256,uint256,uint256,address,uint256)"
)
_COMPOSE_PARAMS_TUPLE_TYPE: str = (
    "(bytes,(uint8,address,bytes,address,address,uint256,uint256)[],"
    "bytes,bytes,address,uint8,address,uint256,uint256,uint256,address,uint256)"
)

# ---------------------------------------------------------------------------
# Helpers.
# ---------------------------------------------------------------------------


def _sig_keccak(sig: str) -> bytes:
    """Return the 4-byte keccak256 selector of a fully-unfolded Solidity signature."""
    return keccak(sig.encode())[:4]


def _hex_to_bytes(hex_str: str) -> bytes:
    """Decode a 0x-prefixed (or bare) hex string into raw bytes.

    Accepts both ``"0x"`` (empty bytes) and ``"0x1234"``. Raises ``ValueError``
    on odd-length payloads.
    """
    stripped = hex_str.removeprefix("0x").removeprefix("0X")
    if stripped == "":
        return b""
    return bytes.fromhex(stripped)


def _swap_step_to_tuple(s: SwapStep) -> tuple[int, str, bytes, str, str, int, int]:
    """Flatten a ``SwapStep`` into the positional tuple ``eth_abi`` expects.

    Order MUST match ``swapStepComponents`` in ``coordinator/src/types/abi.ts``:
    ``(dexKind, router, callData, tokenIn, tokenOut, amountIn, amountOutMin)``.
    """
    return (
        int(s.dex_kind),
        s.router,
        _hex_to_bytes(s.call_data),
        s.token_in,
        s.token_out,
        s.amount_in,
        s.amount_out_min,
    )


def _native_arb_to_tuple(
    p: NativeArbParams,
) -> tuple[
    str,
    int,
    str,
    int,
    list[tuple[int, str, bytes, str, str, int, int]],
    int,
    int,
]:
    """Flatten ``NativeArbParams`` into the eth_abi positional tuple."""
    return (
        p.flash_lender,
        int(p.flash_protocol),
        p.flash_token,
        p.flash_amount,
        [_swap_step_to_tuple(s) for s in p.swaps],
        p.min_profit,
        p.deadline,
    )


def _match_params_to_tuple(
    p: MatchParams,
) -> tuple[bytes, bytes, list[str], list[int], str, int, str, int, int, int, str, int]:
    """Flatten ``MatchParams`` into the eth_abi positional tuple."""
    return (
        _hex_to_bytes(p.cow_settlement_calldata),
        _hex_to_bytes(p.uniswapx_batch_calldata),
        list(p.expected_token_inflows),
        list(p.expected_token_inflow_min),
        p.flash_lender,
        int(p.flash_protocol),
        p.flash_token,
        p.flash_amount,
        p.min_profit,
        p.deadline,
        p.settlement_funding_token,
        p.settlement_funding_max,
    )


def _compose_params_to_tuple(
    p: ComposeParams,
) -> tuple[
    bytes,
    list[tuple[int, str, bytes, str, str, int, int]],
    bytes,
    bytes,
    str,
    int,
    str,
    int,
    int,
    int,
    str,
    int,
]:
    """Flatten ``ComposeParams`` into the eth_abi positional tuple."""
    return (
        _hex_to_bytes(p.across_fill_calldata),
        [_swap_step_to_tuple(s) for s in p.arb_swaps],
        _hex_to_bytes(p.cow_fill_calldata),
        _hex_to_bytes(p.uniswapx_rebalance_calldata),
        p.flash_lender,
        int(p.flash_protocol),
        p.flash_token,
        p.flash_amount,
        p.min_profit,
        p.deadline,
        p.settlement_funding_token,
        p.settlement_funding_max,
    )


# ---------------------------------------------------------------------------
# Public encoders.
# ---------------------------------------------------------------------------


def encode_native_arb(p: NativeArbParams) -> str:
    """ABI-encode ``executeNativeArb(p)`` calldata.

    Returns a 0x-prefixed lowercase-hex string. Byte-identical to viem's
    ``encodeFunctionData({ abi: executorAbi, functionName: 'executeNativeArb', args: [p] })``.
    """
    body = encode([_NATIVE_ARB_TUPLE_TYPE], [_native_arb_to_tuple(p)])
    return "0x" + (EXECUTE_NATIVE_ARB_SELECTOR + body).hex()


def encode_match_internal(p: MatchParams) -> str:
    """ABI-encode ``matchInternal(p)`` calldata.

    Returns a 0x-prefixed lowercase-hex string. Byte-identical to viem's
    ``encodeFunctionData({ abi: executorAbi, functionName: 'matchInternal', args: [p] })``.
    """
    body = encode([_MATCH_PARAMS_TUPLE_TYPE], [_match_params_to_tuple(p)])
    return "0x" + (MATCH_INTERNAL_SELECTOR + body).hex()


def encode_compose_four_leg(p: ComposeParams) -> str:
    """ABI-encode ``composeFourLeg(p)`` calldata.

    Returns a 0x-prefixed lowercase-hex string. Byte-identical to viem's
    ``encodeFunctionData({ abi: executorAbi, functionName: 'composeFourLeg', args: [p] })``.
    """
    body = encode([_COMPOSE_PARAMS_TUPLE_TYPE], [_compose_params_to_tuple(p)])
    return "0x" + (COMPOSE_FOUR_LEG_SELECTOR + body).hex()


def selectors_match_signatures() -> bool:
    """Returns True iff the locked selectors equal ``keccak(canonical_sig)[:4]``.

    Used by the lock test as a CI guard against accidental selector drift —
    if either the canonical signature or the locked selector ever changes,
    one side is wrong.
    """
    return (
        _sig_keccak(EXECUTE_NATIVE_ARB_SIGNATURE) == EXECUTE_NATIVE_ARB_SELECTOR
        and _sig_keccak(MATCH_INTERNAL_SIGNATURE) == MATCH_INTERNAL_SELECTOR
        and _sig_keccak(COMPOSE_FOUR_LEG_SIGNATURE) == COMPOSE_FOUR_LEG_SELECTOR
    )
