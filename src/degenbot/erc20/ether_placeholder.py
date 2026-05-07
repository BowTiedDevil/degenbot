import contextlib
from typing import TYPE_CHECKING

from web3.types import BlockIdentifier

import degenbot.registry
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token
from degenbot.functions import get_number_for_block_identifier
from degenbot.provider.interface import ProviderAdapter
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


class EtherPlaceholder(Erc20Token):
    """
    An Erc20Token-like adapter for pools using the 'all Es' or zero address placeholder to represent
    native Ether.
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
        provider: ProviderAdapter | None = None,
        state_cache_depth: int = 8,
    ) -> None:
        super().__init__(
            address,
            chain_id=chain_id,
            name=self.name,
            symbol=self.symbol,
            decimals=self.decimals,
            state_cache_depth=state_cache_depth,
        )
        self._provider = provider
        # Legacy: self-register in token_registry when not going through Bot
        degenbot.registry.token_registry.add(
            token_address=self.address, chain_id=self._chain_id, token=self
        )
        self._state_cache_depth = state_cache_depth
