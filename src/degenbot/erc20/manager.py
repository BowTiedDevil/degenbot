from __future__ import annotations

from threading import Lock
from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.provider import ProviderAdapter
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from degenbot.bot import Bot
    from eth_typing import ChecksumAddress


class Erc20TokenManager:
    def __init__(
        self,
        *,
        chain_id: ChainId | None = None,
        provider: ProviderAdapter | None = None,
        bot: Bot | None = None,
    ) -> None:
        self._bot = bot
        self._chain_id = chain_id
        self._provider = provider
        self._erc20tokens: dict[ChecksumAddress, Erc20Token] = {}
        self._lock = Lock()

    def _reset(self) -> None:
        self._erc20tokens.clear()

    def get_erc20token(
        self,
        address: str,
        *,
        silent: bool = False,
        # accept any number of keyword arguments, which are
        # passed directly to Erc20Token without validation
        **kwargs: Any,
    ) -> Erc20Token:
        """
        Get the token object from its address
        """

        address = get_checksum_address(address)

        with self._lock:
            if token_helper := self._erc20tokens.get(address):
                return token_helper

        # Delegate to Bot if available
        if self._bot is not None:
            token_helper = self._bot.build_erc20token(
                address,
                chain_id=self._chain_id,
                silent=silent,
            )
        else:
            # Legacy path — check global registry first
            from degenbot.registry import token_registry

            if self._chain_id is not None:
                if (existing := token_registry.get(token_address=address, chain_id=self._chain_id)) is not None:
                    with self._lock:
                        self._erc20tokens[address] = existing
                    return existing

            if address in EtherPlaceholder.addresses:
                token_helper = EtherPlaceholder(
                    address,
                    chain_id=self._chain_id,
                    provider=self._provider,
                )
            else:
                token_helper = Erc20Token(
                    address,
                    chain_id=self._chain_id,
                    provider=self._provider,
                    silent=silent,
                    **kwargs,
                )

        with self._lock:
            self._erc20tokens[address] = token_helper

        return token_helper
