"""Chain identifiers and EVM type aliases — the ``eth_typing`` replacement home.

degenbot consumes only a small slice of ``eth_typing``: the ``ChainId``
int-enum (as hash-equal dict keys into deployment registries) and a handful
of address/hash/block/ABI annotation aliases. This module owns those so the
``eth_typing`` dependency can be dropped with a hard cutover.

Address and hash aliases are plain ``str`` (not ``NewType``): the runtime is
already plain strings (EIP-55 checksummed), matching
``degenbot.types.aliases`` and the ``degenbot._ffi`` stub contract.
"""

from collections.abc import Sequence
from enum import IntEnum
from typing import Literal, NotRequired, TypedDict


class ChainId(IntEnum):
    """Chain identifiers degenbot deploys against.

    ``IntEnum`` so members hash-equal their plain ints (deployment-registry
    lookups use both forms interchangeably).
    """

    ETH = 1
    FTM = 250
    AVAX = 43114
    ARB1 = 42161
    BASE = 8453


#: A hex-encoded string (with optional ``0x`` prefix) — was ``eth_typing.HexStr``.
type HexStr = str

#: A hex-encoded address string — was ``eth_typing.HexAddress``.
type HexAddress = str

#: A hex-encoded 32-byte hash — was ``eth_typing.Hash32``.
type Hash32 = str

#: A canonical address string — was ``eth_typing.Address``.
type Address = str

#: Block reference tags — was ``eth_typing.evm.BlockParams``.
BlockParams = Literal["latest", "earliest", "pending", "safe", "finalized"]


#: String representation of an ABI type — was ``eth_typing.TypeStr``.
type TypeStr = str


class ABIComponent(TypedDict):
    """TypedDict representing an ABI element component."""

    type: str
    name: NotRequired[str]
    components: NotRequired[Sequence["ABIComponent"]]


class ABIComponentIndexed(ABIComponent):
    """TypedDict for a component usable as a topic filter."""

    indexed: bool


class ABIEvent(TypedDict):
    """TypedDict to represent the ABI for an event (was ``eth_typing.ABIEvent``)."""

    name: str
    type: Literal["event"]
    anonymous: NotRequired[bool]
    inputs: NotRequired[Sequence[ABIComponentIndexed]]


__all__ = (
    "ABIComponent",
    "ABIComponentIndexed",
    "ABIEvent",
    "Address",
    "BlockParams",
    "ChainId",
    "Hash32",
    "HexAddress",
    "HexStr",
    "TypeStr",
)
