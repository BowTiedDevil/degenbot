from abc import ABC
from eth_typing import ChecksumAddress


class ArbitrageHelper(ABC):
    pass


class HelperManager(ABC):
    """
    An abstract base class for managers that generate, track and distribute various helper classes
    """

    pass


class PoolHelper(ABC):
    address: ChecksumAddress


class TokenHelper(ABC):
    pass


class TransactionHelper(ABC):
    pass
