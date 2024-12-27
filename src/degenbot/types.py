from collections import OrderedDict
from dataclasses import dataclass
from numbers import Integral
from typing import Any, ClassVar, Protocol, Self, TypeAlias, TypeVar

from eth_typing import BlockNumber, ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes


class EvmInt(Integral):
    def __init__(self, value: Any) -> None:
        self.value = int(value)

    def __hash__(self) -> int:
        return hash(self.value)

    def __abs__(self) -> Self:
        return EvmInt(self.value.__abs__())

    def __ceil__(self) -> Self:
        return EvmInt(self.value.__ceil__())

    def __floor__(self) -> Self:
        return EvmInt(self.value.__floor__())

    def __int__(self) -> int:
        return self.value

    def __neg__(self) -> Self:
        return EvmInt(self.value.__neg__())

    def __pos__(self) -> Self:
        return EvmInt(self.value.__pos__())

    def __pow__(self, exponent: Any, modulus: Any = None) -> Self:
        return EvmInt(self.value.__pow__(int(exponent), modulus))

    def __rpow__(self, other: Any) -> Self:
        return EvmInt(int(other).__pow__(self.value))

    def __floordiv__(self, other: Any) -> Self:
        if not isinstance(other, int):
            other = int(other)

        if self == 0 or other == 0:
            return EvmInt(0)  # EVM behavior for division by zero is to return zero
        if self > 0 and other > 0:
            return EvmInt(self.value.__floordiv__(other))
        if self < 0 and other < 0:
            return EvmInt(abs(self.value).__floordiv__(abs(other)))

        return EvmInt(-(abs(self.value).__floordiv__(abs(other))))

    def __rfloordiv__(self, other: object) -> Self:
        return EvmInt(other).__floordiv__(self.value)

    def __truediv__(self, other: object) -> Self:
        return self.__floordiv__(other)

    def __rtruediv__(self, other: object) -> Self:
        return EvmInt(other) / self

    def __add__(self, other: object) -> Self:
        return EvmInt(self.value.__add__(int(other)))

    def __radd__(self, other: object) -> Self:
        return EvmInt(int(other).__add__(self.value))

    def __and__(self, other: object) -> Self:
        return EvmInt(self.value.__and__(int(other)))

    def __rand__(self, other: object) -> Self:
        return EvmInt(int(other).__and__(self.value))

    def __or__(self, other: object) -> Self:
        return EvmInt(self.value.__or__(int(other)))

    def __ror__(self, other: object) -> Self:
        return EvmInt(other).__or__(self.value)

    def __round__(self) -> Self:
        return EvmInt(self.value.__round__())

    def __trunc__(self) -> Self:
        return EvmInt(self.value.__trunc__())

    def __invert__(self) -> Self:
        return EvmInt(self.value.__invert__())

    def __xor__(self, other: object) -> Self:
        return EvmInt(self.value ^ EvmInt(other).value)

    def __rxor__(self, other: object) -> Self:
        return EvmInt(EvmInt(other).value ^ self.value)

    def __le__(self, other: object) -> bool:
        return self.value.__le__(int(other))

    def __lt__(self, other: object) -> bool:
        return self.value.__lt__(int(other))

    def __eq__(self, other: object) -> bool:
        return self.value.__eq__(int(other))

    def __ge__(self, other: object) -> bool:
        return self.value.__ge__(int(other))

    def __gt__(self, other: object) -> bool:
        return self.value.__gt__(int(other))

    def __mod__(self, other: object) -> Self:
        if other == 0:
            return 0

        return EvmInt(self.value.__mod__(int(other)))

    def __rmod__(self, other: object) -> Self:
        return EvmInt(other).__mod__(self.value)

    def __mul__(self, other: object) -> Self:
        return EvmInt(self.value.__mul__(int(other)))

    def __rmul__(self, other: object) -> Self:
        return self.__mul__(other)

    def __sub__(self, other: object) -> Self:
        return EvmInt(self.value.__sub__(int(other)))

    def __rsub__(self, other: object) -> Self:
        return EvmInt(other).__sub__(self.value)

    def __lshift__(self, places: object) -> Self:
        return EvmInt(self.value.__lshift__(int(places)))

    def __rlshift__(self, other: object) -> Self:
        return EvmInt(other).__lshift__(self.value)

    def __rshift__(self, places: object) -> Self:
        return EvmInt(self.value.__rshift__(int(places)))

    def __rrshift__(self, other: object) -> Self:
        return EvmInt(other).__rshift__(self.value)

    def __repr__(self) -> str:
        return f"{self.__class__.__name__}({self.value})"


class Message:
    """
    A message sent by a `Publisher` to a `Subscriber`
    """


class TextMessage(Message):
    def __init__(self, text: str) -> None:
        self.text = text

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, TextMessage):
            return NotImplemented
        return self.text == other.text

    def __repr__(self) -> str:
        return f"PlaintextMessage(text={self.text})"

    def __str__(self) -> str:
        return self.text


class Publisher(Protocol):
    """
    Can send a `Message` to a `Subscriber`
    """

    _subscribers: set["Subscriber"]

    def subscribe(self, subscriber: "Subscriber") -> None:
        """
        Subscribe to receive messages from this `Publisher`
        """

    def unsubscribe(self, subscriber: "Subscriber") -> None:
        """
        Stop receiving messages from this `Publisher`
        """


class PublisherMixin:
    """
    A set of default methods to accept subscribe & unsubscribe requests. Classes using this mixin
    meet the `Publisher` protocol requirements.
    """

    def subscribe(self: Publisher, subscriber: "Subscriber") -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: "Subscriber") -> None:
        self._subscribers.discard(subscriber)


class Subscriber(Protocol):
    """
    Can subscribe to messages from a `Publisher`
    """

    def notify(self, publisher: Publisher, message: Message) -> None:
        """
        Deliver `message` to `Subscriber`
        """


class AbstractArbitrage:
    id: str


@dataclass(slots=True, frozen=True)
class AbstractExchangeDeployment:
    name: str
    chain_id: int


class AbstractManager:
    """
    Base class for managers that generate, track and distribute various helper classes
    """


class AbstractPoolManager:
    """
    Base class for liquidity pool managers. The class instance dict and get_instance method are
    mechanisms for implementing a singleton strategy so only one pool manager is created for a given
    DEX factory.
    """

    # All pool managers are associated with certain types of pools. Specifying them as a class-level
    # alias allows the type hints to be generalized. Each concrete pool manager should set this
    # class attribute
    Pool: TypeAlias = "AbstractLiquidityPool"

    instances: ClassVar[
        dict[
            tuple[int, ChecksumAddress],
            Self,
        ]
    ] = {}

    @classmethod
    def get_instance(cls, factory_address: str, chain_id: int) -> Self | None:
        return cls.instances.get((chain_id, to_checksum_address(factory_address)))


class AbstractPoolUpdate: ...


@dataclass(slots=True, frozen=True, kw_only=True)
class AbstractPoolState:
    pool: ChecksumAddress
    block: BlockNumber | None


class AbstractSimulationResult: ...


class AbstractLiquidityPool:
    address: ChecksumAddress
    name: str

    def __eq__(self, other: object) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address == other.address
            case HexBytes():
                return self.address.lower() == other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() == "0x" + other.hex().lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: Any) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address < other.address
            case HexBytes():
                return self.address.lower() < other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() < "0x" + other.hex().lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: Any) -> bool:
        match other:
            case AbstractLiquidityPool():
                return self.address > other.address
            case HexBytes():
                return self.address.lower() > other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() > "0x" + other.hex().lower()
            case str():
                return self.address.lower() > other.lower()
            case _:
                return NotImplemented

    def __hash__(self) -> int:
        return hash(self.address)

    def __str__(self) -> str:
        return self.name


class AbstractErc20Token:
    address: ChecksumAddress
    symbol: str
    name: str
    decimals: int

    def __eq__(self, other: object) -> bool:
        match other:
            case AbstractErc20Token():
                return self.address == other.address
            case HexBytes():
                return self.address.lower() == other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() == "0x" + other.hex().lower()
            case str():
                return self.address.lower() == other.lower()
            case _:
                return NotImplemented

    def __lt__(self, other: Any) -> bool:
        match other:
            case AbstractErc20Token():
                return self.address < other.address
            case HexBytes():
                return self.address.lower() < other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() < "0x" + other.hex().lower()
            case str():
                return self.address.lower() < other.lower()
            case _:
                return NotImplemented

    def __gt__(self, other: Any) -> bool:
        match other:
            case AbstractErc20Token():
                return self.address > other.address
            case HexBytes():
                return self.address.lower() > other.to_0x_hex().lower()
            case bytes():
                return self.address.lower() > "0x" + other.hex().lower()
            case str():
                return self.address.lower() > other.lower()
            case _:
                return NotImplemented

    def __hash__(self) -> int:
        return hash(self.address)

    def __str__(self) -> str:
        return self.symbol


class AbstractRegistry: ...


class AbstractTransaction: ...


KT = TypeVar("KT")
VT = TypeVar("VT")


class BoundedCache(OrderedDict[KT, VT]):
    """
    A cache holding key-value pairs, tracked by entry order. The cache automatically removes old
    items if the number of items would exceed the maximum number of entries set by `max_items`.

    Setting a value at an existing key will overwrite that value without affecting ordering.
    """

    def __init__(self, max_items: int) -> None:
        super().__init__()
        self.max_items = max_items

    def __reduce__(self) -> tuple[Any, ...]:
        state = super().__reduce__()
        return (
            state[0],
            (self.max_items,),  # max_items argument must be provided to properly unpickle
            None,
            None,
            state[4],
        )

    def __setitem__(self, key: KT, value: VT) -> None:
        super().__setitem__(key, value)
        if len(self) > self.max_items:
            self.popitem(last=False)

    def copy(self) -> Self:
        new_copy = self.__class__(max_items=self.max_items)
        for k, v in self.items():
            new_copy[k] = v
        return new_copy
