"""The Balancer swap quoters: N-token pair reads over the Rust pair leaves.

A leaf shape: Balancer's swap math is a pure Rust leaf reached through the pool
handle's pair entry, so there is no door arm, no delta decode, and no post-state
rebuild. The quoter translates companion concepts into the leaf's terms (token
pair to indices, the hypothetical balances, the block's scaling factors) and maps
the leaf's verdict.

The stable species adds one verdict of its own: a ComposableStablePool whose
rates are construction-static may be quoting stale rates, which the pool reports
as ``StaleRateResult`` rather than passing off as an exact answer.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

from degenbot.exceptions.pool import StaleRateResult

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.balancer.types import BalancerV2PoolState
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.types.rpc_types import BlockIdentifier


class BalancerSwapQuoter:
    """The weighted pool's pair reads: token pair to indices, balances to the leaf."""

    def __init__(self, handle: Pool, tokens: tuple[Erc20Token, ...]) -> None:
        """Bind the pool handle and the pool's token tuple (index order)."""
        self._handle = handle
        self._tokens = tokens

    def exact_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: BalancerV2PoolState | None = None,
    ) -> int:
        """Calculate the `token_out` amount for an exact input of `quantity`.

        Returns:
            The computed output amount.

        """
        amount_out = cast(
            "int",
            self._handle.calculate_tokens_out_for_pair(
                index_in=self._tokens.index(token_in),
                index_out=self._tokens.index(token_out),
                amount_in=quantity,
                override_balances=self._override_balances(override_state),
            ),
        )
        self._verdict(amount_in=quantity, amount_out=amount_out)
        return amount_out

    def exact_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: BalancerV2PoolState | None = None,
    ) -> int:
        """Calculate the `token_in` amount required to take `quantity` of `token_out` out.

        Returns:
            The computed input amount.

        """
        amount_in = cast(
            "int",
            self._handle.calculate_tokens_in_for_pair(
                index_in=self._tokens.index(token_in),
                index_out=self._tokens.index(token_out),
                amount_out=quantity,
                override_balances=self._override_balances(override_state),
            ),
        )
        self._verdict(amount_in=amount_in, amount_out=quantity)
        return amount_in

    @staticmethod
    def _override_balances(override_state: BalancerV2PoolState | None) -> list[int] | None:
        """Render the hypothetical balances for the leaf (``None`` = the pool's own).

        Returns:
            The hypothetical balances, or ``None``.

        """
        return list(override_state.balances) if override_state is not None else None

    def _verdict(self, *, amount_in: int, amount_out: int) -> None:
        """Apply this species' verdict to a computed answer (the weighted leaf has none)."""


class BalancerStableSwapQuoter(BalancerSwapQuoter):
    """The stable pools' pair reads, over the same leaves plus their rate plumbing.

    A ComposableStablePool's rates move with yield accrual, so a quote against
    construction-static rates is not an exact answer — the leaf still computes it,
    and this quoter reports it as a ``StaleRateResult`` instead of returning it.
    """

    def __init__(
        self,
        handle: Pool,
        tokens: tuple[Erc20Token, ...],
        *,
        stale_rates: bool,
        resolve_scaling_factors: Callable[[BlockIdentifier | None], tuple[int, ...]] | None = None,
    ) -> None:
        """Bind the pair-read seam plus the pool's rate facts.

        ``resolve_scaling_factors`` is ``None`` when the pool's rates are
        construction-static, so the leaf borrows the registration's factors.
        """
        super().__init__(handle, tokens)
        self._stale_rates = stale_rates
        self._resolve_scaling_factors = resolve_scaling_factors

    def exact_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: BalancerV2PoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Calculate the `token_out` amount for an exact input of `quantity`.

        Returns:
            The computed output amount.

        """
        amount_out = cast(
            "int",
            self._handle.calculate_tokens_out_for_pair(
                index_in=self._tokens.index(token_in),
                index_out=self._tokens.index(token_out),
                amount_in=quantity,
                override_balances=self._override_balances(override_state),
                override_scaling_factors=self._scaling_factors(block_identifier),
            ),
        )
        self._verdict(amount_in=quantity, amount_out=amount_out)
        return amount_out

    def exact_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: BalancerV2PoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Calculate the `token_in` amount required to take `quantity` of `token_out` out.

        Returns:
            The computed input amount.

        """
        amount_in = cast(
            "int",
            self._handle.calculate_tokens_in_for_pair(
                index_in=self._tokens.index(token_in),
                index_out=self._tokens.index(token_out),
                amount_out=quantity,
                override_balances=self._override_balances(override_state),
                override_scaling_factors=self._scaling_factors(block_identifier),
            ),
        )
        self._verdict(amount_in=amount_in, amount_out=quantity)
        return amount_in

    def _scaling_factors(self, block_identifier: BlockIdentifier | None) -> list[int] | None:
        """Resolve the block's scaling factors for the leaf.

        Returns:
            The rate-multiplied factors, or ``None`` for a static-rate pool (the
            leaf then borrows the registration's own).

        """
        if self._resolve_scaling_factors is None:
            return None
        return list(self._resolve_scaling_factors(block_identifier))

    def _verdict(self, *, amount_in: int, amount_out: int) -> None:
        """Report a quote that rests on construction-static rates.

        Raises:
            StaleRateResult: If this pool's rates cannot be refreshed.

        """
        if self._stale_rates:
            raise StaleRateResult(amount_in=amount_in, amount_out=amount_out)
