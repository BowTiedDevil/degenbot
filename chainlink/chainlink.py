import brownie


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
            self._contract: brownie.Contract = brownie.Contract(address)
        except Exception as e:
            print(e)

        if self._contract:
            pass
        else:
            try:
                self._contract: brownie.Contract = brownie.Contract.from_explorer(
                    address=address
                )
            except Exception as e:
                raise

        self._decimals: int = self._contract.decimals.call()
        self.update_price()

    def update_price(
        self,
    ) -> None:
        try:
            self.price: float = self._contract.latestRoundData.call()[1] / (
                10 ** self._decimals
            )
        except:
            pass
