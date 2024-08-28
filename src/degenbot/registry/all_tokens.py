
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address

from ..baseclasses import AbstractErc20Token
from ..logging import logger

# Internal state dictionary that maintains a keyed dictionary of all token objects. The top level
# dict is keyed by chain ID, and sub-dicts are keyed by the checksummed token address.
_all_tokens: dict[
    int,
    dict[ChecksumAddress, AbstractErc20Token],
] = {}


class AllTokens:
    def __init__(self, chain_id: int) -> None:
        try:
            _all_tokens[chain_id]
        except KeyError:
            _all_tokens[chain_id] = {}
        finally:
            self.tokens = _all_tokens[chain_id]

    def __contains__(self, token: AbstractErc20Token | str) -> bool:
        if isinstance(token, AbstractErc20Token):
            _token_address = token.address
        else:
            _token_address = to_checksum_address(token)
        return _token_address in self.tokens

    def __delitem__(self, token: AbstractErc20Token | str) -> None:
        if isinstance(token, AbstractErc20Token):
            _token_address = token.address
        else:
            _token_address = to_checksum_address(token)
        del self.tokens[_token_address]

    def __getitem__(self, token_address: str) -> AbstractErc20Token:
        return self.tokens[to_checksum_address(token_address)]

    def __setitem__(self, token_address: str, token_helper: AbstractErc20Token) -> None:
        _token_address = to_checksum_address(token_address)
        if _token_address in self.tokens:  # pragma: no cover
            logger.warning(
                f"Token with address {_token_address} already known. It has been overwritten."
            )
        self.tokens[to_checksum_address(token_address)] = token_helper

    def __len__(self) -> int:  # pragma: no cover
        return len(self.tokens)

    def get(self, token_address: str) -> AbstractErc20Token | None:
        return self.tokens.get(to_checksum_address(token_address))
