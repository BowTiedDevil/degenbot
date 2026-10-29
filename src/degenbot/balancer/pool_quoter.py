"""The Balancer swap quoters: N-token pair reads over the Rust pair leaves.

A leaf shape: Balancer's swap math is a pure Rust leaf reached through the pool
handle's pair entry, so there is no door arm, no delta decode, and no post-state
rebuild. The quoter translates companion concepts into the leaf's terms (token
pair to indices, the hypothetical balances, the block's scaling factors) and maps
the leaf's verdict.

Every read answers a :class:`QuoteVerdict` — the computed amount pair plus the
species' fact about how exact it is — so an answer never travels inside an
exception. The stable species adds one fact of its own: a ComposableStablePool
whose rates are construction-static may be quoting stale rates, so its verdict
is not exact. :func:`answer` is the companions' boundary render, where such a
verdict becomes ``StaleRateResult``.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, cast

from degenbot.exceptions.pool import StaleRateResult

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.balancer.types import BalancerV2PoolState
    from degenbot.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.types.rpc_types import BlockIdentifier


@dataclasses.dataclass(slots=True, frozen=True)
class QuoteVerdict:
    """One computed swap answer and the species' fact about how exact it is.

    ``amount_in`` and ``amount_out`` are the leaf's computed pair;
    ``stale_rates`` states that the answer rests on rates no live provider
    refreshed. An exact answer carries ``stale_rates=False``, so trust reads off
    the value rather than off an exception wrapped around the amounts.
    """

    amount_in: int
    amount_out: int
    stale_rates: bool = False


def answer(verdict: QuoteVerdict, *, exact_input: bool) -> int:
    """Answer a companion swap verb with the amount that verb names.

    The companions' ``calculate_tokens_*`` verbs are the public boundary and
    keep returning an ``int``. A quote that is not exact is therefore refused
    here, with the computed amounts readable on the exception — the contract
    callers catch (README documents the ``except StaleRateResult`` pattern). The
    quoter states the caveat as a value; rendering it is this seam's policy.

    Returns:
        The amount the verb answers with: an exact-input request is answered with
        the output amount, an exact-output request with the input amount.

    Raises:
        StaleRateResult: If the quote rests on construction-static rates.

    """
    if verdict.stale_rates:
        raise StaleRateResult(amount_in=verdict.amount_in, amount_out=verdict.amount_out)
    return verdict.amount_out if exact_input else verdict.amount_in


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
    ) -> QuoteVerdict:
        """Calculate the `token_out` amount for an exact input of `quantity`.

        Returns:
            The computed verdict (this leaf's answers are exact).

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
        return self._verdict(amount_in=quantity, amount_out=amount_out)

    def exact_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: BalancerV2PoolState | None = None,
    ) -> QuoteVerdict:
        """Calculate the `token_in` amount required to take `quantity` of `token_out` out.

        Returns:
            The computed verdict (this leaf's answers are exact).

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
        return self._verdict(amount_in=amount_in, amount_out=quantity)

    @staticmethod
    def _override_balances(override_state: BalancerV2PoolState | None) -> list[int] | None:
        """Render the hypothetical balances for the leaf (``None`` = the pool's own).

        Returns:
            The hypothetical balances, or ``None``.

        """
        return list(override_state.balances) if override_state is not None else None

    def _verdict(  # ruff:ignore[no-self-use]
        self,
        *,
        amount_in: int,
        amount_out: int,
    ) -> QuoteVerdict:
        """State this species' verdict on a computed answer (the weighted leaf has none).

        Returns:
            The verdict: the computed pair, exact.

        """
        return QuoteVerdict(amount_in=amount_in, amount_out=amount_out)


class BalancerStableSwapQuoter(BalancerSwapQuoter):
    """The stable pools' pair reads, over the same leaves plus their rate plumbing.

    A ComposableStablePool's rates move with yield accrual, so a quote against
    construction-static rates is not an exact answer — the leaf still computes it,
    and this quoter states the stale-rate fact on the verdict rather than
    refusing the answer itself.
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
    ) -> QuoteVerdict:
        """Calculate the `token_out` amount for an exact input of `quantity`.

        Returns:
            The computed verdict, carrying the stale-rate fact when this pool's
            rates are construction-static.

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
        return self._verdict(amount_in=quantity, amount_out=amount_out)

    def exact_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        quantity: int,
        override_state: BalancerV2PoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> QuoteVerdict:
        """Calculate the `token_in` amount required to take `quantity` of `token_out` out.

        Returns:
            The computed verdict, carrying the stale-rate fact when this pool's
            rates are construction-static.

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
        return self._verdict(amount_in=amount_in, amount_out=quantity)

    def _scaling_factors(self, block_identifier: BlockIdentifier | None) -> list[int] | None:
        """Resolve the block's scaling factors for the leaf.

        Returns:
            The rate-multiplied factors, or ``None`` for a static-rate pool (the
            leaf then borrows the registration's own).

        """
        if self._resolve_scaling_factors is None:
            return None
        return list(self._resolve_scaling_factors(block_identifier))

    def _verdict(self, *, amount_in: int, amount_out: int) -> QuoteVerdict:
        """State whether this pool's quote rests on construction-static rates.

        Returns:
            The verdict: the computed pair, carrying the stale-rate fact when this
            pool's rates are construction-static.

        """
        return QuoteVerdict(
            amount_in=amount_in,
            amount_out=amount_out,
            stale_rates=self._stale_rates,
        )
