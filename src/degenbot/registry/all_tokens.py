from typing import Dict
from eth_utils.address import to_checksum_address
from eth_typing import ChecksumAddress

from ..baseclasses import BaseToken


_all_tokens: Dict[
    int,
    Dict[ChecksumAddress, BaseToken],
] = {}


class AllTokens:
    def __init__(self, chain_id: int) -> None:
        try:
            _all_tokens[chain_id]
        except KeyError:
            _all_tokens[chain_id] = {}
        finally:
            self.tokens = _all_tokens[chain_id]

    def __delitem__(self, token_address: str) -> None:
        del self.tokens[to_checksum_address(token_address)]

    def __getitem__(self, token_address: str) -> BaseToken:
        return self.tokens[to_checksum_address(token_address)]

    def __setitem__(self, token_address: str, token_helper: BaseToken) -> None:
        self.tokens[to_checksum_address(token_address)] = token_helper

    def __len__(self) -> int:
        return len(self.tokens)

    def get(self, token_address: str) -> BaseToken | None:
        return self.tokens.get(to_checksum_address(token_address))
