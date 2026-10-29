"""The Curve swap quoter: index translation and the dy verdict over Curve's routing.

A leaf shape with Curve's own routing inside it. The routing IS Curve's — a
direct pool read, a metapool-plus-base-pool read, or an all-underlying read are
measured by token membership, and only Curve has that membership notion — so it
travels with the quoter rather than being generalized. What the quoter does not
add is swap math: the Rust ``curve().get_dy`` / ``get_dy_underlying`` leaves own
it, and the ``ValueError`` they raise for a refused read is this family's one
verdict (``EVMRevertError``).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, cast

from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import EVMRevertError, InvalidSwapInputAmount
from degenbot.logging import logger

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot._ffi import CurveView
    from degenbot.curve.types import BasePoolPort, CurveStableswapPoolState
    from degenbot.erc20 import Erc20Token
    from degenbot.types.rpc_types import BlockIdentifier


class CurveQuoteHost(Protocol):
    """The companion reads the Curve quoter needs."""

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        """The pool's own coins."""
        ...

    @property
    def tokens_underlying(self) -> tuple[Erc20Token, ...] | None:
        """The underlying coins a metapool's reads can name."""
        ...

    @property
    def base_pool(self) -> BasePoolPort | None:
        """The metapool's base pool, if any."""
        ...


class CurveSwapQuoter:
    """The Curve family's swap read: membership routing, indices, and the dy verdict."""

    def __init__(
        self,
        host: CurveQuoteHost,
        arm: CurveView,
        *,
        resolve_block: Callable[[BlockIdentifier | None], int],
    ) -> None:
        """Bind the companion reads, the Curve arm, and the pool's block resolver."""
        self._host = host
        self._arm = arm
        self._resolve_block = resolve_block

    def exact_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: CurveStableswapPoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Calculate the `token_out` amount for an exact input of `quantity`.

        The zero-balance refusal is core-owned: the direct arm delegates to
        ``get_dy`` → the Rust ``curve().get_dy``, which raises where the
        balances are read.

        Returns:
            The computed output amount.

        Raises:
            DegenbotValueError: If neither token is held by the pool or its base pool.
            InvalidSwapInputAmount: If `quantity` is not positive.

        """
        block_number = self._resolve_block(block_identifier)

        if quantity <= 0:
            raise InvalidSwapInputAmount

        if override_state:
            logger.debug("Overrides applied:")
            logger.debug(f"Balances: {override_state.balances}")

        tokens = self._host.tokens
        tokens_used_this_pool = [
            token_in in tokens,
            token_out in tokens,
        ]

        base_pool = self._host.base_pool
        tokens_used_in_base_pool = []
        if base_pool is not None:
            tokens_used_in_base_pool = [
                token_in in base_pool.tokens,
                token_out in base_pool.tokens,
            ]

        if all(tokens_used_this_pool):
            return self.get_dy(
                i=tokens.index(token_in),
                j=tokens.index(token_out),
                dx=quantity,
                block_identifier=block_number,
                override_state=override_state,
            )
        if any(tokens_used_this_pool) and any(tokens_used_in_base_pool):
            return self._metapool_with_base(
                token_in,
                token_out,
                quantity,
                block_number,
                override_state,
            )
        if all(tokens_used_in_base_pool):
            tokens_underlying = self._host.tokens_underlying
            if TYPE_CHECKING:
                assert tokens_underlying is not None
            token_in_from_basepool = token_in in tokens_underlying
            token_out_from_basepool = token_out in tokens_underlying
            assert token_in_from_basepool or token_out_from_basepool

            return self._get_dy_underlying(
                i=tokens_underlying.index(token_in),
                j=tokens_underlying.index(token_out),
                dx=quantity,
                block_identifier=block_number,
                override_state=override_state,
            )

        msg = "Tokens not held by pool or in underlying base pool"
        raise DegenbotValueError(message=msg)  # pragma: no cover

    def get_dy(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """Calculate the current output ``dy`` given input ``dx`` over the pool's own coins.

        Returns:
            The computed output amount.

        Raises:
            EVMRevertError: If the Rust read refuses.

        """
        block_number = self._resolve_block(block_identifier)
        override_balances = list(override_state.balances) if override_state is not None else None
        try:
            return cast(
                "int",
                self._arm.get_dy(i, j, dx, block_number, override_balances),
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _get_dy_underlying(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """Metapool underlying ``dy`` — the Rust base-pool delegation.

        Returns:
            The computed output amount.

        Raises:
            EVMRevertError: If the Rust read refuses.

        """
        block_number = self._resolve_block(block_identifier)
        override_balances = list(override_state.balances) if override_state is not None else None
        try:
            return cast(
                "int",
                self._arm.get_dy_underlying(i, j, dx, block_number, override_balances),
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _metapool_with_base(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        block_number: int,
        override_state: CurveStableswapPoolState | None,
    ) -> int:
        """Resolve a swap that spans this metapool and its underlying base pool.

        The zero-balance refusal is core-owned (``curve().get_dy_underlying``
        raises where the balance vectors are read), so this only maps the
        combined underlying indices and delegates.

        Returns:
            The computed output amount.

        """
        tokens = self._host.tokens
        tokens_underlying = self._host.tokens_underlying
        if TYPE_CHECKING:
            assert tokens_underlying is not None

        token_in_from_metapool = token_in in tokens
        token_out_from_metapool = token_out in tokens

        return self._get_dy_underlying(
            i=(
                tokens.index(token_in)
                if token_in_from_metapool
                else tokens_underlying.index(token_in)
            ),
            j=(
                tokens.index(token_out)
                if token_out_from_metapool
                else tokens_underlying.index(token_out)
            ),
            dx=quantity,
            block_identifier=block_number,
            override_state=override_state,
        )
