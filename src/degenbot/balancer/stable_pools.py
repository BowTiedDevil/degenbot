"""Balancer V2 stable pool implementations (MetaStable, ComposableStable)."""
from __future__ import annotations

from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar, Protocol, runtime_checkable
from weakref import WeakSet

from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.stable_math import (
    _calc_in_given_out,
    _calc_out_given_in,
    _calculate_invariant,
    _calculate_invariant_deployed,
)
from degenbot.balancer.swap_amounts import BalancerV2SwapAmounts
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import StaleRateResult
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import BalancerStableHop, HopType, PoolInvariant
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult

from .types import (
    BalancerV2PoolState,
    BalancerV2PoolStateUpdated,
    BalancerV2StablePoolExternalUpdate,
)

_MAX_TWO_TOKEN_COUNT = 2

if TYPE_CHECKING:
    from collections.abc import Sequence
    from fractions import Fraction

    from eth_typing import ChecksumAddress

    from degenbot.erc20 import Erc20Token
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.types.aliases import BlockNumber, ChainId

# Enum for deployed StableMath invariant versions.
# V1: always-roundDown with D_P accumulation (older ComposableStablePools)
# V2: roundUp parameter with P_D accumulation (MetaStablePools, newer pools)
INVARIANT_V1 = 1
INVARIANT_V2 = 2


@runtime_checkable
class BalancerRateProvider(Protocol):
    """Fetches per-block scaling factor rates from on-chain rate providers.

    ComposableStablePools override ``_beforeSwapJoinExit()`` to refresh rate
    caches before reading ``_scalingFactors()``. To match this on-chain behavior,
    off-chain calculations must fetch fresh rates at the block the calculation
    targets.

    Implementations should call ``getRate()`` on each token's rate provider
    contract, using ``eth_call`` with the provided block identifier so the
    result matches on-chain state at that block.
    """

    def get_rates(self, block_identifier: int | str | None = None) -> tuple[int, ...]:
        """Return the current rate for each pool token.

        The returned tuple has one entry per pool token (including BPT if
        present). Tokens without a rate provider should return ONE (1e18).
        """
        ...


class _StaticRateProvider:
    """A rate provider that always returns construction-time rates.

    Used when no I/O-capable provider is injected.
    """

    def __init__(self, rates: tuple[int, ...]) -> None:
        self._rates = rates

    def get_rates(self, block_identifier: int | str | None = None) -> tuple[int, ...]:  # noqa: ARG002
        return self._rates


