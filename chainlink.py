from brownie import Contract

__all__ = ["ChainlinkPriceContract"]


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
            self._contract = Contract(address)
        except Exception as e:
            print(e)
            try:
                self._contract: Contract = Contract.from_explorer(address)
            except Exception as e:
                print(e)

        self._decimals: int = self._contract.decimals()
        self.update_price()

    def update_price(
        self,
    ) -> None:
        try:
            self.price: float = self._contract.latestRoundData()[1] / (
                10**self._decimals
            )
        except:
            pass
