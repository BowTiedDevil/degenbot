"""Curve StableSwap liquidity pool implementation.

Implements the Curve StableSwap invariant for V1-style pools including
plain pools, metapools, lending pools, and crypto pools.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, NamedTuple, Self, cast

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.pool_quoter import CurveSwapQuoter
from degenbot.curve.stableswap_pool_state import PoolStrategies, StableswapPoolState
from degenbot.curve.types import (
    BasePoolPort,
    CurveDataProvider,
    CurveStableswapPoolExternalUpdate,
    CurveStableswapPoolState,
    DVariant,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    SwapStyle,
    YDVariant,
    YVariant,
)
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import MissingCurveData
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot._ffi import CurveView
    from degenbot.types import Pool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.types.rpc_types import BlockIdentifier


class _HandleCurveDataProviderAdapter:
    """Adapts ``Pool.curve()`` as a stored ``CurveDataProvider``.

    The companion holds no Python data-provider object — the
    provider is the stored Rust trait object (ADR-005). This shim
    exposes the 13-method ``CurveDataProvider`` read interface by delegating
    each read to the arm's stored provider, mirroring the Balancer
    ``_HandleRateProviderAdapter``. ``MissingCurveData`` is raised on
    a missing provider / fetch miss so the calc path's existing error
    handling applies unchanged.
    """

    def __init__(self, arm: CurveView, address: ChecksummedAddress) -> None:
        self._arm = arm
        self._address = address

    def block_number(self) -> int:
        v: int | None = self._arm.fetch_block_number()
        if v is None:
            raise MissingCurveData(self._address, "block_number", "no data provider stored")
        return v

    def block_timestamp(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_block_timestamp(block_number)
        if v is None:
            raise MissingCurveData(self._address, "block_timestamp", "no data provider stored")
        return v

    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int:
        v: int | None = self._arm.fetch_token_balance(token_address, holder_address, block_number)
        if v is None:
            raise MissingCurveData(self._address, "token_balance", "no data provider stored")
        return v

    def token_total_supply(self, token_address: str, block_number: int) -> int:
        v: int | None = self._arm.fetch_token_total_supply(token_address, block_number)
        if v is None:
            raise MissingCurveData(self._address, "token_total_supply", "no data provider stored")
        return v

    def lending_rates(self, block_number: int) -> tuple[int, ...]:
        rates: tuple[int, ...] | None = self._arm.fetch_lending_rates(block_number)
        if not rates:
            raise MissingCurveData(self._address, "lending_rates", "no data provider stored")
        return tuple(rates)

    def d(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_d(block_number)
        if v is None:
            raise MissingCurveData(self._address, "d", "no data provider stored")
        return v

    def gamma(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_gamma(block_number)
        if v is None:
            raise MissingCurveData(self._address, "gamma", "no data provider stored")
        return v

    def price_scale(self, block_number: int) -> tuple[int, ...]:
        scales: tuple[int, ...] | None = self._arm.fetch_price_scale(block_number)
        if not scales:
            raise MissingCurveData(self._address, "price_scale", "no data provider stored")
        return tuple(scales)

    def admin_balances(self, block_number: int) -> tuple[int, ...]:
        bal: tuple[int, ...] | None = self._arm.fetch_admin_balances(block_number)
        if not bal:
            raise MissingCurveData(self._address, "admin_balances", "no data provider stored")
        return tuple(bal)

    def redemption_price(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_redemption_price(block_number)
        if v is None:
            raise MissingCurveData(self._address, "redemption_price", "no data provider stored")
        return v

    def base_cache_updated(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_base_cache_updated(block_number)
        if v is None:
            raise MissingCurveData(self._address, "base_cache_updated", "no data provider stored")
        return v

    def base_virtual_price(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_base_virtual_price(block_number)
        if v is None:
            raise MissingCurveData(self._address, "base_virtual_price", "no data provider stored")
        return v

    def virtual_price(self, block_number: int) -> int:
        v: int | None = self._arm.fetch_virtual_price(block_number)
        if v is None:
            raise MissingCurveData(self._address, "virtual_price", "no data provider stored")
        return v


class _CryptoFeeCoefficients(NamedTuple):
    """Normalized crypto-pool fee coefficients (``None`` ⇔ 0)."""

    fee_gamma: int
    mid_fee: int
    offpeg_fee_multiplier: int
    out_fee: int
    gamma: int


def _read_crypto_fee_coefficients(arm: CurveView) -> _CryptoFeeCoefficients:
    """Normalize the arm's crypto-fee tuple (``None`` ⇔ 0).

    Standard stableswap pools store no crypto fees, so those slots come back
    ``None`` and mean 0. The arm is Curve-typed, so the tuple itself is always
    present — only its slots are optional.

    Returns:
        The normalized coefficients.

    """
    crypto_fees = arm.crypto_fees()
    fee_gamma, mid_fee, offpeg_fee_multiplier, out_fee, gamma = crypto_fees
    return _CryptoFeeCoefficients(
        fee_gamma=fee_gamma if fee_gamma is not None else 0,
        mid_fee=mid_fee if mid_fee is not None else 0,
        offpeg_fee_multiplier=(offpeg_fee_multiplier if offpeg_fee_multiplier is not None else 0),
        out_fee=out_fee if out_fee is not None else 0,
        gamma=gamma if gamma is not None else 0,
    )


class CurveStableswapPool(
    StableswapPoolState,
    AbstractLiquidityPool,
):
    """CurveStableswapPool class."""

    type PoolState = CurveStableswapPoolState

    # Constants from contract
    # ref: https://github.com/curvefi/curve-contract/blob/master/contracts/pool-templates/base/SwapTemplateBase.vy
    PRECISION_DECIMALS: int = 18
    PRECISION: int = 10**PRECISION_DECIMALS

    # Class-scope instance-attribute declarations (red-knot): `from_handle`
    # assigns these on `Self`; declare them at class scope so attribute reads
    # in helper/calc methods resolve (mirrors the Balancer/Aerodrome seams).
    address: ChecksummedAddress
    _py_pool: Pool
    _curve: CurveView
    _tokens: tuple[Erc20Token, ...]
    _a_coefficient: int
    _fee: int
    _admin_fee: int
    _rate_multipliers: tuple[int, ...]
    _precision_multipliers: tuple[int, ...]
    _fee_gamma: int
    _mid_fee: int
    _offpeg_fee_multiplier: int
    _out_fee: int
    _gamma: int
    _strategies: PoolStrategies
    _base_pool: BasePoolPort | None
    _tokens_underlying: tuple[Erc20Token, ...] | None
    _lp_token: Erc20Token
    _use_lending: tuple[bool, ...]
    _initial_a_coefficient: int | None
    _future_a_coefficient: int | None
    _initial_a_coefficient_time: int | None
    _future_a_coefficient_time: int | None
    _create_timestamp: int | None
    _data_provider: CurveDataProvider | None
    _cache_block_timestamps: BoundedCache[BlockNumber, int]
    _name: str
    FEE_DENOMINATOR: int = 10**10
    A_PRECISION: int = 100

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        A ``CurveStableswapPool`` is a companion over a Rust-owned
        ``Pool`` handle. The handle can only be produced by
        registering a pool in a ``Bot`` (production: ``Bot.build_pool()``;
        tests: ``make_curve_pool``), then wrapping via
        :meth:`from_handle`. Direct constructor calls are rejected so that
        the only paths to a pool instance are the ones that wire the handle —
        mirroring Polars' ``_from_pydf`` pattern and matching V2/V3/V4/Balancer
        /Aerodrome. Every identity field + the stored I/O trait objects are
        read off the handle; the companion holds nothing an external caller
        can mutate.

        Raises:
            TypeError: Always — direct construction is forbidden.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "A Pool handle is wired by Bot.build_pool() "
            "(production) or make_curve_pool (tests); call "
            f"{type(self).__name__}.from_handle(handle) to wrap a "
            "registered handle."
        )
        raise TypeError(msg)

    @classmethod
    def from_handle(cls, py_pool: Pool) -> Self:
        """Wrap a Rust-owned ``Pool`` handle as a Python companion.

        Single-arg seam (ADR-005): reads *every* identity field + the
        stored data-provider trait object off the handle. The cross-pool
        references (base pool companion + underlying/LP tokens) are recovered
        from the handle too — the base pool via the Rust go-between
        ``curve().base_pool()`` (same shared ``BotState``, no Python registry),
        wrapped in a :class:`_LazyBasePool` that memoises construction.

        Returns:
            The companion wrapping the handle.

        Raises:
            DegenbotValueError: If the handle is not a Curve stableswap pool,
                or its tokens are not registered in the handle's Bot.

        """
        self = object.__new__(cls)

        # The arm accessor IS the family gate: `Pool.curve()` refuses a handle
        # registered under any other family, so a V2/V3/V4/Balancer handle
        # raises here rather than reading a wrong-family sentinel. Every read
        # below goes through the arm, which types its answers as Curve state.
        try:
            arm = py_pool.curve()
        except ValueError as exc:
            msg = f"Pool handle is not a Curve stableswap pool: {exc}"
            raise DegenbotValueError(message=msg) from exc

        self._py_pool = py_pool
        self._curve = arm
        self.address = get_checksum_address(py_pool.address)

        # Tokens — recovered as companion handles (shared BotState).
        py_tokens = arm.get_tokens()
        if py_tokens is None:
            msg = (
                "Curve pool tokens are not registered in the handle's Bot; "
                "register them via Bot.build_pool() / make_erc20 first."
            )
            raise DegenbotValueError(message=msg)
        self._tokens = tuple(Erc20Token.from_handle(t) for t in py_tokens)

        self._a_coefficient = arm.a_coefficient
        self._fee = arm.fee
        self._admin_fee = arm.admin_fee

        # Rate/precision multipliers — the Rust core owns the derivation
        # (degenbot_math::curve::derive_rate_and_precision_multipliers); the
        # handle is the single source of truth.
        self._rate_multipliers = tuple(arm.rate_multipliers)
        self._precision_multipliers = tuple(arm.precision_multipliers)

        # Crypto-pool fees (None ⇔ 0 for standard stableswap pools). The
        # normalization lives in the cohesive helper; the arm types the tuple
        # as always-present, so only its slots are optional.
        (
            self._fee_gamma,
            self._mid_fee,
            self._offpeg_fee_multiplier,
            self._out_fee,
            self._gamma,
        ) = _read_crypto_fee_coefficients(arm)

        # Strategies — reconstruct from the 7 u8 discriminants stored in Rust.
        # auto()-based enums: .value is forwarded verbatim, so Enum(value)
        # round-trips (the factory's _strategies_to_rust_enums is the inverse).
        self._strategies = PoolStrategies(
            d_variant=DVariant(arm.d_variant),
            y_variant=YVariant(arm.y_variant),
            yd_variant=YDVariant(arm.yd_variant),
            swap_style=SwapStyle(arm.swap_style),
            metapool_rate_style=MetapoolRateStyle(arm.metapool_rate_style),
            metapool_underlying_style=MetapoolUnderlyingStyle(arm.metapool_underlying_style),
            lending_rate_style=LendingRateStyle(arm.lending_rate_style),
        )

        # Cross-pool base reference — the go-between returns a handle over the
        # base pool (same core); wrapped lazily so an unused base path pays zero.
        base_handle = arm.base_pool()
        self._base_pool = _LazyBasePool(base_handle) if base_handle is not None else None

        # Underlying + LP tokens — recovered as companion handles.
        underlying = arm.get_tokens_underlying()
        self._tokens_underlying = (
            tuple(Erc20Token.from_handle(t) for t in underlying) if underlying is not None else None
        )
        lp = arm.get_lp_token()
        self._lp_token = Erc20Token.from_handle(lp) if lp is not None else self._tokens[0]

        ul = tuple(arm.use_lending)
        self._use_lending = ul or tuple(False for _ in self._tokens)

        # A-ramping (all None ⇔ a plain non-ramping pool). The arm types the
        # tuple as always-present, so only its slots are optional.
        (
            self._initial_a_coefficient,
            self._future_a_coefficient,
            self._initial_a_coefficient_time,
            self._future_a_coefficient_time,
            self._create_timestamp,
        ) = arm.a_ramp()

        # Data provider — the stored Rust trait object, read through a handle
        # adapter (mirrors the Balancer _HandleRateProviderAdapter).
        self._data_provider = (
            _HandleCurveDataProviderAdapter(arm, self.address) if arm.has_data_provider else None
        )

        # Per-block on-chain caches (companion-owned cache depth; the always-8
        # default applies — no caller passes a non-default value). Nothing is
        # resolved at build time: the metapool base-cache freshness rule lives
        # only in the Rust core
        # (`degenbot_pools::curve_dy_io::resolve_metapool_base_rate`), which
        # resolves the virtual price at calc time — so a provider-less pool is
        # legal.
        self._cache_block_timestamps = BoundedCache(max_items=8)

        fee_string = f"{100 * self.fee / self.FEE_DENOMINATOR:.2f}"
        token_string = "-".join([token.symbol for token in self._tokens])
        self._name = f"{token_string} ({self.__class__.__name__}, {fee_string}%)"
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        token_string = "-".join([token.symbol for token in self._tokens])
        return f"{self.__class__.__name__}(address={self.address}, tokens={token_string}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%, A={self.a_coefficient})"  # ruff:ignore[line-too-long]

    @property
    def balances(self) -> tuple[int, ...]:
        """Balances.

        Read from the Rust core via the ``Pool`` handle.
        Rust ``BotState`` is the single source of truth
        for the mutable ``balances`` slot; this getter returns the live tuple.
        """
        return cast("tuple[int, ...]", tuple(self._py_pool.balance_vector().balances))

    @property
    def state(self) -> CurveStableswapPoolState:
        """State.

        Built from one atomic Rust snapshot (``snapshot_curve()`` —
        ``(balances, block)``) so callers see a coherent tuple (no torn read
        mid-``external_update``). Mirrors V3/V4's ``snapshot_v3()`` contract.

        Raises:
            DegenbotValueError: If the Rust snapshot is absent (the pool is
                not registered in Rust as a Curve pool — unreachable for a
                companion built over a registered handle).

        """
        try:
            view = self._py_pool.balance_vector()
        except ValueError as exc:
            msg = f"No Curve pool state available for {self.address}"
            raise DegenbotValueError(message=msg) from exc
        return CurveStableswapPoolState(
            address=self.address,
            balances=tuple(view.balances),
            block=view.update_block,
        )

    @property
    def update_block(self) -> BlockNumber:
        """Update block (from Rust via the handle)."""
        return self._py_pool.balance_vector().update_block

    @property
    def requires_io_at_calculation_time(self) -> bool:
        """Whether this pool may call data_provider during swap calculations.

        Returns True for pools that need per-block on-chain data (D, gamma,
        price_scale, lending rates, admin balances, virtual price for
        metapools, block timestamps for A ramping). Returns False only for
        plain pools with static rate multipliers and no A ramping.
        """
        if self._strategies.swap_style in {
            SwapStyle.CRYPTO,
            SwapStyle.LIVE_ADMIN,
            SwapStyle.LIVE_ADMIN_DYNAMIC,
            SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION,
            SwapStyle.LIVE_ADMIN_ORACLE,
        }:
            return True
        if self._strategies.lending_rate_style != LendingRateStyle.NONE:
            return True
        if self.base_pool is not None:
            return True
        return any([
            self.future_a_coefficient is not None,
            self.initial_a_coefficient is not None,
        ])

    def external_update(self, update: CurveStableswapPoolExternalUpdate) -> None:
        """Apply an external state update with new balances.

        Delegates to the Rust core (``Pool.apply_curve_balance_update``)
        which journals the prior balances (genesis-anchor V2-style discipline)
        and lands the new balances + ``update_block`` atomically. The
        ``StateCache`` temporal-navigation layer it
        used to write is gone — the Rust reorg journal handles rollback now.

        Raises:
            DegenbotValueError: If the Rust core rejects the update (the pool
                is not registered as a Curve pool — unreachable for a companion
                built over a registered handle).

        """
        applied = self._py_pool.apply_balances(
            list(update.balances),
            update.block_number,
        )
        if not applied:  # pragma: no cover - defensive, unreachable for a Curve handle
            msg = f"external_update rejected for {self.address} (not a Curve pool in Rust)"
            raise DegenbotValueError(message=msg)

    def _resolve_block_number(self, block_identifier: BlockIdentifier | None) -> int:
        """Resolve a block identifier to an integer. Falls back to data provider if available.

        Returns:
            The computed integer value.

        Raises:
            MissingCurveData: See function documentation.

        """
        if isinstance(block_identifier, int):
            return block_identifier
        if self._data_provider is not None:
            return self._data_provider.block_number()
        raise MissingCurveData(
            self.address,
            "block_identifier",
            "block_identifier must be an integer when no provider is available. "
            "Use Bot.update() or pass an explicit block number.",
        )

    def _get_cached_block_timestamp(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve the cached block timestamp.

        Returns:
            The block timestamp as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        cached = self._cache_block_timestamps.get(block_number)
        if cached is not None:
            return cached
        if self._data_provider is None:
            msg = "block_timestamp requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "block_timestamp", msg)
        result = self._data_provider.block_timestamp(block_number)
        self._cache_block_timestamps[block_number] = result
        return result

    def _a(self, timestamp: int | None = None) -> int:
        """Handle ramping A up or down.

        Returns:
            The computed integer value.

        """
        if any([
            self.future_a_coefficient is None,
            self.initial_a_coefficient is None,
        ]):
            return self.a_coefficient * self.A_PRECISION

        if TYPE_CHECKING:
            assert self.initial_a_coefficient is not None
            assert self.initial_a_coefficient_time is not None
            assert self.future_a_coefficient_time is not None
            assert self.future_a_coefficient is not None
            assert self._create_timestamp is not None

        if self._create_timestamp >= self.future_a_coefficient_time:
            return self.future_a_coefficient

        if timestamp is None:
            timestamp = self._get_cached_block_timestamp(0)

        a_1 = self.future_a_coefficient
        t_1 = self.future_a_coefficient_time

        # Modified from contract template to check timestamp argument instead
        # of block.timestamp
        if timestamp < t_1:
            a_0 = self.initial_a_coefficient
            t_0 = self.initial_a_coefficient_time
            if a_1 > a_0:
                scaled_a = a_0 + (a_1 - a_0) * (timestamp - t_0) // (t_1 - t_0)
            else:
                scaled_a = a_0 - (a_0 - a_1) * (timestamp - t_0) // (t_1 - t_0)
        else:
            scaled_a = a_1

        return scaled_a

    def calc_token_amount(
        self,
        *,
        amounts: Sequence[int],
        deposit: bool,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Simplified method to calculate addition or reduction in token supply at.

        deposit or withdrawal without taking fees into account (but looking at
        slippage).
        Needed to prevent front-running, not for precise calculations!

        Returns:
            The computed integer value.

        """
        block_number = self._resolve_block_number(block_identifier)
        # Rust-owned: D computation + LP-total-supply I/O + the
        # deposit/reduction scaling all run in the Rust core on this pool's
        # current balances.
        return cast(
            "int",
            self._curve.calc_token_amount(list(amounts), deposit, block_number),
        )

    def calc_withdraw_one_coin(
        self,
        _token_amount: int,
        i: int,
        block_identifier: BlockIdentifier | None = None,
    ) -> tuple[int, ...]:
        """Calc withdraw one coin.

        Returns:
            The computed value.

        """
        block_number = self._resolve_block_number(block_identifier)
        # Rust-owned: D + get_y_d + the LP-total-supply I/O all
        # run in the Rust core. The companion's extra tuple fields (`dy_0 - dy`,
        # `total_supply`) are not consumed by any caller, so only `dy` is
        # returned.
        dy = self._curve.calc_withdraw_one_coin(_token_amount, i, block_number)
        return (cast("int", dy),)

    @property
    def swap_quoter(self) -> CurveSwapQuoter:
        """The pool's swap quoter.

        Built over the pool's live reads, Curve arm, and block resolver, so a
        replaced read or handle is always honoured.

        Returns:
            The Curve swap quoter for this pool.

        """
        return CurveSwapQuoter(
            self,
            self._curve,
            resolve_block=self._resolve_block_number,
        )

    def get_dy(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """@notice Calculate the current output dy given input dx.

        @dev Index values can be found via the `coins` public getter method
        @param i Index value for the coin to send
        @param j Index value of the coin to recieve
        @param dx Amount of `i` being exchanged
        @return Amount of `j` predicted.

        Reference: https://github.com/curveresearch/notes/blob/main/stableswap.pdf

        Delegates to the Rust-owned `Pool.curve().get_dy`: the I/O orchestration
        (amp/rates/xp + provider fetches) and the pure dy math both run in the
        Rust core, so this is a single handle
        call with no Python provider / cache / calculator on the swap path.

        Returns:
            The computed integer value.

        """
        return self.swap_quoter.get_dy(
            i,
            j,
            dx,
            block_identifier=block_identifier,
            override_state=override_state,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: CurveStableswapPoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current pool reserves.

        The zero-balance refusal is core-owned: the direct-pool arm delegates to
        `get_dy` -> the Rust `curve().get_dy`, which raises the typed `NoLiquidity`
        where the balances are read (the Python-side raise is gone).

        Returns:
            The computed integer value.

        """
        return self.swap_quoter.exact_in(
            token_in,
            token_out,
            token_in_quantity,
            override_state=override_state,
            block_identifier=block_identifier,
        )


class _LazyBasePool:
    """Production adapter satisfying ``BasePoolPort`` for a metapool's base pool.

    Holds the base pool's ``Pool`` handle (resolved by the Rust
    go-between ``curve().base_pool()`` — same shared ``BotState`` core, no
    Python registry lookup) and memoises the base companion on first use.
    Defers construction so a metapool that never takes the base swap path
    pays zero base-pool cost, and at most one companion across a full calc
    (ADR-005). Satisfies the ``BasePoolPort`` surface — the six
    members the ``DyCalculator`` actually calls.

    Defined after ``CurveStableswapPool`` (forward reference); resolved as a
    module global at call time from within ``CurveStableswapPool.from_handle``.
    """

    __slots__ = ("_built", "_handle")

    def __init__(self, handle: Pool) -> None:
        self._handle = handle
        self._built = None

    def _pool(self) -> CurveStableswapPool:
        if self._built is None:
            self._built = CurveStableswapPool.from_handle(self._handle)
        return self._built

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        return self._pool().tokens

    @property
    def balances(self) -> tuple[int, ...]:
        return self._pool().balances

    @property
    def fee(self) -> int:
        return self._pool().fee

    def calc_token_amount(self, *args: Any, **kwargs: Any) -> int:
        return self._pool().calc_token_amount(*args, **kwargs)

    def get_dy(self, *args: Any, **kwargs: Any) -> int:
        return self._pool().get_dy(*args, **kwargs)

    def calc_withdraw_one_coin(self, *args: Any, **kwargs: Any) -> tuple[int, ...]:
        return self._pool().calc_withdraw_one_coin(*args, **kwargs)
