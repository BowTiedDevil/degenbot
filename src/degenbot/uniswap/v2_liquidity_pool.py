"""UniswapV2Pool: constant-product AMM with reserve tracking."""

from __future__ import annotations

import dataclasses
from fractions import Fraction
from typing import TYPE_CHECKING, Any, ClassVar, Self, cast

from degenbot.aerodrome.math import (
    calc_exact_in_stable_camelot as _rs_calc_exact_in_stable_camelot,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    InvalidSwapInputAmount,
    LiquidityPoolError,
)
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap import SwapRefusalKind, SwapRefused, swap_refusal_to_error
from degenbot.uniswap.v2_pool_calc import UniswapV2PoolCalc
from degenbot.uniswap.v2_pool_state import V2PoolState
from degenbot.uniswap.v2_types import (
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)

if TYPE_CHECKING:
    from degenbot.types import DexIdentity, Pool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress


class UniswapV2Pool(V2PoolState, UniswapV2PoolCalc, AbstractLiquidityPool):
    """A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant."""

    variant: ClassVar[str | None] = None

    # Camelot solidly-stable strategy. The
    # companion sets these as INSTANCE attrs off the `Pool` handle's
    # `V2PoolDescriptor` (see `from_handle`) — `stable_swap` selects the
    # stable calc branch for Camelot stable pools; False
    # otherwise. ``fee_denominator`` carries Camelot's integer fee scaling
    # (used by the stable math); None for non-Camelot V2 (volatile calc ignores
    # it). The class-level defaults are the read path ONLY for instances that
    # bypassed `from_handle` (none — the construction guard blocks `__init__`).
    stable_swap: bool = False
    fee_denominator: int | None = None

    # Instance attributes set in `from_handle` (the only construction seam —
    # `__init__` raises). Declared at class scope so the type checker tracks
    # them without inline annotations on the classmethod body
    _py_pool: Pool
    dex: DexIdentity
    address: ChecksummedAddress
    factory: ChecksummedAddress
    _fee_token0: Fraction
    _fee_token1: Fraction
    _token0: Erc20Token
    _token1: Erc20Token
    init_hash: str
    deployer: ChecksummedAddress
    name: str

    type PoolState = UniswapV2PoolState

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``UniswapV2Pool`` is a Python companion over a Rust-owned
        ``Pool`` handle. The handle can only be produced by
        registering a pool in a ``Bot`` — there is no way for a caller to
        hand-build one. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_v2_pool(...)``

        Both register the pool in Rust, obtain the ``Pool``
        handle, and wrap it via :meth:`from_handle` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or make_v2_pool(...) "
            "(tests) to register the pool in Rust and obtain the "
            "Pool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def from_handle(cls, py_pool: Pool) -> Self:
        """Wrap a Rust-owned ``Pool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). The
        handle is self-describing: every identity field (address, factory,
        fees, tokens, dex preset, stable strategy) is read off it — no
        identity is passed as constructor args. Rust owns the mutable state
        (reserves + reorg journal) as ``V2PoolState`` and the immutable
        registration metadata as ``V2PoolDescriptor``; this companion reads
        both through ``self._py_pool``.

        Only ``Bot.build_pool()`` (production) and ``make_v2_pool`` (tests)
        should call this — they have already registered the pool (and, per
        ADR-006, its tokens in the same ``Bot``) and obtained the handle.
        ``cls`` is used so subclasses that only set ClassVars (the documented
        extension contract) inherit this seam and produce instances of the
        subclass.

        Returns:
            A ``cls`` instance wrapping ``py_pool``.

        Raises:
            DegenbotValueError: If the handle is not a V2-family pool
                (``py_pool.variant`` is empty — the ``PoolEntry`` is not
                ``V2``), so the union-handle V2 getters would return
                empty/default identity.
            DegenbotValueError: If the handle has no ``DexIdentity`` preset
                (the pool was not registered with a variant) or the pool's
                tokens are not registered in the same ``Bot`` (ADR-006).

        """
        self: Self = cls.__new__(cls)
        self._py_pool = py_pool

        # Variant-family guard: the handle's ``variant`` getter reads the V2
        # ``PoolEntry`` identity and returns ``""`` for every non-V2 variant
        # (V3/V4/Curve/Balancer). Wrapping such a handle here would read
        # empty/default identity off the union handle (the leaky corner) and
        # later crash with a confusing ``ZeroDivisionError`` on
        # ``Fraction(denom - gamma, denom)`` when ``fee_tokenN`` yields ``(0, 0)``.
        # Fail fast with a clear message instead — the uniform precondition
        # check every ``from_handle`` seam uses (``pool_family`` dispatches on
        # the ``PoolEntry`` variant directly, so it is correct for every
        # registered family; the V2-only ``variant`` getter returns ``""`` for
        # non-V2 and can't serve as a cross-family guard).
        if py_pool.pool_family != "v2":
            msg = (
                "Pool handle is not a V2-family pool "
                f"(got pool_family {py_pool.pool_family!r}); "
                "UniswapV2Pool.from_handle requires a handle registered via "
                "register_v2_pool"
            )
            raise DegenbotValueError(message=msg)

        # DexIdentity preset — resolved from the registered variant tag by
        # Rust (``preset_for_variant``). Always present for a V2 pool
        # registered via ``register_v2_pool`` (which validates the variant).
        dex = py_pool.dex
        if dex is None:  # pragma: no cover
            msg = (
                "Pool handle has no DexIdentity preset; the pool "
                "must be registered with a variant via register_v2_pool"
            )
            raise DegenbotValueError(message=msg)
        self.dex = dex

        # Recover token companions from the SAME shared BotState (ADR-006):
        # ``get_token0``/``get_token1`` return ``Erc20Token`` handles for
        # tokens registered in the same ``Bot`` as the pool. Production
        # builders register tokens via the shared ``Erc20Builder`` (same
        # ``_py_bot``); the test factory registers them explicitly.
        py_token0 = py_pool.get_token0()
        py_token1 = py_pool.get_token1()
        if py_token0 is None or py_token1 is None:  # pragma: no cover
            msg = (
                "pool tokens must be registered in the same Bot as the pool "
                "(ADR-006): get_token0/get_token1 returned None"
            )
            raise DegenbotValueError(message=msg)
        self._token0 = Erc20Token.from_handle(py_token0)
        self._token1 = Erc20Token.from_handle(py_token1)

        self.address = get_checksum_address(py_pool.address)
        self.factory = get_checksum_address(py_pool.factory)

        # ``fee_tokenN`` on the handle is the RETAINED post-fee fraction
        # ``(gamma_numer, fee_denom)`` (Rust convention); the companion stores
        # the FEE ``Fraction`` (e.g. ``Fraction(3, 1000)`` for 0.3%), so the
        # conversion is ``Fraction(denom - gamma, denom)``.
        gamma0, denom0 = py_pool.fee_token0
        gamma1, denom1 = py_pool.fee_token1
        self._fee_token0 = Fraction(denom0 - gamma0, denom0)
        self._fee_token1 = Fraction(denom1 - gamma1, denom1)

        # Deployer/init_hash come from the dex preset (the only path now —
        # the handle carries the canonical deployment identity).
        self.init_hash = dex.init_hash
        self.deployer = get_checksum_address(dex.deployer)

        # Camelot stable strategy + integer fee scale: read off the handle's
        # descriptor (no longer class-level attrs mutated by the builder).
        self.stable_swap = py_pool.stable_swap
        self.fee_denominator = py_pool.fee_denominator

        fee_numerator_0 = 100 * self._fee_token0.numerator / self._fee_token0.denominator
        if self._fee_token0 == self._fee_token1:
            fee_string = f"{fee_numerator_0:.2f}"
        else:
            fee_string = (
                f"{fee_numerator_0:.2f}"
                f"/"
                f"{100 * self._fee_token1.numerator / self._fee_token1.denominator:.2f}"
            )
        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, {fee_string}%)"
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1})"  # ruff:ignore[line-too-long]

    @property
    def update_block(self) -> BlockNumber:
        """Update block.

        Returns:
            The block number of the most recent state update (from Rust).

        """
        return self._py_pool.reserve_pair().update_block

    @property
    def reserves_token0(self) -> int:
        """Reserves token0.

        Returns:
            The reserve amount for token0 (from Rust).

        """
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        """Reserves token1.

        Returns:
            The reserve amount for token1 (from Rust).

        """
        return self.state.reserves_token1

    @property
    def state(self) -> PoolState:
        """State.

        Returns:
            The current pool state, built from one atomic Rust snapshot
            (``_py_pool.snapshot()``) so a Rust-side ``sync_reserves``
            (pump update) can't interleave between the reserve reads.

        Raises:
            DegenbotValueError: If the pool is not registered in Rust (no
                V2 state to snapshot).

        """
        try:
            view = self._py_pool.reserve_pair()
        except ValueError as exc:
            msg = "No V2 pool state available (pool not registered in Rust)"
            raise DegenbotValueError(message=msg) from exc
        return cast(
            "UniswapV2PoolState",
            self.PoolState.__value__(
                address=self.address,
                reserves_token0=view.reserve0,
                reserves_token1=view.reserve1,
                block=view.update_block,
            ),
        )

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """Simulate an exact input swap.

        Returns:
            The simulation result with delta amounts and state transitions.

        Raises:
            DegenbotValueError: If token_in is unknown.
            InvalidSwapInputAmount: If token_in_quantity is not positive.
            swap_refusal_to_error: Renders a refusal that is not a
                swap-geometry verdict — the walk entered an unfetched word
                and the on-demand fetch failed (MissingLiquidityData).
            LiquidityPoolError: If the swap cannot compute against the pool state.

        """
        if token_in not in self.tokens:
            raise DegenbotValueError(message="token_in is unknown.")

        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        # One atomic snapshot drives the whole simulation: the read's deltas
        # and its end reserves both come from the swap-simulation door over
        # `state`, so a Rust-side sync_reserves (pump) can't interleave.
        state = override_state if override_state is not None else self.state
        zero_for_one = token_in == self._token0
        if self.stable_swap:
            # A Camelot-stable read refuses at the door (its arm needs the
            # `10**decimals` scales the V2 registration identity does not
            # carry), so this family keeps its Solidly leaf and its end state is
            # the same override reserves advanced by the deltas that leaf
            # produced.
            token_out_quantity = self.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_state=state,
            )
            token0_delta = -token_out_quantity if zero_for_one is False else token_in_quantity
            token1_delta = -token_out_quantity if zero_for_one is True else token_in_quantity
            return UniswapV2PoolSimulationResult(
                amount0_delta=token0_delta,
                amount1_delta=token1_delta,
                initial_state=state,
                final_state=dataclasses.replace(
                    state,
                    reserves_token0=state.reserves_token0 + token0_delta,
                    reserves_token1=state.reserves_token1 + token1_delta,
                    block=self.update_block if override_state is not None else None,
                ),
            )

        # The typed read (ADR-037/t2): a refusal is the overdraw verdict —
        # the requested input is not computable against those reserves.
        try:
            read = self._py_pool.simulate(
                zero_for_one=zero_for_one,
                amount_specified=-token_in_quantity,
                block=self.update_block,
                override_state=state,
            )
        except SwapRefused as exc:
            if exc.args[0] is not SwapRefusalKind.NotComputable:
                # An infrastructure failure (an unfetchable word), not a
                # swap-geometry verdict — render it honestly.
                raise swap_refusal_to_error(exc) from exc
            raise LiquidityPoolError(
                message=(
                    f"Input amount ({token_in_quantity}) overflows "
                    "uint256 swap math (on-chain revert)"
                ),
            ) from exc
        assert read.reserves_token0 is not None  # pragma: no cover — reserve-pair arm asserted
        assert read.reserves_token1 is not None  # pragma: no cover — reserve-pair arm asserted
        return UniswapV2PoolSimulationResult(
            amount0_delta=int(read.amount0_delta),
            amount1_delta=int(read.amount1_delta),
            initial_state=state,
            final_state=dataclasses.replace(
                state,
                reserves_token0=read.reserves_token0,
                reserves_token1=read.reserves_token1,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """Simulate exact output swap.

        Returns:
            The simulation result with delta amounts and state transitions.

        Raises:
            DegenbotValueError: If token_out is unknown.
            InvalidSwapInputAmount: If token_out_quantity is not positive.
            swap_refusal_to_error: Renders a refusal that is not a
                swap-geometry verdict — the walk entered an unfetched word
                and the on-demand fetch failed (MissingLiquidityData).
            LiquidityPoolError: If the requested output exceeds the pool reserves.

        """
        if token_out not in self.tokens:
            raise DegenbotValueError(message="token_out is unknown.")

        if token_out_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        # See `simulate_exact_input_swap`: one snapshot, one door read, and the
        # end state is the read's own post-swap reserves.
        state = override_state if override_state is not None else self.state
        zero_for_one = token_out == self._token1
        # The typed read (ADR-037/t2): a refusal is the overdraw verdict —
        # the requested output exceeds the pool's reserves.
        try:
            read = self._py_pool.simulate(
                zero_for_one=zero_for_one,
                amount_specified=token_out_quantity,
                block=self.update_block,
                override_state=state,
            )
        except SwapRefused as exc:
            if exc.args[0] is not SwapRefusalKind.NotComputable:
                # An infrastructure failure (an unfetchable word), not a
                # swap-geometry verdict — render it honestly.
                raise swap_refusal_to_error(exc) from exc
            reserves_out = state.reserves_token1 if zero_for_one else state.reserves_token0
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})",  # ruff:ignore[line-too-long]
            ) from exc
        assert read.reserves_token0 is not None  # pragma: no cover — reserve-pair arm asserted
        assert read.reserves_token1 is not None  # pragma: no cover — reserve-pair arm asserted
        return UniswapV2PoolSimulationResult(
            amount0_delta=int(read.amount0_delta),
            amount1_delta=int(read.amount1_delta),
            initial_state=state,
            final_state=dataclasses.replace(
                state,
                reserves_token0=read.reserves_token0,
                reserves_token1=read.reserves_token1,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current reserves.

        Strategy dispatch: Camelot stable pools
        (``stable_swap=True``) use the solidly-stable invariant with Camelot's
        k/get_y; all other V2 pools fall through to ``super()`` — the
        ``UniswapV2PoolCalc`` Rust-delegation path is unperturbed for
        the volatile majority.

        Returns:
            The expected output token amount.

        """
        if self.stable_swap:
            return self._calculate_tokens_out_from_tokens_in_stable_swap(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_state=override_state,
            )
        return super().calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=token_in_quantity,
            override_state=override_state,
        )

    def _calculate_tokens_out_from_tokens_in_stable_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Camelot solidly-stable swap calculation (folded from CamelotPoolCalc).

        Routes through the Rust ``degenbot-solidly-math`` core
        (``solidly_calc_exact_in_stable_camelot``), which bakes in Camelot's
        ``k_camelot``/``get_y_camelot`` variant.

        ``fee_tokenN`` is the FEE ``Fraction`` (e.g. ``Fraction(3, 1000)``).

        Returns:
            The computed integer value.

        """
        precision_multiplier_token0: int = 10**self.token0.decimals
        precision_multiplier_token1: int = 10**self.token1.decimals

        fee = self.fee_token0 if token_in == self.token0 else self.fee_token1
        token_in_index = 0 if token_in == self.token0 else 1

        if override_state is not None:
            reserves_token0 = override_state.reserves_token0
            reserves_token1 = override_state.reserves_token1
        else:
            reserves_token0 = self.reserves_token0
            reserves_token1 = self.reserves_token1

        return cast(
            "int",
            _rs_calc_exact_in_stable_camelot(
                token_in_quantity,
                token_in_index,
                reserves_token0,
                reserves_token1,
                precision_multiplier_token0,
                precision_multiplier_token1,
                fee.numerator,
                fee.denominator,
            ),
        )
