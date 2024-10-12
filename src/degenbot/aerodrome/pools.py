from fractions import Fraction
from threading import Lock
from typing import Any, Literal, cast

import eth_abi.abi
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3
from web3.types import BlockIdentifier

from ..config import web3_connection_manager
from ..erc20_token import Erc20Token
from ..exceptions import (
    AddressMismatch,
    DegenbotValueError,
    ExternalUpdateError,
    LateUpdateError,
    ZeroSwapError,
)
from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..logging import logger
from ..managers.erc20_token_manager import Erc20TokenManager
from ..registry.all_pools import pool_registry
from ..solidly.solidly_functions import general_calc_exact_in_volatile
from ..types import AbstractLiquidityPool
from ..uniswap.v3_liquidity_pool import UniswapV3Pool
from .functions import (
    calc_exact_in_stable,
    generate_aerodrome_v2_pool_address,
    generate_aerodrome_v3_pool_address,
)
from .types import AerodromeV2PoolExternalUpdate, AerodromeV2PoolStateUpdated


class AerodromeV2Pool(AbstractLiquidityPool):
    from .types import AerodromeV2PoolState as PoolState

    FEE_DENOMINATOR = 10_000

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        chain_id: int | None = None,
        deployer_address: str | None = None,
        state_block: int | None = None,
        archive_states: bool = True,
        verify_address: bool = True,
        silent: bool = False,
    ) -> None:
        self.address = to_checksum_address(address)

        self._chain_id = (
            chain_id if chain_id is not None else web3_connection_manager.default_chain_id
        )
        w3 = web3_connection_manager.get_web3(self.chain_id)
        self._update_block = state_block if state_block is not None else w3.eth.block_number

        self.factory, (token0, token1), self.stable, fee, (reserves0, reserves1) = (
            self.get_factory_tokens_stable_reserves_batched(w3=w3, state_block=self._update_block)
        )
        self.deployer_address = (
            to_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )

        self._state_lock = Lock()
        self._state = self.PoolState(
            pool=self.address,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )

        self.fee = Fraction(fee, self.FEE_DENOMINATOR)

        token_manager = Erc20TokenManager(chain_id=self.chain_id)
        self.token0, self.token1 = (
            token_manager.get_erc20token(
                address=token0,
                silent=silent,
            ),
            token_manager.get_erc20token(
                address=token1,
                silent=silent,
            ),
        )

        if verify_address and self.address != self._verified_address():  # pragma: no cover
            raise AddressMismatch("Pool address verification failed.")

        self.name = f"{self.token0}-{self.token1} (AerodromeV2, {100*self.fee.numerator/self.fee.denominator:.2f}%)"  # noqa:E501

        self._pool_state_archive = {self.update_block: self.state} if archive_states else None

        pool_registry.add(pool_address=self.address, chain_id=self.chain_id, pool=self)

        self._subscribers = set()

        if not silent:  # pragma: no cover
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}")
            logger.info(f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}")

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that either cannot be pickled or are unnecessary to perform the calculation
        copied_attributes = ()
        dropped_attributes = (
            "_state_lock",
            "_subscribers",
            "_pool_state_archive",
        )

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def _verified_address(self) -> ChecksumAddress:
        # The implementation address is hard-coded into the contract
        implementation_address = to_checksum_address(
            web3_connection_manager.get_web3(self.chain_id).eth.get_code(self.address)[10:30]
        )

        return generate_aerodrome_v2_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            implementation_address=to_checksum_address(implementation_address),
            stable=self.stable,
        )

    def auto_update(
        self,
        block_number: int | None = None,
        silent: bool = True,
    ) -> bool:
        """
        Retrieves the current reserves from the pool, stores any that have changed, and returns a
        status boolean indicating whether any update was found.

        @dev this method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """
        with self._state_lock:
            if block_number is not None and block_number < self.update_block:
                raise LateUpdateError(
                    f"Current state recorded at block {self.update_block}, update requested for stale block {block_number}"  # noqa:E501
                )

            state_updated = False
            w3 = self.w3
            block_number = w3.eth.get_block_number() if block_number is None else block_number

            reserves0, reserves1 = self.get_reserves(w3=w3, block_identifier=block_number)

            if (self.reserves_token0, self.reserves_token1) != (reserves0, reserves1):
                state_updated = True
                self.reserves_token0 = reserves0
                self.reserves_token1 = reserves1

            self._update_block = block_number

            if state_updated:
                if self._pool_state_archive is not None:  # pragma: no cover
                    self._pool_state_archive[block_number] = self.state

                self._notify_subscribers(
                    message=AerodromeV2PoolStateUpdated(self.state),
                )

                if not silent:  # pragma: no cover
                    logger.info(f"[{self.name}]")
                    logger.info(f"{self.token0}: {self.reserves_token0}")
                    logger.info(f"{self.token1}: {self.reserves_token1}")

            return state_updated

    def external_update(
        self,
        update: AerodromeV2PoolExternalUpdate,
    ) -> bool:
        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                f"Rejected update for block {update.block_number} in the past, "
                f"current update block is {self.update_block}"
            )

        with self._state_lock:
            updated_state = False

            if update.reserves_token0 != self.reserves_token0:
                updated_state = True
                self.reserves_token0 = update.reserves_token0
                logger.debug(f"Token 0 Reserves: {self.reserves_token0}")

            if update.reserves_token1 != self.reserves_token1:
                updated_state = True
                self.reserves_token1 = update.reserves_token1
                logger.debug(f"Token 1 Reserves: {self.reserves_token1}")

            if updated_state:
                if self._pool_state_archive is not None:  # pragma: no branch
                    self._pool_state_archive[update.block_number] = self.state
                self._notify_subscribers(
                    message=AerodromeV2PoolStateUpdated(self.state),
                )
                self._update_block = update.block_number

            return updated_state

    def get_factory_tokens_stable_reserves_batched(
        self,
        w3: Web3,
        state_block: int,
    ) -> tuple[
        ChecksumAddress,  # factory
        tuple[ChecksumAddress, ChecksumAddress],  # tokens
        bool,  # stable
        int,  # fee
        tuple[int, int],  # reserves
    ]:
        with w3.batch_requests() as batch:
            batch.add_mapping(
                {
                    # These calls default to use 'latest' for block number, which is OK since the
                    # values are immutable
                    w3.eth.call: [
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
                    ],
                }
            )
            batch.add(
                # This call uses a specific block so the reserve values are consistent
                w3.eth.call(
                    transaction={
                        "to": self.address,
                        "data": encode_function_calldata(
                            function_prototype="getReserves()",
                            function_arguments=None,
                        ),
                    },
                    block_identifier=state_block,
                )
            )

            factory, token0, token1, stable, reserves = batch.execute()

        factory, *_ = eth_abi.abi.decode(types=["address"], data=cast(HexBytes, factory))
        token0, *_ = eth_abi.abi.decode(types=["address"], data=cast(HexBytes, token0))
        token1, *_ = eth_abi.abi.decode(types=["address"], data=cast(HexBytes, token1))
        stable, *_ = eth_abi.abi.decode(types=["bool"], data=cast(HexBytes, stable))
        reserves0, reserves1, *_ = eth_abi.abi.decode(
            types=["uint112", "uint112"], data=cast(HexBytes, reserves)
        )

        fee, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction={
                    "to": to_checksum_address(cast(str, factory)),
                    "data": encode_function_calldata(
                        function_prototype="getFee(address,bool)",
                        function_arguments=[self.address, stable],
                    ),
                }
            ),
        )

        return (
            to_checksum_address(cast(str, factory)),
            (to_checksum_address(cast(str, token0)), to_checksum_address(cast(str, token1))),
            cast(bool, stable),
            cast(int, fee),
            (cast(int, reserves0), cast(int, reserves1)),
        )

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def reserves_token0(self) -> int:
        return self.state.reserves_token0

    @reserves_token0.setter
    def reserves_token0(self, new_reserves: int) -> None:
        current_state = self.state
        self._state = self.PoolState(
            pool=current_state.pool,
            reserves_token0=new_reserves,
            reserves_token1=current_state.reserves_token1,
        )

    @property
    def reserves_token1(self) -> int:
        return self.state.reserves_token1

    @reserves_token1.setter
    def reserves_token1(self, new_reserves: int) -> None:
        current_state = self.state
        self._state = self.PoolState(
            pool=current_state.pool,
            reserves_token0=current_state.reserves_token0,
            reserves_token1=new_reserves,
        )

    @property
    def state(self) -> PoolState:
        return self._state

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self.token0, self.token1

    @property
    def update_block(self) -> int:
        return self._update_block

    @property
    def w3(self) -> Web3:
        return web3_connection_manager.get_web3(self.chain_id)

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
            raise DegenbotValueError("token_in not recognized.")

        TOKEN_IN: Literal[0, 1] = 0 if token_in == self.token0 else 1

        if token_in_quantity <= 0:  # pragma: no cover
            raise ZeroSwapError("token_in_quantity must be positive")

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        reserves_0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        if self.stable:
            return calc_exact_in_stable(
                amount_in=token_in_quantity,
                token_in=TOKEN_IN,
                reserves0=reserves_0,
                reserves1=reserves_1,
                decimals0=10**self.token0.decimals,
                decimals1=10**self.token1.decimals,
                fee=self.fee,
            )
        else:
            return general_calc_exact_in_volatile(
                amount_in=token_in_quantity,
                token_in=TOKEN_IN,
                reserves0=reserves_0,
                reserves1=reserves_1,
                fee=self.fee,
            )

    def get_reserves(
        self, w3: Web3, block_identifier: BlockIdentifier | None = None
    ) -> tuple[int, int]:
        reserves_token0, reserves_token1, *_ = raw_call(
            w3=w3,
            address=self.address,
            block_identifier=get_number_for_block_identifier(block_identifier, w3),
            calldata=encode_function_calldata(
                function_prototype="getReserves()",
                function_arguments=None,
            ),
            return_types=["uint256", "uint256"],
        )

        return cast(int, reserves_token0), cast(int, reserves_token1)


class AerodromeV3Pool(UniswapV3Pool):
    from .types import AerodromeV3PoolState as PoolState

    TICK_STRUCT_TYPES = [
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
    ]
    SLOT0_STRUCT_TYPES = [
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "bool",
    ]

    def _verified_address(self) -> ChecksumAddress:
        # The implementation address is hard-coded into the contract
        implementation_address = to_checksum_address(
            web3_connection_manager.get_web3(self.chain_id).eth.get_code(self.address)[10:30]
        )

        return generate_aerodrome_v3_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            implementation_address=to_checksum_address(implementation_address),
            tick_spacing=self.tick_spacing,
        )
