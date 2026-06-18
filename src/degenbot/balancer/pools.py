"""Balancer V2 weighted pool implementation."""

from collections.abc import Sequence
from fractions import Fraction
from threading import Lock
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
        address: ChecksumAddress | str,
        *,
        pool_id: bytes,
        vault: str,
        tokens: Sequence[Erc20Token],
        balances: Sequence[int],
        fee: Fraction,
        weights: Sequence[int],
        pow_version: PowVersion = PowVersion.V1,
        chain_id: ChainId | None = None,
        state_block: BlockNumber | None = None,
    ) -> None:
        """Initialize the instance."""
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
        return f"{self.__class__.__name__}(address={self.address}, tokens={len(self._tokens)})"

    def __str__(self) -> str:  # pragma: no cover
        """Return a human-readable string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__} WeightedPool @ {self.address}"

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
