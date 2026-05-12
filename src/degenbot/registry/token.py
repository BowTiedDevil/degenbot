from typing import TYPE_CHECKING

from degenbot.registry.base import AddressRegistry
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token


class TokenRegistry(AddressRegistry["Erc20Token"]):
    """Registry for ERC-20 tokens keyed by (chain_id, token_address)."""

    def __init__(self) -> None:
        super().__init__(name="Token")

    def get(  # type: ignore[override]
        self,
        token_address: str,
        chain_id: ChainId,
    ) -> "Erc20Token | None":
        """Retrieve a token by chain and address."""
        return super().get(chain_id=chain_id, address=token_address)

    def add(  # type: ignore[override]
        self,
        token_address: str,
        chain_id: ChainId,
        token: "Erc20Token",
    ) -> None:
        """Register a token."""
        super().add(item=token, chain_id=chain_id, address=token_address)

    def remove(  # type: ignore[override]
        self,
        token_address: str,
        chain_id: ChainId,
    ) -> None:
        """Remove a token from the registry."""
        super().remove(chain_id=chain_id, address=token_address)
