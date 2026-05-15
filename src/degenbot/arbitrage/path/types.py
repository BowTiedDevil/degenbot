from dataclasses import dataclass

from degenbot.erc20 import Erc20Token


@dataclass(frozen=True, slots=True)
class SwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool


class PathValidationError(Exception): ...
