"""Aerodrome V2 liquidity pool implementations (volatile and stable)."""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any, ClassVar, Literal, Self, cast
from weakref import WeakSet

from eth_abi import decode as abi_decode

from degenbot.aerodrome.functions import (
    calc_exact_in_stable,
)
from degenbot.aerodrome.types import (
    AerodromeV2PoolExternalUpdate,
    AerodromeV2PoolState,
    AerodromeV2PoolStateUpdated,
    AerodromeV3PoolState,
)
from degenbot.aerodrome.v2_pool_calc import AerodromeV2PoolCalc
from degenbot.aerodrome.v2_pool_state import AerodromeV2PoolState as AerodromeV2PoolStateMixin
from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    ExternalUpdateError,
    NoPoolStateAvailable,
)
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.degenbot_rs import PyLiquidityPool
    from degenbot.provider.sync_adapter import ProviderAdapter
    from degenbot.types.aliases import BlockNumber
    from degenbot.uniswap.types import UniswapPoolSwapVector


class AerodromeV2Pool(
    PublisherMixin,
    AerodromeV2PoolStateMixin,
    AerodromeV2PoolCalc,
    AbstractLiquidityPool,
):
    """AerodromeV2Pool class."""

    variant: ClassVar[str | None] = "aerodrome"

    type PoolState = AerodromeV2PoolState

    FEE_DENOMINATOR = 10_000

    # Instance attributes set in `_from_py_pool` (the only construction seam).
    _py_pool: PyLiquidityPool
    address: ChecksumAddress
    factory: ChecksumAddress
    deployer_address: ChecksumAddress
    _initial_state_block: int
    _stable: bool
    _fee: Fraction
    _token0: Erc20Token
    _token1: Erc20Token
    name: str
    _subscribers: WeakSet[Subscriber]

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # noqa: ARG002
        """Direct construction is forbidden.

        ``AerodromeV2Pool`` is a Python companion over a Rust-owned
        ``PyLiquidityPool`` handle. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_aerodrome_v2_pool(...)``

        Both register the pool in Rust, obtain the ``PyLiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool`.

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or "
            "make_aerodrome_v2_pool(...) (tests) to register the pool in "
            "Rust and obtain the PyLiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: PyLiquidityPool) -> Self:
        """Wrap a Rust-owned ``PyLiquidityPool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). Every
        identity field (address, tokens, factory, fee, stable, variant) is
        read off the handle; reserves live in Rust (``AerodromeV2PoolState``)
        and are read via ``snapshot_aerodrome()`` — the Python ``StateCache``
        is gone.

        Returns:
            A ``cls`` instance wrapping ``py_pool``.

        Raises:
            DegenbotValueError: If the handle is not an Aerodrome V2 pool.

        """
        self = cls.__new__(cls)
        self._py_pool = py_pool

        if py_pool.pool_family != "aerodrome-v2":
            msg = (
                "PyLiquidityPool handle is not an Aerodrome V2 pool "
                f"(got pool_family {py_pool.pool_family!r})"
            )
            raise DegenbotValueError(message=msg)

        self.address = get_checksum_address(py_pool.address)
        self.factory = get_checksum_address(py_pool.factory)
        self.deployer_address = self.factory

        py_token0 = py_pool.get_token0()
        py_token1 = py_pool.get_token1()
        if py_token0 is None or py_token1 is None:
            msg = (
                "pool tokens must be registered in the same Bot as the pool "
                "(ADR-006): get_token0/get_token1 returned None"
            )
            raise DegenbotValueError(message=msg)
        self._token0 = Erc20Token._from_py_token(py_token0)  # noqa: SLF001
        self._token1 = Erc20Token._from_py_token(py_token1)  # noqa: SLF001

        self._stable = py_pool.aerodrome_stable
        fee_numer, fee_denom = py_pool.aerodrome_fee
        self._fee = Fraction(fee_numer, fee_denom)

        # Wire calculation strategy at construction.
        self._wire_stable_calculations(stable=self._stable)

        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, {100 * self._fee.numerator / self._fee.denominator:.2f}%)"  # noqa: E501

        self._initial_state_block = py_pool.update_block
        self._subscribers = WeakSet()
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1}, stable={self._stable})"  # noqa:E501

    @property
    def reserves_token0(self) -> int:
        """Reserves token0."""
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        """Reserves token1."""
        return self.state.reserves_token1

    @property
    def state(self) -> PoolState:
        """State.

        Raises:
            DegenbotValueError: If the Rust snapshot is absent.

        """
        snap = self._py_pool.snapshot_aerodrome()
        if snap is None:
            msg = f"No Aerodrome V2 pool state available for {self.address}"
            raise DegenbotValueError(message=msg)
        reserve0, reserve1, block = snap
        return self.PoolState.__value__(
            address=self.address,
            reserves_token0=reserve0,
            reserves_token1=reserve1,
            block=block,
        )

    @property
    def update_block(self) -> BlockNumber:
        """Update block."""
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

    @staticmethod
    def swap_is_viable(
        state: PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Swap is viable.

        Returns:
            The computed boolean value.

        """
        if state.reserves_token0 == 0 or state.reserves_token1 == 0:
            return False
        return state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1

    def external_update(
        self,
        update: AerodromeV2PoolExternalUpdate,
    ) -> None:
        """External update.

        Raises:
            ExternalUpdateError: See function documentation.

        """
        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}",  # noqa:E501
            )

        self._py_pool.apply_aerodrome_sync(
            update.reserves_token0,
            update.reserves_token1,
            update.block_number,
        )
        self._notify_subscribers(
            message=AerodromeV2PoolStateUpdated(self.state),
        )

    def get_pool_identity_values(
        self,
        provider: ProviderAdapter,
        state_block: BlockNumber,
    ) -> tuple[
        ChecksumAddress,  # factory
        tuple[ChecksumAddress, ChecksumAddress],  # tokens
        bool,  # stable
        int,  # fee
        tuple[int, int],  # reserves
    ]:
        """Return pool identity values.

        Returns:
            The computed value.

        """
        immutable_calls = [
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="factory()",
                    function_arguments=None,
                ),
            },
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="token0()",
                    function_arguments=None,
                ),
            },
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="token1()",
                    function_arguments=None,
                ),
            },
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="stable()",
                    function_arguments=None,
                ),
            },
        ]
        factory_data, token0_data, token1_data, stable_data = provider.batch_call(immutable_calls)  # ty:ignore[invalid-argument-type]

        # This call uses a specific block so the reserve values are consistent
        reserves_data = provider.call_raw(
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="getReserves()",
                    function_arguments=None,
                ),
            },
            block=state_block,
        )

        (factory,) = abi_decode(["address"], factory_data)
        (token0,) = abi_decode(["address"], token0_data)
        (token1,) = abi_decode(["address"], token1_data)
        (stable,) = abi_decode(["bool"], stable_data)
        reserves0, reserves1, _ = abi_decode(["uint256", "uint256", "uint256"], reserves_data)

        factory_checksum = get_checksum_address(cast("str", factory))
        (fee,) = abi_decode(
            ["uint256"],
            provider.call_raw({
                "to": factory_checksum,
                "data": encode_function_calldata(
                    function_prototype="getFee(address,bool)",
                    function_arguments=[self.address, stable],
                ),
            }),
        )

        return (
            factory_checksum,
            (get_checksum_address(cast("str", token0)), get_checksum_address(cast("str", token1))),
            cast("bool", stable),
            cast("int", fee),
            (cast("int", reserves0), cast("int", reserves1)),
        )

    def discard_states_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """Discard cached states earlier than the given block.

        Raises:
            NoPoolStateAvailable: See function documentation.

        """
        try:
            self._py_pool.discard_aerodrome_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    def restore_state_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """Restore the last pool state recorded prior to a target block.

        Use this method to maintain consistent state data following a chain re-organization.

        The pool will notify all subscribers of the new state with a `AerodromeV2PoolStateUpdated`
        event.

        Raises:
            NoPoolStateAvailable: See function documentation.

        """
        try:
            self._py_pool.restore_aerodrome_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e
        self._notify_subscribers(message=AerodromeV2PoolStateUpdated(self.state))

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: See function documentation.

        """
        aero_state: AerodromeV2PoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, AerodromeV2PoolState):
                msg = f"Expected AerodromeV2PoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            aero_state = state_override

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

        initial_state = aero_state or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=aero_state,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: AerodromeV2PoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap for output.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: See function documentation.

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

        initial_state = state_override or self.state
        amount_in = self.calculate_tokens_in_from_tokens_out(
            token_out=token_out_obj,
            token_out_quantity=amount_out,
            override_state=state_override,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AerodromeV2PoolState | None = None,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,  # noqa: ARG002
    ) -> HopType:
        """Convert to hop state.

        Returns:
            The computed value.

        """
        # token_in/token_out unused — 2-token pools determine pair from zero_for_one.
        # Callers should ensure these match pool.token0/pool.token1 if provided.
        state = state_override or self.state
        fee = self.extract_fee(zero_for_one=zero_for_one)

        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
            decimals_in = self._token0.decimals
            decimals_out = self._token1.decimals
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
            decimals_in = self._token1.decimals
            decimals_out = self._token0.decimals

        if self._stable_calc_mode:
            reserves0 = state.reserves_token0
            reserves1 = state.reserves_token1
            decimals0 = 10**self._token0.decimals
            decimals1 = 10**self._token1.decimals
            token_in: Literal[0, 1] = 0 if zero_for_one else 1

            def _stable_swap_fn(
                amount_in: int,
                /,
                _reserves0: int = reserves0,
                _reserves1: int = reserves1,
                _decimals0: int = decimals0,
                _decimals1: int = decimals1,
                _fee: Fraction = fee,
                _token_in: Literal[0, 1] = token_in,
            ) -> int:
                return calc_exact_in_stable(
                    amount_in=amount_in,
                    token_in=_token_in,
                    reserves0=_reserves0,
                    reserves1=_reserves1,
                    decimals0=_decimals0,
                    decimals1=_decimals1,
                    fee=_fee,
                )

            return SolidlyStableHop(
                reserve_in=reserve_in,
                reserve_out=reserve_out,
                fee=fee,
                decimals_in=decimals_in,
                decimals_out=decimals_out,
                swap_fn=_stable_swap_fn,
            )

        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> UniswapV2PoolSwapAmounts:
        """Build swap amount.

        Returns:
            The computed value.

        """
        return UniswapV2PoolSwapAmounts(
            pool=self.address,
            amounts_in=(amount_in, 0) if zero_for_one else (0, amount_in),
            amounts_out=(0, amount_out) if zero_for_one else (amount_out, 0),
        )


class AerodromeV3Pool(UniswapV3Pool):
    """AerodromeV3Pool class."""

    variant: ClassVar[str | None] = "aerodrome"

    type PoolState = AerodromeV3PoolState

    TICK_STRUCT_TYPES = (
        "uint128",
        "int128",
        "int128",
        "uint256",
        "uint256",
        "uint256",
        "int56",
        "uint160",
        "uint32",
        "bool",
    )

    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "bool",
    )
