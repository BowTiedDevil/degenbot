"""Balancer V2 weighted pool implementation."""

from collections.abc import Sequence
from fractions import Fraction
from typing import ClassVar
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.balancer.libraries.constants import PowVersion
from degenbot.balancer.libraries.scaling_helpers import (
    _compute_scaling_factor,
    _downscale_down,
    _downscale_up,
    _upscale,
    _upscale_array,
)
from degenbot.balancer.libraries.weighted_math import (
    _add_swap_fee_amount,
    _calc_in_given_out,
    _calc_out_given_in,
    _subtract_swap_fee_amount,
)
from degenbot.balancer.swap_amounts import BalancerV2SwapAmounts
from degenbot.balancer.types import (
    BalancerV2PoolState,
    BalancerV2PoolStateUpdated,
    BalancerV2WeightedPoolExternalUpdate,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyLiquidityPool
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import BalancerWeightedHop, HopType
from degenbot.types.pool_protocols import SimulationResult

_MAX_TWO_TOKEN_COUNT = 2

# Hex representation of the TWO constant (2e18 = 0x1bc16d674ec80000)
# This value only appears in the bytecode of V2 WeightedPool contracts that include
# the fast-path optimizations for y == ONE / TWO / FOUR in powDown/powUp.
_V2_TWO_HEX = "1bc16d674ec80000"


def detect_pow_version(bytecode: str) -> PowVersion:
    """Detect which FixedPoint library version a pool contract uses from its bytecode.

    V2 (WeightedPool) contracts include fast paths for y == ONE, TWO, FOUR in
    powDown/powUp. These reference the TWO and FOUR constants, which are absent
    from V1 (WeightedPool2Tokens) bytecode.

    The detection works by checking for the TWO constant (0x1bc16d674ec80000)
    in the deployed bytecode. This constant is used in the `y == TWO` fast-path
    comparison and does not appear in V1 contracts.

    Returns:
        The computed value.

    """
    if _V2_TWO_HEX in bytecode:
        return PowVersion.V2
    return PowVersion.V1


class BalancerV2Pool(PublisherMixin, AbstractLiquidityPool):
    """BalancerV2Pool class."""

    variant: ClassVar[str | None] = "balancer_weighted"
    type PoolState = BalancerV2PoolState
    FEE_DENOMINATOR = 1 * 10**18

    def __init__(
        self,
        py_pool: PyLiquidityPool,
        *,
        address: ChecksumAddress | str,
        pool_id: bytes,
        vault: str,
        tokens: Sequence[Erc20Token],
        fee: Fraction,
        weights: Sequence[int],
        pow_version: PowVersion = PowVersion.V1,
        chain_id: ChainId | None = None,
        state_block: BlockNumber | None = None,
    ) -> None:
        """Initialize a Balancer V2 weighted pool companion over a PyLiquidityPool handle.

        A companion over a ``PyLiquidityPool`` handle (ADR-005 slice 12b):
        Rust ``BotState`` owns the mutable ``balances`` / ``update_block`` slot +
        the reorg journal; this companion reads them through ``self._py_pool``
        (``snapshot_balancer_weighted()`` for an atomic ``(balances, block)``
        tuple, ``balancer_balances`` / ``update_block`` getters) and delegates
        ``external_update`` to ``apply_balancer_weighted_balance_update``.
        Immutable config (vault, pool_id, tokens, weights, scaling_factors,
        fee, pow_version) stays Python-side — matches V3/V4/Curve.

        The companion is constructed AFTER ``register_balancer_weighted_pool``
        has landed the registration ``balances`` / ``update_block`` into Rust,
        so this constructor takes no balance argument — read live from the
        handle on demand.

        Constructed from pre-fetched data only. Use ``Bot.build_pool()`` to
        fetch from chain; tests use ``make_balancer_weighted_pool``.

        I/O boundary:
            Construction and calculation are I/O-free — all immutable
            parameters (fee, weights, tokens, pow_version) are provided by the
            builder/factory. No rate providers for weighted pools (the scaling
            factors come purely from token decimals, computed at registration).
        """
        self._py_pool = py_pool
        self.address = get_checksum_address(address)

        self._chain_id = chain_id if chain_id is not None else tokens[0].chain_id
        state_block = state_block if state_block is not None else 0

        self.pool_id = pool_id
        self.pool_specialization = int.from_bytes(self.pool_id[20:22], byteorder="big")
        self.vault = get_checksum_address(vault)
        self._tokens = tuple(tokens)
        self.scaling_factors = tuple(_compute_scaling_factor(token) for token in self._tokens)
        self.fee = fee
        self.weights = tuple(weights)
        self.pow_version = pow_version

        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__}(address={self.address}, tokens={len(self._tokens)})"

    def __str__(self) -> str:  # pragma: no cover
        """Return a human-readable string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__} WeightedPool @ {self.address}"

    @property
    def balances(self) -> tuple[int, ...]:
        """Balances.

        Read from the Rust core via the ``PyLiquidityPool`` handle
        (ADR-005 slice 12b). Rust ``BotState`` is the single source of truth
        for the mutable ``balances`` slot; this getter returns the live tuple.
        """
        return tuple(self._py_pool.balancer_balances)

    @property
    def chain_id(self) -> int | None:
        """Return chain id."""
        return self._chain_id

    @property
    def state(self) -> PoolState:
        """State.

        Built from one atomic Rust snapshot
        (``snapshot_balancer_weighted()`` — ``(balances, block)``) so
        callers see a coherent tuple (no torn read mid-``external_update``).
        Mirrors V3/V4's ``snapshot_v3()`` and Curve's ``snapshot_curve()``
        contract.

        Raises:
            DegenbotValueError: If the Rust snapshot is absent (the pool is
                not registered in Rust as a Balancer weighted pool —
                unreachable for a companion built over a registered handle).

        """
        snap = self._py_pool.snapshot_balancer_weighted()
        # snapshot_balancer_weighted returns None only for a non-Balancer-
        # weighted pool_id; this companion is always built over a registered
        # Balancer weighted handle, so the snapshot is always present.
        # Defensive: treat None as no-state.
        if snap is None:  # pragma: no cover - defensive, unreachable in practice
            msg = f"No Balancer weighted pool state available for {self.address}"
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

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """Calculate tokens out from tokens in.

        Returns:
            The computed integer value.

        """
        token_in_index = self._tokens.index(token_in)
        token_out_index = self._tokens.index(token_out)

        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)

        amount_new = _subtract_swap_fee_amount(
            amount=token_in_quantity,
            fee_percentage=fee_scaled,
        )

        if override_state is not None:
            balances = list(override_state.balances)
        else:
            balances = list(self.balances)  # make a copy because _upscale_array will mutate it

        _upscale_array(amounts=balances, scaling_factors=self.scaling_factors)
        amount_new = _upscale(amount_new, scaling_factor=self.scaling_factors[token_in_index])

        amount_out = _calc_out_given_in(
            balance_in=int(balances[token_in_index]),
            weight_in=self.weights[token_in_index],
            balance_out=int(balances[token_out_index]),
            weight_out=self.weights[token_out_index],
            amount_in=int(amount_new),
            version=self.pow_version,
        )

        return int(
            _downscale_down(amount=amount_out, scaling_factor=self.scaling_factors[token_out_index])
        )

    def calculate_tokens_in_from_tokens_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """Compute how many tokens must be sent to take `token_out_quantity` out.

        Returns:
            The computed integer value.

        """
        token_in_index = self._tokens.index(token_in)
        token_out_index = self._tokens.index(token_out)

        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)

        if override_state is not None:
            balances = list(override_state.balances)
        else:
            balances = list(self.balances)  # make a copy because _upscale_array will mutate it

        _upscale_array(amounts=balances, scaling_factors=self.scaling_factors)
        amount_out_scaled = _upscale(
            token_out_quantity,
            scaling_factor=self.scaling_factors[token_out_index],
        )

        amount_in = _calc_in_given_out(
            balance_in=int(balances[token_in_index]),
            weight_in=self.weights[token_in_index],
            balance_out=int(balances[token_out_index]),
            weight_out=self.weights[token_out_index],
            amount_out=int(amount_out_scaled),
            version=self.pow_version,
        )

        # Downscale first, then add fee — matching Solidity's onSwap GIVEN_OUT path
        amount_in_token = int(
            _downscale_up(
                amount=amount_in,
                scaling_factor=self.scaling_factors[token_in_index],
            )
        )

        return _add_swap_fee_amount(
            amount=amount_in_token,
            fee_percentage=fee_scaled,
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
            The computed value.

        Raises:
            DegenbotValueError: See function documentation.

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
        update: BalancerV2WeightedPoolExternalUpdate,
    ) -> None:
        """Apply an external state update with new balances.

        Delegates to the Rust core
        (``PyLiquidityPool.apply_balancer_weighted_balance_update``) which
        journals the prior balances (genesis-anchor V2-style discipline) and
        lands the new balances + ``update_block`` atomically
        (ADR-005 slice 12b). The ``_state_lock`` + double-check-after-acquire
        pattern it used to apply is gone — Rust's internal write lock handles
        atomicity; the registration-state precondition is enforced by the
        Rust core's silent-no-op-on-older-block contract.

        Raises:
            DegenbotValueError: If the Rust core rejects the update (the pool
                is not registered as a Balancer weighted pool — unreachable
                for a companion built over a registered handle).

        """
        applied = self._py_pool.apply_balancer_weighted_balance_update(
            list(update.balances), update.block_number
        )
        if not applied:  # pragma: no cover - defensive, unreachable for a weighted handle
            msg = (
                f"external_update rejected for {self.address} "
                "(not a Balancer weighted pool in Rust)"
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

        Raises:
            DegenbotValueError: See function documentation.

        """
        state = state_override or self.state

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

        # Build swap_fn wrapping the pool's calculate_tokens_out_from_tokens_in.
        # This provides integer-accurate evaluation in mixed-path solvers.
        def swap_fn(amount_in: int) -> int:
            return self.calculate_tokens_out_from_tokens_in(
                token_in=self._tokens[i],
                token_out=self._tokens[j],
                token_in_quantity=amount_in,
                override_state=state_override,
            )

        return BalancerWeightedHop(
            reserve_in=state.balances[i],
            reserve_out=state.balances[j],
            fee=self.fee,
            weight_in=self.weights[i],
            weight_out=self.weights[j],
            swap_fn=swap_fn,
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

        Raises:
            DegenbotValueError: See function documentation.

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
