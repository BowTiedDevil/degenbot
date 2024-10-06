from bisect import bisect_left
from collections.abc import Iterable
from fractions import Fraction
from threading import Lock
from typing import Any, cast

import eth_abi.abi
from eth_typing import BlockIdentifier, ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from typing_extensions import Self
from web3 import Web3

from .. import config
from ..constants import ZERO_ADDRESS
from ..erc20_token import Erc20Token
from ..exceptions import (
    AddressMismatch,
    DegenbotValueError,
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..logging import logger
from ..managers.erc20_token_manager import Erc20TokenManager
from ..registry.all_pools import AllPools
from ..types import AbstractLiquidityPool
from ..uniswap.deployments import FACTORY_DEPLOYMENTS, UniswapV2ExchangeDeployment
from .types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)
from .v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
    generate_v2_pool_address,
)


class UniswapV2Pool(AbstractLiquidityPool):
    """
    A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant.
    """

    UNISWAP_V2_MAINNET_POOL_INIT_HASH = (
        "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
    )

    @classmethod
    def from_exchange(
        cls,
        address: str,
        exchange: UniswapV2ExchangeDeployment,
        **kwargs: Any,
    ) -> Self:
        """
        Create a new `UniswapV2Pool` with exchange information taken from the provided deployment.
        """

        return cls(
            address=address,
            deployer_address=exchange.factory.deployer,
            init_hash=exchange.factory.pool_init_hash,
            **kwargs,
        )

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        fee: Fraction | Iterable[Fraction] = Fraction(3, 1000),
        state_block: int | None = None,
        archive_states: bool = True,
        verify_address: bool = True,
        silent: bool = False,
    ) -> None:
        """
        An abstract representation of an x*y=k invariant automatic matchmaker, based on Uniswap V2.

        Arguments
        ---------
        address : str
            Address for the deployed pool contract.
        deployer_address : str, optional
            The address for the deployment contract.
        init_hash : str, optional
            The init hash for the factory contract. If one is not provided, the deployments in
            `degenbot.exchanges` will be searched first. If no matching deployment is found, the
            default Uniswap V2 hash will be used.
        fee : Fraction | Iterable[Fraction, Fraction]
            The swap fee imposed by the pool. Defaults to `Fraction(3,1000)` which is equivalent
            to 0.3%. For split-fee pools of unequal value, provide an iterable with `Fraction`
            fees ordered by token position.
        state_block : int, optional
            Fetch initial state values from the chain at a particular block height. Defaults to
            the latest block if omitted.
        verify_address: bool
            Control if the pool address is verified against the deterministic CREATE2 address.
            The deployer address, token addresses, and pool init code hash must be known.
        silent : bool
            Suppress status output.
        """

        if address == ZERO_ADDRESS:
            raise LiquidityPoolError("Invalid pool address")

        self.address = to_checksum_address(address)

        w3 = config.get_web3()
        chain_id = w3.eth.chain_id
        self._update_block = state_block if state_block is not None else w3.eth.block_number

        self.factory, (token0, token1), (reserves0, reserves1) = (
            self.get_factory_tokens_reserves_batched(w3=w3, state_block=self._update_block)
        )

        token_manager = Erc20TokenManager(chain_id)
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

        self._state_lock = Lock()
        self._state = UniswapV2PoolState(
            pool=self.address,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )

        self.deployer = (
            to_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )

        try:
            # Use degenbot deployment values if available
            factory_deployment = FACTORY_DEPLOYMENTS[chain_id][self.factory]
            self.init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                self.deployer = factory_deployment.deployer
        except KeyError:
            # Deployment is unknown. Uses any inputs provided, otherwise use default values from
            # original Uniswap contracts
            self.init_hash = (
                init_hash if init_hash is not None else self.UNISWAP_V2_MAINNET_POOL_INIT_HASH
            )

        if isinstance(fee, Iterable):
            self.fee_token0, self.fee_token1 = fee
        else:
            self.fee_token0 = self.fee_token1 = fee

        if verify_address and self.address != self._verified_address():  # pragma: no branch
            raise AddressMismatch("Pool address verification failed.")

        fee_string = (
            f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}"
            if self.fee_token0 == self.fee_token1
            else (
                f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}/{100*self.fee_token1.numerator/self.fee_token1.denominator:.2f}"  # noqa:E501
            )
        )
        self.name = f"{self.token0}-{self.token1} (V2, {fee_string}%)"

        self._pool_state_archive = {self.update_block: self.state} if archive_states else None

        AllPools(chain_id)[self.address] = self

        self._subscribers = set()

        if not silent:  # pragma: no cover
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}")
            logger.info(f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}")

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that either cannot be pickled or are unnecessary to perform the calculation
        copied_attributes = ()
        dropped_attributes = (
            "_contract",
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

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self.token0}, token1={self.token1})"  # noqa:E501

    def _verified_address(self) -> ChecksumAddress:
        return generate_v2_pool_address(
            deployer_address=self.deployer,
            token_addresses=(self.token0.address, self.token1.address),
            init_hash=self.init_hash,
        )

    def get_factory_tokens_reserves_batched(
        self,
        w3: Web3,
        state_block: int,
    ) -> tuple[
        ChecksumAddress,  # factory
        tuple[ChecksumAddress, ChecksumAddress],  # tokens
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

            factory, token0, token1, reserves = batch.execute()

        factory, *_ = eth_abi.abi.decode(types=["address"], data=cast(HexBytes, factory))
        token0, *_ = eth_abi.abi.decode(types=["address"], data=cast(HexBytes, token0))
        token1, *_ = eth_abi.abi.decode(types=["address"], data=cast(HexBytes, token1))
        reserves0, reserves1, *_ = eth_abi.abi.decode(
            types=["uint112", "uint112"], data=cast(HexBytes, reserves)
        )

        return (
            to_checksum_address(cast(str, factory)),
            (to_checksum_address(cast(str, token0)), to_checksum_address(cast(str, token1))),
            (cast(int, reserves0), cast(int, reserves1)),
        )

    @property
    def update_block(self) -> int:
        return self._update_block

    @property
    def reserves_token0(self) -> int:
        return self.state.reserves_token0

    @reserves_token0.setter
    def reserves_token0(self, new_reserves: int) -> None:
        current_state = self.state
        self._state = UniswapV2PoolState(
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
        self._state = UniswapV2PoolState(
            pool=current_state.pool,
            reserves_token0=current_state.reserves_token0,
            reserves_token1=new_reserves,
        )

    @property
    def state(self) -> UniswapV2PoolState:
        return self._state

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self.token0, self.token1

    def calculate_tokens_in_from_ratio_out(
        self,
        token_in: Erc20Token,
        ratio_absolute: Fraction,
    ) -> int:
        """
        Calculates the maximum token input for the target output ratio after
        fees, defined as (quantity out / quantity in), at current pool
        reserves. The ratio must be passed as an absolute value reflecting the
        decimal amounts specified by the ERC-20 token contract
        (e.g. 10 * 10 ** (18-8) ETH/BTC).
        """

        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(f"Token in {token_in} not held by this pool.")

        if token_in == self.token0:
            # formula: dx = y0/C - x0/(1-FEE), where C = token1/token0
            return max(
                0,
                int(
                    self.reserves_token1 / ratio_absolute
                    - self.reserves_token0 / (1 - self.fee_token0)
                ),
            )
        else:
            # formula: dy = x0/C - y0/(1-FEE), where C = token0/token1
            return max(
                0,
                int(
                    self.reserves_token0 / ratio_absolute
                    - self.reserves_token1 / (1 - self.fee_token1)
                ),
            )

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool
        reserves.

        Accepts a `UniswapV2PoolState` state override for calculation against an arbitrary state
        in lieu of the recorded state.
        """

        if token_out_quantity <= 0:  # pragma: no cover
            raise ZeroSwapError("token_out_quantity must be positive")

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_out == self.token1:
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
            fee = self.fee_token0
        elif token_out == self.token0:
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
            fee = self.fee_token1
        else:  # pragma: no cover
            raise DegenbotValueError(
                f"Could not identify token_out: {token_out}! This pool holds: {self.token0} {self.token1}"  # noqa:E501
            )

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"
            )

        return constant_product_calc_exact_out(
            amount_out=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        """

        if token_in_quantity <= 0:  # pragma: no cover
            raise ZeroSwapError("token_in_quantity must be positive")

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_in == self.token0:
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
            fee = self.fee_token0
        elif token_in == self.token1:
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
            fee = self.fee_token1
        else:  # pragma: no cover
            raise DegenbotValueError(
                f"Could not identify token_in: {token_in}! Pool holds: {self.token0} {self.token1}"
            )

        return constant_product_calc_exact_in(
            amount_in=token_in_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
        )

    def external_update(
        self,
        update: UniswapV2PoolExternalUpdate,
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
                    message=UniswapV2PoolStateUpdated(self.state),
                )
                self._update_block = update.block_number

            return updated_state

    def get_absolute_price(
        self, token: Erc20Token, override_state: UniswapV2PoolState | None = None
    ) -> Fraction:
        """
        Get the absolute price for the given token, expressed in units of the other.
        """

        return 1 / self.get_absolute_rate(token, override_state=override_state)

    def get_absolute_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute rate for the given token, expressed in units of the other.
        """

        state = self.state if override_state is None else override_state

        if token == self.token0:
            return Fraction(state.reserves_token0) / Fraction(state.reserves_token1)
        elif token == self.token1:
            return Fraction(state.reserves_token1) / Fraction(state.reserves_token0)
        else:  # pragma: no cover
            raise DegenbotValueError(f"Unknown token {token}")

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal price for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return 1 / self.get_nominal_rate(token, override_state=override_state)

    def get_nominal_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal rate for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        state = self.state if override_state is None else override_state

        if token == self.token0:
            return Fraction(state.reserves_token0, 10**self.token0.decimals) * Fraction(
                10**self.token1.decimals, state.reserves_token1
            )
        elif token == self.token1:
            return Fraction(state.reserves_token1, 10**self.token1.decimals) * Fraction(
                10**self.token0.decimals, state.reserves_token0
            )
        else:  # pragma: no cover
            raise DegenbotValueError(f"Unknown token {token}")

    def get_reserves(self, w3: Web3, block_identifier: BlockIdentifier) -> tuple[int, int]:
        reserves_token0, reserves_token1, *_ = raw_call(
            w3=w3,
            address=self.address,
            calldata=encode_function_calldata(
                function_prototype="getReserves()",
                function_arguments=None,
            ),
            return_types=["uint256", "uint256"],
            block_identifier=get_number_for_block_identifier(block_identifier),
        )
        return reserves_token0, reserves_token1

    def discard_states_before_block(self, block: int) -> None:
        """
        Discard states recorded prior to a target block.
        """

        if self._pool_state_archive is None:  # pragma: no cover
            raise NoPoolStateAvailable("No archived states are available")

        with self._state_lock:
            known_blocks = sorted(list(self._pool_state_archive.keys()))

            # Finds the index prior to the requested block number
            block_index = bisect_left(known_blocks, block)

            # The earliest known state already meets the criterion, so return early
            if block_index == 0:
                return

            if block_index == len(known_blocks):
                raise NoPoolStateAvailable(f"No pool state known prior to block {block}")

            for known_block in known_blocks[:block_index]:
                del self._pool_state_archive[known_block]

    def restore_state_before_block(
        self,
        block: int,
    ) -> None:
        """
        Restore the last pool state recorded prior to a target block.

        Use this method to maintain consistent state data following a chain re-organization.
        """

        if self._pool_state_archive is None:  # pragma: no cover
            raise NoPoolStateAvailable("No archived states are available")

        with self._state_lock:
            known_blocks = sorted(list(self._pool_state_archive.keys()))

            # Finds the index prior to the requested block number
            block_index = bisect_left(known_blocks, block)

            if block_index == 0:
                raise NoPoolStateAvailable(f"No pool state known prior to block {block}")

            # The last known state already meets the criterion, so return early
            if block_index == len(known_blocks):
                return

            # Remove states at and after the specified block
            for known_block in known_blocks[block_index:]:
                del self._pool_state_archive[known_block]

            # Restore previous state and block
            self._update_block, self._state = list(self._pool_state_archive.items())[-1]
            self._notify_subscribers(message=UniswapV2PoolStateUpdated(self.state))

    def simulate_add_liquidity(
        self,
        added_reserves_token0: int,
        added_reserves_token1: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        if override_state:
            logger.debug(f"State override: {override_state}")

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
                initial_state=override_state if override_state is not None else self.state.copy(),
                final_state=UniswapV2PoolState(
                    pool=self.address,
                    reserves_token0=reserves_token0 + added_reserves_token0,
                    reserves_token1=reserves_token1 + added_reserves_token1,
                ),
            )

    def simulate_remove_liquidity(
        self,
        removed_reserves_token0: int,
        removed_reserves_token1: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        if override_state:
            logger.debug(f"State override: {override_state}")

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
                initial_state=self.state.copy(),
                final_state=UniswapV2PoolState(
                    pool=self.address,
                    reserves_token0=reserves_token0 - removed_reserves_token0,
                    reserves_token1=reserves_token1 - removed_reserves_token1,
                ),
            )

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError("token_in is unknown.")

        if token_in_quantity == 0:  # pragma: no cover
            raise DegenbotValueError("Zero input swap requested.")

        if override_state:
            logger.debug(f"State override: {override_state}")

        current_state = override_state if override_state is not None else self.state.copy()
        zero_for_one = token_in == self.token0

        token_out_quantity = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=token_in_quantity,
            override_state=current_state,
        )
        token0_delta = -token_out_quantity if zero_for_one is False else token_in_quantity
        token1_delta = -token_out_quantity if zero_for_one is True else token_in_quantity

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            initial_state=current_state,
            final_state=UniswapV2PoolState(
                pool=self.address,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
            ),
        )

    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError("token_out is unknown.")

        if token_out_quantity == 0:  # pragma: no cover
            raise DegenbotValueError("Zero output swap requested.")

        if override_state:
            logger.debug(f"State override: {override_state}")

        current_state = override_state if override_state is not None else self.state.copy()
        zero_for_one = token_out == self.token1

        token_in_quantity = self.calculate_tokens_in_from_tokens_out(
            token_out=token_out,
            token_out_quantity=token_out_quantity,
            override_state=current_state,
        )
        token0_delta = token_in_quantity if zero_for_one is True else -token_out_quantity
        token1_delta = token_in_quantity if zero_for_one is False else -token_out_quantity

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            initial_state=current_state,
            final_state=UniswapV2PoolState(
                pool=self.address,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
            ),
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
                raise ExternalUpdateError(
                    f"Current state recorded at block {self.update_block}, received update for stale block {block_number}"  # noqa:E501
                )

            state_updated = False
            w3 = config.get_web3()
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
                    message=UniswapV2PoolStateUpdated(self.state),
                )

                if not silent:  # pragma: no cover
                    logger.info(f"[{self.name}]")
                    logger.info(f"{self.token0}: {self.reserves_token0}")
                    logger.info(f"{self.token1}: {self.reserves_token1}")

            return state_updated


class UnregisteredLiquidityPool(UniswapV2Pool):
    """
    A disconnected version of `UniswapV2Pool` for use where a pool helper is expected, but no
    chain data available to read the necessary values.

    The pool helper is not added to the pool registry and no reserve values are set.
    """

    def __init__(
        self,
        address: ChecksumAddress | str,
        tokens: list[Erc20Token],
        fee: Fraction | Iterable[Fraction] = Fraction(3, 1000),
    ) -> None:
        self.address = to_checksum_address(address)
        self._state_lock = Lock()
        self._state = UniswapV2PoolState(pool=self.address, reserves_token0=0, reserves_token1=0)
        self.token0 = min(tokens)
        self.token1 = max(tokens)

        if isinstance(fee, Iterable):
            self.fee_token0, self.fee_token1 = fee
        else:
            self.fee_token0 = self.fee_token1 = fee

        self._subscribers = set()
