from bisect import bisect_left
from collections.abc import Iterable
from fractions import Fraction
from threading import Lock
from typing import Any, Literal

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from typing_extensions import override
from web3.contract.contract import Contract

from degenbot.exchanges.uniswap.deployments import FACTORY_DEPLOYMENTS
from degenbot.exchanges.uniswap.types import UniswapV2ExchangeDeployment

from .. import config
from ..baseclasses import AbstractLiquidityPool
from ..erc20_token import Erc20Token
from ..exceptions import (
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from ..logging import logger
from ..manager.token_manager import Erc20TokenHelperManager
from ..registry.all_pools import AllPools
from .abi import CAMELOT_POOL_ABI, UNISWAP_V2_POOL_ABI
from .v2_dataclasses import (
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

UNISWAP_V2_MAINNET_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)
CAMELOT_ARBITRUM_POOL_INIT_HASH = (
    "0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"
)


class LiquidityPool(AbstractLiquidityPool):
    """
    A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant.
    """

    @classmethod
    def from_exchange(
        cls,
        address: str,
        exchange: UniswapV2ExchangeDeployment,
        **kwargs: Any,
    ) -> "LiquidityPool":
        """
        Create a new `LiquidityPool` with exchange information taken from the provided deployment.
        """

        return cls(
            address=address,
            factory_address=exchange.factory.address,
            deployer_address=exchange.factory.deployer,
            abi=exchange.factory.pool_abi,
            init_hash=exchange.factory.pool_init_hash,
            **kwargs,
        )

    def __init__(
        self,
        address: ChecksumAddress | str,
        tokens: list[Erc20Token] | None = None,
        name: str | None = None,
        update_method: Literal["polling", "external"] = "polling",
        abi: list[Any] | None = None,
        factory_address: str | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        fee: Fraction | Iterable[Fraction] = Fraction(3, 1000),
        silent: bool = False,
        state_block: int | None = None,
        archive_states: bool = True,
        verify_address: bool = True,
    ) -> None:
        """
        An abstract representation of an x*y=k invariant automatic matchmaker, based on Uniswap V2.

        Arguments
        ---------
        address : str
            Address for the deployed pool contract.
        tokens : List[Erc20Token], optional
            "Erc20Token" objects for the tokens held by the deployed pool.
        name : str, optional
            Name of the contract, e.g. "DAI-WETH".
        update_method : str
            A string that sets the method used to fetch updates to the pool. Can be "polling",
            which fetches updates from the chain object using the contract object, or "external"
            which relies on updates being provided from outside the object.
        abi : list, optional
            Contract ABI.
        factory_address : str, optional
            The address for the factory contract.
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
        silent : bool
            Suppress status output.
        state_block : int, optional
            Fetch initial state values from the chain at a particular block height. Defaults to
            the latest block if omitted.
        verify_address: bool
            Control if the pool address is verified against the deterministic CREATE2 address.
            The deployer address, token addresses, and pool init code hash must be known.
        """

        self.factory: ChecksumAddress
        self._state: UniswapV2PoolState | None = None
        self._state_lock = Lock()

        self.address: ChecksumAddress = to_checksum_address(address)
        self.abi = abi if abi is not None else UNISWAP_V2_POOL_ABI

        w3 = config.get_web3()
        w3_contract = self.w3_contract
        chain_id = w3.eth.chain_id

        if state_block is None:
            state_block = w3.eth.block_number
        self._update_block = state_block

        self.factory = (
            to_checksum_address(factory_address)
            if factory_address is not None
            else w3_contract.functions.factory().call()
        )
        self.deployer_address = (
            to_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )

        if init_hash is not None:
            self.init_hash = init_hash
        else:
            try:
                self.init_hash = FACTORY_DEPLOYMENTS[chain_id][self.factory].pool_init_hash
            except KeyError:
                self.init_hash = UNISWAP_V2_MAINNET_POOL_INIT_HASH

        if isinstance(fee, Iterable):
            self.fee_token0, self.fee_token1 = fee
        else:
            self.fee_token0 = self.fee_token1 = fee

        self._update_method = update_method

        if tokens is not None:
            self.token0, self.token1 = sorted(tokens)
        else:
            _token_manager = Erc20TokenHelperManager(chain_id)
            self.token0 = _token_manager.get_erc20token(
                address=w3_contract.functions.token0().call(),
                silent=silent,
            )
            self.token1 = _token_manager.get_erc20token(
                address=w3_contract.functions.token1().call(),
                silent=silent,
            )

        self.tokens = (self.token0, self.token1)

        if verify_address:
            verified_address = self._verified_address()
            if verified_address != self.address:
                raise ValueError(
                    f"Pool address verification failed. Provided: {self.address}, "
                    f"expected: {verified_address}"
                )

        if name is not None:
            self.name = name
        else:
            fee_string = (
                f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}"
                if self.fee_token0 == self.fee_token1
                else (
                    f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}"
                    "/"
                    f"{100*self.fee_token1.numerator/self.fee_token1.denominator:.2f}"
                )
            )
            self.name = f"{self.token0}-{self.token1} (V2, {fee_string}%)"

        self._pool_state_archive: dict[int, UniswapV2PoolState] = {}
        if archive_states:
            self._pool_state_archive[self._update_block] = self.state

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
        return (
            f"{self.__class__.__name__}(address={self.address}, "
            f"token0={self.token0}, token1={self.token1})"
        )

    def _verified_address(self) -> ChecksumAddress:
        return generate_v2_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            init_hash=self.init_hash,
        )

    @property
    def w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(address=self.address, abi=self.abi)

    @property
    def update_block(self) -> int:
        return self._update_block

    @property
    def reserves_token0(self) -> int:
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        return self.state.reserves_token1

    @property
    @override
    def state(self) -> UniswapV2PoolState:
        if self._state is None:
            current_block = config.get_web3().eth.get_block_number()
            logger.debug(f"Getting initial state for {self} at block {current_block}")
            reserves0, reserves1, *_ = self.w3_contract.functions.getReserves().call(
                block_identifier=current_block
            )
            self._update_block = current_block
            self._state = UniswapV2PoolState(
                pool=self.address,
                reserves_token0=reserves0,
                reserves_token1=reserves1,
            )
            self._pool_state_archive[current_block] = self._state
        return self._state

    @state.setter
    @override
    def state(self, new_state: UniswapV2PoolState) -> None:
        self._state = new_state

    def auto_update(
        self,
        block_number: int | None = None,
        silent: bool = True,
    ) -> tuple[bool, UniswapV2PoolState]:
        found_updates = self.update_reserves(
            silent=silent,
            print_reserves=not silent,
            update_block=block_number,
            override_update_method="polling",
        )
        return found_updates, self.state

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
            raise ValueError(f"Token in {token_in} not held by this pool.")

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
            raise ValueError(
                f"Could not identify token_out: {token_out}! This pool holds: "
                f"{self.token0} {self.token1}"
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
            raise ValueError(
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
        silent: bool = True,
    ) -> bool:
        return self.update_reserves(
            silent=silent,
            external_token0_reserves=update.reserves_token0,
            external_token1_reserves=update.reserves_token1,
            print_reserves=not silent,
            override_update_method="external",
        )

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
            raise ValueError(f"Unknown token {token}")

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
            raise ValueError(f"Unknown token {token}")

    def discard_states_before_block(self, block: int) -> None:
        """
        Discard states recorded prior to a target block.
        """

        if not self._pool_state_archive:
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

            for block in known_blocks[:block_index]:
                del self._pool_state_archive[block]

    def restore_state_before_block(
        self,
        block: int,
    ) -> None:
        """
        Restore the last pool state recorded prior to a target block.

        Use this method to maintain consistent state data following a chain re-organization.
        """

        if not self._pool_state_archive:
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
            for block in known_blocks[block_index:]:
                del self._pool_state_archive[block]

            # Restore previous state and block
            self._update_block, self.state = list(self._pool_state_archive.items())[-1]
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
            raise ValueError("token_in is unknown.")

        if token_in_quantity == 0:  # pragma: no cover
            raise ValueError("Zero input swap requested.")

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
            raise ValueError("token_out is unknown.")

        if token_out_quantity == 0:  # pragma: no cover
            raise ValueError("Zero output swap requested.")

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

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        external_token0_reserves: int | None = None,
        external_token1_reserves: int | None = None,
        override_update_method: str | None = None,
        update_block: int | None = None,
    ) -> bool:
        """
        Updates token reserves. If update method is set to "polling", uses Web3 to read current
        contract values. If set to "external", uses the provided reserves without verification.
        """

        _w3_contract = self.w3_contract

        update_method = override_update_method or self._update_method

        if update_block is None:
            update_block = config.get_web3().eth.get_block_number()

        # Discard stale updates, but allow updating the same pool multiple times per block
        # (necessary if sending sync events individually)
        if update_block < self._update_block:
            raise ExternalUpdateError(
                f"Current state recorded at block {self._update_block}, received update for stale "
                f"block {update_block}"
            )
        else:
            self._update_block = update_block

        state_updated = False
        if update_method == "polling":
            try:
                reserves0, reserves1, *_ = _w3_contract.functions.getReserves().call(
                    block_identifier=self._update_block
                )
                if (self.reserves_token0, self.reserves_token1) != (reserves0, reserves1):
                    self.state = UniswapV2PoolState(
                        pool=self.address,
                        reserves_token0=reserves0,
                        reserves_token1=reserves1,
                    )
                    # self.reserves_token0, self.reserves_token1 = reserves0, reserves1
                    self._pool_state_archive[update_block] = self.state

                    self._notify_subscribers(
                        message=UniswapV2PoolStateUpdated(self.state),
                    )
                    state_updated = True

                    if not silent:  # pragma: no cover
                        logger.info(f"[{self.name}]")
                        if print_reserves:
                            logger.info(f"{self.token0}: {self.reserves_token0}")
                            logger.info(f"{self.token1}: {self.reserves_token1}")
            except Exception as e:
                print(f"LiquidityPool: Exception in update_reserves (polling): {e}")
        elif update_method == "external":
            if not (external_token0_reserves is not None and external_token1_reserves is not None):
                raise ValueError(
                    "Called update_reserves without providing reserve values for both tokens!"
                )

            # Skip follow-up processing if no updated values were found
            if (
                external_token0_reserves == self.reserves_token0
                and external_token1_reserves == self.reserves_token1
            ):
                state_updated = False
            else:
                self.state = UniswapV2PoolState(
                    pool=self.address,
                    reserves_token0=external_token0_reserves,
                    reserves_token1=external_token1_reserves,
                )
                # self.reserves_token0 = external_token0_reserves
                # self.reserves_token1 = external_token1_reserves

                self._pool_state_archive[update_block] = self.state
                self._notify_subscribers(
                    message=UniswapV2PoolStateUpdated(self.state),
                )

            if not silent:  # pragma: no cover
                logger.info(f"[{self.name}]")
                if print_reserves:
                    logger.info(f"{self.token0}: {self.reserves_token0}")
                    logger.info(f"{self.token1}: {self.reserves_token1}")
        else:  # pragma: no cover
            raise ValueError(f"Update method {update_method} is not recognized.")

        return state_updated


