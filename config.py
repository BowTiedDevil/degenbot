from typing import Optional
from web3 import Web3

_web3: Optional[Web3] = None


def get_web3() -> Optional[Web3]:
    return _web3


def set_web3(w3: Web3):
    global _web3
    _web3 = w3
