from collections.abc import Sequence
from fractions import Fraction
from threading import Lock
from typing import Any, ClassVar
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.stable_math import (
    _calc_in_given_out,
    _calc_out_given_in,
    _calculate_invariant_deployed,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult

from .types import BalancerV2PoolState


class BalancerV2StablePool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
    """
    Balancer V2 Stable Pool (MetaStablePool or ComposableStablePool).

    Supports token-to-token swaps using StableMath with deployed-contract-matching
    invariant computation (roundUp=True). For ComposableStablePools, the BPT token
    is automatically dropped from the invariant and swap calculations.

    Swap fee application order:
      GIVEN_IN:  subtractFee → upscale → compute(outGivenIn) → downscaleDown
      GIVEN_OUT: upscale → compute(inGivenOut) → downscaleUp → addFee
    """

    type PoolState = BalancerV2PoolState
    FEE_DENOMINATOR = 1 * 10**18

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_state_lock": Lock,
        "_subscribers": WeakSet,
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
        chain_id: ChainId | None = None,
        state_block: BlockNumber | None = None,
    ) -> None:
        self.address = get_checksum_address(address)

        self._chain_id = chain_id if chain_id is not None else tokens[0].chain_id
        state_block = state_block if state_block is not None else 0

        self.pool_id = pool_id
        self.pool_specialization = int.from_bytes(self.pool_id[20:22], byteorder="big")
        self.vault = get_checksum_address(vault)
        self._tokens = tuple(tokens)
        self.fee = fee
        self.amp = amp
        self.scaling_factors = tuple(scaling_factors)
        self.bpt_idx = bpt_idx

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
        pool_type = "ComposableStablePool" if self.bpt_idx is not None else "MetaStablePool"
        return (
            f"{self.__class__.__name__}(address={self.address}, "
            f"type={pool_type}, tokens={len(self._tokens)})"
        )

    def __str__(self) -> str:  # pragma: no cover
        pool_type = "ComposableStablePool" if self.bpt_idx is not None else "MetaStablePool"
        return f"{self.__class__.__name__} {pool_type} @ {self.address}"

    @property
    def balances(self) -> tuple[int, ...]:
        return self.state.balances

    @property
    def chain_id(self) -> int | None:
        return self._chain_id

    @property
    def state(self) -> PoolState:
        return self._state

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        return self._tokens

    @staticmethod
    def _upscale(amount: int, scaling_factor: int) -> int:
        """Upscale a token amount using the scaling factor (mulDown)."""
        return amount * scaling_factor // ONE

    @staticmethod
    def _downscale_down(amount: int, scaling_factor: int) -> int:
        """Downscale a token amount, rounding down (divDown)."""
        return amount * ONE // scaling_factor

    @staticmethod
    def _downscale_up(amount: int, scaling_factor: int) -> int:
        """Downscale a token amount, rounding up (divUp)."""
        return (amount * ONE + scaling_factor - 1) // scaling_factor

    def _subtract_swap_fee_amount(self, amount: int) -> int:
        """Subtract swap fee from amount (mulUp for fee, matches deployed contract)."""
        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)
        fee_amount = (amount * fee_scaled + ONE - 1) // ONE  # mulUp
        return amount - fee_amount

    def _add_swap_fee_amount(self, amount: int) -> int:
        """Add swap fee to amount (divUp, matches deployed contract)."""
        fee_scaled = int(self.fee * self.FEE_DENOMINATOR)
        numerator = amount * ONE
        denominator = ONE - fee_scaled
        return numerator // denominator + (1 if numerator % denominator > 0 else 0)

    def _upscale_balances(self, balances: list[int]) -> list[int]:
        """Upscale all balances using scaling factors."""
        return [b * sf // ONE for b, sf in zip(balances, self.scaling_factors, strict=False)]

    def _compute_invariant(self, upscaled_balances: list[int]) -> int:
        """Compute invariant using deployed version with roundUp=True."""
        # For ComposableStablePool, drop BPT before computing invariant
        if self.bpt_idx is not None:
            balances_for_inv = [upscaled_balances[i] for i in self._non_bpt_indices]
        else:
            balances_for_inv = upscaled_balances

        return _calculate_invariant_deployed(self.amp, balances_for_inv, round_up=True)

    def _skip_bpt_index(self, index: int) -> int:
        """Map a full token list index to the non-BPT index.

        Matches Solidity's _skipBptIndex: returns index if index < bpt_idx,
        otherwise index - 1.
        """
        if self.bpt_idx is None:
            return index
        return index if index < self.bpt_idx else index - 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """
        Compute the amount of token_out received for a GIVEN_IN swap.

        Flow (matches deployed MetaStablePool and ComposableStablePool):
        1. Subtract swap fee from raw input amount
        2. Upscale fee-adjusted input and balances
        3. Compute outGivenIn (in scaled space, using adjusted indices)
        4. Downscale down the output
        """
        if override_state is not None:
            raise NotImplementedError

        token_in_idx = self._tokens.index(token_in)
        token_out_idx = self._tokens.index(token_out)

        # Step 1: Subtract fee from raw amount
        amount_after_fee = self._subtract_swap_fee_amount(token_in_quantity)

        # Step 2: Upscale balances and fee-adjusted amount
        balances = list(self.balances)
        upscaled_balances = self._upscale_balances(balances)
        amount_in_scaled = self._upscale(amount_after_fee, self.scaling_factors[token_in_idx])

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
        return self._downscale_down(amount_out_scaled, self.scaling_factors[token_out_idx])

    def calculate_tokens_in_from_tokens_out(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """
        Compute the amount of token_in needed for a GIVEN_OUT swap.

        Flow (matches deployed MetaStablePool and ComposableStablePool):
        1. Upscale output amount and balances
        2. Compute inGivenOut (in scaled space, using adjusted indices)
        3. Downscale up the input amount
        4. Add swap fee to raw amount
        """
        if override_state is not None:
            raise NotImplementedError

        token_in_idx = self._tokens.index(token_in)
        token_out_idx = self._tokens.index(token_out)

        # Step 1: Upscale balances and output amount
        balances = list(self.balances)
        upscaled_balances = self._upscale_balances(balances)
        amount_out_scaled = self._upscale(token_out_quantity, self.scaling_factors[token_out_idx])

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
        in_raw = self._downscale_up(amount_in_scaled, self.scaling_factors[token_in_idx])

        # Step 4: Add fee
        return self._add_swap_fee_amount(in_raw)

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
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
        return self.fee

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: BalancerV2PoolState | None = None,
    ) -> None:
        msg = (
            "Balancer stable pool to_hop_state is not yet implemented. "
            "Pair-wise hop state extraction from N-token pools is not straightforward."
        )
        raise NotImplementedError(msg)
