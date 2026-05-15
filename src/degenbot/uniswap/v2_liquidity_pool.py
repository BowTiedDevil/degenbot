# ruff: noqa: PLR0904


import contextlib
import dataclasses
from collections import deque
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import UniswapV2PoolTable
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.liquidity_pool import (
    ExternalUpdateError,
    NoPoolStateAvailable,
)
from degenbot.logging import logger
from degenbot.types.abstract import AbstractArbitrage, AbstractLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import ConstantProductHop, HopType
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v2_functions import (
    generate_v2_pool_address,
)
from degenbot.uniswap.v2_pool_calc import UniswapV2PoolCalc
from degenbot.uniswap.v2_pool_state import V2PoolState
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)


class UniswapV2Pool(PublisherMixin, PoolPickleMixin, V2PoolState, UniswapV2PoolCalc, AbstractLiquidityPool):
    """
    A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant.
    """

    variant: ClassVar[str | None] = None

    type PoolState = UniswapV2PoolState
    type DatabasePoolType = UniswapV2PoolTable

    _state: PoolState
    _state_cache: deque[PoolState]

    UNISWAP_V2_MAINNET_POOL_INIT_HASH = (
        "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
    )

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
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: BlockNumber | None = None,
        state_cache_depth: int = 8,
        token0: Erc20Token = ...,
        token1: Erc20Token = ...,
        factory: str = ...,
        fee_token0: Fraction = ...,
        fee_token1: Fraction = ...,
        reserves_token0: int = ...,
        reserves_token1: int = ...,
    ) -> None:
        """
        An I/O-free representation of an x*y=k invariant automatic matchmaker, based on Uniswap V2.

        Construct via Bot.build_pool() or manager.get_pool() to fetch data from the chain.
        """

        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else token0.chain_id
        self._token0 = token0
        self._token1 = token1
        self.factory = get_checksum_address(factory)
        self._fee_token0 = fee_token0
        self._fee_token1 = fee_token1

        # Derive deployer/init_hash from factory deployments or fallback
        self.init_hash = (
            init_hash if init_hash is not None else self.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        )
        self.deployer = get_checksum_address(deployer_address or self.factory)

        with contextlib.suppress(KeyError):
            factory_deployment = FACTORY_DEPLOYMENTS[self._chain_id][self.factory]
            self.init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                self.deployer = factory_deployment.deployer

        _state_block = state_block if state_block is not None else 0

        fee_string = (
            f"{100 * self._fee_token0.numerator / self._fee_token0.denominator:.2f}"
            if self._fee_token0 == self._fee_token1
            else (
                f"{100 * self._fee_token0.numerator / self._fee_token0.denominator:.2f}"
                f"/"
                f"{100 * self._fee_token1.numerator / self._fee_token1.denominator:.2f}"
            )
        )
        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, {fee_string}%)"

        initial_state = self.PoolState.__value__(
            address=self.address,
            reserves_token0=reserves_token0,
            reserves_token1=reserves_token1,
            block=_state_block,
        )
        self._state_cache: deque[UniswapV2PoolState] = deque(maxlen=max(1, state_cache_depth))
        self._state_cache.append(initial_state)
        self._state_lock = Lock()
        self._subscribers: WeakSet[Subscriber] = WeakSet()

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1})"  # noqa:E501

    def _verified_address(self) -> ChecksumAddress:
        return generate_v2_pool_address(
            deployer_address=self.deployer,
            token_addresses=(self._token0.address, self._token1.address),
            init_hash=self.init_hash,
        )

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

    @property
    def reserves_token0(self) -> int:
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        return self.state.reserves_token1

    @property
    def state(self) -> PoolState:
        return self._state_cache[-1]

    @staticmethod
    def swap_is_viable(
        state: PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        if state.reserves_token0 == 0 or state.reserves_token1 == 0:
            return False
        return state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1

    def external_update(
        self,
        update: UniswapV2PoolExternalUpdate,
    ) -> None:
        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}"  # noqa:E501
            )

        with self._state_lock:
            if (
                update.reserves_token0,
                update.reserves_token1,
            ) == (
                self.reserves_token0,
                self.reserves_token1,
            ):
                return

            working_state = dataclasses.replace(
                self.state,
                reserves_token0=update.reserves_token0,
                reserves_token1=update.reserves_token1,
                block=update.block_number,
            )

            if self.state.block == update.block_number:
                self._state_cache.pop()
            self._state_cache.append(working_state)

            self._notify_subscribers(
                message=UniswapV2PoolStateUpdated(self.state),
            )

    def discard_states_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """
        Discard cached states earlier than the given block.
        """

        with self._state_lock:
            # The oldest state already satisfies the request
            if (earliest_block := self._state_cache[0].block) and earliest_block >= block:
                return

            # The newest state is older than the target block, so there is no state to return to
            if (newest_block := self._state_cache[-1].block) and newest_block < block:
                raise NoPoolStateAvailable(block=block)

            # Discard older states until the earliest block meets or crosses the target
            while (earliest_block := self._state_cache[0].block) is None or earliest_block < block:
                self._state_cache.popleft()

            assert self.state.block is not None
            assert self.state.block >= block

    def restore_state_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """
        Restore the last pool state recorded prior to a target block.
        """

        with self._state_lock:
            # The newest state already satisfies the request
            if (newest_block := self._state_cache[-1].block) and newest_block < block:
                return

            # No earlier state is available
            if (earliest_block := self._state_cache[0].block) and earliest_block >= block:
                raise NoPoolStateAvailable(block=block)

            # Discard blocks until the last block is older than the target
            while self._state_cache[-1].block is None or self._state_cache[-1].block >= block:
                self._state_cache.pop()

            self._notify_subscribers(message=UniswapV2PoolStateUpdated(self.state))

    def simulate_add_liquidity(
        self,
        added_reserves_token0: int,
        added_reserves_token1: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        Simulate adding liquidity.
        """
        with self._state_lock:
            reserves_token0 = (
                override_state.reserves_token0 if override_state else self.reserves_token0
            )
            reserves_token1 = (
                override_state.reserves_token1 if override_state else self.reserves_token1
            )

            return UniswapV2PoolSimulationResult(
                amount0_delta=added_reserves_token0,
                amount1_delta=added_reserves_token1,
                initial_state=override_state or self.state,
                final_state=dataclasses.replace(
                    self.state,
                    reserves_token0=reserves_token0 + added_reserves_token0,
                    reserves_token1=reserves_token1 + added_reserves_token1,
                    block=self.update_block if override_state is not None else None,
                ),
            )

    def simulate_remove_liquidity(
        self,
        removed_reserves_token0: int,
        removed_reserves_token1: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        Simulate removing liquidity.
        """
        with self._state_lock:
            reserves_token0 = (
                override_state.reserves_token0 if override_state else self.reserves_token0
            )
            reserves_token1 = (
                override_state.reserves_token1 if override_state else self.reserves_token1
            )

            return UniswapV2PoolSimulationResult(
                amount0_delta=-removed_reserves_token0,
                amount1_delta=-removed_reserves_token1,
                initial_state=override_state or self.state,
                final_state=dataclasses.replace(
                    self.state,
                    reserves_token0=reserves_token0 - removed_reserves_token0,
                    reserves_token1=reserves_token1 - removed_reserves_token1,
                    block=self.update_block if override_state is not None else None,
                ),
            )

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        Simulate an exact input swap.
        """
        if token_in not in self.tokens:
            raise DegenbotValueError(message="token_in is unknown.")

        zero_for_one = token_in == self._token0
        token_out_quantity = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=token_in_quantity,
            override_state=override_state,
        )
        token0_delta = -token_out_quantity if zero_for_one is False else token_in_quantity
        token1_delta = -token_out_quantity if zero_for_one is True else token_in_quantity

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            initial_state=override_state or self.state,
            final_state=dataclasses.replace(
                self.state,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        if token_out not in self.tokens:
            raise DegenbotValueError(message="token_out is unknown.")

        zero_for_one = token_out == self._token1

        token_in_quantity = self.calculate_tokens_in_from_tokens_out(
            token_out=token_out,
            token_out_quantity=token_out_quantity,
            override_state=override_state,
        )
        token0_delta = token_in_quantity if zero_for_one is True else -token_out_quantity
        token1_delta = token_in_quantity if zero_for_one is False else -token_out_quantity

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            initial_state=override_state or self.state,
            final_state=dataclasses.replace(
                self.state,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: UniswapV2PoolState | None = None,
    ) -> SimulationResult:
        if token_in == self._token0.address:
            token_in_obj = self._token0
        elif token_in == self._token1.address:
            token_in_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        result = self.simulate_exact_input_swap(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=state_override,
        )
        zero_for_one = token_in_obj == self._token0
        amount_out = -result.amount1_delta if zero_for_one else -result.amount0_delta
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=result.initial_state,
            final_state=result.final_state,
        )

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: UniswapV2PoolState | None = None,
    ) -> SimulationResult:
        if token_out == self._token0.address:
            token_out_obj = self._token0
        elif token_out == self._token1.address:
            token_out_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        result = self.simulate_exact_output_swap(
            token_out=token_out_obj,
            token_out_quantity=amount_out,
            override_state=state_override,
        )
        zero_for_one = token_out_obj == self._token1
        amount_in = result.amount0_delta if zero_for_one else result.amount1_delta
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=result.initial_state,
            final_state=result.final_state,
        )

    def get_arbitrage_helpers(self) -> tuple[AbstractArbitrage, ...]:
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        )

    def reserves_for_cache(self) -> tuple[int, int]:
        """Return (reserve_token0, reserve_token1) for the Rust solver cache."""
        return (self.state.reserves_token0, self.state.reserves_token1)

    def fee_for_cache(self) -> Fraction:
        """Return the forward-direction fee for the Rust solver cache."""
        return self._fee_token0

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV2PoolState | None = None,
    ) -> HopType:
        state = state_override or self.state
        fee = self.extract_fee(zero_for_one=zero_for_one)
        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )
