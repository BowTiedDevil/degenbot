from typing import Dict, Optional, Union
from eth_utils.address import to_checksum_address
from eth_typing import ChecksumAddress

from ..baseclasses import TokenHelper


_all_tokens: Dict[
    int,
    Dict[str, TokenHelper],
] = {}


class AllTokens:
    def __init__(self, chain_id):
        try:
            _all_tokens[chain_id]
        except KeyError:
            _all_tokens[chain_id] = {}
        finally:
            self.tokens = _all_tokens[chain_id]

    def __delitem__(self, token_address: str):
        del self.tokens[to_checksum_address(token_address)]

    def __getitem__(self, token_address: str):
        return self.tokens[to_checksum_address(token_address)]

    def __setitem__(
        self,
        token_address: Union[ChecksumAddress, str],
        token_helper: TokenHelper,
    ):
        self.tokens[to_checksum_address(token_address)] = token_helper

    def __len__(self):
        return len(self.tokens)

    def get(self, token_address: str) -> Optional[TokenHelper]:
        return self.tokens.get(token_address)
