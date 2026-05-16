from collections.abc import Sequence
from fractions import Fraction
from threading import Lock
from typing import Any, ClassVar
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.balancer.libraries.fixed_point import mul_up
from degenbot.balancer.libraries.scaling_helpers import (
    _compute_scaling_factor,
    _downscale_down,
    _upscale,
    _upscale_array,
)
from degenbot.balancer.libraries.weighted_math import _calc_out_given_in, _subtract_swap_fee_amount
from degenbot.balancer.types import BalancerV2PoolState
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import HopType
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult


class BalancerV2Pool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
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
        weights: Sequence[int],
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
        self.scaling_factors = tuple(_compute_scaling_factor(token) for token in self._tokens)
        self.fee = fee
        self.weights = tuple(weights)

        self._state_lock = Lock()
        self._state = BalancerV2PoolState(
            address=self.address,
            block=state_block,
            balances=tuple(balances),
        )
        self._subscribers: WeakSet[Subscriber] = WeakSet()

    @property
    def balances(self) -> tuple[int, ...]:
        return self.state.balances

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def state(self) -> PoolState:
        return self._state

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        return self._tokens

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        token_in_index = self._tokens.index(token_in)
        token_out_index = self._tokens.index(token_out)

        fee_amount = mul_up(token_in_quantity, self.fee * self.FEE_DENOMINATOR)

        amount_new = _subtract_swap_fee_amount(
            amount=token_in_quantity,
            fee_percentage=self.fee * self.FEE_DENOMINATOR,
        )

        assert token_in_quantity - fee_amount == amount_new

        if override_state is not None:
            # TODO: add functionality
            raise NotImplementedError
        balances = list(self.balances)  # make a copy because _upscale_array will mutate it

        _upscale_array(amounts=balances, scaling_factors=self.scaling_factors)
        amount_new = _upscale(amount_new, scaling_factor=self.scaling_factors[token_in_index])

        amount_out = _calc_out_given_in(
            balance_in=int(balances[token_in_index]),
            weight_in=self.weights[token_in_index],
            balance_out=int(balances[token_out_index]),
            weight_out=self.weights[token_out_index],
            amount_in=int(amount_new),
        )

        return int(
            _downscale_down(amount=amount_out, scaling_factor=self.scaling_factors[token_out_index])
        )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: BalancerV2PoolState | None = None,
    ) -> SimulationResult:
        token_in_obj = next((t for t in self._tokens if t.address == token_in), None)
        if token_in_obj is None:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        token_out_obj = next((t for t in self._tokens if t.address == token_out), None)
        if token_out_obj is None:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        initial_state = state_override or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            token_out=token_out_obj,
            override_state=state_override,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return self.fee

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: BalancerV2PoolState | None = None,
    ) -> HopType:

        msg = (
            "Balancer pool to_hop_state is not yet implemented. "
            "Pair-wise hop state extraction from N-token pools is not straightforward."
        )
        raise NotImplementedError(msg)
