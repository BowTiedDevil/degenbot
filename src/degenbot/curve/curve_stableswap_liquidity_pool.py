"""Curve StableSwap liquidity pool implementation.

Implements the Curve StableSwap invariant for V1-style pools including
plain pools, metapools, lending pools, and crypto pools.
"""

import contextlib
from collections.abc import Iterable, Sequence
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar, cast
from weakref import WeakSet

from eth_typing import ChecksumAddress
from web3.types import BlockIdentifier

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.types import (
    AdminBalancesFetcher,
    CurveStableswapPoolExternalUpdate,
    CurveStableswapPoolState,
    CurveStableSwapPoolStateUpdated,
    DFetcher,
    DVariant,
    GammaFetcher,
    LendingRateFetcher,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    PoolStrategies,
    PriceScaleFetcher,
    RedemptionPriceFetcher,
    SwapStyle,
    TimestampFetcher,
    VirtualPriceFetcher,
    YDVariant,
    YVariant,
)
from degenbot.curve.stableswap_pool_state import StableswapPoolState
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.arbitrage import NoLiquidity
from degenbot.exceptions.pool import MissingCurveData
from degenbot.exceptions.pool import EVMRevertError
from degenbot.exceptions.pool import InvalidSwapInputAmount
from degenbot.logging import logger
from degenbot.types.abstract import AbstractArbitrage, AbstractLiquidityPool
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

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
        # Fetchers (can't pickle closures)
        "_virtual_price_fetcher",
        "_base_virtual_price_fetcher",
        "_base_cache_updated_fetcher",
        "_timestamp_fetcher",
        "_redemption_price_fetcher",
        "_admin_balances_fetcher",
        "_block_number_fetcher",
        "_total_supply_fetcher",
        "_token_balance_fetcher",
        "_lending_rate_fetcher",
        "_D_fetcher",
        "_gamma_fetcher",
        "_price_scale_fetcher",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_state_lock": Lock,
        "_subscribers": WeakSet,
        # Fetchers default to None after unpickle
        "_virtual_price_fetcher": lambda: None,
        "_base_virtual_price_fetcher": lambda: None,
        "_base_cache_updated_fetcher": lambda: None,
        "_timestamp_fetcher": lambda: None,
        "_redemption_price_fetcher": lambda: None,
        "_admin_balances_fetcher": lambda: None,
        "_block_number_fetcher": lambda: None,
        "_total_supply_fetcher": lambda: None,
        "_token_balance_fetcher": lambda: None,
        "_lending_rate_fetcher": lambda: None,
        "_D_fetcher": lambda: None,
        "_gamma_fetcher": lambda: None,
        "_price_scale_fetcher": lambda: None,
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
        # Optional fetchers for on-demand data
        virtual_price_fetcher: VirtualPriceFetcher | None = None,
        base_virtual_price_fetcher: VirtualPriceFetcher | None = None,
        base_cache_updated_fetcher: VirtualPriceFetcher | None = None,
        timestamp_fetcher: TimestampFetcher | None = None,
        redemption_price_fetcher: RedemptionPriceFetcher | None = None,
        admin_balances_fetcher: AdminBalancesFetcher | None = None,
        block_number_fetcher: Any | None = None,
        total_supply_fetcher: Any | None = None,
        token_balance_fetcher: Any | None = None,
        # Crypto pool fetchers
        D_fetcher: DFetcher | None = None,
        gamma_fetcher: GammaFetcher | None = None,
        price_scale_fetcher: PriceScaleFetcher | None = None,
        # Lending rate fetcher (replaces provider_call + _stored_rates_from_* methods)
        lending_rate_fetcher: LendingRateFetcher | None = None,
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
        strategies: PoolStrategies = PoolStrategies(),
    ) -> None:
        """
        A Curve V1 (StableSwap) pool.

        Constructed from pre-fetched data only. Use Bot.build_pool() to fetch from chain.
        """

        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else tokens[0].chain_id
        state_block = state_block if state_block is not None else 0

        self._tokens: tuple[Erc20Token, ...] = tuple(tokens)
        self.a_coefficient = a_coefficient
        self.fee = fee
        self.admin_fee = admin_fee

        # Derive rate/precision multipliers from token decimals
        self.rate_multipliers = tuple(
            10 ** (2 * self.PRECISION_DECIMALS - token.decimals) for token in self._tokens
        )
        if precision_multipliers is not None:
            self.precision_multipliers = tuple(precision_multipliers)
            # Recompute rate_multipliers to be consistent
            self.rate_multipliers = tuple(
                pm * 10**self.PRECISION_DECIMALS for pm in self.precision_multipliers
            )
        else:
            self.precision_multipliers = tuple(
                cast("int", 10 ** (self.PRECISION_DECIMALS - token.decimals))
                for token in self._tokens
            )

        # Set defaults for optional/variant attributes
        self.fee_gamma = fee_gamma if fee_gamma is not None else 0
        self.mid_fee = mid_fee if mid_fee is not None else 0
        self.offpeg_fee_multiplier = (
            offpeg_fee_multiplier if offpeg_fee_multiplier is not None else 0
        )
        self.out_fee = out_fee if out_fee is not None else 0
        self.gamma = gamma if gamma is not None else 0

        # Variant computation strategies (resolved by builder from pool address)
        self._strategies = strategies

        # Pool configuration
        self.base_pool = base_pool
        self.tokens_underlying = tuple(tokens_underlying) if tokens_underlying else None
        self.lp_token = lp_token if lp_token is not None else self._tokens[0]
        self.use_lending = tuple(use_lending) if use_lending else tuple(False for _ in self._tokens)

        # A ramping configuration
        self.initial_a_coefficient = initial_a_coefficient
        self.initial_a_coefficient_time = initial_a_coefficient_time
        self.future_a_coefficient = future_a_coefficient
        self.future_a_coefficient_time = future_a_coefficient_time
        self._create_timestamp = create_timestamp

        # Fetchers for on-demand data
        self._virtual_price_fetcher = virtual_price_fetcher
        self._base_virtual_price_fetcher = base_virtual_price_fetcher
        self._base_cache_updated_fetcher = base_cache_updated_fetcher
        self._timestamp_fetcher = timestamp_fetcher
        self._redemption_price_fetcher = redemption_price_fetcher
        self._admin_balances_fetcher = admin_balances_fetcher
        self._block_number_fetcher = block_number_fetcher
        self._total_supply_fetcher = total_supply_fetcher
        self._token_balance_fetcher = token_balance_fetcher
        self._D_fetcher = D_fetcher
        self._gamma_fetcher = gamma_fetcher
        self._price_scale_fetcher = price_scale_fetcher
        self._lending_rate_fetcher = lending_rate_fetcher

        self.base_cache_updated: int | None = None
        self.base_virtual_price: int = 0
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

        self._block_timestamps: dict[BlockNumber, int] = {}
        self._cached_rates: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_contract_D: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cached_gamma: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )

        # Pre-populate base cache values for metapools at construction time,
        # matching the contract's _vp_rate_ro() cache behavior.
        if self.base_pool is not None and state_block != 0:
            with contextlib.suppress(Exception):
                self.base_cache_updated = self._get_base_cache_updated(block_number=state_block)
            with contextlib.suppress(Exception):
                self.base_virtual_price = self._get_base_virtual_price(block_number=state_block)

        fee_string = f"{100 * self.fee / self.FEE_DENOMINATOR:.2f}"
        token_string = "-".join([token.symbol for token in self._tokens])
        self.name = f"{token_string} ({self.__class__.__name__}, {fee_string}%)"

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        # I/O access for on-chain data fetching
        # I/O is done via fetcher callbacks injected by Bot.build_pool()

    def __repr__(self) -> str:  # pragma: no cover
        token_string = "-".join([token.symbol for token in self._tokens])
        return f"{self.__class__.__name__}(address={self.address}, tokens={token_string}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%, A={self.a_coefficient})"  # noqa:E501

    @property
    def balances(self) -> tuple[int, ...]:
        return self.state.balances

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def state(self) -> CurveStableswapPoolState:
        return self._state

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

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
        """Fetch token balance using the token_balance_fetcher if available."""
        if self._token_balance_fetcher is not None:
            return self._token_balance_fetcher(token, address, block_identifier=block_identifier)
        raise MissingCurveData(
            self.address,
            "token_balance",
            "Token balance fetch requires I/O. Provide a token_balance_fetcher callback.",
        )

    def _fetch_token_total_supply(
        self, token: Erc20Token, *, block_identifier: int | None = None
    ) -> int:
        """Fetch token total supply using the total_supply_fetcher if available."""
        if self._total_supply_fetcher is not None:
            return self._total_supply_fetcher(token, block_identifier=block_identifier)
        raise MissingCurveData(
            self.address,
            "token_total_supply",
            "Token total supply fetch requires I/O. Provide a total_supply_fetcher callback.",
        )

    def _resolve_block_number(self, block_identifier: BlockIdentifier | None) -> int:
        """Resolve a block identifier to an integer. Falls back to block_number_fetcher if available."""
        if isinstance(block_identifier, int):
            return block_identifier
        if self._block_number_fetcher is not None:
            return self._block_number_fetcher()
        raise MissingCurveData(
            self.address,
            "block_identifier",
            "block_identifier must be an integer when no provider is available. "
            "Use Bot.update() or pass an explicit block number.",
        )

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

        if self._create_timestamp >= self.future_a_coefficient_time:
            return self.future_a_coefficient

        if timestamp is None:
            if self._timestamp_fetcher is not None:
                timestamp = self._timestamp_fetcher(0)
            else:
                raise MissingCurveData(
                    self.address,
                    "timestamp",
                    "Timestamp is required for A ramping calculation but no timestamp_fetcher is available",
                )

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

        # Fetch and cache block timestamp for A ramping calculations
        if block_number not in self._block_timestamps:
            if self._timestamp_fetcher is None:
                raise MissingCurveData(
                    self.address,
                    "block_timestamp",
                    "Block timestamp requires a timestamp_fetcher callback. "
                    "Provide one via Bot.build_pool().",
                )
            self._block_timestamps[block_number] = self._timestamp_fetcher(block_number)

        xp = self._xp(rates=self.rate_multipliers, balances=pool_balances)
        amp = self._a(timestamp=self._block_timestamps[block_number])
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

        # Fetch and cache block timestamp for A ramping calculations
        if block_number not in self._block_timestamps:
            if self._timestamp_fetcher is None:
                raise MissingCurveData(
                    self.address,
                    "block_timestamp",
                    "Block timestamp requires a timestamp_fetcher callback. "
                    "Provide one via Bot.build_pool().",
                )
            self._block_timestamps[block_number] = self._timestamp_fetcher(block_number)

        n_coins = len(self._tokens)
        amp = self._a(timestamp=self._block_timestamps[block_number])
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

    def _get_scaled_redemption_price(self, block_number: BlockNumber) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_scaled_redemption_price[block_number]

        if self._redemption_price_fetcher is not None:
            result = self._redemption_price_fetcher(block_number)
            self._cached_scaled_redemption_price[block_number] = result
            return result

        raise MissingCurveData(
            self.address,
            "redemption_price",
            "Redemption price requires a redemption_price_fetcher callback. "
            "Provide one via Bot.build_pool().",
        )

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

        def _dynamic_fee(xpi: int, xpj: int, _fee: int, _feemul: int) -> int:
            if _feemul <= self.FEE_DENOMINATOR:
                return _fee
            xps2 = (xpi + xpj) ** 2
            return (_feemul * _fee) // (
                (_feemul - self.FEE_DENOMINATOR) * 4 * xpi * xpj // xps2 + self.FEE_DENOMINATOR
            )

        pool_balances = override_state.balances if override_state is not None else self.balances
        rates = self.rate_multipliers

        block_number = self._resolve_block_number(block_identifier)

        # Fetch and cache block timestamp for A ramping calculations
        if block_number not in self._block_timestamps:
            if self._timestamp_fetcher is None:
                raise MissingCurveData(
                    self.address,
                    "block_timestamp",
                    "Block timestamp requires a timestamp_fetcher callback. "
                    "Provide one via Bot.build_pool().",
                )
            self._block_timestamps[block_number] = self._timestamp_fetcher(block_number)

        if self.base_pool is not None:
            match self._strategies.metapool_rate_style:
                case MetapoolRateStyle.PRECISION_VP:
                    rates = (
                        self.PRECISION,
                        self._get_virtual_price(block_number=block_number),
                    )
                case MetapoolRateStyle.REDEMPTION_VP:
                    rates = (
                        self._get_scaled_redemption_price(block_number=block_number),
                        self._get_virtual_price(block_number=block_number),
                    )
                case MetapoolRateStyle.STANDARD:
                    rates = (
                        self.rate_multipliers[0],
                        self._get_virtual_price(block_number=block_number),
                    )

            xp = self._xp(rates=rates, balances=pool_balances)
            x = xp[i] + (dx * rates[i] // self.PRECISION)
            y = self._get_y(i, j, x, xp)
            dy = xp[j] - y - 1
            fee = self.fee * dy // self.FEE_DENOMINATOR
            return (dy - fee) * self.PRECISION // rates[j]

        match self._strategies.swap_style:
            case SwapStyle.LIVE_ADMIN:
                live_balances = [
                    self._fetch_token_balance(token, self.address, block_identifier=block_number)
                    for token in self._tokens
                ]
                admin_balances = self._get_admin_balances(block_number=block_number)

                balances = [
                    pool_balance - admin_balance
                    for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
                ]

                xp = self._xp(rates=rates, balances=balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = xp[j] - y - 1
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return (dy - fee) * self.PRECISION // rates[j]

            case SwapStyle.CRYPTO:
                # Crypto pool path — uses D(), gamma(), price_scale() from chain
                if self._D_fetcher is None:
                    raise MissingCurveData(
                        self.address,
                        "D_fetcher",
                        "Crypto pool requires a D_fetcher callback.",
                    )
                if self._gamma_fetcher is None:
                    raise MissingCurveData(
                        self.address,
                        "gamma_fetcher",
                        "Crypto pool requires a gamma_fetcher callback.",
                    )
                if self._price_scale_fetcher is None:
                    raise MissingCurveData(
                        self.address,
                        "price_scale_fetcher",
                        "Crypto pool requires a price_scale_fetcher callback.",
                    )

                # Fetch cached or on-chain D
                try:
                    d = self._cached_contract_D[block_number]
                except KeyError:
                    d = self._D_fetcher(block_number)
                    self._cached_contract_D[block_number] = d

                # Fetch cached or on-chain gamma
                try:
                    gamma_val = self._cached_gamma[block_number]
                except KeyError:
                    gamma_val = self._gamma_fetcher(block_number)
                    self._cached_gamma[block_number] = gamma_val

                # Fetch cached or on-chain price_scale
                try:
                    price_scale = self._cached_price_scale[block_number]
                except KeyError:
                    price_scale = self._price_scale_fetcher(block_number)
                    self._cached_price_scale[block_number] = price_scale

                n_coins = len(self._tokens)

                assert i != j, "coin index out of range"
                assert i < n_coins, "coin index out of range"
                assert j < n_coins, "coin index out of range"
                assert dx > 0, "do not exchange 0 coins"

                # Tricrypto precisions (hard-coded in the contract)
                precisions = [
                    10**12,  # USDT
                    10**10,  # WBTC
                    1,  # WETH
                ]

                xp_ = list(pool_balances)
                xp_[i] += dx
                xp_[0] *= precisions[0]

                for k in range(n_coins - 1):
                    xp_[k + 1] = xp_[k + 1] * price_scale[k] * precisions[k + 1] // self.PRECISION

                amp = self._a(timestamp=self._block_timestamps[block_number])
                y = self._newton_y(amp, gamma_val, xp_, d, j)
                dy = xp_[j] - y - 1

                xp_[j] = y
                if j > 0:
                    dy = dy * self.PRECISION // price_scale[j - 1]
                dy //= precisions[j]

                f = self._reduction_coefficient(xp_, self.fee_gamma, n_coins)
                fee_calc = (self.mid_fee * f + self.out_fee * (10**18 - f)) // 10**18

                dy -= fee_calc * dy // 10**10
                return dy

            case SwapStyle.RATE_ADJUSTED:
                # Rate-adjusted path: dy converted to target units before fee
                # Used by 3pool, Compound, PAX, etc.
                rates = self._resolve_rates(
                    rates=rates,
                    block_number=block_number,
                    pool_balances=pool_balances,
                )
                xp = self._xp(rates=rates, balances=pool_balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = (xp[j] - y - 1) * self.PRECISION // rates[j]
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return dy - fee

            case SwapStyle.NO_ONE_FEE_RATE:
                # AETH/RETH path: dy = xp[j] - y (no -1), then fee, then rate convert
                rates = self._resolve_rates(
                    rates=rates,
                    block_number=block_number,
                    pool_balances=pool_balances,
                )
                xp = self._xp(rates=rates, balances=pool_balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = xp[j] - y
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return (dy - fee) * self.PRECISION // rates[j]

            case SwapStyle.CYTOKEN:
                # CYTOKEN path: dy = xp[j] - y - 1, then fee inside rate conversion
                rates = self._resolve_rates(
                    rates=rates,
                    block_number=block_number,
                    pool_balances=pool_balances,
                )
                xp = self._xp(rates=rates, balances=pool_balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = xp[j] - y - 1
                return (dy - (self.fee * dy // self.FEE_DENOMINATOR)) * self.PRECISION // rates[j]

            case SwapStyle.RATE_ADJUSTED_NO_ONE:
                # YTOKEN variant: dy = (xp[j] - y) * PRECISION // rates[j], fee on converted dy
                # Same as RATE_ADJUSTED but without the -1 subtraction
                rates = self._resolve_rates(
                    rates=rates,
                    block_number=block_number,
                    pool_balances=pool_balances,
                )
                xp = self._xp(rates=rates, balances=pool_balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = (xp[j] - y) * self.PRECISION // rates[j]
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return dy - fee

            case SwapStyle.STANDARD:
                # Standard path: select rates based on lending rate style
                rates = self._resolve_rates(
                    rates=rates,
                    block_number=block_number,
                    pool_balances=pool_balances,
                )
                xp = self._xp(rates=rates, balances=pool_balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = xp[j] - y - 1
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return (dy - fee) * self.PRECISION // rates[j]

            case SwapStyle.RAW_BALANCE:
                # Raw balance path: no rate conversion, fee applied directly
                xp = tuple(pool_balances)
                x = xp[i] + dx
                y = self._get_y(i, j, x, xp)
                dy = xp[j] - y - 1
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return dy - fee

            case SwapStyle.LIVE_ADMIN_ORACLE:
                live_balances = [
                    self._fetch_token_balance(token, self.address, block_identifier=block_number)
                    for token in self._tokens
                ]
                admin_balances = self._get_admin_balances(block_number=block_number)
                balances = [
                    pool_balance - admin_balance
                    for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
                ]
                rates = self._resolve_rates(
                    rates=rates,
                    block_number=block_number,
                    pool_balances=pool_balances,
                )
                xp = self._xp(rates=rates, balances=balances)
                x = xp[i] + (dx * rates[i] // self.PRECISION)
                y = self._get_y(i, j, x, xp)
                dy = xp[j] - y - 1
                fee = self.fee * dy // self.FEE_DENOMINATOR
                return (dy - fee) * self.PRECISION // rates[j]

            case SwapStyle.LIVE_ADMIN_DYNAMIC:
                live_balances = [
                    self._fetch_token_balance(token, self.address, block_identifier=block_number)
                    for token in self._tokens
                ]
                admin_balances = self._get_admin_balances(block_number=block_number)

                xp_ = [
                    pool_balance - admin_balance
                    for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
                ]
                x = xp_[i] + dx
                y = self._get_y(i, j, x, xp_)
                dy = xp_[j] - y
                fee_ = (
                    _dynamic_fee(
                        xpi=(xp_[i] + x) // 2,
                        xpj=(xp_[j] + y) // 2,
                        _fee=self.fee,
                        _feemul=self.offpeg_fee_multiplier,
                    )
                    * dy
                    // self.FEE_DENOMINATOR
                )
                return dy - fee_

            case SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION:
                live_balances = [
                    self._fetch_token_balance(token, self.address, block_identifier=block_number)
                    for token in self._tokens
                ]
                admin_balances = self._get_admin_balances(block_number=block_number)
                balances = [
                    pool_balance - admin_balance
                    for pool_balance, admin_balance in zip(live_balances, admin_balances, strict=True)
                ]

                xp_ = [
                    balance * rate
                    for balance, rate in zip(balances, self.precision_multipliers, strict=True)
                ]

                x = xp_[i] + dx * self.precision_multipliers[i]
                y = self._get_y(i, j, x, xp_)
                dy = (xp_[j] - y) // self.precision_multipliers[j]

                fee_ = (
                    _dynamic_fee(
                        xpi=(xp_[i] + x) // 2,
                        xpj=(xp_[j] + y) // 2,
                        _fee=self.fee,
                        _feemul=self.offpeg_fee_multiplier,
                    )
                    * dy
                    // self.FEE_DENOMINATOR
                )
                return dy - fee_

    def _get_dy_underlying(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        if TYPE_CHECKING:
            assert self.base_pool is not None

        pool_balances = override_state.balances if override_state is not None else self.balances

        block_number = self._resolve_block_number(block_identifier)

        if self._strategies.metapool_underlying_style == MetapoolUnderlyingStyle.REDEMPTION:
            base_n_coins = len(self.base_pool.tokens)
            max_coin = len(self._tokens) - 1
            redemption_coin = 0

            # dx and dy in underlying units
            rates = (
                self._get_scaled_redemption_price(block_number=block_number),
                vp_rate := self._get_virtual_price(block_number=block_number),
            )
            xp = self._xp(rates=rates, balances=pool_balances)

            # Use base_i or base_j if they are >= 0
            base_i = i - max_coin
            base_j = j - max_coin
            meta_i = max_coin
            meta_j = max_coin
            if base_i < 0:
                meta_i = i
            if base_j < 0:
                meta_j = j

            if base_i < 0:
                x = xp[i] + (
                    dx
                    * self._get_scaled_redemption_price(block_number=block_number)
                    // self.PRECISION
                )
            elif base_j < 0:
                # i is from BasePool
                # At first, get the amount of pool tokens
                base_inputs = [0] * base_n_coins
                base_inputs[base_i] = dx
                # Token amount transformed to underlying "dollars"
                x = (
                    self.base_pool.calc_token_amount(
                        amounts=base_inputs,
                        deposit=True,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
                    )
                    * vp_rate
                    // self.PRECISION
                )
                # Accounting for deposit/withdraw fees approximately
                x -= x * self.base_pool.fee // (2 * self.FEE_DENOMINATOR)
                # Adding number of pool tokens
                x += xp[max_coin]
            else:
                # If both are from the base pool
                return self.base_pool.get_dy(
                    i=base_i,
                    j=base_j,
                    dx=dx,
                    override_state=(override_state.base if override_state is not None else None),
                )

            # This pool is involved only when in-pool assets are used
            y = self._get_y(meta_i, meta_j, x, xp)
            dy = xp[meta_j] - y - 1
            dy -= self.fee * dy // self.FEE_DENOMINATOR
            if j == redemption_coin:
                dy = (dy * self.PRECISION) // self._get_scaled_redemption_price(
                    block_number=block_number
                )

            # If output is going via the metapool
            if base_j >= 0:
                # j is from BasePool
                # The fee is already accounted for
                dy, *_ = self.base_pool.calc_withdraw_one_coin(
                    _token_amount=dy * self.PRECISION // vp_rate,
                    i=base_j,
                    block_identifier=block_number,
                )

            return dy

        if self._strategies.metapool_underlying_style == MetapoolUnderlyingStyle.PRECISION_VP:
            base_n_coins = len(self.base_pool.tokens)
            max_coin = len(self._tokens) - 1

            rates = (self.PRECISION, self._get_virtual_price(block_number=block_number))
            xp = self._xp(rates=rates, balances=pool_balances)

            base_i = 0
            base_j = 0
            meta_i = 0
            meta_j = 0

            if i != 0:
                base_i = i - max_coin
                meta_i = 1
            if j != 0:
                base_j = j - max_coin
                meta_j = 1

            if i == 0:
                x = xp[i] + dx * (rates[0] // 10**18)
            elif j == 0:
                # i is from BasePool
                # At first, get the amount of pool tokens
                base_inputs = [0] * base_n_coins
                base_inputs[base_i] = dx
                # Token amount transformed to underlying "dollars"
                x = (
                    self.base_pool.calc_token_amount(
                        amounts=base_inputs,
                        deposit=True,
                        block_identifier=block_number,
                        override_state=(
                            override_state.base if override_state is not None else None
                        ),
                    )
                    * rates[1]
                    // self.PRECISION
                )
                # Accounting for deposit/withdraw fees approximately
                x -= x * self.base_pool.fee // (2 * self.FEE_DENOMINATOR)
                # Adding number of pool tokens
                x += xp[max_coin]
            else:
                # If both are from the base pool
                return self.base_pool.get_dy(
                    i=base_i,
                    j=base_j,
                    dx=dx,
                    block_identifier=block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )

            # This pool is involved only when in-pool assets are used
            y = self._get_y(meta_i, meta_j, x, xp)
            dy = xp[meta_j] - y - 1
            dy -= self.fee * dy // self.FEE_DENOMINATOR

            # If output is going via the metapool
            if j == 0:
                dy //= rates[0] // 10**18
            else:
                # j is from BasePool
                # The fee is already accounted for
                dy, *_ = self.base_pool.calc_withdraw_one_coin(
                    _token_amount=dy * self.PRECISION // rates[1],
                    i=base_j,
                    block_identifier=block_number,
                )

            return dy

        elif self._strategies.metapool_underlying_style == MetapoolUnderlyingStyle.STANDARD:
            pass

        working_rates = list(self.rate_multipliers)

        vp_rate = self._get_virtual_price(block_number=block_number)
        working_rates[-1] = vp_rate

        xp = self._xp(rates=tuple(working_rates), balances=pool_balances)
        precisions = self.precision_multipliers

        base_n_coins = len(self.base_pool.tokens)
        max_coin = len(self._tokens) - 1

        # Use base_i or base_j if they are >= 0
        base_i = i - max_coin
        base_j = j - max_coin
        meta_i = max_coin
        meta_j = max_coin
        if base_i < 0:
            meta_i = i
        if base_j < 0:
            meta_j = j

        if base_i < 0:
            x = xp[i] + dx * precisions[i]
        elif base_j < 0:
            # i is from BasePool
            # At first, get the amount of pool tokens
            base_inputs = [0] * base_n_coins
            base_inputs[base_i] = dx
            # Token amount transformed to underlying "dollars"
            x = (
                self.base_pool.calc_token_amount(
                    amounts=base_inputs,
                    deposit=True,
                    block_identifier=block_number,
                    override_state=(override_state.base if override_state is not None else None),
                )
                * vp_rate
                // self.PRECISION
            )
            # Accounting for deposit/withdraw fees approximately
            x -= x * self.base_pool.fee // (2 * self.FEE_DENOMINATOR)
            # Adding number of pool tokens
            x += xp[max_coin]
        else:
            # If both are from the base pool
            return self.base_pool.get_dy(
                i=base_i,
                j=base_j,
                dx=dx,
                block_identifier=block_number,
                override_state=(override_state.base if override_state is not None else None),
            )

        # This pool is involved only when in-pool assets are used
        y = self._get_y(meta_i, meta_j, x, xp)
        dy = xp[meta_j] - y - 1
        dy -= self.fee * dy // self.FEE_DENOMINATOR

        # If output is going via the metapool
        if base_j < 0:
            dy //= precisions[meta_j]
        else:
            # j is from BasePool
            # The fee is already accounted for
            dy, *_ = self.base_pool.calc_withdraw_one_coin(
                _token_amount=dy * self.PRECISION // vp_rate,
                i=base_j,
                block_identifier=block_number,
            )

        return dy

    def _get_base_cache_updated(self, block_number: BlockNumber) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_base_cache_updated[block_number]

        if self._base_cache_updated_fetcher is not None:
            result = self._base_cache_updated_fetcher(block_number)
            self._cached_base_cache_updated[block_number] = result
            self.base_cache_updated = result
            return result

        raise MissingCurveData(
            self.address,
            "base_cache_updated",
            "base_cache_updated requires a base_cache_updated_fetcher callback "
            "via Bot.build_pool().",
        )

    def _get_base_virtual_price(self, block_number: BlockNumber) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_base_virtual_price[block_number]

        if self._base_virtual_price_fetcher is not None:
            result = self._base_virtual_price_fetcher(block_number)
            self._cached_base_virtual_price[block_number] = result
            return result

        raise MissingCurveData(
            self.address,
            "base_virtual_price",
            "Base virtual price requires a base_virtual_price_fetcher callback. "
            "Provide one via Bot.build_pool().",
        )

    def _get_virtual_price(self, block_number: BlockNumber) -> int:
        if TYPE_CHECKING:
            assert self.base_pool is not None

        with contextlib.suppress(KeyError):
            return self._cached_virtual_price[block_number]

        base_virtual_price: int
        if (
            self.base_cache_updated is None
            or self._block_timestamps.get(block_number, 0)
            > self.base_cache_updated + self.BASE_CACHE_EXPIRES
        ):
            # Cache is not set or has expired — fetch live virtual price from base pool
            if self._virtual_price_fetcher is not None:
                base_virtual_price = self._virtual_price_fetcher(block_number)
            else:
                raise MissingCurveData(
                    self.address,
                    "virtual_price",
                    "Virtual price requires a virtual_price_fetcher callback. "
                    "Provide one via Bot.build_pool().",
                )
        else:
            # Cache is still valid — use the cached base_virtual_price
            base_virtual_price = self.base_virtual_price

        self._cached_virtual_price[block_number] = base_virtual_price
        self.base_virtual_price = base_virtual_price
        return base_virtual_price

    def _get_admin_balances(self, block_number: BlockNumber) -> tuple[int, ...]:
        with contextlib.suppress(KeyError):
            return self._cached_admin_balances[block_number]

        if self._admin_balances_fetcher is not None:
            result = self._admin_balances_fetcher(block_number)
            self._cached_admin_balances[block_number] = result
            return result

        raise MissingCurveData(
            self.address,
            "admin_balances",
            "Admin balances require an admin_balances_fetcher callback. "
            "Provide one via Bot.build_pool().",
        )

    def _get_d(self, _xp: Sequence[int], _amp: int) -> int:
        """
        Solve for the Curve stableswap invariant D, using a modified Newton's method.

        Mainnet V1 Curve pools have several calculation variants to calculate the D and D_prev
        values. The pool addresses using each variant are grouped and the appropriate function is
        set at runtime.
        """

        def calc_d(
            *,
            a_nn: int,
            s: int,
            d: int,
            d_p: int,
            n_coins: int,
            a_precision: int,
        ) -> int:
            return (
                (a_nn * s // a_precision + d_p * n_coins)
                * d
                // ((a_nn - a_precision) * d // a_precision + (n_coins + 1) * d_p)
            )

        def calc_d_variant_alpha(
            *,
            a_nn: int,
            s: int,
            d: int,
            d_p: int,
            n_coins: int,
            a_precision: int,  # noqa:ARG001
        ) -> int:
            return (a_nn * s + d_p * n_coins) * d // ((a_nn - 1) * d + (n_coins + 1) * d_p)

        def calc_dp(
            *,
            d: int,
            d_p: int,
            xp: Sequence[int],
        ) -> int:
            for x in xp:
                d_p = d_p * d // (x * n_coins)
            return d_p

        def calc_dp_variant_alpha(
            *,
            d: int,
            d_p: int,
            xp: Sequence[int],
        ) -> int:
            for x in xp:
                d_p = d_p * d // (x * n_coins + 1)
            return d_p

        def calc_dp_variant_beta(
            *,
            d: int,
            d_p: int,  # noqa:ARG001
            xp: Sequence[int],
        ) -> int:
            return d * d // xp[0] * d // xp[1] // n_coins**2

        def calc_dp_variant_gamma(
            *,
            d: int,
            d_p: int,  # noqa:ARG001
            xp: Sequence[int],
        ) -> int:
            return d * d // xp[0] * d // xp[1] // cast("int", n_coins**n_coins)

        d_func = calc_d
        dp_func = calc_dp
        match self._strategies.d_variant:
            case DVariant.VARIANT_ALPHA:
                d_func = calc_d_variant_alpha
            case DVariant.VARIANT_ALPHA_DP_ALPHA:
                d_func = calc_d_variant_alpha
                dp_func = calc_dp_variant_alpha
            case DVariant.VARIANT_DP_ALPHA:
                dp_func = calc_dp_variant_alpha
            case DVariant.VARIANT_BETA_DP:
                dp_func = calc_dp_variant_beta
            case DVariant.VARIANT_GAMMA_DP:
                dp_func = calc_dp_variant_gamma
            case DVariant.STANDARD:
                pass

        d = s = sum(_xp)
        if s == 0:
            return 0
        n_coins = len(self._tokens)
        a_nn = _amp * n_coins

        for _ in range(255):  # pragma: no branch
            d_p = d_prev = d
            d_p = dp_func(d=d, d_p=d_p, xp=_xp)
            d = d_func(a_nn=a_nn, s=s, d=d, d_p=d_p, n_coins=n_coins, a_precision=self.A_PRECISION)
            if d_prev < d:
                if d - d_prev <= 1:
                    return d
            elif d_prev - d <= 1:
                return d

        raise EVMRevertError(error="D calculation did not converge.")  # pragma: no cover

    def _get_y(self, i: int, j: int, x: int, xp: Sequence[int]) -> int:
        """
        Calculate x[j] if one makes x[i] = x

        Done by solving quadratic equation iteratively.
        x_1**2 + x_1 * (sum' - (A*n**n - 1) * D / (A * n**n)) = D ** (n + 1) / (
            n ** (2 * n) * prod' * A
        )
        x_1**2 + b*x_1 = c

        x_1 = (x_1**2 + c) / (2*x_1 + b)
        """

        # x in the input is converted to the same price/precision

        n_coins = len(self._tokens)

        assert i != j, "same coin"
        assert j >= 0, "j below zero"
        assert j < n_coins, "j above N_COINS"

        # should be unreachable, but good for safety
        assert i >= 0
        assert i < n_coins

        amp = (
            self._a(timestamp=self._block_timestamps[self.update_block]) // self.A_PRECISION
            if self._strategies.y_variant == YVariant.VARIANT_0
            else self._a(timestamp=self._block_timestamps[self.update_block])
        )
        c = y = d = self._get_d(xp, amp)

        s = 0
        for coin_index in range(n_coins):
            if coin_index == i:
                x_ = x
            elif coin_index != j:
                x_ = xp[coin_index]
            else:
                continue
            s += x_
            c = c * d // (x_ * n_coins)

        a_nn = amp * n_coins
        if self._strategies.y_variant in (YVariant.VARIANT_0, YVariant.VARIANT_1):
            c = c * d // (a_nn * n_coins)
            b = s + d // a_nn
        else:
            c = c * d * self.A_PRECISION // (a_nn * n_coins)
            b = s + d * self.A_PRECISION // a_nn

        for _ in range(255):  # pragma: no branch
            y_prev = y
            y = (y * y + c) // (2 * y + b - d)
            if y > y_prev:
                if y - y_prev <= 1:
                    return y
            elif y_prev - y <= 1:
                return y

        raise EVMRevertError(error="y calculation did not converge.")  # pragma: no cover

    def _get_y_d(self, a: int, i: int, xp: Sequence[int], d: int) -> int:
        n_coins = len(self._tokens)

        assert i >= 0  # dev: i below zero
        assert i < n_coins  # dev: i above N_COINS

        c = y = d

        s = 0
        for coin_index in range(n_coins):
            if coin_index != i:
                x = xp[coin_index]
            else:
                continue
            s += x
            c = c * d // (x * n_coins)

        a_nn = a * n_coins
        if self._strategies.yd_variant == YDVariant.VARIANT_0:
            b = s + d * self.A_PRECISION // a_nn
            c = c * d * self.A_PRECISION // (a_nn * n_coins)
        else:
            b = s + d // a_nn
            c = c * d // (a_nn * n_coins)

        for _ in range(255):  # pragma: no branch
            y_prev = y
            y = (y * y + c) // (2 * y + b - d)
            if y > y_prev:
                if y - y_prev <= 1:
                    return y
            elif y_prev - y <= 1:
                return y

        raise EVMRevertError(error="y_d calculation did not converge.")  # pragma: no cover

    def _resolve_rates(
        self,
        *,
        rates: tuple[int, ...],
        block_number: int,
        pool_balances: tuple[int, ...],
    ) -> tuple[int, ...]:
        """Select rates based on the pool's lending rate style.

        Returns rate_multipliers for NONE, or calls the lending rate fetcher
        for lending pools.
        """
        if self._strategies.lending_rate_style == LendingRateStyle.NONE:
            return rates

        if self._lending_rate_fetcher is None:
            raise MissingCurveData(
                self.address,
                "lending_rate",
                "Lending rate fetcher is required for pools with lending tokens. "
                "Provide one via Bot.build_pool().",
            )
        return self._lending_rate_fetcher(block_number)

    def _xp(self, rates: Iterable[int], balances: Iterable[int]) -> tuple[int, ...]:
        return tuple(
            rate * balance // self.PRECISION for rate, balance in zip(rates, balances, strict=True)
        )

    def _newton_y(self, ann: int, gamma: int, xp: Sequence[int], d: int, token_index: int) -> int:
        """
        Calculating xp[i] given other balances xp[0..N_COINS-1] and invariant D.
        _ann = A * N**N

        Used by crypto (volatile) Curve pools.
        """
        n_coins = len(self._tokens)
        a_multiplier = self.A_PRECISION

        # Safety checks
        assert (
            n_coins**n_coins * a_multiplier - 1 < ann < 10000 * n_coins**n_coins * a_multiplier + 1
        ), "unsafe value for A"
        assert 10**10 - 1 < gamma < 10**16 + 1, "unsafe values for gamma"
        assert 10**17 - 1 < d < 10**15 * 10**18 + 1, "unsafe values for D"

        for index in range(n_coins):
            if index != token_index:
                frac = xp[index] * 10**18 // d
                assert 10**16 - 1 < frac < 10**20 + 1, (  # dev: unsafe values x[i]
                    f"{frac=} out of range"
                )

        y = d // n_coins
        k_0_i = 10**18
        s_i = 0

        x_sorted = list(xp)
        x_sorted[token_index] = 0
        x_sorted = sorted(x_sorted, reverse=True)  # From high to low

        convergence_limit = max(x_sorted[0] // 10**14, d // 10**14, 100)
        for j_ in range(2, n_coins + 1):
            x_ = x_sorted[n_coins - j_]
            y = y * d // (x_ * n_coins)  # Small _x first
            s_i += x_

        for k_ in range(n_coins - 1):
            k_0_i = k_0_i * x_sorted[k_] * n_coins // d  # Large _x first

        for _ in range(255):  # pragma: no branch
            y_prev = y

            k_0 = k_0_i * y * n_coins // d
            s = s_i + y

            g1k0 = gamma + 10**18
            g1k0 = g1k0 - k_0 + 1 if g1k0 > k_0 else k_0 - g1k0 + 1

            mul1 = 10**18 * d // gamma * g1k0 // gamma * g1k0 * a_multiplier // ann
            mul2 = 10**18 + (2 * 10**18) * k_0 // g1k0

            yfprime = 10**18 * y + s * mul2 + mul1
            dyfprime = d * mul2

            if yfprime < dyfprime:
                y = y_prev // 2
                continue

            yfprime -= dyfprime
            fprime = yfprime // y

            y_minus = mul1 // fprime
            y_plus = (yfprime + 10**18 * d) // fprime + y_minus * 10**18 // k_0
            y_minus += 10**18 * s // fprime

            y = y_prev // 2 if y_plus < y_minus else y_plus - y_minus
            diff = y - y_prev if y > y_prev else y_prev - y

            if diff < max(convergence_limit, y // 10**14):
                frac = y * 10**18 // d
                assert 10**16 - 1 < frac < 10**20 + 1, "unsafe value for y"
                return y

        raise EVMRevertError(
            error=f"_newton_y() did not converge for pool {self.address}"
        )  # pragma: no cover

    @staticmethod
    def _reduction_coefficient(x: Sequence[int], fee_gamma: int, n_coins: int) -> int:
        """
        fee_gamma / (fee_gamma + (1 - K))
        where
        K = prod(x) / (sum(x) / N)**N
        (all normalized to 1e18)

        Used by crypto (volatile) Curve pools for dynamic fee calculation.
        """
        k = 10**18
        s = 0
        for x_i in x:
            s += x_i
        for x_i in x:
            k = k * n_coins * x_i // s
        if fee_gamma > 0:
            k = fee_gamma * 10**18 // (fee_gamma + 10**18 - k)
        return k

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

    def get_arbitrage_helpers(self) -> tuple[AbstractArbitrage, ...]:
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: CurveStableswapPoolState | None = None,
    ) -> SimulationResult:
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

        initial_state = state_override or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,  # type: ignore[arg-type]
            token_out=token_out_obj,  # type: ignore[arg-type]
            token_in_quantity=amount_in,
            override_state=state_override,
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
