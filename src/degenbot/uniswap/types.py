import dataclasses

from degenbot.erc20 import Erc20Token


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapPoolSwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool

    def __post_init__(self) -> None:
        assert self.token_in != self.token_out
