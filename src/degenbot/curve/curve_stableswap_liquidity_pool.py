"""Curve StableSwap liquidity pool implementation.

Implements the Curve StableSwap invariant for V1-style pools including
plain pools, metapools, lending pools, and crypto pools.
"""

import contextlib
import dataclasses
from collections.abc import Iterable, Sequence
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar, cast
from weakref import WeakSet

from eth_typing import ChecksumAddress
from web3.types import BlockIdentifier

from degenbot.calculations.stableswap import (
    stableswap_get_d,
    stableswap_get_y,
    stableswap_get_y_d,
    stableswap_newton_y,
    stableswap_reduction_coefficient,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.stableswap_pool_state import StableswapPoolState
from degenbot.curve.types import (
    CurveDataProvider,
    CurveStableswapPoolExternalUpdate,
    CurveStableswapPoolState,
    CurveStableSwapPoolStateUpdated,
    DyCalculationInputs,
    LendingRateStyle,
    PoolStrategies,
    SwapStyle,
    YVariant,
)
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.arbitrage import NoLiquidity
from degenbot.exceptions.pool import EVMRevertError, InvalidSwapInputAmount, MissingCurveData
from degenbot.logging import logger
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import (
    BoundedCache,
    PublisherMixin,
    Subscriber,
)
from degenbot.types.hop_types import CurveStableswapHop, HopType, PoolInvariant
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult


class CurveStableswapPool(
    PublisherMixin,
    PoolPickleMixin,
    StableswapPoolState,
    AbstractLiquidityPool,
):
    type PoolState = CurveStableswapPoolState
    _state_cache: BoundedCache[BlockNumber, PoolState]

    LOG_HANDLERS: ClassVar[dict[str, Any]] = {}  # Curve stays on polling

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
        "_data_provider",  # can't pickle closures
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_state_lock": Lock,
        "_subscribers": WeakSet,
        "_data_provider": lambda: None,
    }

    # Constants from contract
    # ref: https://github.com/curvefi/curve-contract/blob/master/contracts/pool-templates/base/SwapTemplateBase.vy
    PRECISION_DECIMALS: int = 18
    PRECISION: int = 10**PRECISION_DECIMALS
    FEE_DENOMINATOR: int = 10**10
    A_PRECISION: int = 100
    MAX_COINS: int = 8
    BASE_CACHE_EXPIRES: int = 10 * 60  # 10 minutes in seconds

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        tokens: Sequence[Erc20Token],
        a_coefficient: int,
        fee: int,
        admin_fee: int,
        balances: Sequence[int],
        chain_id: ChainId | None = None,
        state_block: BlockNumber | None = None,
        state_cache_depth: int = 8,
        # On-chain data access (replaces 13 individual callback parameters)
        data_provider: CurveDataProvider | None = None,
        # Pool configuration
        base_pool: "CurveStableswapPool | None" = None,
        tokens_underlying: Sequence[Erc20Token] | None = None,
        lp_token: Erc20Token | None = None,
        use_lending: Sequence[bool] | None = None,
        precision_multipliers: Sequence[int] | None = None,
        # A ramping configuration
        initial_a_coefficient: int | None = None,
        future_a_coefficient: int | None = None,
        initial_a_coefficient_time: int | None = None,
        future_a_coefficient_time: int | None = None,
        create_timestamp: int | None = None,
        # Crypto pool parameters
        fee_gamma: int | None = None,
        mid_fee: int | None = None,
        out_fee: int | None = None,
        gamma: int | None = None,
        offpeg_fee_multiplier: int | None = None,
        # Strategy enums (resolved by builder from pool address)
        strategies: PoolStrategies | None = None,
    ) -> None:
        """
        A Curve V1 (StableSwap) pool.

        Constructed from pre-fetched data only. Use Bot.build_pool() to fetch from chain.

        I/O boundary:
            Construction is I/O-free — all immutable parameters (A, fee, tokens, strategies)
            are provided by the builder. However, get_dy() and related calculation
            methods may call CurveDataProvider methods for per-block on-chain data.

            I/O-free at calculation time for: plain pools (STANDARD, RAW_BALANCE).
            I/O-required at calculation time for: lending pools, crypto pools,
            live-admin pools, metapools, and pools with A ramping.

            The data_provider must be available for calculation-time I/O. Pools
            constructed without a data_provider can only perform calculations
            that don't require on-chain data.
        """

        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else tokens[0].chain_id
        state_block = state_block if state_block is not None else 0

        self._tokens: tuple[Erc20Token, ...] = tuple(tokens)
        self._a_coefficient = a_coefficient
        self._fee = fee
        self._admin_fee = admin_fee

        # Derive rate/precision multipliers from token decimals
        self._rate_multipliers = tuple(
            10 ** (2 * self.PRECISION_DECIMALS - token.decimals) for token in self._tokens
        )
        if precision_multipliers is not None:
            self._precision_multipliers = tuple(precision_multipliers)
            # Recompute rate_multipliers to be consistent
            self._rate_multipliers = tuple(
                pm * 10**self.PRECISION_DECIMALS for pm in self._precision_multipliers
            )
        else:
            self._precision_multipliers = tuple(
                cast("int", 10 ** (self.PRECISION_DECIMALS - token.decimals))
                for token in self._tokens
            )

        # Set defaults for optional/variant attributes
        self._fee_gamma = fee_gamma if fee_gamma is not None else 0
        self._mid_fee = mid_fee if mid_fee is not None else 0
        self._offpeg_fee_multiplier = (
            offpeg_fee_multiplier if offpeg_fee_multiplier is not None else 0
        )
        self._out_fee = out_fee if out_fee is not None else 0
        self._gamma = gamma if gamma is not None else 0

        # Variant computation strategies (resolved by builder from pool address)
        self._strategies = strategies if strategies is not None else PoolStrategies()

        # Pool configuration
        self._base_pool = base_pool
        self._tokens_underlying = tuple(tokens_underlying) if tokens_underlying else None
        self._lp_token = lp_token if lp_token is not None else self._tokens[0]
        self._use_lending = (
            tuple(use_lending) if use_lending else tuple(False for _ in self._tokens)
        )

        # A ramping configuration
        self._initial_a_coefficient = initial_a_coefficient
        self._initial_a_coefficient_time = initial_a_coefficient_time
        self._future_a_coefficient = future_a_coefficient
        self._future_a_coefficient_time = future_a_coefficient_time
        self._create_timestamp = create_timestamp

        # On-chain data access
        self._data_provider = data_provider

        self._coin_index_type = "uint256"

        # State caches
        self._state_cache: BoundedCache[BlockNumber, CurveStableswapPoolState] = BoundedCache(
            max_items=state_cache_depth
        )
        self._state = CurveStableswapPoolState(
            address=self.address,
            balances=tuple(balances),
            block=state_block,
        )
        self._state_cache[state_block] = self._state
        self._state_lock = Lock()

        # Per-block on-chain caches (formerly CurveOnChainCache, Plan 068)
        # Note: _rates from CurveOnChainCache is dead code (never accessed) — not migrated
        self._cache_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_contract_D: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_gamma: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_block_timestamps: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        # Latest-value mirrors for virtual_price expiry logic
        self._base_cache_updated_value: int | None = None
        self._base_virtual_price_value: int = 0

        # Pre-populate base cache values for metapools at construction time,
        # matching the contract's _vp_rate_ro() cache behavior.
        if self.base_pool is not None and state_block != 0:
            with contextlib.suppress(Exception):
                self._get_cached_base_cache_updated(block_number=state_block)
            with contextlib.suppress(Exception):
                self._get_cached_base_virtual_price(block_number=state_block)

        fee_string = f"{100 * self.fee / self.FEE_DENOMINATOR:.2f}"
        token_string = "-".join([token.symbol for token in self._tokens])
        self._name = f"{token_string} ({self.__class__.__name__}, {fee_string}%)"

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        # I/O access for on-chain data fetching
        # I/O is done via data_provider injected by Bot.build_pool()

    def __repr__(self) -> str:  # pragma: no cover
        token_string = "-".join([token.symbol for token in self._tokens])
        return f"{self.__class__.__name__}(address={self.address}, tokens={token_string}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%, A={self.a_coefficient})"  # noqa:E501

    @property
    def balances(self) -> tuple[int, ...]:
        return self.state.balances

    @property
    def chain_id(self) -> int | None:
        return self._chain_id

    @property
    def state(self) -> CurveStableswapPoolState:
        return self._state

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

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
        """Apply an external state update with new balances."""
        new_state = CurveStableswapPoolState(
            address=self.address,
            balances=update.balances,
            block=update.block_number,
        )
        with self._state_lock:
            self._state = new_state
            self._state_cache[update.block_number] = new_state

        self._notify_subscribers(
            CurveStableSwapPoolStateUpdated(state=new_state),
        )

    def _fetch_token_balance(
        self, token: Erc20Token, address: ChecksumAddress, *, block_identifier: int | None = None
    ) -> int:
        """Fetch token balance using the data provider if available."""
        if self._data_provider is not None and block_identifier is not None:
            return self._data_provider.token_balance(token.address, address, block_identifier)
        raise MissingCurveData(
            self.address,
            "token_balance",
            "Token balance fetch requires I/O. Provide a data_provider.",
        )

    def _fetch_token_total_supply(
        self, token: Erc20Token, *, block_identifier: int | None = None
    ) -> int:
        """Fetch token total supply using the data provider if available."""
        if self._data_provider is not None and block_identifier is not None:
            return self._data_provider.token_total_supply(token.address, block_identifier)
        raise MissingCurveData(
            self.address,
            "token_total_supply",
            "Token total supply fetch requires I/O. Provide a data_provider.",
        )

    def _resolve_block_number(self, block_identifier: BlockIdentifier | None) -> int:
        """Resolve a block identifier to an integer. Falls back to data provider if available."""
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

    # ── Per-block cached accessors (formerly CurveOnChainCache, Plan 068) ──

    def _get_cached_block_timestamp(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached block timestamp."""
        with contextlib.suppress(KeyError):
            return self._cache_block_timestamps[block_number]
        if self._data_provider is None:
            msg = "block_timestamp requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "block_timestamp", msg)
        result = self._data_provider.block_timestamp(block_number)
        self._cache_block_timestamps[block_number] = result
        return result

    def _get_cached_contract_D(self, block_number: BlockNumber) -> int:  # noqa: N802
        """Fetch or retrieve cached contract D value (crypto pools)."""
        with contextlib.suppress(KeyError):
            return self._cache_contract_D[block_number]
        if self._data_provider is None:
            msg = "contract_D requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "D", msg)
        result = self._data_provider.D(block_number)
        self._cache_contract_D[block_number] = result
        return result

    def _get_cached_gamma(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached gamma value (crypto pools)."""
        with contextlib.suppress(KeyError):
            return self._cache_gamma[block_number]
        if self._data_provider is None:
            msg = "gamma requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "gamma", msg)
        result = self._data_provider.gamma(block_number)
        self._cache_gamma[block_number] = result
        return result

    def _get_cached_price_scale(self, block_number: BlockNumber) -> tuple[int, ...]:
        """Fetch or retrieve cached price scale (crypto pools)."""
        with contextlib.suppress(KeyError):
            return self._cache_price_scale[block_number]
        if self._data_provider is None:
            msg = "price_scale requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "price_scale", msg)
        result = self._data_provider.price_scale(block_number)
        self._cache_price_scale[block_number] = result
        return result

    def _get_cached_admin_balances(self, block_number: BlockNumber) -> tuple[int, ...]:
        """Fetch or retrieve cached admin balances."""
        with contextlib.suppress(KeyError):
            return self._cache_admin_balances[block_number]
        if self._data_provider is None:
            msg = "admin_balances requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "admin_balances", msg)
        result = self._data_provider.admin_balances(block_number)
        self._cache_admin_balances[block_number] = result
        return result

    def _get_cached_scaled_redemption_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached scaled redemption price."""
        with contextlib.suppress(KeyError):
            return self._cache_scaled_redemption_price[block_number]
        if self._data_provider is None:
            msg = (
                "scaled_redemption_price requires a data_provider."
                " Provide one via Bot.build_pool()."
            )
            raise MissingCurveData(self.address, "redemption_price", msg)
        result = self._data_provider.redemption_price(block_number)
        self._cache_scaled_redemption_price[block_number] = result
        return result

    def _get_cached_base_cache_updated(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached base_cache_updated block number.

        Updates _base_cache_updated_value as a side effect.
        """
        with contextlib.suppress(KeyError):
            return self._cache_base_cache_updated[block_number]
        if self._data_provider is None:
            msg = "base_cache_updated requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "base_cache_updated", msg)
        result = self._data_provider.base_cache_updated(block_number)
        self._cache_base_cache_updated[block_number] = result
        # Side effect: mirror for virtual_price expiry logic
        self._base_cache_updated_value = result
        return result

    def _get_cached_base_virtual_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached base pool virtual price."""
        with contextlib.suppress(KeyError):
            return self._cache_base_virtual_price[block_number]
        if self._data_provider is None:
            msg = "base_virtual_price requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "base_virtual_price", msg)
        result = self._data_provider.base_virtual_price(block_number)
        self._cache_base_virtual_price[block_number] = result
        return result

    def _get_cached_virtual_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached virtual price.

        For metapools, uses base_cache_updated expiry logic:
        - If base cache has expired or is unset, fetches live virtual_price
        - If base cache is still valid, uses cached base_virtual_price
        """
        with contextlib.suppress(KeyError):
            return self._cache_virtual_price[block_number]

        # Determine virtual price from base pool cache expiry
        # (metapool logic matching the contract's _vp_rate_ro() cache behavior)
        base_virtual_price: int
        if (
            self._base_cache_updated_value is None
            or self._cache_block_timestamps.get(block_number, 0)
            > self._base_cache_updated_value + self.BASE_CACHE_EXPIRES
        ):
            # Cache is not set or has expired — fetch live virtual_price
            if self._data_provider is None:
                msg = "virtual_price requires a data_provider. Provide one via Bot.build_pool()."
                raise MissingCurveData(self.address, "virtual_price", msg)
            base_virtual_price = self._data_provider.virtual_price(block_number)
        else:
            # Cache is still valid — use the cached base_virtual_price
            base_virtual_price = self._base_virtual_price_value

        self._cache_virtual_price[block_number] = base_virtual_price
        self._base_virtual_price_value = base_virtual_price
        return base_virtual_price

    def _a(self, timestamp: int | None = None) -> int:
        """
        Handle ramping A up or down
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
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """
        Simplified method to calculate addition or reduction in token supply at
        deposit or withdrawal without taking fees into account (but looking at
        slippage).
        Needed to prevent front-running, not for precise calculations!
        """

        n_coins = len(self._tokens)

        pool_balances = (
            list(override_state.balances) if override_state is not None else list(self.balances)
        )

        block_number = self._resolve_block_number(block_identifier)

        block_timestamp = self._get_cached_block_timestamp(block_number)
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
            self.lp_token, block_identifier=block_number
        )

        diff = d_1 - d_0 if deposit else d_0 - d_1

        return diff * token_amount // d_0

    def calc_withdraw_one_coin(
        self, _token_amount: int, i: int, block_identifier: BlockIdentifier | None = None
    ) -> tuple[int, ...]:
        block_number = self._resolve_block_number(block_identifier)

        block_timestamp = self._get_cached_block_timestamp(block_number)
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
        """
        pool_balances = override_state.balances if override_state is not None else self.balances

        # Resolve block timestamp
        block_timestamp = self._get_cached_block_timestamp(block_number)

        # Resolve amp (A ramping)
        amp = self._a(timestamp=block_timestamp)

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

        # get_y closure — encapsulates amp resolution, variant dispatch, EVMRevertError wrapping
        def get_y(i: int, j: int, x: int, xp_: Sequence[int]) -> int:
            return self._get_y(i, j, x, xp_)

        # newton_y closure — encapsulates crypto invariant solving
        def newton_y(ann: int, gamma: int, xp_: Sequence[int], d: int, token_index: int) -> int:
            return self._newton_y(ann, gamma, xp_, d, token_index)

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
            get_y=get_y,
            newton_y=newton_y,
        )

        swap_style = self._strategies.swap_style

        # ── Crypto-specific I/O ──
        if swap_style == SwapStyle.CRYPTO:
            d_val = self._get_cached_contract_D(block_number)
            gamma_val = self._get_cached_gamma(block_number)
            price_scale_val = self._get_cached_price_scale(block_number)

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
            admin_balances = self._get_cached_admin_balances(block_number)
            effective_balances = tuple(
                lb - ab for lb, ab in zip(live_balances, admin_balances, strict=True)
            )

            # For LIVE_ADMIN_ORACLE, re-resolve rates using effective balances
            if swap_style == SwapStyle.LIVE_ADMIN_ORACLE:
                oracle_rates = self._resolve_rates(
                    rates=self.rate_multipliers,
                    block_number=block_number,
                    pool_balances=effective_balances,
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
        """
        @notice Calculate the current output dy given input dx
        @dev Index values can be found via the `coins` public getter method
        @param i Index value for the coin to send
        @param j Index value of the coin to recieve
        @param dx Amount of `i` being exchanged
        @return Amount of `j` predicted

        Reference: https://github.com/curveresearch/notes/blob/main/stableswap.pdf
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
        """
        inputs = self._resolve_calculation_inputs_via_io(block_number, override_state)

        # Resolve virtual price
        virtual_price = self._get_cached_virtual_price(block_number)

        # Resolve scaled redemption price (may not be available for all metapools)
        scaled_redemption_price: int | None = None
        with contextlib.suppress(MissingCurveData):
            scaled_redemption_price = self._get_cached_scaled_redemption_price(block_number)

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
        """
        try:
            return stableswap_get_d(
                xp=_xp,
                amp=_amp,
                n_coins=len(self._tokens),
                a_precision=self.A_PRECISION,
                d_variant=self._strategies.d_variant,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _get_y(self, i: int, j: int, x: int, xp: Sequence[int]) -> int:
        """Calculate x[j] if one makes x[i] = x.

        Delegates to the pure function stableswap_get_y. Resolves amp from
        the pool's A-ramping state and block timestamps before calling.
        """
        amp = (
            self._a(timestamp=self._get_cached_block_timestamp(self.update_block))
            // self.A_PRECISION
            if self._strategies.y_variant == YVariant.VARIANT_0
            else self._a(
                timestamp=self._get_cached_block_timestamp(self.update_block)
            )
        )
        try:
            return stableswap_get_y(
                i,
                j,
                x=x,
                xp=xp,
                amp=amp,
                n_coins=len(self._tokens),
                a_precision=self.A_PRECISION,
                y_variant=self._strategies.y_variant,
                d_variant=self._strategies.d_variant,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _get_y_d(self, a: int, i: int, xp: Sequence[int], d: int) -> int:
        """Calculate y given A, xp, and D.

        Delegates to the pure function stableswap_get_y_d.
        """
        try:
            return stableswap_get_y_d(
                a,
                i,
                xp=xp,
                d=d,
                n_coins=len(self._tokens),
                a_precision=self.A_PRECISION,
                yd_variant=self._strategies.yd_variant,
            )
        except ValueError as e:
            raise EVMRevertError(error=str(e)) from e

    def _resolve_rates(
        self,
        *,
        rates: tuple[int, ...],
        block_number: int,
        pool_balances: tuple[int, ...],
    ) -> tuple[int, ...]:
        """Select rates based on the pool's lending rate style.

        Returns rate_multipliers for NONE, or calls the data provider
        for lending pools.
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
        """
        try:
            return stableswap_newton_y(
                ann,
                gamma,
                xp=xp,
                d=d,
                token_index=token_index,
                n_coins=len(self._tokens),
                a_multiplier=self.A_PRECISION,
            )
        except ValueError as e:
            raise EVMRevertError(
                error=f"_newton_y() did not converge for pool {self.address}"
            ) from e

    @staticmethod
    def _reduction_coefficient(x: Sequence[int], fee_gamma: int, n_coins: int) -> int:
        """fee_gamma / (fee_gamma + (1 - K)) where K = prod(x) / (sum(x) / N)**N.

        Delegates to the pure function stableswap_reduction_coefficient.
        """
        return stableswap_reduction_coefficient(x, fee_gamma, n_coins)

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: CurveStableswapPoolState | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
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

            # TODO: see if any of these checks are unnecessary (partial zero balance OK?)
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
            message="Tokens not held by pool or in underlying base pool"
        )  # pragma: no cover

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
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

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return Fraction(self.fee, self.FEE_DENOMINATOR)

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: CurveStableswapPoolState | None = None,
    ) -> HopType:
        """Create a hop state for this pool.

        For 2-token pools, zero_for_one maps to token[0] -> token[1] direction.
        For metapools or base pools, swap direction is still determined by
        token index in the pool's tokens tuple.

        NOTE: swap_fn is not pickleable for ProcessPoolExecutor. For
        multiprocessing, use constant-product approximation or build
        hop states in the subprocess from pool IDs.
        """
        state = state_override or self.state
        balances = state.balances

        # For 2-token pools, zero_for_one maps to tokens[0] -> tokens[1]
        # For N-token pools, this is ambiguous; the caller should use
        # token_in/token_out kwargs when available
        if zero_for_one:
            i, j = 0, 1
        else:
            i, j = 1, 0

        # Validate indices exist
        if i >= len(balances) or j >= len(balances):
            raise DegenbotValueError(
                message=f"Invalid swap indices ({i}, {j}) for pool with {len(balances)} tokens"
            )

        # Create swap_fn closure wrapping get_dy
        # NOTE: This closure captures `self` and is not pickleable!
        def swap_fn(dx: int) -> int:
            return self.get_dy(i=i, j=j, dx=dx, override_state=state_override)

        return CurveStableswapHop(
            reserve_in=balances[i],
            reserve_out=balances[j],
            fee=Fraction(self.fee, self.FEE_DENOMINATOR),
            curve_a=self.a_coefficient,
            curve_n_coins=len(self._tokens),
            curve_d=0,  # D is computed dynamically in get_dy via _get_y
            token_index_in=i,
            token_index_out=j,
            precisions=self.precision_multipliers,
            swap_fn=swap_fn,
            invariant=PoolInvariant.CURVE_STABLESWAP,
        )
