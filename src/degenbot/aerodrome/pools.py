from __future__ import annotations

import dataclasses
from collections import deque
from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar, Literal, cast
from weakref import WeakSet

import eth_abi.abi

from degenbot.aerodrome.functions import (
    calc_exact_in_stable,
)
from degenbot.aerodrome.types import (
    AerodromeV2PoolExternalUpdate,
    AerodromeV2PoolState,
    AerodromeV2PoolStateUpdated,
    AerodromeV3PoolState,
)
from degenbot.aerodrome.v2_pool_calc import AerodromeV2PoolCalc
from degenbot.aerodrome.v2_pool_state import AerodromeV2PoolState as AerodromeV2PoolStateMixin
from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    ExternalUpdateError,
    NoPoolStateAvailable,
)
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from fractions import Fraction

    from eth_typing import ChecksumAddress

    from degenbot.erc20 import Erc20Token
    from degenbot.provider.interface import ProviderAdapter
    from degenbot.types.aliases import BlockNumber, ChainId
    from degenbot.uniswap.types import UniswapPoolSwapVector


class AerodromeV2Pool(
    PublisherMixin,
    PoolPickleMixin,
    AerodromeV2PoolStateMixin,
    AerodromeV2PoolCalc,
    AbstractLiquidityPool,
):
    variant: ClassVar[str | None] = "aerodrome"

    type PoolState = AerodromeV2PoolState

    _state: PoolState
    _state_cache: deque[PoolState]

    FEE_DENOMINATOR = 10_000

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
        token0: Erc20Token,
        token1: Erc20Token,
        factory: str,
        fee: Fraction,
        stable: bool,
        reserves_token0: int,
        reserves_token1: int,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        state_block: BlockNumber | None = None,
        state_cache_depth: int = 8,
    ) -> None:
        self.address = get_checksum_address(address)

        self._chain_id = chain_id if chain_id is not None else token0.chain_id
        state_block = state_block if state_block is not None else 0
        self._initial_state_block = state_block

        self.factory = get_checksum_address(factory)
        self.deployer_address = (
            get_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )
        self._stable = stable
        self._fee = fee
        self._token0 = token0
        self._token1 = token1

        # Wire calculation strategy at construction — no runtime if self._stable dispatch
        self._wire_stable_calculations(stable=stable)

        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, {100 * self._fee.numerator / self._fee.denominator:.2f}%)"  # noqa:E501

        self._state_lock = Lock()

        initial_state = self.PoolState.__value__(
            address=self.address,
            reserves_token0=reserves_token0,
            reserves_token1=reserves_token1,
            block=state_block,
        )

        self._state_cache = deque(maxlen=max(1, state_cache_depth))
        self._state_cache.append(initial_state)

        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1}, stable={self._stable})"  # noqa:E501

    @property
    def chain_id(self) -> int | None:
        return self._chain_id

    @property
    def reserves_token0(self) -> int:
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        return self.state.reserves_token1

    @property
    def state(self) -> PoolState:
        return self._state_cache[-1]

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

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
        update: AerodromeV2PoolExternalUpdate,
    ) -> None:
        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}"  # noqa:E501
            )

        with self._state_lock:
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
                message=AerodromeV2PoolStateUpdated(self.state),
            )

    def get_pool_identity_values(
        self,
        provider: ProviderAdapter,
        state_block: BlockNumber,
    ) -> tuple[
        ChecksumAddress,  # factory
        tuple[ChecksumAddress, ChecksumAddress],  # tokens
        bool,  # stable
        int,  # fee
        tuple[int, int],  # reserves
    ]:
        immutable_calls = [
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="factory()",
                    function_arguments=None,
                ),
            },
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="token0()",
                    function_arguments=None,
                ),
            },
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="token1()",
                    function_arguments=None,
                ),
            },
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="stable()",
                    function_arguments=None,
                ),
            },
        ]
        factory_data, token0_data, token1_data, stable_data = provider.batch_call(immutable_calls)  # ty:ignore[invalid-argument-type]

        # This call uses a specific block so the reserve values are consistent
        reserves_data = provider.call_raw(
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype="getReserves()",
                    function_arguments=None,
                ),
            },
            block=state_block,
        )

        (factory,) = eth_abi.abi.decode(types=["address"], data=factory_data)
        (token0,) = eth_abi.abi.decode(types=["address"], data=token0_data)
        (token1,) = eth_abi.abi.decode(types=["address"], data=token1_data)
        (stable,) = eth_abi.abi.decode(types=["bool"], data=stable_data)
        reserves0, reserves1, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"], data=reserves_data
        )

        factory_checksum = get_checksum_address(cast("str", factory))
        (fee,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call_raw({
                "to": factory_checksum,
                "data": encode_function_calldata(
                    function_prototype="getFee(address,bool)",
                    function_arguments=[self.address, stable],
                ),
            }),
        )

        return (
            factory_checksum,
            (get_checksum_address(cast("str", token0)), get_checksum_address(cast("str", token1))),
            cast("bool", stable),
            cast("int", fee),
            (cast("int", reserves0), cast("int", reserves1)),
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

            # Discard older states until the earliest block is crossed
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

        Use this method to maintain consistent state data following a chain re-organization.

        The pool will notify all subscribers of the new state with a `UniswapV3PoolStateUpdated`
        event.
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

        self._notify_subscribers(message=AerodromeV2PoolStateUpdated(self.state))

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        aero_state: AerodromeV2PoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, AerodromeV2PoolState):
                msg = f"Expected AerodromeV2PoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            aero_state = state_override

        if token_in == self._token0.address:
            token_in_obj = self._token0
            expected_token_out = self._token1.address
        elif token_in == self._token1.address:
            token_in_obj = self._token1
            expected_token_out = self._token0.address
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        if token_out != expected_token_out:
            msg = f"token_out {token_out} does not match expected {expected_token_out}"
            raise DegenbotValueError(message=msg)

        initial_state = aero_state or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=aero_state,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: AerodromeV2PoolState | None = None,
    ) -> SimulationResult:
        if token_out == self._token0.address:
            token_out_obj = self._token0
            expected_token_in = self._token1.address
        elif token_out == self._token1.address:
            token_out_obj = self._token1
            expected_token_in = self._token0.address
        else:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        if token_in != expected_token_in:
            msg = f"token_in {token_in} does not match expected {expected_token_in}"
            raise DegenbotValueError(message=msg)

        initial_state = state_override or self.state
        amount_in = self.calculate_tokens_in_from_tokens_out(
            token_out=token_out_obj,
            token_out_quantity=amount_out,
            override_state=state_override,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def reserves_for_cache(self) -> tuple[int, int]:
        """Return (reserve_token0, reserve_token1) for the Rust solver cache."""
        return (self.state.reserves_token0, self.state.reserves_token1)

    def fee_for_cache(self) -> Fraction:
        """Return the pool fee for the Rust solver cache."""
        return self._fee

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AerodromeV2PoolState | None = None,
    ) -> HopType:
        state = state_override or self.state
        fee = self.extract_fee(zero_for_one=zero_for_one)

        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
            decimals_in = self._token0.decimals
            decimals_out = self._token1.decimals
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
            decimals_in = self._token1.decimals
            decimals_out = self._token0.decimals

        if self._stable_calc_mode:
            reserves0 = state.reserves_token0
            reserves1 = state.reserves_token1
            decimals0 = 10**self._token0.decimals
            decimals1 = 10**self._token1.decimals
            token_in: Literal[0, 1] = 0 if zero_for_one else 1

            def _stable_swap_fn(
                amount_in: int,
                /,
                _reserves0: int = reserves0,
                _reserves1: int = reserves1,
                _decimals0: int = decimals0,
                _decimals1: int = decimals1,
                _fee: Fraction = fee,
                _token_in: Literal[0, 1] = token_in,
            ) -> int:
                return calc_exact_in_stable(
                    amount_in=amount_in,
                    token_in=_token_in,
                    reserves0=_reserves0,
                    reserves1=_reserves1,
                    decimals0=_decimals0,
                    decimals1=_decimals1,
                    fee=_fee,
                )

            return SolidlyStableHop(
                reserve_in=reserve_in,
                reserve_out=reserve_out,
                fee=fee,
                decimals_in=decimals_in,
                decimals_out=decimals_out,
                swap_fn=_stable_swap_fn,
            )

        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> UniswapV2PoolSwapAmounts:
        return UniswapV2PoolSwapAmounts(
            pool=self.address,
            amounts_in=(amount_in, 0) if zero_for_one else (0, amount_in),
            amounts_out=(0, amount_out) if zero_for_one else (amount_out, 0),
        )


class AerodromeV3Pool(UniswapV3Pool):
    variant: ClassVar[str | None] = "aerodrome"

    type PoolState = AerodromeV3PoolState

    TICK_STRUCT_TYPES = (
        "uint128",
        "int128",
        "int128",
        "uint256",
        "uint256",
        "uint256",
        "int56",
        "uint160",
        "uint32",
        "bool",
    )

    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "bool",
    )
