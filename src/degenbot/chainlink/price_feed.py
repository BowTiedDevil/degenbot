"""Chainlink price feed oracle contract interface."""
from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi
from web3 import Web3

from degenbot.checksum_cache import get_checksum_address

if TYPE_CHECKING:
    from degenbot.bot import Bot
    from degenbot.types.aliases import ChainId


class ChainlinkPriceContract:
    """Represents an on-chain Chainlink price oracle.

    Constructed with the oracle address and chain_id. The ``price`` property fetches
    the current price and decimals on access.

    The ``price`` property returns the decimal-corrected nominal token price in USD
    (e.g. 1 DAI = 1.0 USD).
    """

    def __init__(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        decimals: int | None = None,
        bot: Bot | None = None,
    ) -> None:
        """Initialize the instance."""
        self.address = get_checksum_address(address)
        self._chain_id = chain_id
        self._decimals = decimals
        self._bot = bot

    @property
    def chain_id(self) -> ChainId | None:
        """Chain id."""
        return self._chain_id

    @property
    def decimals(self) -> int:
        """Return decimals.

        Raises:
            ValueError: See function documentation.

        """
        if self._decimals is None:
            if self._bot is None:
                msg = "ChainlinkPriceContract requires a `bot` to fetch decimals"
                raise ValueError(msg)
            chain_id = self._chain_id or self._bot.connections.default_chain_id
            provider = self._bot.connections.get_provider(chain_id)
            (decimals,) = eth_abi.abi.decode(
                types=["uint8"],
                data=provider.call_raw(
                    {"to": self.address, "data": Web3.keccak(text="decimals()")[:4]},
                ),
            )
            self._decimals = decimals
        return self._decimals

    @property
    def price(self) -> float:
        """Price.

        Raises:
            ValueError: See function documentation.

        """
        if self._bot is None:
            msg = "ChainlinkPriceContract requires a `bot` to fetch price"
            raise ValueError(msg)
        chain_id = self._chain_id or self._bot.connections.default_chain_id
        provider = self._bot.connections.get_provider(chain_id)
        round_data = provider.call_raw(
            {"to": self.address, "data": Web3.keccak(text="latestRoundData()")[:4]},
        )
        _round_id, answer, _started_at, _updated_at, _answered_in_round = eth_abi.abi.decode(
            types=["uint80", "int256", "uint256", "uint256", "uint80"],
            data=round_data,
        )
        return float(answer / (10**self.decimals))
