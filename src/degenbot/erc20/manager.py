from __future__ import annotations

import warnings
from threading import Lock
from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import Bot


class Erc20TokenManager:
    """
    Legacy token manager. Prefer ``Bot.build_erc20token()`` / ``Bot.get_token()`` instead.

    This class is deprecated and will be removed in a future release.
    """

    def __init_subclass__(cls, **kwargs: Any) -> None:
        warnings.warn(
            "Erc20TokenManager is deprecated. Use Bot.build_erc20token() / Bot.get_token() instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        super().__init_subclass__(**kwargs)

    def __init__(
        self,
        *,
        chain_id: ChainId | None = None,
        bot: Bot,
    ) -> None:
        warnings.warn(
            "Erc20TokenManager is deprecated. Use Bot.build_erc20token() / Bot.get_token() instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        self._bot = bot
        self._chain_id = chain_id
        self._erc20tokens: dict[ChecksumAddress, Erc20Token] = {}
        self._lock = Lock()

    def _reset(self) -> None:
        self._erc20tokens.clear()

    def get_erc20token(
        self,
        address: str,
        *,
        silent: bool = False,
    ) -> Erc20Token:
        """
        Get the token object from its address.
        """

        address = get_checksum_address(address)

        with self._lock:
            if token_helper := self._erc20tokens.get(address):
                return token_helper

        token_helper = self._bot.build_erc20token(
            address,
            chain_id=self._chain_id,
            silent=silent,
        )

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
