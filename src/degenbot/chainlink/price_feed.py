"""Chainlink price feed oracle contract interface."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyChainlinkPriceFeed

if TYPE_CHECKING:
    from degenbot.bot import Bot
    from degenbot.provider import AlloyProvider
    from degenbot.types.aliases import ChainId


class ChainlinkPriceContract:
    """Represents an on-chain Chainlink price oracle.

    Constructed with the oracle address and chain_id. The ``price`` property fetches
    the current price and decimals on access.

    The ``price`` property returns the decimal-corrected nominal token price in USD
    (e.g. 1 DAI = 1.0 USD).

    This is a delegating shell over the Rust ``PyChainlinkPriceFeed`` reader
    (the ``degenbot-price`` core crate, ADR-005). The inline ``provider.call_raw``
    / ``abi_decode`` bodies are retired — ``eth_call`` + canonical ABI decode now
    run in Rust. The float ``price`` is computed here (display layer) from the raw
    ``int256`` answer + ``decimals``, preserving the prior ``float(answer / 10**decimals)``
    behavior exactly.
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
        self._py_feed: PyChainlinkPriceFeed | None = None

    def _get_py_feed(self) -> PyChainlinkPriceFeed:
        """Lazily build the Rust reader over the bot's AlloyProvider.

        Returns:
            The cached ``PyChainlinkPriceFeed`` reader.

        Raises:
            ValueError: If no ``bot`` was set (the provider is required).

        """
        if self._py_feed is None:
            if self._bot is None:
                msg = "ChainlinkPriceContract requires a `bot` to fetch price"
                raise ValueError(msg)
            provider: AlloyProvider = self._bot.provider
            alloy_provider = provider.to_alloy_provider()
            self._py_feed = PyChainlinkPriceFeed(
                self.address,
                alloy_provider,
                chain_id=self._chain_id,
            )
        return self._py_feed

    @property
    def chain_id(self) -> ChainId | None:
        """Chain id."""
        return self._chain_id

    @property
    def decimals(self) -> int:
        """Decimals.

        Raises:
            ValueError: See function documentation.

        """
        if self._decimals is None:
            if self._bot is None:
                msg = "ChainlinkPriceContract requires a `bot` to fetch decimals"
                raise ValueError(msg)
            self._decimals = self._get_py_feed().decimals()
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
        # Delegate latestRoundData + decimals to the Rust reader (eth_call +
        # canonical ABI decode in Rust). The float division stays Python
        # (display layer), preserving the prior float-exact behavior including
        # the fractional part (the Rust `price()` truncates to whole units).
        _round_id, answer, _started_at, _updated_at, _answered_in_round = (
            self._get_py_feed().latest_round_data()
        )
        return float(answer) / (10**self.decimals)
