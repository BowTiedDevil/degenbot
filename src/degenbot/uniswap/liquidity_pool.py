"""LiquidityPool: constant-product AMM with reserve tracking."""

import dataclasses
from fractions import Fraction
from typing import Any, ClassVar
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts
from degenbot.calculations.camelot import get_y_camelot, k_camelot
from degenbot.calculations.solidly_stable import calc_exact_in_stable
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyDexIdentity, PyLiquidityPool
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    ExternalUpdateError,
    NoPoolStateAvailable,
)
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.log_decoders import V2_SYNC_TOPIC, decode_v2_sync
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v2_functions import (
    generate_v2_pool_address,
)
from degenbot.uniswap.v2_pool_calc import UniswapV2PoolCalc
from degenbot.uniswap.v2_pool_state import V2PoolState
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)


class LiquidityPool(
    PublisherMixin, PoolPickleMixin, V2PoolState, UniswapV2PoolCalc, AbstractLiquidityPool
):
    """A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant."""

    variant: ClassVar[str | None] = None

    # Camelot solidly-stable strategy (ADR-005 slice 7 step 4a fold). Default
    # False for all non-Camelot V2 pools — the stable calc + SolidlyStableHop
    # branch only fire when a builder flips this (Camelot stable pools).
    # ``fee_denominator`` carries Camelot's integer fee scaling (used by the
    # stable math); None for non-Camelot V2 (volatile calc ignores it).
    stable_swap: bool = False
    fee_denominator: int | None = None

    LOG_HANDLERS: ClassVar[dict[str, Any]] = {
        V2_SYNC_TOPIC: decode_v2_sync,
    }

    type PoolState = UniswapV2PoolState

    UNISWAP_V2_MAINNET_POOL_INIT_HASH = (
        "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
    )

    # Pickle (TRANSIENT — removed by TODO-cddc72d1 / Slice 15): drop the
    # non-picklable Rust handle (``_py_pool`` wraps an ``Arc<RwLock<Bot>>``)
    # and the weakref subscriber set. Unpickled pools are detached snapshots
    # (no live Rust handle; state reads raise ``AttributeError``). Slice 15
    # retires Python-pickle multiprocessing entirely (replaced by Rust-side
    # parallel solve fan-out) and deletes ``PoolPickleMixin`` + these tests.
    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_subscribers",
        "_py_pool",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_subscribers": WeakSet,
    }

    def __init__(
        self,
        py_pool: PyLiquidityPool,
        *,
        address: ChecksumAddress | str,
        token0: Erc20Token,
        token1: Erc20Token,
        dex: PyDexIdentity | None = None,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        factory: str | None = None,
        fee_token0: Fraction | None = None,
        fee_token1: Fraction | None = None,
    ) -> None:
        """I/O-free companion over a ``PyLiquidityPool`` handle (ADR-005).

        Rust owns the mutable state (reserves + reorg journal) as
        ``V2PoolState``; this companion reads it through ``self._py_pool``
        (via the atomic ``snapshot()``) and delegates ``external_update``
        (Sync) / discard / restore to the handle. Immutable identity (tokens,
        factory, fees) stays Python-side this slice — calc (slice 5) and
        identity (later) follow.

        ``dex`` (ADR-005 slice 7 step 3) carries the canonical DexIdentity
        preset: the variant tag, reserves ABI shape, canonical-chain
        factory/init-hash, + default fees. When ``dex`` is provided AND the
        explicit ``factory``/``init_hash``/``deployer_address``/``fee_token0``/
        ``fee_token1`` params are ``None``, the preset fills them in. Explicit
        params always take precedence — so existing callers (which pass all
        explicit params) are unaffected.

        Construct via ``Bot.build_pool()`` (which registers in Rust and
        hands the handle here); tests use ``make_v2_pool``.

        Raises:
            DegenbotValueError: If ``factory`` is not provided explicitly
                AND not resolvable from ``dex``.

        """
        self._py_pool = py_pool
        self.dex = dex

        # Fill in None params from the dex preset, if provided (explicit > dex
        # > class-default precedence). ``dex.fee_tokenN`` is the RETAINED
        # post-fee fraction ``(gamma_numer, fee_denom)`` (Rust convention); the
        # Python companion stores the FEE ``Fraction`` (e.g. ``Fraction(3, 1000)``
        # for 0.3%), so the conversion is ``Fraction(denom - gamma, denom)``.
        if dex is not None:
            if factory is None:
                factory = dex.factory
            if init_hash is None:
                init_hash = dex.init_hash
            if deployer_address is None:
                deployer_address = dex.deployer
            if fee_token0 is None:
                gamma0, denom0 = dex.fee_token0
                fee_token0 = Fraction(denom0 - gamma0, denom0)
            if fee_token1 is None:
                gamma1, denom1 = dex.fee_token1
                fee_token1 = Fraction(denom1 - gamma1, denom1)

        if factory is None:
            msg = "factory must be provided explicitly or resolvable from dex"
            raise DegenbotValueError(message=msg)
        assert fee_token0 is not None
        assert fee_token1 is not None

        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else token0.chain_id
        self._token0 = token0
        self._token1 = token1
        self.factory = get_checksum_address(factory)
        self._fee_token0 = fee_token0
        self._fee_token1 = fee_token1

        # Derive deployer/init_hash from constructor args, the dex preset, or
        # class defaults (in that precedence order).
        self.init_hash = (
            init_hash if init_hash is not None else self.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        )
        self.deployer = get_checksum_address(deployer_address or self.factory)

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

        self._subscribers: WeakSet[Subscriber] = WeakSet()

    @property
    def chain_id(self) -> int | None:
        """Return chain id.

        Returns:
            The chain ID, or None if not set.

        """
        return self._chain_id

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1})"  # noqa: E501

    def _verified_address(self) -> ChecksumAddress:
        """Compute the deterministic pool address via CREATE2.

        Returns:
            The checksummed address that should match this pool's address.

        """
        return generate_v2_pool_address(
            deployer_address=self.deployer,
            token_addresses=(self._token0.address, self._token1.address),
            init_hash=self.init_hash,
        )

    @property
    def update_block(self) -> BlockNumber:
        """Update block.

        Returns:
            The block number of the most recent state update (from Rust).

        """
        return self._py_pool.update_block

    @property
    def reserves_token0(self) -> int:
        """Return reserves token0.

        Returns:
            The reserve amount for token0 (from Rust).

        """
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        """Return reserves token1.

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

    @staticmethod
    def swap_is_viable(
        state: PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Swap is viable.

        Returns:
            True if a swap can proceed with the given state, False otherwise.

        """
        if state.reserves_token0 == 0 or state.reserves_token1 == 0:
            return False
        return state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1

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
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}"  # noqa: E501
            )

        if (
            update.reserves_token0,
            update.reserves_token1,
        ) == (
            self.reserves_token0,
            self.reserves_token1,
        ):
            return

        self._py_pool.sync_reserves(
            reserve0=update.reserves_token0,
            reserve1=update.reserves_token1,
            block_number=update.block_number,
        )
        self._notify_subscribers(
            message=UniswapV2PoolStateUpdated(self.state),
        )

    def discard_states_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """Discard cached states earlier than the given block.

        Raises:
            NoPoolStateAvailable: If the target is past the newest delta.

        """
        try:
            self._py_pool.discard_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    def restore_state_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """Restore the last pool state recorded prior to a target block.

        Raises:
            NoPoolStateAvailable: If no state exists prior to the target block.

        """
        try:
            self._py_pool.restore_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e
        self._notify_subscribers(message=UniswapV2PoolStateUpdated(self.state))

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

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: UniswapV2PoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap for output.

        Returns:
            The simulation result with amounts and state transitions.

        Raises:
            DegenbotValueError: If tokens are unknown or mismatched.

        """
        if token_out == self._token0.address:
            token_out_obj = self._token0
            expected_token_in = self._token1.address
        elif token_out == self._token1.address:
            token_out_obj = self._token1
            expected_token_in = self._token0.address
        else:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        if token_in != expected_token_in:
            msg = f"token_in {token_in} does not match expected {expected_token_in}"
            raise DegenbotValueError(message=msg)

        result = self.simulate_exact_output_swap(
            token_out=token_out_obj,
            token_out_quantity=amount_out,
            override_state=state_override,
        )
        zero_for_one = token_out_obj == self._token1
        amount_in = result.amount0_delta if zero_for_one else result.amount1_delta
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

        Uses Camelot's own k + get_y functions (``k_camelot``/``get_y_camelot``)
        instead of the standard Solidly ones. ``fee_denominator`` scales the
        integer fee numerator; ``fee_tokenN`` is the FEE ``Fraction``.

        Returns:
            The computed integer value.

        """
        if override_state is not None:  # pragma: no cover
            pass

        # ``stable_swap=True`` (set by the builder) implies the builder also
        # set ``fee_denominator`` (Camelot's integer fee scale). Narrow for
        # the typed math below.
        assert self.fee_denominator is not None
        fee_denominator = self.fee_denominator

        precision_multiplier_token0: int = 10**self.token0.decimals
        precision_multiplier_token1: int = 10**self.token1.decimals

        fee_percent = fee_denominator * (
            self.fee_token0 if token_in == self.token0 else self.fee_token1
        )

        reserves_token0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_token1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        # Remove fee from amount received
        token_in_quantity -= token_in_quantity * fee_percent // fee_denominator
        xy = k_camelot(
            balance_0=reserves_token0,
            balance_1=reserves_token1,
            decimals_0=precision_multiplier_token0,
            decimals_1=precision_multiplier_token1,
        )
        reserves_token0 = reserves_token0 * 10**18 // precision_multiplier_token0
        reserves_token1 = reserves_token1 * 10**18 // precision_multiplier_token1
        reserve_a, reserve_b = (
            (reserves_token0, reserves_token1)
            if token_in == self.token0
            else (reserves_token1, reserves_token0)
        )
        token_in_quantity = (
            token_in_quantity * 10**18 // precision_multiplier_token0
            if token_in == self.token0
            else token_in_quantity * 10**18 // precision_multiplier_token1
        )
        y = reserve_b - get_y_camelot(token_in_quantity + reserve_a, xy, reserve_b)

        return (
            y
            * (
                precision_multiplier_token1
                if token_in == self.token0
                else precision_multiplier_token0
            )
            // 10**18
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
                return calc_exact_in_stable(
                    amount_in=amount_in,
                    token_in=_token_in,
                    reserves0=_reserves0,
                    reserves1=_reserves1,
                    decimals0=_decimals0,
                    decimals1=_decimals1,
                    fee=_fee,
                    k_func=k_camelot,
                    get_y_func=get_y_camelot,  # ty:ignore[invalid-argument-type]
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
