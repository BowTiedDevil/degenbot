"""Swap encoding and payload generation.

- EncodedCall: a minimal EVM call fragment (to, data, value)
- encode() on each SwapAmounts subclass produces per-hop calldata
- ApprovalStrategy: pluggable approval injection
- PayloadComposer: pluggable call composition (multicall, custom executor, etc.)
- generate_payloads(): pipeline that wires encoding, approval, and composition
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.arbitrage.types import AbstractSwapAmounts


@dataclass(frozen=True, slots=True)
class EncodedCall:
    """A single EVM call ready for on-chain submission."""

    to: ChecksumAddress
    data: bytes
    value: int = 0


@runtime_checkable
class ApprovalStrategy(Protocol):
    """Pluggable approval injection.

    Given the swap amounts and the encoded calls, return any
    approval calls that should be prepended.
    """

    def approvals_for(
        self,
        swap_amounts: tuple[AbstractSwapAmounts, ...],
        calls: list[EncodedCall],
    ) -> list[EncodedCall]: ...


@runtime_checkable
class PayloadComposer(Protocol):
    """Pluggable call composition.

    Given the list of encoded calls, compose them into the
    format a target contract expects.
    """

    def compose(self, calls: list[EncodedCall]) -> list[EncodedCall]: ...


class NoApprovals:
    """Approval strategy that adds no approval calls."""

    def approvals_for(  # noqa: PLR6301
        self,
        swap_amounts: tuple[AbstractSwapAmounts, ...],  # noqa: ARG002
        calls: list[EncodedCall],  # noqa: ARG002
    ) -> list[EncodedCall]:
        return []


class FlatComposer:
    """Composer that returns calls as-is (no wrapping)."""

    def compose(self, calls: list[EncodedCall]) -> list[EncodedCall]:  # noqa: PLR6301
        return calls


def generate_payloads(
    swap_amounts: tuple[AbstractSwapAmounts, ...] | list[AbstractSwapAmounts],
    *,
    recipient: ChecksumAddress,
    approval_strategy: ApprovalStrategy | None = None,
    composer: PayloadComposer | None = None,
) -> list[EncodedCall]:
    """Generate encoded swap payloads for an arbitrage path.

    Pipeline:
    1. Encode each swap via its SwapAmounts.encode() method
    2. Inject approval calls via the ApprovalStrategy
    3. Compose final payload via the PayloadComposer

    Args:
        swap_amounts: Ordered sequence of swap amounts.
        recipient: Address that receives swap output.
        approval_strategy: Injects approval calls. Default: NoApprovals.
        composer: Composes final call list. Default: FlatComposer.
    """
    if approval_strategy is None:
        approval_strategy = NoApprovals()
    if composer is None:
        composer = FlatComposer()

    # Step 1: Encode each swap
    swap_calls = [swap.encode(recipient=recipient) for swap in swap_amounts]

    # Step 2: Inject approvals
    approval_calls = approval_strategy.approvals_for(
        tuple(swap_amounts),
        swap_calls,
    )

    # Step 3: Compose
    all_calls = approval_calls + swap_calls
    return composer.compose(all_calls)
