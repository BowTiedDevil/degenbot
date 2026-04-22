from dataclasses import dataclass
from enum import Enum

from degenbot.erc20 import Erc20Token


@dataclass(frozen=True, slots=True)
class SwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool


class PoolCompatibility(Enum):
    COMPATIBLE = "compatible"
    INCOMPATIBLE_INVARIANT = "incompatible_invariant"
    INCOMPATIBLE_TOKENS = "incompatible_tokens"


class PathValidationError(Exception): ...
