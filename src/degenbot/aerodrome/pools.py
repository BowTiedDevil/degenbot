# ruff: noqa: PLR0904

from __future__ import annotations

import dataclasses
from collections import deque
from fractions import Fraction
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
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.liquidity_pool import (
    ExternalUpdateError,
    InvalidSwapInputAmount,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.functions import encode_function_calldata
from degenbot.logging import logger
from degenbot.solidly.solidly_functions import general_calc_exact_in_volatile
from degenbot.types.abstract import AbstractAerodromeV2Pool
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.v2_functions import constant_product_calc_exact_out
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from hexbytes import HexBytes

    from degenbot.erc20 import Erc20Token
    from degenbot.provider.interface import ProviderAdapter
    from degenbot.types.aliases import BlockNumber, ChainId
    from degenbot.uniswap.types import UniswapPoolSwapVector


class AerodromeV2Pool(PublisherMixin, PoolPickleMixin, AbstractAerodromeV2Pool):
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
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def token0(self) -> Erc20Token:
        return self._token0

    @property
    def token1(self) -> Erc20Token:
        return self._token1

    @property
    def fee(self) -> Fraction:
        return self._fee

    @property
    def fee_token0(self) -> Fraction:
        """Return the fee for token0 → token1 swaps (same as fee_token1 for Aerodrome)."""
        return self._fee

    @property
    def fee_token1(self) -> Fraction:
        """Return the fee for token1 → token0 swaps (same as fee_token0 for Aerodrome)."""
        return self._fee

    @property
    def stable(self) -> bool:
        return self._stable

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
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self._token0, self._token1

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

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: PoolState | None = None,
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool
        reserves.

        Accepts a `PoolState` state override for calculation against an arbitrary state
        in lieu of the recorded state.
        """

        if token_out_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_out == self._token1:
            reserves_in = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )
            reserves_out = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )

        elif token_out == self._token0:
            reserves_in = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )
            reserves_out = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )

        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_out: {token_out}! This pool holds: {self._token0} {self._token1}"  # noqa:E501
            )

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"  # noqa:E501
            )

        if self._stable:
            raise NotImplementedError

        return constant_product_calc_exact_out(
            amount_out=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=self._fee,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        """

        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not recognized.")

        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        reserves_0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        if self._stable:
            return calc_exact_in_stable(
                amount_in=token_in_quantity,
                token_in=0 if token_in == self._token0 else 1,
                reserves0=reserves_0,
                reserves1=reserves_1,
                decimals0=10**self._token0.decimals,
                decimals1=10**self._token1.decimals,
                fee=self._fee,
            )
        return general_calc_exact_in_volatile(
            amount_in=token_in_quantity,
            token_in=0 if token_in == self._token0 else 1,
            reserves0=reserves_0,
            reserves1=reserves_1,
            fee=self._fee,
        )

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

    def get_absolute_price(
        self, token: Erc20Token, override_state: PoolState | None = None
    ) -> Fraction:
        """
        Get the absolute price for the given token, expressed in units of the other.
        """

        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute exchange rate for the given token, expressed in terms of a unit amount of
        its paired token.

        e.g. taking the USDC-WETH pool in https://blog.uniswap.org/uniswap-v3-math-primer — the
        WETH/USDC exchange rate is 649004842.70137. Rounding down, this signifies that the smallest
        swap (1 USDC) results in a 649004842 WETH output.

        The exchange rate for a V2 pool is a simple ratio of the output token reserves to the input
        token reserves.
        """

        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            Fraction(state.reserves_token1, state.reserves_token0)
            if token == self._token1
            else Fraction(state.reserves_token0, state.reserves_token1)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal price for the given token, expressed per nominal unit of its paired token.
        The price is corrected for the decimal place values of both tokens.
        """

        return 1 / self.get_nominal_exchange_rate(token=token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal rate for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self._token1.decimals, 10**self._token0.decimals)
            if token == self._token0
            else Fraction(10**self._token0.decimals, 10**self._token1.decimals)
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
        factory_data, token0_data, token1_data, stable_data = provider.batch_call(immutable_calls)

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

        (factory,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", factory_data))
        (token0,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token0_data))
        (token1,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token1_data))
        (stable,) = eth_abi.abi.decode(types=["bool"], data=cast("HexBytes", stable_data))
        reserves0, reserves1, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"], data=cast("HexBytes", reserves_data)
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
        state_override: AerodromeV2PoolState | None = None,
    ) -> SimulationResult:
        if token_in == self._token0.address:
            token_in_obj = self._token0
        elif token_in == self._token1.address:
            token_in_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        initial_state = state_override or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=state_override,
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
        elif token_out == self._token1.address:
            token_out_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

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

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        return self._fee

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

        if self._stable:
            reserves0 = state.reserves_token0
            reserves1 = state.reserves_token1
            decimals0 = 10**self._token0.decimals
            decimals1 = 10**self._token1.decimals
            token_in: Literal[0, 1] = 0 if zero_for_one else 1

            def _stable_swap_fn(
                amount_in: int,
                __reserves0: int = reserves0,
                __reserves1: int = reserves1,
                __decimals0: int = decimals0,
                __decimals1: int = decimals1,
                __fee: Fraction = fee,
                __token_in: Literal[0, 1] = token_in,
            ) -> int:
                return calc_exact_in_stable(
                    amount_in=amount_in,
                    token_in=__token_in,
                    reserves0=__reserves0,
                    reserves1=__reserves1,
                    decimals0=__decimals0,
                    decimals1=__decimals1,
                    fee=__fee,
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
    )  # type:ignore[assignment]

    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "bool",
    )  # type:ignore[assignment]
