"""Curve StableSwap liquidity pool implementation.

Implements the Curve StableSwap invariant for V1-style pools including
plain pools, metapools, lending pools, and crypto pools.
"""

import contextlib
import dataclasses
from collections.abc import Iterable, Sequence
from typing import TYPE_CHECKING, Any, Self
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.math import (
    stableswap_get_d as curve_stableswap_get_d,
)
from degenbot.curve.math import (
    stableswap_get_y as curve_stableswap_get_y,
)
from degenbot.curve.math import (
    stableswap_get_y_d as curve_stableswap_get_y_d,
)
from degenbot.curve.math import (
    stableswap_newton_y as curve_stableswap_newton_y,
)
from degenbot.curve.math import (
    stableswap_reduction_coefficient as curve_stableswap_reduction_coefficient,
)
from degenbot.curve.per_block_cache import PerBlockCache
from degenbot.curve.stableswap_pool_state import StableswapPoolState
from degenbot.curve.strategies import PoolStrategies
from degenbot.curve.types import (
    BasePoolPort,
    CurveDataProvider,
    CurveStableswapPoolExternalUpdate,
    CurveStableswapPoolState,
    CurveStableSwapPoolStateUpdated,
    DVariant,
    DyCalculationInputs,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    SwapStyle,
    YDVariant,
    YVariant,
)
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.arbitrage import NoLiquidity
from degenbot.exceptions.pool import EVMRevertError, InvalidSwapInputAmount, MissingCurveData
from degenbot.logging import logger
from degenbot.types import PyLiquidityPool
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import (
    PublisherMixin,
    Subscriber,
)
from degenbot.types.pool_protocols import SimulationResult
from degenbot.types.rpc_types import BlockIdentifier


def _compute_rate_and_precision_multipliers(
    tokens: Sequence[Erc20Token],
    precision_multipliers: Sequence[int] | None,
    precision_decimals: int,
) -> tuple[tuple[int, ...], tuple[int, ...]]:
    """Derive Curve rate + precision multipliers from token decimals.

    Single source of truth — shared by ``CurveStableswapPool.__init__`` and
    ``make_curve_pool`` (the factory passes the derived ``rate_multipliers`` to
    ``PyBot.register_curve_pool`` so the Rust core stores the same values the
    companion keeps; they're consumed by the future Rust ``get_dy``, ADR-005
    slice 11c). Mirrors the pre-companion derivation exactly.

    Returns:
        ``(rate_multipliers, precision_multipliers)``.

    """
    rate_multipliers = tuple(10 ** (2 * precision_decimals - token.decimals) for token in tokens)
    if precision_multipliers is not None:
        pms = tuple(precision_multipliers)
        rate_multipliers = tuple(pm * 10**precision_decimals for pm in pms)
    else:
        pms = tuple(10 ** (precision_decimals - token.decimals) for token in tokens)
    return rate_multipliers, pms


