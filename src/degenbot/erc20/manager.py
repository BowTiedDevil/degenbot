from __future__ import annotations

from threading import Lock
from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.provider import ProviderAdapter
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
        import warnings
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
        **kwargs: Any,
    ) -> Erc20Token:
        """
        Get the token object from its address.
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
                )
            else:
                # Fetch name/symbol/decimals from chain using provider
                from degenbot.erc20.erc20 import Erc20Token as _Erc20Token

                prov = self._provider
                if prov is None:
                    from degenbot.connection import connection_manager
                    prov = connection_manager.get_provider(self._chain_id)

                if not prov.get_code(address):
                    from degenbot.exceptions import DegenbotValueError
                    raise DegenbotValueError(message="No contract deployed at this address")

                name, symbol, decimals = _Erc20Token.fetch_name_symbol_decimals_batched(
                    address=address, provider=prov,
                )
                token_helper = Erc20Token(
                    address,
                    chain_id=self._chain_id,
                    name=name,
                    symbol=symbol,
                    decimals=decimals,
                    **{k: v for k, v in kwargs.items() if k in ("oracle_address",)},
                )

        with self._lock:
            self._erc20tokens[address] = token_helper

        # Register in global token_registry for backward compat (if not already registered)
        from degenbot.registry import token_registry
        if token_registry.get(token_address=token_helper.address, chain_id=token_helper.chain_id) is None:
            token_registry.add(token_address=token_helper.address, chain_id=token_helper.chain_id, token=token_helper)

        return token_helper
