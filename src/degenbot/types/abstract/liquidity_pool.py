"""Abstract liquidity pool protocol definition."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING

from degenbot.types.address_comparable import AddressComparable

if TYPE_CHECKING:
    from degenbot._ffi import BotIo, Pool
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.chain import ChecksummedAddress


class AbstractLiquidityPool(AddressComparable, ABC):
    """AbstractLiquidityPool class."""

    address: ChecksummedAddress
    name: str
    _py_pool: Pool

    def refresh_via(self, io: BotIo, *, block_number: int | None = None) -> bool:
        """Fetch this pool's current on-chain state and apply it when it differs.

        Delegates to the pool handle's core-owned refresh entry: the driver
        supplies the `BotIo` façade and (optionally) the block, and the core
        owns the per-family fetch/compare/apply choreography. `block_number`
        defaults to the current chain head.

        `block_number` is a resolved block number (``int``) or ``None`` — the
        core refresh takes ``int | None`` and does not understand block-tag
        strings. Resolution of tags (``"latest"`` / ``"pending"`` / a block
        hash) to a number belongs at the driver seam that owns the RPC clock
        (``Bot.update`` resolves before calling here); that tag surface is
        deferred scope and is deliberately NOT built here.

        Returns:
            True when a differing state was applied, False when the on-chain
            state already matched (or the requested block was older than the
            pool's own clock).

        """
        return self._py_pool.refresh_via(io, block_number)

    @property
    @abstractmethod
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        ...

    def __str__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the object.

        """
        return self.name
