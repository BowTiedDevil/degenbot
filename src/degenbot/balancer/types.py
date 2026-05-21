import dataclasses

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2PoolState(AbstractPoolState):
    balances: tuple[int, ...]


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2WeightedPoolExternalUpdate:
    """State update for a Balancer V2 weighted pool."""
    block_number: BlockNumber
    balances: tuple[int, ...]


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2StablePoolExternalUpdate:
    """State update for a Balancer V2 stable pool.

    amp is currently omitted — stable pools treat amp as immutable after
    construction in this plan. A future slice may add amp tracking to
    support A ramping.
    """
    block_number: BlockNumber
    balances: tuple[int, ...]


@dataclasses.dataclass(slots=True, frozen=True)
class BalancerV2PoolStateUpdated(PoolStateMessage):
    """Message published when a Balancer V2 pool's state changes."""
    state: BalancerV2PoolState
