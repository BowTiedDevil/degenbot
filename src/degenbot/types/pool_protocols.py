"""
Pool simulation protocols.

Define structural interfaces for pool behavior. Pools satisfy these
protocols by implementing the required methods — no inheritance needed.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from fractions import Fraction

    from eth_typing import ChecksumAddress

    from degenbot.types.abstract import AbstractPoolState
    from degenbot.types.concrete import Subscriber
    from degenbot.types.hop_types import HopType


@dataclass(slots=True, frozen=True, kw_only=True)
class SimulationResult:
    """
    Pool-agnostic simulation output.

    Returned by PoolSimulation.simulate_swap for all pool types.
    """

    amount_in: int
    amount_out: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


@runtime_checkable
class PoolSimulation(Protocol):
    """
    Required interface for all pools.

    Supports exact-input swap simulation and pub/sub for state updates.
    """

    @property
    def address(self) -> ChecksumAddress: ...

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult: ...

    def subscribe(self, subscriber: Subscriber) -> None: ...

    def unsubscribe(self, subscriber: Subscriber) -> None: ...


@runtime_checkable
class ReverseSimulatablePool(Protocol):
    """
    Optional interface for pools that support exact-output simulation.

    Not all pool types can compute input from desired output.
    """

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult: ...


@runtime_checkable
class StateManageablePool(Protocol):
    """
    Optional interface for pools with on-chain state management.

    Curve and Balancer pools typically don't implement this.
    """

    def external_update(self, update: object) -> None: ...

    def auto_update(self) -> None: ...

    def discard_states_before_block(self, block: int) -> None: ...

    def restore_state_before_block(self, block: int) -> None: ...


@runtime_checkable
class ArbitrageCapablePool(PoolSimulation, Protocol):
    """
    Interface for pools participating in arbitrage paths.

    Extends PoolSimulation with hop state conversion and fee extraction,
    absorbing the old PoolAdapter protocol.
    """

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
    ) -> HopType: ...

    def extract_fee(self, zero_for_one: bool) -> Fraction: ...  # noqa: FBT001
