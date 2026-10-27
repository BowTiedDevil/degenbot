"""Aerodrome V2 liquidity pool implementations (volatile and stable)."""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any, ClassVar, Self, cast

from degenbot.aerodrome.types import (
    AerodromeV2PoolState,
    AerodromeV3PoolState,
)
from degenbot.aerodrome.v2_pool_calc import AerodromeV2PoolCalc
from degenbot.aerodrome.v2_pool_state import AerodromeV2PoolState as AerodromeV2PoolStateMixin
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot.types import Pool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.uniswap.types import UniswapPoolSwapVector


class AerodromeV2Pool(
    AerodromeV2PoolStateMixin,
    AerodromeV2PoolCalc,
    AbstractLiquidityPool,
):
    """AerodromeV2Pool class."""

    variant: ClassVar[str | None] = "aerodrome"

    type PoolState = AerodromeV2PoolState

    FEE_DENOMINATOR = 10_000

    # Instance attributes set in `from_handle` (the only construction seam).
    _py_pool: Pool
    address: ChecksummedAddress
    factory: ChecksummedAddress
    deployer_address: ChecksummedAddress
    _initial_state_block: int
    _stable: bool
    _fee: Fraction
    _token0: Erc20Token
    _token1: Erc20Token
    name: str

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``AerodromeV2Pool`` is a Python companion over a Rust-owned
        ``Pool`` handle. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_aerodrome_v2_pool(...)``

        Both register the pool in Rust, obtain the ``Pool``
        handle, and wrap it via :meth:`from_handle`.

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or "
            "make_aerodrome_v2_pool(...) (tests) to register the pool in "
            "Rust and obtain the Pool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def from_handle(cls, py_pool: Pool) -> Self:
        """Wrap a Rust-owned ``Pool`` handle as a Python companion.

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

        # Species guard: `reserve_pair()` is shape-level (V2 + Aerodrome V2), so
        # the arm cannot separate this companion from its V2 sibling — the tag is
        # the class-dispatch decision.
        if py_pool.pool_family != "aerodrome-v2":
            msg = (
                f"Pool handle is not an Aerodrome V2 pool (got pool_family {py_pool.pool_family!r})"
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
        self._token0 = Erc20Token.from_handle(py_token0)
        self._token1 = Erc20Token.from_handle(py_token1)

        self._stable = py_pool.aerodrome_stable
        fee_numer, fee_denom = py_pool.aerodrome_fee
        self._fee = Fraction(fee_numer, fee_denom)

        # Wire calculation strategy at construction.
        self._wire_calculations(stable=self._stable)

        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, {100 * self._fee.numerator / self._fee.denominator:.2f}%)"  # ruff:ignore[line-too-long]

        self._initial_state_block = py_pool.reserve_pair().update_block
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
        try:
            view = self._py_pool.reserve_pair()
        except ValueError as exc:
            msg = f"No Aerodrome V2 pool state available for {self.address}"
            raise DegenbotValueError(message=msg) from exc
        return cast(
            "AerodromeV2PoolState",
            self.PoolState.__value__(
                address=self.address,
                reserves_token0=view.reserve0,
                reserves_token1=view.reserve1,
                block=view.update_block,
            ),
        )

    @property
    def update_block(self) -> BlockNumber:
        """Update block."""
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self._py_pool.reserve_pair().update_block

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


class AerodromeV3Pool(UniswapV3Pool):
    """AerodromeV3Pool class."""

    variant: ClassVar[str | None] = "aerodrome"

    # The runtime alias override is the intended specialization of
    # ``UniswapV3Pool.PoolState``; ty cannot prove assignability across two
    # ``TypeAliasType`` values.
    type PoolState = AerodromeV3PoolState  # ty: ignore[invalid-attribute-override]

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
