"""Abstract deployment type with chain ID and address."""
from dataclasses import dataclass

from degenbot.types.aliases import ChainId


@dataclass(slots=True, frozen=True)
class AbstractExchangeDeployment:
    """AbstractExchangeDeployment class."""

    name: str
    chain_id: ChainId
