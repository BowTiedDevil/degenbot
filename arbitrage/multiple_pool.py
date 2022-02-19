from brownie import Contract
from ..liquiditypool.liquidity_pool import LiquidityPool
from ..token import Erc20Token


class MultiplePool:
    def __init__(
        self,
        factory_address: str,
        token_addresses: list[Erc20Token],
        name: str = "",
        update_method="polling",
    ):

        # TODO figure out asserts, ensure all token addresses are unique and passed in the correct order

        self.tokens = []
        for address in token_addresses:
            self.tokens.append(Erc20Token(address=address))

        if name:
            self.name = name
        else:
            self.name = " - ".join([token.symbol for token in self.tokens])

        self.token_path = [token.address for token in self.tokens]

        _factory = Contract.from_explorer(factory_address)
        self.pools = []
        for i in range(len(self.token_path) - 1):
            self.pools.append(
                LiquidityPool(
                    address=_factory.getPair(
                        self.token_path[i], self.token_path[i + 1]
                    ),
                    name=" - ".join([self.tokens[i].symbol, self.tokens[i + 1].symbol]),
                    tokens=[self.tokens[i], self.tokens[i + 1]],
                    # may be overridden to "event" in constructor if polling is supported
                    update_method=update_method,
                )
            )
            print(f"Loaded LP: {self.tokens[i].symbol} - {self.tokens[i+1].symbol}")

    def __str__(self):
        return self.name
