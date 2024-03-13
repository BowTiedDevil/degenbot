from dataclasses import dataclass


@dataclass(slots=True, frozen=True)
class BaseDexDeployment:
    name: str
    chain_id: int
