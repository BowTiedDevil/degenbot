"""Balancer V2 stable pool implementations (MetaStable, ComposableStable)."""

from __future__ import annotations

from fractions import Fraction
from itertools import starmap
from typing import TYPE_CHECKING, Any, ClassVar, Protocol, Self, runtime_checkable
from weakref import WeakSet

from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.scaling_helpers import _compute_scaling_factor
from degenbot.balancer.math import (
    fixed_point_div_down as _rs_div_down,
)
from degenbot.balancer.math import (
    fixed_point_div_up as _rs_div_up,
)
from degenbot.balancer.math import (
    fixed_point_mul_down as _rs_mul_down,
)
from degenbot.balancer.math import (
    stable_calc_in_given_out as _rs_calc_in_given_out,
)
from degenbot.balancer.math import (
    stable_calc_out_given_in as _rs_calc_out_given_in,
)
from degenbot.balancer.math import (
    stable_calculate_invariant as _rs_calculate_invariant,
)
from degenbot.balancer.math import (
    stable_calculate_invariant_deployed as _rs_calculate_invariant_deployed,
)
from degenbot.balancer.math import (
    weighted_subtract_swap_fee_amount as _rs_subtract_swap_fee_amount,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import StaleRateResult
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.concrete import PublisherMixin, Subscriber

from .types import (
    BalancerV2PoolState,
    BalancerV2PoolStateUpdated,
    BalancerV2StablePoolExternalUpdate,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress

    from degenbot.types import LiquidityPool

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


class _HandleRateProviderAdapter:
    """Thin Python shim that delegates to the stored Rust rate-provider trait object.

    Returned by ``BalancerV2StablePool.rate_provider`` when the stored
    provider is dynamic (the sealed seam keeps the provider in Rust, not on
    the companion). Exposes the ``get_rates`` surface so callers that hold a
    reference to ``pool.rate_provider`` keep working; the canonical read path
    is ``_resolve_scaling_factors`` (which calls the handle directly).
    """

    def __init__(self, py_pool: LiquidityPool) -> None:
        self._py_pool = py_pool

    def get_rates(self, block_identifier: int | str | None = None) -> tuple[int, ...]:
        """Fetch rates via the handle (delegates to the Rust trait object).

        Returns:
            The per-token rates at the requested block.

        """
        block_opt = block_identifier if isinstance(block_identifier, int) else None
        rates = self._py_pool.fetch_balancer_stable_rates(block_opt)
        if rates is None:  # pragma: no cover — rate_provider is non-None
            return ()
        return tuple(rates)


class BalancerV2StablePool(PublisherMixin, AbstractLiquidityPool):
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

    # Class-scope instance-attribute declarations (red-knot): `_from_py_pool`
    # assigns these on `Self`; declare them at class scope so attribute reads
    # in helper methods resolve (mirrors the weighted companion).
    address: ChecksumAddress
    pool_id: bytes
    pool_specialization: int
    vault: ChecksumAddress
    _py_pool: LiquidityPool
    _tokens: tuple[Erc20Token, ...]
    scaling_factors: tuple[int, ...]
    fee: Fraction
    amp: int
    bpt_idx: int | None
    invariant_version: int
    _base_scaling_factors: tuple[int, ...]
    _non_bpt_indices: tuple[int, ...]
    _rate_provider_is_static: bool
    _subscribers: WeakSet[Subscriber]

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``BalancerV2StablePool`` is a Python companion over a Rust-owned
        ``LiquidityPool`` handle. Use the registered entry points instead:

        - Production: ``Bot.build_pool(address)``
        - Tests: ``make_balancer_stable_pool(...)``

        Both register the pool in Rust (including the optional rate provider
        as the stored I/O trait object), obtain the ``LiquidityPool``
        handle, and wrap it via :meth:`_from_py_pool`.

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.build_pool(address) (production) or "
            "make_balancer_stable_pool(...) (tests) to register the pool in "
            "Rust and obtain the LiquidityPool handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_pool(cls, py_pool: LiquidityPool) -> Self:
        """Wrap a Rust-owned ``LiquidityPool`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). Every
        identity field (vault, pool_id, tokens, amp, scaling_factors,
        swap_fee, bpt_idx, invariant_version) is read off the handle; the rate
        provider is the stored I/O trait object (queried via
        ``fetch_balancer_stable_rates`` / ``balancer_stable_rate_provider_is_static``);
        base scaling factors are derived from token decimals.

        Returns:
            A ``cls`` instance wrapping ``py_pool``.

        Raises:
            DegenbotValueError: If the handle is not a Balancer stable pool
                or any token is not registered.

        """
        self = cls.__new__(cls)
        self._py_pool = py_pool

        if py_pool.pool_family != "balancer-stable":
            msg = (
                "LiquidityPool handle is not a Balancer stable pool "
                f"(got pool_family {py_pool.pool_family!r})"
            )
            raise DegenbotValueError(message=msg)

        self.address = get_checksum_address(py_pool.balancer_stable_vault)
        # The vault IS the address book entry; the pool address is encoded
        # in the pool_id. Resolve the pool address off the pool_id.
        self.vault = get_checksum_address(py_pool.balancer_stable_vault)

        pool_id_hex = py_pool.balancer_stable_pool_id_hex.removeprefix("0x")
        self.pool_id = bytes.fromhex(pool_id_hex)
        self.pool_specialization = int.from_bytes(self.pool_id[20:22], byteorder="big")
        # Pool address = first 20 bytes of the pool_id.
        self.address = get_checksum_address("0x" + self.pool_id[:20].hex())

        py_tokens = py_pool.get_balancer_stable_tokens()
        if py_tokens is None:
            msg = (
                "pool tokens must be registered in the same Bot as the pool "
                "(ADR-006): get_balancer_stable_tokens returned None"
            )
            raise DegenbotValueError(message=msg)
        self._tokens = tuple(
            Erc20Token._from_py_token(t)  # ruff:ignore[private-member-access]
            for t in py_tokens
        )

        self.amp = py_pool.balancer_amp
        self.bpt_idx = py_pool.balancer_bpt_index
        self.invariant_version = py_pool.balancer_invariant_version

        swap_fee_scaled = py_pool.balancer_stable_swap_fee
        self.fee = (
            Fraction(swap_fee_scaled, self.FEE_DENOMINATOR)
            if self.FEE_DENOMINATOR != 0
            else Fraction(0)
        )

        # Full scaling factors (rate-multiplied) live on the Rust identity;
        # read under one lock as ints.
        self.scaling_factors = tuple(int(x) for x in py_pool.balancer_stable_scaling_factors)

        # Base scaling factors: decimal-adjustment-only (10^(18-dec)).
        self._base_scaling_factors = tuple(_compute_scaling_factor(token) for token in self._tokens)

        # Rate provider: the stored I/O trait object. The companion never
        # holds a direct Python reference — it queries the handle (which
        # re-enters the Py adapter for dynamic providers, returns the static
        # 1e18 fallback for the no-provider case).
        self._rate_provider_is_static = py_pool.balancer_stable_rate_provider_is_static

        # Precompute non-BPT index mapping for ComposableStablePool.
        if self.bpt_idx is not None:
            self._non_bpt_indices = tuple(i for i in range(len(self._tokens)) if i != self.bpt_idx)
        else:
            self._non_bpt_indices = tuple(range(len(self._tokens)))

        self._subscribers = WeakSet()
        return self

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
        """Balances.

        Read from the Rust core via the ``LiquidityPool`` handle
        (ADR-005 slice 12d). Rust ``BotState`` is the single source of truth
        for the mutable ``balances`` slot; this getter returns the live tuple
        (one U256 per token, including BPT for Composable pools).
        """
        return tuple(self._py_pool.balancer_stable_balances)

    @property
    def state(self) -> PoolState:
        """State.

        Built from one atomic Rust snapshot
        (``snapshot_balancer_stable()`` — ``(balances, block)``) so callers
        see a coherent tuple (no torn read mid-``external_update``). Mirrors
        V3/V4's ``snapshot_v3()`` / Curve's ``snapshot_curve()`` /
        Weighted's ``snapshot_balancer_weighted()`` contract.

        Raises:
            DegenbotValueError: If the Rust snapshot is absent (the pool is
                not registered in Rust as a Balancer stable pool —
                unreachable for a companion built over a registered handle).

        """
        snap = self._py_pool.snapshot_balancer_stable()
        if snap is None:  # pragma: no cover - defensive, unreachable in practice
            msg = f"No Balancer stable pool state available for {self.address}"
            raise DegenbotValueError(message=msg)
        balances, block = snap
        return BalancerV2PoolState(
            address=self.address,
            balances=tuple(balances),
            block=block,
        )

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        return self._tokens

    @property
    def rate_provider(self) -> BalancerRateProvider | None:
        """The rate provider for per-block rate resolution, if available.

        With the sealed seam (ADR-005 MBWSGP) the provider is the stored Rust
        I/O trait object, queried via the handle. This property returns
        ``None`` when the stored provider is static (the no-I/O fallback).
        """
        if self._rate_provider_is_static:
            return None
        return _HandleRateProviderAdapter(self._py_pool)

    @property
    def requires_io_at_calculation_time(self) -> bool:
        """Whether this pool may call its rate_provider during swap calculations.

        Returns True for ComposableStablePools with a live rate provider
        (time-varying rates from yield-bearing tokens). Returns False for
        MetaStablePools and for pools with only a static rate provider.
        """
        # ComposableStablePools always need fresh rates for exact matching
        return self.bpt_idx is not None and not self._rate_provider_is_static

    @staticmethod
    def _upscale(amount: int, scaling_factor: int) -> int:
        """Upscale a token amount using the scaling factor (mulDown).

        Returns:
            The computed integer value.

        """
        return _rs_mul_down(amount, scaling_factor)

    @staticmethod
    def _downscale_down(amount: int, scaling_factor: int) -> int:
        """Downscale a token amount, rounding down (divDown).

        Returns:
            The computed integer value.

        """
        return _rs_div_down(amount, scaling_factor)

    @staticmethod
    def _downscale_up(amount: int, scaling_factor: int) -> int:
        """Downscale a token amount, rounding up (divUp).

        Returns:
            The computed integer value.

        """
        return _rs_div_up(amount, scaling_factor)

    def _subtract_swap_fee_amount(self, amount: int) -> int:
        """Subtract swap fee from amount (mulUp for fee, matches deployed contract).

        Returns:
            The computed integer value.

        """
        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)
        return _rs_subtract_swap_fee_amount(amount, fee_scaled)

    def _add_swap_fee_amount(self, amount: int) -> int:
        """Add swap fee to amount (divUp, matches deployed contract).

        Returns:
            The computed integer value.

        """
        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)
        return _rs_div_up(amount, ONE - fee_scaled)

    def _resolve_scaling_factors(
        self,
        block_identifier: int | str | None = None,
    ) -> tuple[int, ...]:
        """Resolve scaling factors at the given block.

        If a live rate provider is available, fetches rates for the block
        and computes scaling factors as base_sf * rate // ONE (mulDown).
        Otherwise, falls back to construction-time scaling factors.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: If no rate provider is available on the
                handle.

        """
        if self._rate_provider_is_static:
            return self.scaling_factors

        # Resolve block_identifier → Option<u64> for the Rust trait object.
        # A str ("latest") or None maps to None (latest); an int maps to Some.
        block_opt = block_identifier if isinstance(block_identifier, int) else None
        rates = self._py_pool.fetch_balancer_stable_rates(block_opt)
        if rates is None:
            msg = "no Balancer stable rate provider available"
            raise DegenbotValueError(message=msg)
        return tuple(starmap(_rs_mul_down, zip(self._base_scaling_factors, rates, strict=True)))

    @staticmethod
    def _upscale_balances(balances: Sequence[int], scaling_factors: Sequence[int]) -> list[int]:
        """Upscale all balances using the given scaling factors.

        Returns:
            A list of results.

        """
        return list(starmap(_rs_mul_down, zip(balances, scaling_factors, strict=False)))

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
            return _rs_calculate_invariant(self.amp, balances_for_inv)
        return _rs_calculate_invariant_deployed(self.amp, balances_for_inv, round_up=True)

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
        return self.bpt_idx is not None and self._rate_provider_is_static

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

        Raises:
            StaleRateResult: See function documentation.

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

        amount_out_scaled = _rs_calc_out_given_in(
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

        Raises:
            StaleRateResult: See function documentation.

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

        amount_in_scaled = _rs_calc_in_given_out(
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

    def external_update(
        self,
        update: BalancerV2StablePoolExternalUpdate,
    ) -> None:
        """Apply an external state update with new balances.

        Delegates to the Rust core
        (``LiquidityPool.apply_balancer_stable_balance_update``) which
        journals the prior balances (genesis-anchor V2-style discipline) and
        lands the new balances + ``update_block`` atomically
        (ADR-005 slice 12d). The ``_state_lock`` + double-check-after-acquire
        pattern is gone — Rust's internal write lock handles atomicity; the
        registration-state precondition is enforced by the Rust core's
        silent-no-op-on-older-block contract.

        Raises:
            DegenbotValueError: If the Rust core rejects the update (the pool
                is not registered as a Balancer stable pool — unreachable for
                a companion built over a registered handle).

        """
        if self.state.block is not None and update.block_number < self.state.block:
            return
        applied = self._py_pool.apply_balancer_stable_balance_update(
            list(update.balances),
            update.block_number,
        )
        if not applied:  # pragma: no cover - defensive, unreachable for a stable handle
            msg = (
                f"external_update rejected for {self.address} (not a Balancer stable pool in Rust)"
            )
            raise DegenbotValueError(message=msg)
        new_state = BalancerV2PoolState(
            address=self.address,
            balances=update.balances,
            block=update.block_number,
        )
        self._notify_subscribers(
            BalancerV2PoolStateUpdated(state=new_state),
        )
