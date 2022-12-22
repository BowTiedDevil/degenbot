import time
from decimal import Decimal
from brownie import Contract
from brownie.network.account import LocalAccount


class Router:
    """
    Represents a Uniswap V2 router contract
    """

    def __init__(
        self,
        address: str,
        name: str,
        user: LocalAccount,
        abi: list = None,
    ) -> None:
        self.address = address

        try:
            self._contract = Contract(self.address)
        except Exception as e:
            print(e)
            if abi:
                self._contract = Contract.from_abi(
                    name=name, address=address, abi=abi
                )
            else:
                self._contract = Contract.from_explorer(address=address)

        self.name = name
        self._user = user
        print(f"• {name}")

    def __str__(self) -> str:
        return self.name

    def token_swap(
        self,
        token_in_quantity: int,
        token_in_address: str,
        token_out_quantity: int,
        token_out_address: str,
        slippage: Decimal,
        deadline: int = 60,
        scale=0,
    ) -> bool:
        try:
            params = {}
            params["from"] = self._user.address
            # if scale:
            #     params['priority_fee'] = get_scaled_priority_fee()

            self._contract.swapExactTokensForTokens(
                token_in_quantity,
                int(token_out_quantity * (1 - slippage)),
                [token_in_address, token_out_address],
                self._user.address,
                1000 * int(time.time() + deadline),
                params,
            )
            return True
        except Exception as e:
            print(f"Exception: {e}")
            return False
