from abc import ABC, abstractmethod
from brownie import Contract
from brownie.convert import to_address
from .v3_lp_abi import V3_LP_ABI


class BaseV3LiquidityPool(ABC):
    def __init__(self, address):
        self.address = to_address(address)

        try:
            self._brownie_contract = Contract(address=address)
        except:
            try:
                self._brownie_contract = Contract.from_explorer(
                    address=address, silent=True
                )
            except:
                try:
                    self._brownie_contract = Contract.from_abi(
                        name="", address=address, abi=V3_LP_ABI
                    )
                except:
                    raise

        try:
            self.token0 = self._brownie_contract.token0()
            self.token1 = self._brownie_contract.token1()
            self.fee = self._brownie_contract.fee()
            self.slot0 = self._brownie_contract.slot0()
            self.liquidity = self._brownie_contract.liquidity()
            self.tick_spacing = self._brownie_contract.tickSpacing()
            self.sqrt_price_x96 = self.slot0[0]
            self.tick = self.slot0[1]
        except:
            raise

    def update(self):
        updates = False
        try:
            if (slot0 := self._brownie_contract.slot0()) != self.slot0:
                updates = True
                self.slot0 = slot0
                self.sqrt_price_x96 = self.slot0[0]
                self.tick = self.slot0[1]
            if (
                liquidity := self._brownie_contract.liquidity()
            ) != self.liquidity:
                updates = True
                self.liquidity = liquidity

        except:
            raise
        else:
            return updates, {
                "slot0": self.slot0,
                "liquidity": self.liquidity,
                "sqrt_price_x96": self.sqrt_price_x96,
                "tick": self.tick,
            }


class V3LiquidityPool(BaseV3LiquidityPool):
    pass
