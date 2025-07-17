import dataclasses

from degenbot.types.abstract import AbstractPoolState


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2PoolState(AbstractPoolState):
    balances: tuple[int, ...]
