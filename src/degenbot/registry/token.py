"""Token registry: address-keyed store of built ERC-20 instances."""
from typing import TYPE_CHECKING

from degenbot.registry.base import AddressRegistry
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token


class TokenRegistry(AddressRegistry["Erc20Token"]):
    """Registry for ERC-20 tokens keyed by (chain_id, token_address)."""

    def __init__(self) -> None:
        """Initialize the instance."""
        super().__init__(name="Token")

    def get(
        self,
        token_address: str,
        chain_id: ChainId,
    ) -> "Erc20Token | None":
        """Retrieve a token by chain and address."""
        return self._get(chain_id=chain_id, address=token_address)

    def add(
        self,
        token_address: str,
        chain_id: ChainId,
        token: "Erc20Token",
    ) -> None:
        """Register a token."""
        self._add(item=token, chain_id=chain_id, address=token_address)

    def remove(
        self,
        token_address: str,
        chain_id: ChainId,
    ) -> None:
        """Remove a token from the registry."""
        self._remove(chain_id=chain_id, address=token_address)