class CamelotLiquidityPool(LiquidityPool):
    def __init__(
        self,
        address: str,
        tokens: list[Erc20Token] | None = None,
        name: str | None = None,
        update_method: Literal["polling", "external"] = "polling",
        abi: list[Any] | None = None,
        silent: bool = False,
    ) -> None:
        address = to_checksum_address(address)

        _w3 = config.get_web3()
        _w3_contract = config.get_web3().eth.contract(address=address, abi=abi or CAMELOT_POOL_ABI)

        state_block = _w3.eth.get_block_number()

        (
            _,
            _,
            fee_token0,
            fee_token1,
        ) = _w3_contract.functions.getReserves().call(block_identifier=state_block)
        self.fee_denominator = _w3_contract.functions.FEE_DENOMINATOR().call(
            block_identifier=state_block
        )
        fee_token0 = Fraction(fee_token0, self.fee_denominator)
        fee_token1 = Fraction(fee_token1, self.fee_denominator)

        super().__init__(
            address=address,
            tokens=tokens,
            name=name,
            update_method=update_method,
            abi=abi or CAMELOT_POOL_ABI,
            init_hash=CAMELOT_ARBITRUM_POOL_INIT_HASH,
            fee=(fee_token0, fee_token1),
            silent=silent,
            state_block=state_block,
        )

        self.stable_swap: bool = _w3_contract.functions.stableSwap().call()

    def _calculate_tokens_out_from_tokens_in_stable_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        if override_state is not None:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_in_quantity <= 0:  # pragma: no cover
            raise ZeroSwapError("token_in_quantity must be positive")

        precision_multiplier_token0: int = 10**self.token0.decimals
        precision_multiplier_token1: int = 10**self.token1.decimals

        def _k(balance_0: int, balance_1: int) -> int:
            _x: int = balance_0 * 10**18 // precision_multiplier_token0
            _y: int = balance_1 * 10**18 // precision_multiplier_token1
            _a: int = _x * _y // 10**18
            _b: int = (_x * _x // 10**18) + (_y * _y // 10**18)
            return _a * _b // 10**18  # x^3*y+y^3*x >= k

        def _get_y(x_0: int, xy: int, y: int) -> int:
            for _ in range(255):
                y_prev = y
                k = _f(x_0, y)
                if k < xy:
                    dy = (xy - k) * 10**18 // _d(x_0, y)
                    y = y + dy
                else:
                    dy = (k - xy) * 10**18 // _d(x_0, y)
                    y = y - dy

                if y > y_prev:
                    if y - y_prev <= 1:
                        return y
                else:
                    if y_prev - y <= 1:
                        return y

            return y

        def _f(x_0: int, y: int) -> int:
            return (
                x_0 * (y * y // 10**18 * y // 10**18) // 10**18
                + (x_0 * x_0 // 10**18 * x_0 // 10**18) * y // 10**18
            )

        def _d(x_0: int, y: int) -> int:
            return 3 * x_0 * (y * y // 10**18) // 10**18 + (x_0 * x_0 // 10**18 * x_0 // 10**18)

        fee_percent = self.fee_denominator * (
            self.fee_token0 if token_in == self.token0 else self.fee_token1
        )

        reserves_token0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_token1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        # Remove fee from amount received
        token_in_quantity -= token_in_quantity * fee_percent // self.fee_denominator
        xy = _k(reserves_token0, reserves_token1)
        reserves_token0 = reserves_token0 * 10**18 // precision_multiplier_token0
        reserves_token1 = reserves_token1 * 10**18 // precision_multiplier_token1
        reserve_a, reserve_b = (
            (reserves_token0, reserves_token1)
            if token_in == self.token0
            else (reserves_token1, reserves_token0)
        )
        token_in_quantity = (
            token_in_quantity * 10**18 // precision_multiplier_token0
            if token_in == self.token0
            else token_in_quantity * 10**18 // precision_multiplier_token1
        )
        y = reserve_b - _get_y(token_in_quantity + reserve_a, xy, reserve_b)

        return (
            y
            * (
                precision_multiplier_token1
                if token_in == self.token0
                else precision_multiplier_token0
            )
            // 10**18
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        if self.stable_swap:
            return self._calculate_tokens_out_from_tokens_in_stable_swap(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_state=override_state,
            )
        else:
            return super().calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_state=override_state,
            )


class UnregisteredLiquidityPool(LiquidityPool):
    """
    A disconnected version of `LiquidityPool` for use where a pool helper is expected, but no
    chain data available to read the necessary values.

    The pool helper is not added to the pool registry, no Contract object is created, and no
    reserve values are set.
    """

    def __init__(
        self,
        address: ChecksumAddress | str,
        tokens: list[Erc20Token],
        abi: list[Any] | None = None,
        fee: Fraction | Iterable[Fraction] = Fraction(3, 1000),
    ) -> None:
        self._state_lock = Lock()
        self.abi = abi if abi is not None else UNISWAP_V2_POOL_ABI
        self.address = to_checksum_address(address)
        self.token0 = min(tokens)
        self.token1 = max(tokens)
        self.tokens = (self.token0, self.token1)

        if isinstance(fee, Iterable):
            self.fee_token0, self.fee_token1 = fee
        else:
            self.fee_token0 = self.fee_token1 = fee
