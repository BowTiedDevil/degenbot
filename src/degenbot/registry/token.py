import contextlib
from typing import TYPE_CHECKING, Self

import degenbot.exceptions
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.registry import RegistryAlreadyInitialized
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.erc20 import Erc20Token


class TokenRegistry:
    instance: Self | None = None

    @classmethod
    def get_instance(cls) -> Self | None:
        return cls.instance

    def __init__(self) -> None:
        if self.__class__.instance is not None:
            raise RegistryAlreadyInitialized(
                message="A registry has already been initialized. Access it using the get_instance() class method"  # noqa:E501
            )
        self.__class__.instance = self

        self._all_tokens: dict[
            tuple[
                int,  # chain ID
                ChecksumAddress,  # token address
            ],
            Erc20Token,
        ] = {}

    def get(self, token_address: str, chain_id: ChainId) -> "Erc20Token | None":
        return self._all_tokens.get(
            (chain_id, get_checksum_address(token_address)),
        )

    def add(self, token_address: str, chain_id: ChainId, token: "Erc20Token") -> None:
        token_address = get_checksum_address(token_address)
        if self.get(token_address=token_address, chain_id=chain_id):
            raise degenbot.exceptions.DegenbotValueError(message="Token is already registered")
        self._all_tokens[(chain_id, token_address)] = token

    def remove(self, token_address: str, chain_id: ChainId) -> None:
        token_address = get_checksum_address(token_address)

        with contextlib.suppress(KeyError):
            del self._all_tokens[(chain_id, token_address)]
