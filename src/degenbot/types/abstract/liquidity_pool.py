"""Abstract liquidity pool protocol definition."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING

from degenbot.types.address_comparable import AddressComparable

from degenbot.exceptions.base import DegenbotValueError

if TYPE_CHECKING:
    from degenbot._ffi import BotIo, Pool
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.types.pool_type import PoolTypeDescriptor


class AbstractLiquidityPool(AddressComparable, ABC):
    """AbstractLiquidityPool class."""

    address: ChecksummedAddress
    name: str
    _py_pool: Pool
    _bound_pool_type: PoolTypeDescriptor | None = None

    @property
    def pool_type(self) -> PoolTypeDescriptor:
        """The immutable resolved identity of this pool.

        Family/variant/kind/factory as resolved at the single build entry —
        the identity that governs swap math (fee model, storage layout). The
        concrete class is not identity: ``sushiswap-v3`` deployments share
        ``UniswapV3Pool`` with vanilla v3 by design. Read-only by construction.
        """
        if self._bound_pool_type is None:
            msg = (
                f"{type(self).__name__}({self.address}) carries no bound identity; "
                "wrap through Bot.build_pool() or bind_pool_type()."
            )
            raise DegenbotValueError(message=msg)
        return self._bound_pool_type

    def bind_pool_type(self, descriptor: PoolTypeDescriptor) -> None:
        """Bind the resolved pool identity exactly once (builder-set).

        Registration metadata is immutable for the life of the pool, like the
        core's own registration identity. Re-binding the identical value is a
        no-op (the get-or-register path answers an existing handle); re-binding
        a different value is a seam bug and raises.
        """
        if self._bound_pool_type is not None and self._bound_pool_type != descriptor:
            msg = (
                f"{type(self).__name__}({self.address}) identity already bound to "
                f"{self._bound_pool_type!r}; refusing to re-bind {descriptor!r}"
            )
            raise DegenbotValueError(message=msg)
        self._bound_pool_type = descriptor

    def refresh_via(self, io: BotIo, *, block_number: int | str | None = None) -> bool:
        """Fetch this pool's current on-chain state and apply it when it differs.

        Delegates to the pool handle's core-owned refresh entry: the driver
        supplies the `BotIo` façade and (optionally) the block, and the core
        owns the per-family fetch/compare/apply choreography. `block_number`
        defaults to the current chain head.

        `block_number` is a block number (``int``), one canonical tag string
        (``"earliest"`` / ``"latest"`` / ``"pending"``), or ``None``. The tag
        becomes a number at the handle seam that owns the RPC clock — it is
        never coerced to the head, and never drops to ``None``; the core
        refresh entry stays numeric. A string that is not a canonical tag is
        refused.

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
