"""Aerodrome V2 liquidity pool implementations (volatile and stable)."""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any, ClassVar, Self, cast
from weakref import WeakSet

from degenbot.abi import decode as abi_decode
from degenbot.aerodrome.types import (
    AerodromeV2PoolExternalUpdate,
    AerodromeV2PoolState,
    AerodromeV2PoolStateUpdated,
    AerodromeV3PoolState,
)
from degenbot.aerodrome.v2_pool_calc import AerodromeV2PoolCalc
from degenbot.aerodrome.v2_pool_state import AerodromeV2PoolState as AerodromeV2PoolStateMixin
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    ExternalUpdateError,
    NoPoolStateAvailable,
)
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot.provider import AlloyProvider
    from degenbot.types import LiquidityPool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
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
    _py_pool: LiquidityPool
    address: ChecksummedAddress
    factory: ChecksummedAddress
    deployer_address: ChecksummedAddress
    _initial_state_block: int
    _stable: bool
    _fee: Fraction
    _token0: Erc20Token
    _token1: Erc20Token
    name: str
    _subscribers: WeakSet[Subscriber]

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``AerodromeV2Pool`` is a Python companion over a Rust-owned
        ``LiquidityPool`` handle. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_aerodrome_v2_pool(...)``

        Both register the pool in Rust, obtain the ``LiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool`.

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or "
            "make_aerodrome_v2_pool(...) (tests) to register the pool in "
            "Rust and obtain the LiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: LiquidityPool) -> Self:
        """Wrap a Rust-owned ``LiquidityPool`` handle as a Python companion.

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
                "LiquidityPool handle is not an Aerodrome V2 pool "
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
        self._token0 = Erc20Token._from_py_token(py_token0)  # ruff:ignore[private-member-access]
        self._token1 = Erc20Token._from_py_token(py_token1)  # ruff:ignore[private-member-access]

        self._stable = py_pool.aerodrome_stable
        fee_numer, fee_denom = py_pool.aerodrome_fee
        self._fee = Fraction(fee_numer, fee_denom)

        # Wire calculation strategy at construction.
        self._wire_stable_calculations(stable=self._stable)

        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, {100 * self._fee.numerator / self._fee.denominator:.2f}%)"  # ruff:ignore[line-too-long]

        self._initial_state_block = py_pool.update_block
        self._subscribers = WeakSet()
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1}, stable={self._stable})"  # ruff:ignore[line-too-long]

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
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}",  # ruff:ignore[line-too-long]
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
        provider: AlloyProvider,
        state_block: BlockNumber,
    ) -> tuple[
        ChecksummedAddress,  # factory
        tuple[ChecksummedAddress, ChecksummedAddress],  # tokens
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
