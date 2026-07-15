"""UniswapV2Pool: constant-product AMM with reserve tracking."""

import dataclasses
from fractions import Fraction
from typing import Any, ClassVar, Self
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot._ffi import PyDexIdentity, PyLiquidityPool
from degenbot._ffi import (
    solidly_calc_exact_in_stable_camelot as _rs_calc_exact_in_stable_camelot,
)
from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import ExternalUpdateError
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.v2_pool_calc import UniswapV2PoolCalc
from degenbot.uniswap.v2_pool_state import V2PoolState
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)


class UniswapV2Pool(PublisherMixin, V2PoolState, UniswapV2PoolCalc, AbstractLiquidityPool):
    """A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant."""

    variant: ClassVar[str | None] = None

    # Camelot solidly-stable strategy (ADR-005 slice 7 step 4a fold). The
    # companion sets these as INSTANCE attrs off the `PyLiquidityPool` handle's
    # `V2PoolDescriptor` (see `_from_py_pool`) — `stable_swap` selects the
    # stable calc + SolidlyStableHop branch for Camelot stable pools; False
    # otherwise. ``fee_denominator`` carries Camelot's integer fee scaling
    # (used by the stable math); None for non-Camelot V2 (volatile calc ignores
    # it). The class-level defaults are the read path ONLY for instances that
    # bypassed `_from_py_pool` (none — the construction guard blocks `__init__`).
    stable_swap: bool = False
    fee_denominator: int | None = None

    # Instance attributes set in `_from_py_pool` (the only construction seam —
    # `__init__` raises). Declared at class scope so the type checker tracks
    # them without inline annotations on the classmethod body
    _py_pool: PyLiquidityPool
    dex: PyDexIdentity
    address: ChecksumAddress
    factory: ChecksumAddress
    _fee_token0: Fraction
    _fee_token1: Fraction
    _token0: Erc20Token
    _token1: Erc20Token
    init_hash: str
    deployer: ChecksumAddress
    name: str
    _subscribers: WeakSet[Subscriber]

    type PoolState = UniswapV2PoolState

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # noqa: ARG002
        """Direct construction is forbidden.

        ``UniswapV2Pool`` is a Python companion over a Rust-owned
        ``PyLiquidityPool`` handle. The handle can only be produced by
        registering a pool in a ``PyBot`` — there is no way for a caller to
        hand-build one. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_v2_pool(...)``

        Both register the pool in Rust, obtain the ``PyLiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or make_v2_pool(...) "
            "(tests) to register the pool in Rust and obtain the "
            "PyLiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: PyLiquidityPool) -> Self:
        """Wrap a Rust-owned ``PyLiquidityPool`` handle as a Python companion.

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
        # check every ``_from_py_pool`` seam uses (``pool_family`` dispatches on
        # the ``PoolEntry`` variant directly, so it is correct for every
        # registered family; the V2-only ``variant`` getter returns ``""`` for
        # non-V2 and can't serve as a cross-family guard).
        if py_pool.pool_family != "v2":
            msg = (
                "PyLiquidityPool handle is not a V2-family pool "
                f"(got pool_family {py_pool.pool_family!r}); "
                "UniswapV2Pool._from_py_pool requires a handle registered via "
                "register_v2_pool"
            )
            raise DegenbotValueError(message=msg)

        # DexIdentity preset — resolved from the registered variant tag by
        # Rust (``preset_for_variant``). Always present for a V2 pool
        # registered via ``register_v2_pool`` (which validates the variant).
        dex = py_pool.dex
        if dex is None:  # pragma: no cover
            msg = (
                "PyLiquidityPool handle has no DexIdentity preset; the pool "
                "must be registered with a variant via register_v2_pool"
            )
            raise DegenbotValueError(message=msg)
        self.dex = dex

        # Recover token companions from the SAME shared BotState (ADR-006):
        # ``get_token0``/``get_token1`` return ``PyErc20Token`` handles for
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
        self._token0 = Erc20Token._from_py_token(py_token0)  # noqa: SLF001
        self._token1 = Erc20Token._from_py_token(py_token1)  # noqa: SLF001

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

        self._subscribers = WeakSet()
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1})"  # noqa: E501

    @property
    def update_block(self) -> BlockNumber:
        """Update block.

        Returns:
            The block number of the most recent state update (from Rust).

        """
        return self._py_pool.update_block

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
        snapshot = self._py_pool.snapshot()
        if snapshot is None:
            msg = "No V2 pool state available (pool not registered in Rust)"
            raise DegenbotValueError(message=msg)
        reserve0, reserve1, block = snapshot
        return self.PoolState.__value__(
            address=self.address,
            reserves_token0=reserve0,
            reserves_token1=reserve1,
            block=block,
        )

    def external_update(
        self,
        update: UniswapV2PoolExternalUpdate,
    ) -> None:
        """External update.

        Raises:
            ExternalUpdateError: If the update is for a past block.

        """
        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}",  # noqa: E501
            )

        self._py_pool.sync_reserves(
            reserve0=update.reserves_token0,
            reserve1=update.reserves_token1,
            block_number=update.block_number,
        )
        self._notify_subscribers(
            message=UniswapV2PoolStateUpdated(self.state),
        )

    def simulate_add_liquidity(
        self,
        added_reserves_token0: int,
        added_reserves_token1: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """Simulate adding liquidity.

        Returns:
            The simulation result with delta amounts and state transitions.

        """
        state = override_state if override_state is not None else self.state
        return UniswapV2PoolSimulationResult(
            amount0_delta=added_reserves_token0,
            amount1_delta=added_reserves_token1,
            initial_state=state,
            final_state=dataclasses.replace(
                state,
                reserves_token0=state.reserves_token0 + added_reserves_token0,
                reserves_token1=state.reserves_token1 + added_reserves_token1,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def simulate_remove_liquidity(
        self,
        removed_reserves_token0: int,
        removed_reserves_token1: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """Simulate removing liquidity.

        Returns:
            The simulation result with delta amounts and state transitions.

        """
        state = override_state if override_state is not None else self.state
        return UniswapV2PoolSimulationResult(
            amount0_delta=-removed_reserves_token0,
            amount1_delta=-removed_reserves_token1,
            initial_state=state,
            final_state=dataclasses.replace(
                state,
                reserves_token0=state.reserves_token0 - removed_reserves_token0,
                reserves_token1=state.reserves_token1 - removed_reserves_token1,
                block=self.update_block if override_state is not None else None,
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

        """
        if token_in not in self.tokens:
            raise DegenbotValueError(message="token_in is unknown.")

        # One atomic snapshot drives the whole simulation: calc + final_state
        # both read `state.reserves_*`, so a Rust-side sync_reserves (pump)
        # can't interleave between the calc and the delta computation.
        state = override_state if override_state is not None else self.state
        zero_for_one = token_in == self._token0
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

        """
        if token_out not in self.tokens:
            raise DegenbotValueError(message="token_out is unknown.")

        state = override_state if override_state is not None else self.state
        zero_for_one = token_out == self._token1

        token_in_quantity = self.calculate_tokens_in_from_tokens_out(
            token_out=token_out,
            token_out_quantity=token_out_quantity,
            override_state=state,
        )
        token0_delta = token_in_quantity if zero_for_one is True else -token_out_quantity
        token1_delta = token_in_quantity if zero_for_one is False else -token_out_quantity

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

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap.

        Returns:
            The simulation result with amounts and state transitions.

        Raises:
            DegenbotValueError: If tokens are unknown or mismatched.

        """
        v2_state: UniswapV2PoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, UniswapV2PoolState):
                msg = f"Expected UniswapV2PoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            v2_state = state_override

        if token_in == self._token0.address:
            token_in_obj = self._token0
            expected_token_out = self._token1.address
        elif token_in == self._token1.address:
            token_in_obj = self._token1
            expected_token_out = self._token0.address
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        if token_out != expected_token_out:
            msg = f"token_out {token_out} does not match expected {expected_token_out}"
            raise DegenbotValueError(message=msg)

        result = self.simulate_exact_input_swap(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=v2_state,
        )
        zero_for_one = token_in_obj == self._token0
        amount_out = -result.amount1_delta if zero_for_one else -result.amount0_delta
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=result.initial_state,
            final_state=result.final_state,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current reserves.

        Strategy dispatch (ADR-005 slice 7 step 4a fold): Camelot stable pools
        (``stable_swap=True``) use the solidly-stable invariant with Camelot's
        k/get_y; all other V2 pools fall through to ``super()`` — the
        ``UniswapV2PoolCalc`` Rust-delegation path (slice 5) is unperturbed for
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
        ``k_camelot``/``get_y_camelot`` variant (ADR-005 slice 7).

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

        return _rs_calc_exact_in_stable_camelot(
            token_in_quantity,
            token_in_index,
            reserves_token0,
            reserves_token1,
            precision_multiplier_token0,
            precision_multiplier_token1,
            fee.numerator,
            fee.denominator,
        )

    def to_hop_state(
        self,
        *,
        zero_for_one: bool,
        state_override: UniswapV2PoolState | None = None,
        token_in: Erc20Token | None = None,  # noqa: ARG002
        token_out: Erc20Token | None = None,  # noqa: ARG002
    ) -> HopType:
        """Convert to hop state.

        Returns:
            A ``SolidlyStableHop`` for Camelot stable pools (``stable_swap``),
            else a ``ConstantProductHop`` carrying the directional
            ``fee_out`` (Camelot allows asymmetric fees per direction).

        """
        # token_in/token_out unused — 2-token pools determine pair from zero_for_one.
        # Callers should ensure these match pool.token0/pool.token1 if provided.
        state = state_override if state_override is not None else self.state
        fee_in = self.extract_fee(zero_for_one=zero_for_one)
        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
            decimals_in = self.token0.decimals
            decimals_out = self.token1.decimals
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
            decimals_in = self.token1.decimals
            decimals_out = self.token0.decimals

        if self.stable_swap:

            def _camelot_stable_swap_fn(
                amount_in: int,
                /,
                _reserves0: int = state.reserves_token0,
                _reserves1: int = state.reserves_token1,
                _decimals0: int = 10**self.token0.decimals,
                _decimals1: int = 10**self.token1.decimals,
                _fee: Fraction = fee_in,
                _token_in: int = 0 if zero_for_one else 1,
            ) -> int:
                return _rs_calc_exact_in_stable_camelot(
                    amount_in,
                    _token_in,
                    _reserves0,
                    _reserves1,
                    _decimals0,
                    _decimals1,
                    _fee.numerator,
                    _fee.denominator,
                )

            return SolidlyStableHop(
                reserve_in=reserve_in,
                reserve_out=reserve_out,
                fee=fee_in,
                decimals_in=decimals_in,
                decimals_out=decimals_out,
                swap_fn=_camelot_stable_swap_fn,
            )

        fee_out = self.fee_token1 if zero_for_one else self.fee_token0
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_in,
            fee_out=fee_out,
        )

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> UniswapV2PoolSwapAmounts:
        """Build swap amount.

        Returns:
            The swap amounts object for encoding.

        """
        return UniswapV2PoolSwapAmounts(
            pool=self.address,
            amounts_in=(amount_in, 0) if zero_for_one else (0, amount_in),
            amounts_out=(0, amount_out) if zero_for_one else (amount_out, 0),
        )
