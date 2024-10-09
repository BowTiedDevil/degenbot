import contextlib
from typing import TYPE_CHECKING

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from typing_extensions import Self

from degenbot.exceptions import DegenbotValueError, RegistryAlreadyInitialized

if TYPE_CHECKING:
    from ..erc20_token import Erc20Token


class TokenRegistry:
    instance: Self | None = None

    @classmethod
    def get_instance(cls) -> Self | None:
        return cls.instance

    def __init__(self) -> None:
        if self.instance is not None:
            raise RegistryAlreadyInitialized(
                "A registry has already been initialized. Access it using the get_instance() class method"  # noqa:E501
            )

        self._all_tokens: dict[
            tuple[
                int,  # chain ID
                ChecksumAddress,  # token address
            ],
            Erc20Token,
        ] = dict()

    def get(self, token_address: str, chain_id: int) -> "Erc20Token | None":
        return self._all_tokens.get(
            (chain_id, to_checksum_address(token_address)),
        )

    def add(self, token_address: str, chain_id: int, token: "Erc20Token") -> None:
        token_address = to_checksum_address(token_address)
        if self.get(token_address=token_address, chain_id=chain_id):
            raise DegenbotValueError("Token is already registered")
        self._all_tokens[(chain_id, token_address)] = token

    def remove(self, token_address: str, chain_id: int) -> None:
        token_address = to_checksum_address(token_address)

        with contextlib.suppress(KeyError):
            del self._all_tokens[(chain_id, token_address)]


token_registry = TokenRegistry()
