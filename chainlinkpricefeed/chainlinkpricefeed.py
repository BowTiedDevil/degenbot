from brownie import Contract


class ChainlinkPriceContract:
    """
    Represents an on-chain Chainlink price oracle
    """

    def __init__(self, address: str) -> None:
        try:
            self._contract: Contract = Contract.from_explorer(address=address)
            self._decimals: int = self._contract.decimals.call()
            self.price: float = self.update_price
        except:
            raise

    def update_price(self) -> float:
        try:
            latest_price: float = self._contract.latestRoundData.call()[1] / (
                10 ** self._decimals()
            )
            self.price = latest_price
            return latest_price
        except Exception as e:
            print(f"Exception in update_price: {e}")
