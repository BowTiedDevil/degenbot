import contextlib
from typing import TYPE_CHECKING

from web3.types import BlockIdentifier

import degenbot.registry
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token
from degenbot.functions import get_number_for_block_identifier
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
        state_cache_depth: int = 8,
    ) -> None:
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        self._cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]] = {}
        self.address = get_checksum_address(address)
        degenbot.registry.token_registry.add(
            token_address=self.address, chain_id=self._chain_id, token=self
        )
        self._state_cache_depth = state_cache_depth

    def get_balance(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        address = get_checksum_address(address)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_balance[address][block_number]

        balance = self.w3.eth.get_balance(
            address,
            block_identifier=block_number,
        )

        balance_cache_at_address: BoundedCache[BlockNumber, int]
        try:
            balance_cache_at_address = self._cached_balance[address]
        except KeyError:
            balance_cache_at_address = BoundedCache(max_items=self._state_cache_depth)

        balance_cache_at_address[block_number] = balance
        self._cached_balance[address] = balance_cache_at_address
        return balance
