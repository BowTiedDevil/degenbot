"""Wrapped Ether placeholder for native ETH in pool reserves."""

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token
from degenbot.types.aliases import ChainId


class EtherPlaceholder(Erc20Token):
    """An Erc20Token-like adapter for the 'all Es' or zero address placeholder.

    Used by pools to represent native Ether.
    """

    addresses = (
        ZERO_ADDRESS,
        get_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"),
    )
    symbol = "ETH"
    name = "Ether Placeholder"
    decimals = 18

    def __init__(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_cache_depth: int = 8,
    ) -> None:
        """Initialize the instance."""
        super().__init__(
            address,
            chain_id=chain_id,
            name=self.name,
            symbol=self.symbol,
            decimals=self.decimals,
            state_cache_depth=state_cache_depth,
        )
        self._state_cache_depth = state_cache_depth
