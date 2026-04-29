from threading import Lock
from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.provider.interface import ProviderAdapter
from degenbot.registry import token_registry
from degenbot.types.abstract import AbstractManager
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


class Erc20TokenManager(AbstractManager):
    def __init__(
        self,
        *,
        chain_id: ChainId | None = None,
        provider: ProviderAdapter | None = None,
    ) -> None:
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        self._erc20tokens: dict[ChecksumAddress, Erc20Token] = {}
        self._lock = Lock()
        self._provider = provider

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

        if token_helper := self._erc20tokens.get(address):
            return token_helper

        if token_helper := token_registry.get(token_address=address, chain_id=self._chain_id):
            return token_helper

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