class _HandleCurveDataProviderAdapter:
    """Adapts a ``PyLiquidityPool`` handle as a stored ``CurveDataProvider``.

    The (BQM2OA) companion holds no Python data-provider object — the
    provider is the stored Rust trait object (ADR-005 JFGCHJ). This shim
    exposes the 13-method ``CurveDataProvider`` read interface by delegating
    each call to the handle's stored provider, mirroring the Balancer
    ``_HandleRateProviderAdapter`` (MBWSGP). ``MissingCurveData`` is raised on
    a missing provider / fetch miss so the calc path's existing error
    handling applies unchanged.
    """

    def __init__(self, py_pool: PyLiquidityPool) -> None:
        self._py_pool = py_pool

    def block_number(self) -> int:
        v = self._py_pool.fetch_curve_block_number()
        if v is None:
            raise MissingCurveData(self._py_pool.address, "block_number", "no data provider stored")
        return v

    def block_timestamp(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_block_timestamp(block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "block_timestamp", "no data provider stored"
            )
        return v

    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int:
        v = self._py_pool.fetch_curve_token_balance(token_address, holder_address, block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "token_balance", "no data provider stored"
            )
        return v

    def token_total_supply(self, token_address: str, block_number: int) -> int:
        v = self._py_pool.fetch_curve_token_total_supply(token_address, block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "token_total_supply", "no data provider stored"
            )
        return v

    def lending_rates(self, block_number: int) -> tuple[int, ...]:
        rates = self._py_pool.fetch_curve_lending_rates(block_number)
        if not rates:
            raise MissingCurveData(
                self._py_pool.address, "lending_rates", "no data provider stored"
            )
        return tuple(rates)

    def d(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_d(block_number)
        if v is None:
            raise MissingCurveData(self._py_pool.address, "d", "no data provider stored")
        return v

    def gamma(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_gamma(block_number)
        if v is None:
            raise MissingCurveData(self._py_pool.address, "gamma", "no data provider stored")
        return v

    def price_scale(self, block_number: int) -> tuple[int, ...]:
        scales = self._py_pool.fetch_curve_price_scale(block_number)
        if not scales:
            raise MissingCurveData(self._py_pool.address, "price_scale", "no data provider stored")
        return tuple(scales)

    def admin_balances(self, block_number: int) -> tuple[int, ...]:
        bal = self._py_pool.fetch_curve_admin_balances(block_number)
        if not bal:
            raise MissingCurveData(
                self._py_pool.address, "admin_balances", "no data provider stored"
            )
        return tuple(bal)

    def redemption_price(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_redemption_price(block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "redemption_price", "no data provider stored"
            )
        return v

    def base_cache_updated(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_base_cache_updated(block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "base_cache_updated", "no data provider stored"
            )
        return v

    def base_virtual_price(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_base_virtual_price(block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "base_virtual_price", "no data provider stored"
            )
        return v

    def virtual_price(self, block_number: int) -> int:
        v = self._py_pool.fetch_curve_virtual_price(block_number)
        if v is None:
            raise MissingCurveData(
                self._py_pool.address, "virtual_price", "no data provider stored"
            )
        return v


class CurveStableswapPool(
    PublisherMixin,
    StableswapPoolState,
    AbstractLiquidityPool,
):
    """CurveStableswapPool class."""

    type PoolState = CurveStableswapPoolState

    # Constants from contract
    # ref: https://github.com/curvefi/curve-contract/blob/master/contracts/pool-templates/base/SwapTemplateBase.vy
    PRECISION_DECIMALS: int = 18
    PRECISION: int = 10**PRECISION_DECIMALS

    # Class-scope instance-attribute declarations (red-knot): `_from_py_pool`
    # assigns these on `Self`; declare them at class scope so attribute reads
    # in helper/calc methods resolve (mirrors the Balancer/Aerodrome seams).
    address: ChecksumAddress
    _py_pool: PyLiquidityPool
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
    _cache: PerBlockCache
    _coin_index_type: str
    _name: str
    _subscribers: WeakSet[Subscriber]
    FEE_DENOMINATOR: int = 10**10
    A_PRECISION: int = 100
    MAX_COINS: int = 8
    # BASE_CACHE_EXPIRES moved to PerBlockCache

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # noqa: ARG002
        """Direct construction is forbidden.

        A ``CurveStableswapPool`` is a companion over a Rust-owned
        ``PyLiquidityPool`` handle. The handle can only be produced by
        registering a pool in a ``PyBot`` (production: ``Bot.build_pool()``;
        tests: ``make_curve_pool``), then wrapping via
        :meth:`_from_py_pool`. Direct constructor calls are rejected so that
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
            "A PyLiquidityPool handle is wired by Bot.build_pool() "
            "(production) or make_curve_pool (tests); call "
            f"{type(self).__name__}._from_py_pool(handle) to wrap a "
            "registered handle."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: PyLiquidityPool) -> Self:
        """Wrap a Rust-owned ``PyLiquidityPool`` handle as a Python companion.

        Single-arg seam (ADR-005 BQM2OA): reads *every* identity field + the
        stored data-provider trait object off the handle. The cross-pool
        references (base pool companion + underlying/LP tokens) are recovered
        from the handle too — the base pool via the Rust go-between
        ``curve_base_pool()`` (same shared ``BotState``, no Python registry),
        wrapped in a :class:`_LazyBasePool` that memoises construction.

        Returns:
            The companion wrapping the handle.

        Raises:
            DegenbotValueError: If the handle is not a Curve stableswap pool,
                or its tokens are not registered in the handle's Bot.

        """
        self = object.__new__(cls)

        # Family assertion — a V2/V3/V4/Balancer handle must raise, not crash.
        family = py_pool.pool_family
        if family != "curve":
            msg = (
                f"PyLiquidityPool handle is not a Curve stableswap pool "
                f"(got pool_family {family!r})"
            )
            raise DegenbotValueError(message=msg)

        self._py_pool = py_pool
        self.address = get_checksum_address(py_pool.address)

        # Tokens — recovered as companion handles (shared BotState).
        py_tokens = py_pool.get_curve_tokens()
        if py_tokens is None:
            msg = (
                "Curve pool tokens are not registered in the handle's Bot; "
                "register them via Bot.build_pool() / make_erc20 first."
            )
            raise DegenbotValueError(message=msg)
        self._tokens = tuple(
            Erc20Token._from_py_token(t)  # noqa: SLF001
            for t in py_tokens
        )

        self._a_coefficient = py_pool.curve_a_coefficient
        self._fee = py_pool.curve_fee
        self._admin_fee = py_pool.curve_admin_fee

        # Rate/precision multipliers — the Rust core stores exactly what the
        # builder registered (computed via _compute_rate_and_precision_multipliers);
        # the handle is the single source of truth.
        self._rate_multipliers = tuple(py_pool.curve_rate_multipliers)
        self._precision_multipliers = tuple(py_pool.curve_precision_multipliers)

        # Crypto-pool fees (None ⇔ 0 for standard stableswap pools). Guard narrows
        # the tuple[...] | None handle return (family asserted above, so never
        # None in practice).
        crypto_fees = py_pool.curve_crypto_fees()
        assert crypto_fees is not None  # pragma: no cover — curve family asserted
        fee_gamma, mid_fee, offpeg_fee_multiplier, out_fee, gamma = crypto_fees
        self._fee_gamma = fee_gamma if fee_gamma is not None else 0
        self._mid_fee = mid_fee if mid_fee is not None else 0
        self._offpeg_fee_multiplier = (
            offpeg_fee_multiplier if offpeg_fee_multiplier is not None else 0
        )
        self._out_fee = out_fee if out_fee is not None else 0
        self._gamma = gamma if gamma is not None else 0

        # Strategies — reconstruct from the 7 u8 discriminants stored in Rust.
        # auto()-based enums: .value is forwarded verbatim, so Enum(value)
        # round-trips (the factory's _strategies_to_rust_enums is the inverse).
        self._strategies = PoolStrategies(
            d_variant=DVariant(py_pool.curve_d_variant),
            y_variant=YVariant(py_pool.curve_y_variant),
            yd_variant=YDVariant(py_pool.curve_yd_variant),
            swap_style=SwapStyle(py_pool.curve_swap_style),
            metapool_rate_style=MetapoolRateStyle(py_pool.curve_metapool_rate_style),
            metapool_underlying_style=MetapoolUnderlyingStyle(
                py_pool.curve_metapool_underlying_style
            ),
            lending_rate_style=LendingRateStyle(py_pool.curve_lending_rate_style),
        )

        # Cross-pool base reference — the go-between returns a handle over the
        # base pool (same core); wrapped lazily so an unused base path pays zero.
        base_handle = py_pool.curve_base_pool()
        self._base_pool = _LazyBasePool(base_handle) if base_handle is not None else None

        # Underlying + LP tokens — recovered as companion handles.
        underlying = py_pool.get_curve_tokens_underlying()
        self._tokens_underlying = (
            tuple(Erc20Token._from_py_token(t) for t in underlying)  # noqa: SLF001
            if underlying is not None
            else None
        )
        lp = py_pool.get_curve_lp_token()
        self._lp_token = (
            Erc20Token._from_py_token(lp) if lp is not None else self._tokens[0]  # noqa: SLF001
        )

        ul = tuple(py_pool.curve_use_lending)
        self._use_lending = ul or tuple(False for _ in self._tokens)

        # A-ramping (all None ⇔ a plain non-ramping pool). Guard narrows the
        # tuple[...] | None handle return (family asserted above, so never
        # None in practice).
        curve_a_ramp = py_pool.curve_a_ramp()
        assert curve_a_ramp is not None  # pragma: no cover — curve family asserted
        (
            self._initial_a_coefficient,
            self._future_a_coefficient,
            self._initial_a_coefficient_time,
            self._future_a_coefficient_time,
            self._create_timestamp,
        ) = curve_a_ramp

        # Data provider — the stored Rust trait object, read through a handle
        # adapter (mirrors the Balancer _HandleRateProviderAdapter).
        self._data_provider = (
            _HandleCurveDataProviderAdapter(py_pool) if py_pool.curve_has_data_provider else None
        )

        # Per-block on-chain caches (cache depth is companion-owned mutable
        # cache config; the always-8 default applies — no caller passes a
        # non-default value (verified at task time), so this stays companion-side
        # and single-arg is preserved).
        self._cache = PerBlockCache(
            data_provider=self._data_provider,
            address=self.address,
            base_pool_is_set=self.base_pool is not None,
        )

        self._coin_index_type = "uint256"

        # The registration block (genesis journal delta). Used to pre-populate
        # base-cache virtual-price values for metapools at construction time.
        registration_block = py_pool.update_block
        if self.base_pool is not None and registration_block != 0:
            with contextlib.suppress(Exception):
                self._cache.get_cached_virtual_price(block_number=registration_block)

        fee_string = f"{100 * self.fee / self.FEE_DENOMINATOR:.2f}"
        token_string = "-".join([token.symbol for token in self._tokens])
        self._name = f"{token_string} ({self.__class__.__name__}, {fee_string}%)"

        self._subscribers = WeakSet()
        return self

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        token_string = "-".join([token.symbol for token in self._tokens])
        return f"{self.__class__.__name__}(address={self.address}, tokens={token_string}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%, A={self.a_coefficient})"  # noqa:E501

    @property
    def balances(self) -> tuple[int, ...]:
        """Balances.

        Read from the Rust core via the ``PyLiquidityPool`` handle
        (ADR-005 slice 11b). Rust ``BotState`` is the single source of truth
        for the mutable ``balances`` slot; this getter returns the live tuple.
        """
        return tuple(self._py_pool.balances)

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
        snap = self._py_pool.snapshot_curve()
        # snapshot_curve returns None only for a non-Curve pool_id; this
        # companion is always built over a registered Curve handle, so the
        # snapshot is always present. Defensive: treat None as no-state.
        if snap is None:  # pragma: no cover - defensive, unreachable in practice
            msg = f"No Curve pool state available for {self.address}"
            raise DegenbotValueError(message=msg)
        balances, block = snap
        return CurveStableswapPoolState(
            address=self.address,
            balances=tuple(balances),
            block=block,
        )

    @property
    def update_block(self) -> BlockNumber:
        """Update block (from Rust via the handle)."""
        return self._py_pool.update_block

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

        Delegates to the Rust core (``PyLiquidityPool.apply_curve_balance_update``)
        which journals the prior balances (genesis-anchor V2-style discipline)
        and lands the new balances + ``update_block`` atomically
        (ADR-005 slice 11b). The ``StateCache`` temporal-navigation layer it
        used to write is gone — the Rust reorg journal handles rollback now.

        Raises:
            DegenbotValueError: If the Rust core rejects the update (the pool
                is not registered as a Curve pool — unreachable for a companion
                built over a registered handle).

        """
        applied = self._py_pool.apply_curve_balance_update(
            list(update.balances),
            update.block_number,
        )
        if not applied:  # pragma: no cover - defensive, unreachable for a Curve handle
            msg = f"external_update rejected for {self.address} (not a Curve pool in Rust)"
            raise DegenbotValueError(message=msg)
        new_state = CurveStableswapPoolState(
            address=self.address,
            balances=update.balances,
            block=update.block_number,
        )
        self._notify_subscribers(
            CurveStableSwapPoolStateUpdated(state=new_state),
        )

    def _fetch_token_balance(
        self,
        token: Erc20Token,
        address: ChecksumAddress,
        *,
        block_identifier: int | None = None,
    ) -> int:
        """Fetch token balance using the data provider if available.

        Returns:
            The computed integer value.

        Raises:
            MissingCurveData: See function documentation.

        """
        if self._data_provider is not None and block_identifier is not None:
            return self._data_provider.token_balance(token.address, address, block_identifier)
        raise MissingCurveData(
            self.address,
            "token_balance",
            "Token balance fetch requires I/O. Provide a data_provider.",
        )

    def _fetch_token_total_supply(
        self,
        token: Erc20Token,
        *,
        block_identifier: int | None = None,
    ) -> int:
        """Fetch token total supply using the data provider if available.

        Returns:
            The computed integer value.

        Raises:
            MissingCurveData: See function documentation.

        """
        if self._data_provider is not None and block_identifier is not None:
            return self._data_provider.token_total_supply(token.address, block_identifier)
        raise MissingCurveData(
            self.address,
            "token_total_supply",
            "Token total supply fetch requires I/O. Provide a data_provider.",
        )

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
            timestamp = self._cache.get_cached_block_timestamp(0)

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
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """Simplified method to calculate addition or reduction in token supply at.

        deposit or withdrawal without taking fees into account (but looking at
        slippage).
        Needed to prevent front-running, not for precise calculations!

        Returns:
            The computed integer value.

        """
        n_coins = len(self._tokens)

        pool_balances = (
            list(override_state.balances) if override_state is not None else list(self.balances)
        )

        block_number = self._resolve_block_number(block_identifier)

        block_timestamp = self._cache.get_cached_block_timestamp(block_number)
        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        amp = self._a(timestamp=block_timestamp)
        d_0 = self._get_d(_xp=xp, _amp=amp)

        for i in range(n_coins):
            if deposit:
                pool_balances[i] += amounts[i]
            else:
                pool_balances[i] -= amounts[i]

        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        d_1 = self._get_d(xp, amp)
        token_amount: int = self._fetch_token_total_supply(
            self.lp_token,
            block_identifier=block_number,
        )

        diff = d_1 - d_0 if deposit else d_0 - d_1

        return diff * token_amount // d_0

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

        block_timestamp = self._cache.get_cached_block_timestamp(block_number)
        n_coins = len(self._tokens)
        amp = self._a(timestamp=block_timestamp)
        total_supply = self._fetch_token_total_supply(self.lp_token, block_identifier=block_number)
        precisions = self.precision_multipliers
        xp = self._xp(rates=self.rate_multipliers, balances=self.balances)
        d_0 = self._get_d(xp, amp)
        d_1 = d_0 - _token_amount * d_0 // total_supply
        new_y = self._get_y_d(amp, i, xp, d_1)
        dy_0 = (xp[i] - new_y) // precisions[i]

        xp_reduced = list(xp)
        fee = self.fee * n_coins // (4 * (n_coins - 1))
        for j in range(n_coins):
            dx_expected = xp[j] * d_1 // d_0 - new_y if j == i else xp[j] - xp[j] * d_1 // d_0
            xp_reduced[j] -= fee * dx_expected // self.FEE_DENOMINATOR

        dy = xp_reduced[i] - self._get_y_d(amp, i, xp_reduced, d_1)
        dy = (dy - 1) // precisions[i]

        return dy, dy_0 - dy, total_supply

    def _resolve_calculation_inputs_via_io(
        self,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> DyCalculationInputs:
        """Pre-resolve all data needed by DyCalculator implementations.

        All I/O, cache lookups, and rate resolution happen here.
        The calculator receives a frozen snapshot — no pool access needed.

        Returns:
            The computed value.

        Raises:
            MissingCurveData: See function documentation.

        """
        pool_balances = override_state.balances if override_state is not None else self.balances

        # Resolve block timestamp
        block_timestamp = self._cache.get_cached_block_timestamp(block_number)

        # Resolve amp with y_variant-aware A_PRECISION handling.
        # stableswap_get_y expects amp to be already divided by A_PRECISION
        # for VARIANT_0, and undivided for other variants.
        raw_amp = self._a(timestamp=block_timestamp)
        amp = (
            raw_amp // self.A_PRECISION
            if self._strategies.y_variant == YVariant.VARIANT_0
            else raw_amp
        )

        # Resolve rates (lending-rate I/O)
        if self._strategies.lending_rate_style == LendingRateStyle.NONE:
            resolved_rates = self.rate_multipliers
        else:
            if self._data_provider is None:
                raise MissingCurveData(
                    self.address,
                    "lending_rate",
                    "Data provider is required for pools with"
                    " lending tokens. Provide one via Bot.build_pool().",
                )
            resolved_rates = self._data_provider.lending_rates(block_number)

        # Compute XP
        xp = tuple(
            rate * balance // self.PRECISION
            for rate, balance in zip(resolved_rates, pool_balances, strict=True)
        )

        inputs = DyCalculationInputs(
            PRECISION=self.PRECISION,
            FEE_DENOMINATOR=self.FEE_DENOMINATOR,
            fee=self.fee,
            n_coins=len(self.tokens),
            balances=pool_balances,
            rate_multipliers=self.rate_multipliers,
            precision_multipliers=self.precision_multipliers,
            offpeg_fee_multiplier=self.offpeg_fee_multiplier,
            fee_gamma=self.fee_gamma,
            mid_fee=self.mid_fee,
            out_fee=self.out_fee,
            address=self.address,
            resolved_rates=resolved_rates,
            xp=xp,
            block_number=block_number,
            block_timestamp=block_timestamp,
            amp=amp,
            d_variant=self._strategies.d_variant,
            y_variant=self._strategies.y_variant,
            yd_variant=self._strategies.yd_variant,
            a_precision=self.A_PRECISION,
        )

        swap_style = self._strategies.swap_style

        # ── Crypto-specific I/O ──
        if swap_style == SwapStyle.CRYPTO:
            d_val = self._cache.get_cached_contract_d(block_number)
            gamma_val = self._cache.get_cached_gamma(block_number)
            price_scale_val = self._cache.get_cached_price_scale(block_number)

            return dataclasses.replace(
                inputs,
                d=d_val,
                gamma=gamma_val,
                price_scale=price_scale_val,
            )

        # ── Live-admin-specific I/O ──
        if swap_style in {
            SwapStyle.LIVE_ADMIN,
            SwapStyle.LIVE_ADMIN_DYNAMIC,
            SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION,
            SwapStyle.LIVE_ADMIN_ORACLE,
        }:
            if self._data_provider is None:
                raise MissingCurveData(
                    self.address,
                    "data_provider",
                    "Live-admin pool requires a data_provider"
                    " for token balances and admin balances.",
                )
            live_balances = tuple(
                self._data_provider.token_balance(token.address, self.address, block_number)
                for token in self._tokens
            )
            admin_balances = self._cache.get_cached_admin_balances(block_number)
            effective_balances = tuple(
                lb - ab for lb, ab in zip(live_balances, admin_balances, strict=True)
            )

            # For LIVE_ADMIN_ORACLE, re-resolve rates using effective balances
            if swap_style == SwapStyle.LIVE_ADMIN_ORACLE:
                oracle_rates = self._resolve_rates(
                    rates=self.rate_multipliers,
                    block_number=block_number,
                )
                oracle_xp = tuple(
                    rate * balance // self.PRECISION
                    for rate, balance in zip(oracle_rates, effective_balances, strict=True)
                )
            else:
                oracle_rates = resolved_rates
                oracle_xp = tuple(
                    rate * balance // self.PRECISION
                    for rate, balance in zip(resolved_rates, effective_balances, strict=True)
                )

            return dataclasses.replace(
                inputs,
                live_balances=live_balances,
                admin_balances=admin_balances,
                effective_balances=effective_balances,
                balances=effective_balances,
                resolved_rates=oracle_rates,
                xp=oracle_xp,
            )

        return inputs

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

        Returns:
            The computed integer value.

        """
        block_number = self._resolve_block_number(block_identifier)

        if self.base_pool is not None:
            # Metapool path — resolve metapool-specific inputs
            inputs = self._resolve_metapool_inputs_via_io(block_number, override_state)
            assert self._strategies.metapool_dy_calculator is not None
            return self._strategies.metapool_dy_calculator.calculate(
                i,
                j,
                dx,
                inputs=inputs,
                override_state=override_state,
            )

        # Non-metapool path — resolve standard/crypto/live-admin inputs
        inputs = self._resolve_calculation_inputs_via_io(block_number, override_state)

        assert self._strategies.dy_calculator is not None
        return self._strategies.dy_calculator.calculate(
            i,
            j,
            dx,
            inputs=inputs,
            override_state=override_state,
        )

    def _resolve_metapool_inputs_via_io(
        self,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> DyCalculationInputs:
        """Pre-resolve data needed by metapool DyCalculator implementations.

        Extends the base inputs with metapool-specific I/O (virtual price,
        redemption price, base pool reference).

        Returns:
            The computed value.

        """
        inputs = self._resolve_calculation_inputs_via_io(block_number, override_state)

        # Resolve virtual price
        virtual_price = self._cache.get_cached_virtual_price(block_number)

        # Resolve scaled redemption price (may not be available for all metapools)
        scaled_redemption_price: int | None = None
        with contextlib.suppress(MissingCurveData):
            scaled_redemption_price = self._cache.get_cached_scaled_redemption_price(block_number)

        return dataclasses.replace(
            inputs,
            virtual_price=virtual_price,
            scaled_redemption_price=scaled_redemption_price,
            base_pool=self.base_pool,
        )

    def _get_dy_underlying(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        block_number = self._resolve_block_number(block_identifier)
        inputs = self._resolve_metapool_inputs_via_io(block_number, override_state)

        assert self._strategies.metapool_underlying_dy_calculator is not None
        return self._strategies.metapool_underlying_dy_calculator.calculate(
            i,
            j,
            dx,
            inputs=inputs,
            override_state=override_state,
        )

    def _get_d(self, _xp: Sequence[int], _amp: int) -> int:
        """Solve for the Curve stableswap invariant D.

        Delegates to the pure function stableswap_get_d. Kept as a thin
        wrapper for backwards compatibility with callers that access pool state.

        Returns:
            The computed integer value.

        Raises:
            EVMRevertError: See function documentation.

        """
        try:
            return curve_stableswap_get_d(
                list(_xp),
                _amp,
                len(self._tokens),
                self.A_PRECISION,
                self._strategies.d_variant.value,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _get_y(self, i: int, j: int, x: int, xp: Sequence[int]) -> int:
        """Calculate x[j] if one makes x[i] = x.

        Delegates to the pure function stableswap_get_y. Resolves amp from
        the pool's A-ramping state and block timestamps before calling.

        Returns:
            The computed integer value.

        Raises:
            EVMRevertError: See function documentation.

        """
        amp = (
            self._a(timestamp=self._cache.get_cached_block_timestamp(self.update_block))
            // self.A_PRECISION
            if self._strategies.y_variant == YVariant.VARIANT_0
            else self._a(timestamp=self._cache.get_cached_block_timestamp(self.update_block))
        )
        try:
            return curve_stableswap_get_y(
                i,
                j,
                x,
                list(xp),
                amp,
                len(self._tokens),
                self.A_PRECISION,
                self._strategies.y_variant.value,
                self._strategies.d_variant.value,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _get_y_d(self, a: int, i: int, xp: Sequence[int], d: int) -> int:
        """Calculate y given A, xp, and D.

        Delegates to the pure function stableswap_get_y_d.

        Returns:
            The computed integer value.

        Raises:
            EVMRevertError: See function documentation.

        """
        try:
            return curve_stableswap_get_y_d(
                a,
                i,
                list(xp),
                d,
                len(self._tokens),
                self.A_PRECISION,
                self._strategies.yd_variant.value,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _resolve_rates(
        self,
        *,
        rates: tuple[int, ...],
        block_number: int,
    ) -> tuple[int, ...]:
        """Select rates based on the pool's lending rate style.

        Returns rate_multipliers for NONE, or calls the data provider
        for lending pools.

        Returns:
            The computed value.

        Raises:
            MissingCurveData: See function documentation.

        """
        if self._strategies.lending_rate_style == LendingRateStyle.NONE:
            return rates

        if self._data_provider is None:
            raise MissingCurveData(
                self.address,
                "lending_rate",
                "Data provider is required for pools with lending tokens. "
                "Provide one via Bot.build_pool().",
            )
        return self._data_provider.lending_rates(block_number)

    def _xp(self, rates: Iterable[int], balances: Iterable[int]) -> tuple[int, ...]:
        return tuple(
            rate * balance // self.PRECISION for rate, balance in zip(rates, balances, strict=True)
        )

    def _newton_y(self, ann: int, gamma: int, xp: Sequence[int], d: int, token_index: int) -> int:
        """Calculate xp[i] given other balances and invariant D using Newton's method.

        Delegates to the pure function stableswap_newton_y.
        Used by crypto (volatile) Curve pools.

        Returns:
            The computed integer value.

        Raises:
            EVMRevertError: See function documentation.

        """
        try:
            return curve_stableswap_newton_y(
                ann,
                gamma,
                list(xp),
                d,
                token_index,
                len(self._tokens),
                self.A_PRECISION,
            )
        except ValueError as e:
            raise EVMRevertError(
                error=f"_newton_y() did not converge for pool {self.address}",
            ) from e

    @staticmethod
    def _reduction_coefficient(x: Sequence[int], fee_gamma: int, n_coins: int) -> int:
        """fee_gamma / (fee_gamma + (1 - K)) where K = prod(x) / (sum(x) / N)**N.

        Delegates to the pure function stableswap_reduction_coefficient.

        Returns:
            The computed integer value.

        """
        return curve_stableswap_reduction_coefficient(list(x), fee_gamma, n_coins)

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: CurveStableswapPoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Calculate the expected token OUTPUT for a target INPUT at current pool reserves.

        Returns:
            The computed integer value.

        Raises:
            DegenbotValueError: See function documentation.
            InvalidSwapInputAmount: See function documentation.
            NoLiquidity: See function documentation.

        """
        block_number = self._resolve_block_number(block_identifier)

        if token_in_quantity <= 0:
            raise InvalidSwapInputAmount

        if override_state:
            logger.debug("Overrides applied:")
            logger.debug(f"Balances: {override_state.balances}")

        tokens_used_this_pool = [
            token_in in self._tokens,
            token_out in self._tokens,
        ]

        tokens_used_in_base_pool = []
        if self.base_pool is not None:
            tokens_used_in_base_pool = [
                token_in in self.base_pool.tokens,
                token_out in self.base_pool.tokens,
            ]

        if all(tokens_used_this_pool):
            if any(balance == 0 for balance in self.balances):
                raise NoLiquidity(message="One or more of the tokens has a zero balance.")

            return self.get_dy(
                i=self._tokens.index(token_in),
                j=self._tokens.index(token_out),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )
        if any(tokens_used_this_pool) and any(tokens_used_in_base_pool):
            if TYPE_CHECKING:
                assert self.base_pool is not None
                assert self.tokens_underlying is not None

            # TODO:  # noqa: FIX002 see if any of these checks are unnecessary (partial zero balance OK?)
            if any(balance == 0 for balance in self.base_pool.balances):
                raise NoLiquidity(message="One or more of the base pool tokens has a zero balance.")
            if any(balance == 0 for balance in self.balances):
                raise NoLiquidity(message="One or more of the tokens has a zero balance.")

            token_in_from_metapool = token_in in self._tokens
            token_out_from_metapool = token_out in self._tokens
            assert token_in_from_metapool or token_out_from_metapool

            if token_in_from_metapool and self.balances[self._tokens.index(token_in)] == 0:
                raise NoLiquidity(message=f"{token_in} has a zero balance.")
            if token_out_from_metapool and self.balances[self._tokens.index(token_out)] == 0:
                raise NoLiquidity(message=f"{token_out} has a zero balance.")

            token_in_from_basepool = token_in in self.base_pool.tokens
            token_out_from_basepool = token_out in self.base_pool.tokens
            assert token_in_from_basepool or token_out_from_basepool

            if (
                token_in_from_basepool
                and self.base_pool.balances[self.base_pool.tokens.index(token_in)] == 0
            ):
                raise NoLiquidity(message=f"{token_in} has a zero balance.")
            if (
                token_out_from_basepool
                and self.base_pool.balances[self.base_pool.tokens.index(token_out)] == 0
            ):
                raise NoLiquidity(message=f"{token_out} has a zero balance.")

            return self._get_dy_underlying(
                i=(
                    self._tokens.index(token_in)
                    if token_in_from_metapool
                    else self.tokens_underlying.index(token_in)
                ),
                j=(
                    self._tokens.index(token_out)
                    if token_out_from_metapool
                    else self.tokens_underlying.index(token_out)
                ),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )
        if all(tokens_used_in_base_pool):
            if TYPE_CHECKING:
                assert self.tokens_underlying is not None
            token_in_from_basepool = token_in in self.tokens_underlying
            token_out_from_basepool = token_out in self.tokens_underlying
            assert token_in_from_basepool or token_out_from_basepool

            return self._get_dy_underlying(
                i=self.tokens_underlying.index(token_in),
                j=self.tokens_underlying.index(token_out),
                dx=token_in_quantity,
                block_identifier=block_number,
                override_state=override_state,
            )

        raise DegenbotValueError(
            message="Tokens not held by pool or in underlying base pool",
        )  # pragma: no cover

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
        curve_state: CurveStableswapPoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, CurveStableswapPoolState):
                msg = f"Expected CurveStableswapPoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            curve_state = state_override
        token_in_obj = next((t for t in self._tokens if t.address == token_in), None)
        if token_in_obj is None:
            all_tokens = list(self._tokens)
            if self.base_pool is not None:
                all_tokens.extend(self.base_pool.tokens)
            if token_in not in {t.address for t in all_tokens}:
                raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        token_out_obj = next((t for t in self._tokens if t.address == token_out), None)
        if token_out_obj is None:
            all_tokens = list(self._tokens)
            if self.base_pool is not None:
                all_tokens.extend(self.base_pool.tokens)
            if token_out not in {t.address for t in all_tokens}:
                raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        if token_in_obj is None or token_out_obj is None:
            msg = f"token_in {token_in} or token_out {token_out} not found in pool tokens"
            raise DegenbotValueError(message=msg)

        initial_state = curve_state or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_out=token_out_obj,
            token_in_quantity=amount_in,
            override_state=curve_state,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )


class _LazyBasePool:
    """Production adapter satisfying ``BasePoolPort`` for a metapool's base pool.

    Holds the base pool's ``PyLiquidityPool`` handle (resolved by the Rust
    go-between ``curve_base_pool()`` — same shared ``BotState`` core, no
    Python registry lookup) and memoises the base companion on first use.
    Defers construction so a metapool that never takes the base swap path
    pays zero base-pool cost, and at most one companion across a full calc
    (ADR-005 BQM2OA). Satisfies the ``BasePoolPort`` surface — the six
    members the ``DyCalculator`` actually calls.

    Defined after ``CurveStableswapPool`` (forward reference); resolved as a
    module global at call time from within ``CurveStableswapPool._from_py_pool``.
    """

    __slots__ = ("_built", "_handle")

    def __init__(self, handle: PyLiquidityPool) -> None:
        self._handle = handle
        self._built = None

    def _pool(self) -> CurveStableswapPool:
        if self._built is None:
            self._built = CurveStableswapPool._from_py_pool(self._handle)  # noqa: SLF001
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
