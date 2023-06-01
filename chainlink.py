from brownie import Contract  # type: ignore


class ChainlinkPriceContract:
    """
    Represents an on-chain Chainlink price oracle.
    Variable 'price' is decimal-corrected and represents the "nominal" token price in USD (e.g. 1 DAI = 1.0 USD)
    """

    def __init__(
        self,
        address: str,
    ) -> None:
        try:
            self._brownie_contract = Contract(address)
        except Exception as e:
            print(e)
            try:
                self._brownie_contract = Contract.from_explorer(address)
            except Exception as e:
                print(e)

        self._decimals: int = self._brownie_contract.decimals()
        self.update_price()

    def update_price(
        self,
    ) -> None:
        self.price: float = self._brownie_contract.latestRoundData()[1] / (
            10**self._decimals
        )