class BalancerV2StablePool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
    """Balancer V2 Stable Pool (MetaStablePool or ComposableStablePool).

    Supports token-to-token swaps using StableMath. For ComposableStablePools,
    the BPT token is automatically dropped from the invariant and swap calculations.

    Swap fee application order:
      GIVEN_IN:  subtractFee → upscale → compute(outGivenIn) → downscaleDown
      GIVEN_OUT: upscale → compute(inGivenOut) → downscaleUp → addFee

    Invariant versions:
      V1 (INVARIANT_V1): always-roundDown, D_P accumulation. Used by older
        deployed ComposableStablePools (e.g. TUSD BSP, bb-s-USD). Matches the
        monorepo ``_calculate_invariant``.
      V2 (INVARIANT_V2): roundUp parameter, P_D accumulation. Used by
        MetaStablePools and newer deployed pools. The swap path calls with
        round_up=True. Matches ``_calculate_invariant_deployed``.

    Rate handling:
      ComposableStablePools have time-varying rates (yield accrual in
      bb-a-* tokens). The deployed contract refreshes rate caches before
      each swap via ``_beforeSwapJoinExit()``. For exact-integer matching,
      inject a ``BalancerRateProvider`` that replicates this cache-aware
      logic (read ``getTokenRateCache``, check expiry, call ``getRate()`` if
      expired). Without a live rate provider, the pool uses construction-time
      scaling factors and raises ``StaleRateResult`` for
      ComposableStablePools to warn that rates may be stale.

      MetaStablePools have no rate cache — they call ``getRate()`` directly.
    """

    variant: ClassVar[str | None] = "balancer_stable"

    type PoolState = BalancerV2PoolState
    FEE_DENOMINATOR = 1 * 10**18

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
        "_rate_provider",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_state_lock": Lock,
        "_subscribers": WeakSet,
        "_rate_provider": lambda: None,
    }

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        pool_id: bytes,
        vault: str,
        tokens: Sequence[Erc20Token],
        balances: Sequence[int],
        fee: Fraction,
        amp: int,
        scaling_factors: Sequence[int],
        bpt_idx: int | None = None,
        base_scaling_factors: Sequence[int] | None = None,
        rate_provider: BalancerRateProvider | None = None,
        invariant_version: int = INVARIANT_V2,
        chain_id: ChainId | None = None,
        state_block: BlockNumber | None = None,
    ) -> None:
        """Initialize the instance."""
        self.address = get_checksum_address(address)

        self._chain_id = chain_id if chain_id not in {None, 0} else tokens[0].chain_id
        state_block = state_block if state_block is not None else 0

        self.pool_id = pool_id
        self.pool_specialization = int.from_bytes(self.pool_id[20:22], byteorder="big")
        self.vault = get_checksum_address(vault)
        self._tokens = tuple(tokens)
        self.fee = fee
        self.amp = amp
        self.bpt_idx = bpt_idx
        self.invariant_version = invariant_version

        # Base scaling factors: 10^(18 - token_decimals) for each token.
        # These are the decimal-adjustment-only scaling factors (no rate).
        # Used alongside rate_provider rates to compute full scaling factors
        # as base_sf * rate // ONE (mulDown, matching deployed contract).
        if base_scaling_factors is not None:
            self._base_scaling_factors = tuple(base_scaling_factors)
        else:
            # Fallback: if base_scaling_factors not provided, derive from
            # scaling_factors assuming rate=ONE (i.e. scaling_factor IS base_sf)
            self._base_scaling_factors = tuple(scaling_factors)

        # Construction-time scaling factors (base_sf * rate_at_construction // ONE).
        # Used as fallback when no rate_provider is available.
        self.scaling_factors = tuple(scaling_factors)

        # Rate provider for per-block rate resolution.
        # If not provided, a static provider wraps the construction-time rates.
        if rate_provider is not None:
            self._rate_provider = rate_provider
        else:
            # Extract construction-time rates from scaling_factors.
            # rate = sf * ONE // base_sf for each token.
            construction_rates = tuple(
                sf * ONE // bsf
                for sf, bsf in zip(scaling_factors, self._base_scaling_factors, strict=True)
            )
            self._rate_provider = _StaticRateProvider(construction_rates)

        # Precompute non-BPT index mapping for ComposableStablePool
        if bpt_idx is not None:
            self._non_bpt_indices = tuple(i for i in range(len(tokens)) if i != bpt_idx)
        else:
            self._non_bpt_indices = tuple(range(len(tokens)))

        self._state_lock = Lock()
        self._state = BalancerV2PoolState(
            address=self.address,
            block=state_block,
            balances=tuple(balances),
        )
        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        pool_type = "ComposableStablePool" if self.bpt_idx is not None else "MetaStablePool"
        return (
            f"{self.__class__.__name__}(address={self.address}, "
            f"type={pool_type}, tokens={len(self._tokens)})"
        )

    def __str__(self) -> str:  # pragma: no cover
        """Return a human-readable string representation.

        Returns:
            A string representation of the object.

        """
        pool_type = "ComposableStablePool" if self.bpt_idx is not None else "MetaStablePool"
        return f"{self.__class__.__name__} {pool_type} @ {self.address}"

    @property
    def balances(self) -> tuple[int, ...]:
        """Balances."""
        return self.state.balances

    @property
    def chain_id(self) -> int | None:
        """Return chain id."""
        return self._chain_id

    @property
    def state(self) -> PoolState:
        """State."""
        return self._state

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        return self._tokens

    @property
    def rate_provider(self) -> BalancerRateProvider | None:
        """The rate provider for per-block rate resolution, if available."""
        prov = self._rate_provider
        if isinstance(prov, _StaticRateProvider):
            return None
        return prov

    @property
    def requires_io_at_calculation_time(self) -> bool:
        """Whether this pool may call its rate_provider during swap calculations.

        Returns True for ComposableStablePools with a live rate provider
        (time-varying rates from yield-bearing tokens). Returns False for
        MetaStablePools and for pools with only a static rate provider.
        """
        # ComposableStablePools always need fresh rates for exact matching
        return self.bpt_idx is not None and not isinstance(self._rate_provider, _StaticRateProvider)

    @staticmethod
    def _upscale(amount: int, scaling_factor: int) -> int:
        """Upscale a token amount using the scaling factor (mulDown).

        Returns:
            The computed integer value.

        """
        return amount * scaling_factor // ONE

    @staticmethod
    def _downscale_down(amount: int, scaling_factor: int) -> int:
        """Downscale a token amount, rounding down (divDown).

        Returns:
            The computed integer value.

        """
        return amount * ONE // scaling_factor

    @staticmethod
    def _downscale_up(amount: int, scaling_factor: int) -> int:
        """Downscale a token amount, rounding up (divUp).

        Returns:
            The computed integer value.

        """
        return (amount * ONE + scaling_factor - 1) // scaling_factor

    def _subtract_swap_fee_amount(self, amount: int) -> int:
        """Subtract swap fee from amount (mulUp for fee, matches deployed contract).

        Returns:
            The computed integer value.

        """
        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)
        fee_amount = (amount * fee_scaled + ONE - 1) // ONE  # mulUp
        return amount - fee_amount

    def _add_swap_fee_amount(self, amount: int) -> int:
        """Add swap fee to amount (divUp, matches deployed contract).

        Returns:
            The computed integer value.

        """
        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)
        numerator = amount * ONE
        denominator = ONE - fee_scaled
        return numerator // denominator + (1 if numerator % denominator > 0 else 0)

    def _resolve_scaling_factors(
        self, block_identifier: int | str | None = None
    ) -> tuple[int, ...]:
        """Resolve scaling factors at the given block.

        If a live rate provider is available, fetches rates for the block
        and computes scaling factors as base_sf * rate // ONE (mulDown).
        Otherwise, falls back to construction-time scaling factors.

        Returns:
            The computed value.

        """
        if isinstance(self._rate_provider, _StaticRateProvider):
            return self.scaling_factors

        rates = self._rate_provider.get_rates(block_identifier)
        return tuple(
            bsf * rate // ONE for bsf, rate in zip(self._base_scaling_factors, rates, strict=True)
        )

    @staticmethod
    def _upscale_balances(balances: Sequence[int], scaling_factors: Sequence[int]) -> list[int]:
        """Upscale all balances using the given scaling factors.

        Returns:
            A list of results.

        """
        return [b * sf // ONE for b, sf in zip(balances, scaling_factors, strict=False)]

    def _compute_invariant(self, upscaled_balances: list[int]) -> int:
        """Compute invariant using the pool's deployed StableMath version.

        V1 (INVARIANT_V1): always-roundDown, D_P accumulation.
        V2 (INVARIANT_V2): round_up=True for swaps, P_D accumulation.

        Returns:
            The computed integer value.

        """
        # For ComposableStablePool, drop BPT before computing invariant
        if self.bpt_idx is not None:
            balances_for_inv = [upscaled_balances[i] for i in self._non_bpt_indices]
        else:
            balances_for_inv = upscaled_balances

        if self.invariant_version == INVARIANT_V1:
            return _calculate_invariant(self.amp, balances_for_inv)
        return _calculate_invariant_deployed(self.amp, balances_for_inv, round_up=True)

    def _skip_bpt_index(self, index: int) -> int:
        """Map a full token list index to the non-BPT index.

        Matches Solidity's _skipBptIndex: returns index if index < bpt_idx,
        otherwise index - 1.

        Returns:
            The computed integer value.

        """
        if self.bpt_idx is None:
            return index
        return index if index < self.bpt_idx else index - 1

    def _should_warn_stale_rates(self) -> bool:
        """Whether a StaleRateResult should wrap the computed result.

        ComposableStablePools have time-varying rates (bb-a-* tokens accrue
        yield). Without a live rate provider, the construction-time rates
        become stale as blocks pass. MetaStablePools have near-static rates
        (wstETH/wETH conversion), so stale rates are not a concern.

        Returns:
            The computed boolean value.

        """
        if self.bpt_idx is None:
            # MetaStablePool: rates are near-static, no warning needed
            return False
        # ComposableStablePool: warn if using static (stale) rates
        return isinstance(self._rate_provider, _StaticRateProvider)

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
        block_identifier: int | str | None = None,
    ) -> int:
        """Compute the amount of token_out received for a GIVEN_IN swap.

        Flow (matches deployed MetaStablePool and ComposableStablePool):
        1. Subtract swap fee from raw input amount
        2. Upscale fee-adjusted input and balances (using block-specific rates)
        3. Compute outGivenIn (in scaled space, using adjusted indices)
        4. Downscale down the output

        For ComposableStablePools without a live rate provider, the result
        is wrapped in ``StaleRateResult`` because construction-time
        rates may be stale. Pass ``block_identifier`` with a live rate provider
        for exact-integer matching.

        Returns:
            The computed integer value.

        """
        if override_state is not None:
            balances = list(override_state.balances)
        else:
            balances = list(self.balances)

        token_in_idx = self._tokens.index(token_in)
        token_out_idx = self._tokens.index(token_out)

        # Resolve scaling factors at the target block
        sf = self._resolve_scaling_factors(block_identifier)

        # Step 1: Subtract fee from raw amount
        amount_after_fee = self._subtract_swap_fee_amount(token_in_quantity)

        # Step 2: Upscale balances and fee-adjusted amount
        upscaled_balances = self._upscale_balances(balances, sf)
        amount_in_scaled = self._upscale(amount_after_fee, sf[token_in_idx])

        # Step 3: Compute outGivenIn with adjusted indices
        adjusted_in = self._skip_bpt_index(token_in_idx)
        adjusted_out = self._skip_bpt_index(token_out_idx)

        # For ComposableStablePool, use non-BPT balances
        if self.bpt_idx is not None:
            inv_balances = [upscaled_balances[i] for i in self._non_bpt_indices]
        else:
            inv_balances = upscaled_balances

        invariant = self._compute_invariant(upscaled_balances)

        amount_out_scaled = _calc_out_given_in(
            self.amp,
            list(inv_balances),
            adjusted_in,
            adjusted_out,
            amount_in_scaled,
            invariant,
        )

        # Step 4: Downscale down
        result = self._downscale_down(amount_out_scaled, sf[token_out_idx])

        if self._should_warn_stale_rates():
            raise StaleRateResult(
                amount_in=token_in_quantity,
                amount_out=result,
            )

        return result

    def calculate_tokens_in_from_tokens_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
        block_identifier: int | str | None = None,
    ) -> int:
        """Compute the amount of token_in needed for a GIVEN_OUT swap.

        Flow (matches deployed MetaStablePool and ComposableStablePool):
        1. Upscale output amount and balances (using block-specific rates)
        2. Compute inGivenOut (in scaled space, using adjusted indices)
        3. Downscale up the input amount
        4. Add swap fee to raw amount

        For ComposableStablePools without a live rate provider, the result
        is wrapped in ``StaleRateResult`` because construction-time
        rates may be stale. Pass ``block_identifier`` with a live rate provider
        for exact-integer matching.

        Returns:
            The computed integer value.

        """
        if override_state is not None:
            balances = list(override_state.balances)
        else:
            balances = list(self.balances)

        token_in_idx = self._tokens.index(token_in)
        token_out_idx = self._tokens.index(token_out)

        # Resolve scaling factors at the target block
        sf = self._resolve_scaling_factors(block_identifier)

        # Step 1: Upscale balances and output amount
        upscaled_balances = self._upscale_balances(balances, sf)
        amount_out_scaled = self._upscale(token_out_quantity, sf[token_out_idx])

        # Step 2: Compute inGivenOut with adjusted indices
        adjusted_in = self._skip_bpt_index(token_in_idx)
        adjusted_out = self._skip_bpt_index(token_out_idx)

        # For ComposableStablePool, use non-BPT balances
        if self.bpt_idx is not None:
            inv_balances = [upscaled_balances[i] for i in self._non_bpt_indices]
        else:
            inv_balances = upscaled_balances

        invariant = self._compute_invariant(upscaled_balances)

        amount_in_scaled = _calc_in_given_out(
            self.amp,
            list(inv_balances),
            adjusted_in,
            adjusted_out,
            amount_out_scaled,
            invariant,
        )

        # Step 3: Downscale up
        in_raw = self._downscale_up(amount_in_scaled, sf[token_in_idx])

        # Step 4: Add fee
        result = self._add_swap_fee_amount(in_raw)

        if self._should_warn_stale_rates():
            raise StaleRateResult(
                amount_in=result,
                amount_out=token_out_quantity,
            )

        return result

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
        block_identifier: int | str | None = None,
    ) -> SimulationResult:
        """Simulate swap.

        Returns:
            The computed value.

        """
        balancer_state: BalancerV2PoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, BalancerV2PoolState):
                msg = f"Expected BalancerV2PoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            balancer_state = state_override

        token_in_obj = next((t for t in self._tokens if t.address == token_in), None)
        if token_in_obj is None:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        token_out_obj = next((t for t in self._tokens if t.address == token_out), None)
        if token_out_obj is None:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        initial_state = balancer_state or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            token_out=token_out_obj,
            override_state=balancer_state,
            block_identifier=block_identifier,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        """Return the pool fee regardless of direction.

        Returns:
            The computed value.

        """
        return self.fee

    def external_update(
        self,
        update: BalancerV2StablePoolExternalUpdate,
    ) -> None:
        """Apply an external state update with new balances."""
        if self.state.block is not None and update.block_number < self.state.block:
            return
        with self._state_lock:
            # Re-check after acquiring lock
            if self.state.block is not None and update.block_number < self.state.block:
                return
            self._state = BalancerV2PoolState(
                address=self.address,
                block=update.block_number,
                balances=update.balances,
            )
        self._notify_subscribers(
            BalancerV2PoolStateUpdated(state=self._state),
        )

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: BalancerV2PoolState | None = None,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,
    ) -> HopType:
        """Create a hop state for this pool.

        For 2-token pools, zero_for_one maps to token[0] -> token[1] direction.
        For N-token pools, pass token_in/token_out to select the pair.

        Returns:
            The computed value.

        """
        state = state_override or self.state
        balances = state.balances

        if token_in is not None and token_out is not None:
            try:
                i = self._tokens.index(token_in)
            except ValueError:
                msg = f"token_in ({token_in}) is not a pool token"
                raise DegenbotValueError(message=msg) from None
            try:
                j = self._tokens.index(token_out)
            except ValueError:
                msg = f"token_out ({token_out}) is not a pool token"
                raise DegenbotValueError(message=msg) from None
        elif token_in is not None or token_out is not None:
            msg = "token_in and token_out must both be provided, or both omitted"
            raise DegenbotValueError(message=msg)
        elif zero_for_one:
            i, j = 0, 1
        else:
            i, j = 1, 0

        sf = self._resolve_scaling_factors()
        upscaled = self._upscale_balances(balances, sf)
        inv = self._compute_invariant(upscaled)

        # Build swap_fn with StaleRateResult handling.
        # When the pool has a static rate provider, calculate_tokens_out_from_tokens_in
        # raises StaleRateResult. The wrapped swap_fn catches this and extracts the
        # approximate amount_out so the solver can continue with best-effort values.
        def swap_fn(amount_in: int) -> int:
            try:
                return self.calculate_tokens_out_from_tokens_in(
                    token_in=self._tokens[i],
                    token_out=self._tokens[j],
                    token_in_quantity=amount_in,
                    override_state=state_override,
                )
            except StaleRateResult as e:
                return e.amount_out

        return BalancerStableHop(
            reserve_in=balances[i],
            reserve_out=balances[j],
            fee=self.fee,
            amp=self.amp,
            n_tokens=len(self._non_bpt_indices),
            invariant=inv,
            token_index_in=self._skip_bpt_index(i),
            token_index_out=self._skip_bpt_index(j),
            swap_fn=swap_fn,
            pool_invariant=PoolInvariant.BALANCER_STABLESWAP,
        )

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,
    ) -> BalancerV2SwapAmounts:
        """Build a BalancerV2SwapAmounts for this pool's swap.

        For N > 2 token pools, token_in and token_out must be provided.
        Use BalancerPairView for ArbitragePathPool conformance.

        Returns:
            The computed value.

        """
        # Resolve token pair
        if token_in is not None and token_out is not None:
            pass  # Use caller-specified pair
        elif len(self._tokens) > _MAX_TWO_TOKEN_COUNT:
            msg = (
                f"Pool {self.address} has {len(self._tokens)} tokens. "
                f"build_swap_amount requires token_in and token_out for N > 2 pools. "
                f"Use BalancerPairView for ArbitragePathPool conformance."
            )
            raise DegenbotValueError(message=msg)
        elif zero_for_one:
            token_in = self._tokens[0]
            token_out = self._tokens[1]
        else:
            token_in = self._tokens[1]
            token_out = self._tokens[0]

        return BalancerV2SwapAmounts(
            pool_id=self.pool_id,
            vault=self.vault,
            zero_for_one=zero_for_one,
            amount_in=amount_in,
            amount_out=amount_out,
            token_in=token_in.address,
            token_out=token_out.address,
        )
