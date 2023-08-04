import dataclasses
from decimal import Decimal
from fractions import Fraction
from typing import List, Optional, Union
from warnings import warn

from brownie import Contract, Wei, chain  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.exceptions import (
    DeprecationError,
    ExternalUpdateError,
    LiquidityPoolError,
    ZeroSwapError,
)
from degenbot.logging import logger
from degenbot.token import Erc20Token
from degenbot.types import PoolHelper
from degenbot.uniswap.abi import CAMELOT_POOL_ABI, UNISWAP_V2_POOL_ABI
from degenbot.uniswap.v2.router import Router


@dataclasses.dataclass(slots=True)
class UniswapV2PoolState:
    pool: "LiquidityPool"
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True)
class UniswapV2PoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: UniswapV2PoolState
    future_state: UniswapV2PoolState


class LiquidityPool(PoolHelper):
    """
    Represents a Uniswap V2 liquidity pool
    """

    def __init__(
        self,
        address: str,
        tokens: Optional[List[Erc20Token]] = None,
        name: Optional[str] = None,
        update_method: str = "polling",
        router: Optional[Router] = None,
        abi: Optional[list] = None,
        factory_address: Optional[str] = None,
        factory_init_hash: Optional[str] = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Fraction = Fraction(3, 1000),
        fee_token0: Optional[Fraction] = None,
        fee_token1: Optional[Fraction] = None,
        silent: bool = False,
        update_reserves_on_start: bool = True,
        unload_brownie_contract_after_init: bool = False,
        state_block: Optional[int] = None,
    ) -> None:
        """
        Create a new `LiquidityPool` object for interaction with a Uniswap
        V2 pool.

        Arguments
        ---------
        address : str
            Address for the deployed pool contract.
        tokens : List[Erc20Token], optional
            Erc20Token objects for the tokens held by the deployed pool.
        name : str, optional
            Name of the contract, e.g. "DAI-WETH".
        update_method : str
            A string that sets the method used to fetch updates to the pool.
            Can be "polling", which fetches updates from the chain object
            using the contract object, or "external" which relies on updates
            being provided from outside the object.
        router : Router, optional
            A reference to a Router object, which can be used to execute swaps
            using the attributes held within this object.
        abi : list, optional
            Contract ABI.
        factory_address : str, optional
            The address for the factory contract. The default assumes a
            mainnet Uniswap V2 factory contract. If creating a
            `LiquidityPool` object based on another ecosystem, provide this
            value or the address check will fail.
        factory_init_hash : str, optional
            The init hash for the factory contract. The default assumes a
            mainnet Uniswap V2 factory contract.
        fee : Fraction
            The swap fee imposed by the pool. Defaults to `Fraction(3,1000)`
            which is equivalent to 0.3%.
        fee_token0 : Fraction, optional
            Swap fee for token0. Same purpose as `fee` except useful for
            pools with different fees for each token.
        fee_token1 : Fraction, optional
            Swap fee for token1. Same purpose as `fee` except useful for
            pools with different fees for each token.
        silent : bool
            Suppress status output.
        update_reserves_on_start : bool
            Update the reserves during instantiation.
        unload_brownie_contract_after_init : bool
            Remove the Brownie contract helper before completion. Saves
            memory for objects that are externally-updated, and do not
            need to perform calls to the chain after creation.
        state_block: int, optional
            Fetch initial state values from the chain at a particular block
            height. Defaults to the latest block if omitted.
        """

        self.uniswap_version = 2

        self.address: ChecksumAddress = Web3.toChecksumAddress(address)

        if router:
            self.router = router

        if isinstance(fee, Decimal):
            warn(
                f"WARNING: fee set as a Decimal value instead of Fraction. The fee has been converted inside the LP helper from {repr(fee)} to {repr(Fraction(fee))}, please adjust your code to specify a Fraction to remove this warning. e.g. Fraction(3,1000) is equivalent to Decimal('0.003')."
            )
            fee = Fraction(fee)

        if factory_address is None != factory_init_hash is None:
            raise ValueError(
                f"Init hash not provided for factory {factory_address}"
            )

        if not isinstance(fee, Fraction):
            raise TypeError(
                f"LP fee was not correctly passed! "
                f"Expected '{Fraction().__class__.__name__}', "
                f"was '{fee.__class__.__name__}'"
            )

        self.fee_token0 = fee_token0 if fee_token0 is not None else fee
        self.fee_token1 = fee_token1 if fee_token1 is not None else fee

        if self.fee_token0 and self.fee_token1:
            self.fee = None
        else:
            self.fee = fee

        self._update_method = update_method
        self._ratio_token0_in: Optional[Decimal] = None
        self._ratio_token1_in: Optional[Decimal] = None
        self.token0_max_swap = 0
        self.token1_max_swap = 0
        self.new_reserves = False
        self.update_block = state_block if state_block else chain.height

        if abi is None:
            abi = UNISWAP_V2_POOL_ABI

        self._brownie_contract = Contract.from_abi(
            name=f"{self.address}",
            abi=abi,
            address=self.address,
            persist=False,
        )
        self.abi = abi

        self.factory = Web3.toChecksumAddress(self._brownie_contract.factory())

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(f"Expected 2 tokens, found {len(tokens)}")
            for token in tokens:
                if token.address == self._brownie_contract.token0():
                    self.token0 = token
                elif token.address == self._brownie_contract.token1():
                    self.token1 = token
                else:
                    raise ValueError(f"{token} not found in pool {self}")
        else:
            from degenbot.manager import Erc20TokenHelperManager

            _token_manager = Erc20TokenHelperManager(chain.id)
            self.token0 = _token_manager.get_erc20token(
                address=self._brownie_contract.token0(),
                min_abi=True,
                silent=silent,
                unload_brownie_contract_after_init=True,
            )
            self.token1 = _token_manager.get_erc20token(
                address=self._brownie_contract.token1(),
                min_abi=True,
                silent=silent,
                unload_brownie_contract_after_init=True,
            )

        if factory_address is not None and factory_init_hash is not None:
            from degenbot.uniswap.v2.functions import generate_v2_pool_address

            computed_pool_address = generate_v2_pool_address(
                token_addresses=[self.token0.address, self.token1.address],
                factory_address=factory_address,
                init_hash=factory_init_hash,
            )
            if computed_pool_address != self.address:
                raise ValueError(
                    f"Pool address {self.address} does not match deterministic address {computed_pool_address} from factory"
                )

        if name is not None:
            self.name = name
        else:
            if self.fee is not None:
                fee_string = (
                    f"{100*self.fee.numerator/self.fee.denominator:.2f}"
                )
            elif self.fee_token0 is not None and self.fee_token1 is not None:
                if self.fee_token0 != self.fee_token1:
                    fee_string = f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}/{100*self.fee_token1.numerator/self.fee_token1.denominator:.2f}"
                elif self.fee_token0 == self.fee_token1:
                    fee_string = f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}"

            self.name = f"{self.token0}-{self.token1} (V2, {fee_string}%)"

        if update_reserves_on_start:
            (
                self.reserves_token0,
                self.reserves_token1,
                *_,
            ) = self._brownie_contract.getReserves(
                block_identifier=self.update_block
            )[
                0:2
            ]
        else:
            self.reserves_token0 = self.reserves_token1 = 0

        if self._update_method == "event":
            raise ValueError(
                "The 'event' update method is inaccurate and unsupported, please update your bot to use the default 'polling' method"
            )

        if (
            self._update_method == "external"
            and unload_brownie_contract_after_init
        ):
            self._brownie_contract = None

        self.state = UniswapV2PoolState(
            pool=self,
            reserves_token0=self.reserves_token0,
            reserves_token1=self.reserves_token1,
        )

        from degenbot.manager import AllPools

        AllPools(chain.id)[self.address] = self

        if not silent:
            logger.info(self.name)
            logger.info(
                f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}"
            )
            logger.info(
                f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}"
            )

    def __eq__(self, other) -> bool:
        if issubclass(type(other), PoolHelper):
            return self.address == other.address
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            raise NotImplementedError

    # The Brownie contract object cannot be pickled, so remove it and return the state
    def __getstate__(self):
        keys_to_remove = [
            "_brownie_contract",
        ]
        state = self.__dict__.copy()
        for key in keys_to_remove:
            if key in state:
                del state[key]
        return state

    def __hash__(self):
        return hash(self.address)

    def __repr__(self):
        return f"LiquidityPool(address={self.address}, token0={self.token0}, token1={self.token1})"

    def __setstate__(self, state):
        self.__dict__ = state

    def __str__(self):
        return self.name

    def _update_pool_state(self):
        self.state = UniswapV2PoolState(
            pool=self,
            reserves_token0=self.reserves_token0,
            reserves_token1=self.reserves_token1,
        )

    def calculate_tokens_in_from_ratio_out(self) -> None:
        """
        Calculates the maximum token inputs for the target output ratios at current pool reserves
        """

        # token0 in, token1 out
        # formula: dx = y0*C - x0/(1-FEE), where C = token0/token1
        if self._ratio_token0_in:
            self.token0_max_swap = max(
                0,
                int(self.reserves_token1 * self._ratio_token0_in)
                - int(self.reserves_token0 / (1 - self.fee_token0)),
            )
        else:
            self.token0_max_swap = 0

        # token1 in, token0 out
        # formula: dy = x0*C - y0(1/FEE), where C = token1/token0
        if self._ratio_token1_in:
            self.token1_max_swap = max(
                0,
                int(self.reserves_token0 * self._ratio_token1_in)
                - int(self.reserves_token1 / (1 - self.fee_token1)),
            )
        else:
            self.token1_max_swap = 0

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_in: Optional[Erc20Token] = None,
        token_out: Optional[Erc20Token] = None,
        override_reserves_token0: Optional[int] = None,
        override_reserves_token1: Optional[int] = None,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT
        at current pool reserves. Uses the `self.token0` and `self.token1`
        references to determine which token is being swapped in.

        @dev This method accepts overrides in the form of individual tokens
        reserves or a single override dictionary. The override dictionary is
        used by other helpers and is the preferred method. The individual
        overrides are left here for backward compatibility with older scripts,
        and will be deprecated in the future.
        """

        if override_state:
            if override_reserves_token0 or override_reserves_token1:
                raise ValueError(
                    "Provide a single override via `override_state` or individual reserves."
                )
            override_reserves_token0 = override_state.reserves_token0
            override_reserves_token1 = override_state.reserves_token1

        if (override_reserves_token0 and not override_reserves_token1) or (
            not override_reserves_token0 and override_reserves_token1
        ):
            raise ValueError(
                "Must provide reserve override values for both tokens"
            )

        if override_reserves_token0 and override_reserves_token1:
            logger.debug("Reserve overrides applied:")
            logger.debug(f"token0: {override_reserves_token0}")
            logger.debug(f"token1: {override_reserves_token1}")

        if token_in is not None:
            if token_in not in [self.token0, self.token1]:
                raise ValueError(
                    f"Could not identify token_in: {token_in}! This pool holds: {self.token0} {self.token1}"
                )
            if token_in == self.token0:
                reserves_in = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                fee = self.fee_token0
                reserves_out = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
            elif token_in == self.token1:
                reserves_in = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
                reserves_out = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                fee = self.fee_token1
            else:
                raise ValueError("wtf happened here? (token_in)")
        elif token_out is not None:
            if token_out not in [self.token0, self.token1]:
                raise ValueError(
                    f"Could not identify token_out: {token_out}! This pool holds: {self.token0} {self.token1}"
                )
            if token_out == self.token1:
                reserves_in = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                reserves_out = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
                fee = self.fee_token0
            elif token_out == self.token0:
                reserves_in = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
                reserves_out = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                fee = self.fee_token1
            else:
                raise ValueError("wtf happened here? (token_in)")

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"
            )

        numerator = reserves_in * token_out_quantity * fee.denominator
        denominator = (reserves_out - token_out_quantity) * (
            fee.denominator - fee.numerator
        )
        return numerator // denominator + 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_reserves_token0: Optional[int] = None,
        override_reserves_token1: Optional[int] = None,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        if token_in_quantity <= 0:
            raise ZeroSwapError("token_in_quantity must be positive")

        if override_state:
            if override_reserves_token0 or override_reserves_token1:
                raise ValueError(
                    "Provide a single override via `override_state` or individual reserves."
                )
            override_reserves_token0 = override_state.reserves_token0
            override_reserves_token1 = override_state.reserves_token1

        if (override_reserves_token0 and not override_reserves_token1) or (
            not override_reserves_token0 and override_reserves_token1
        ):
            raise ValueError(
                "Must provide reserve override values for both tokens"
            )

        if override_reserves_token0 and override_reserves_token1:
            logger.debug("Reserve overrides applied:")
            logger.debug(f"token0: {override_reserves_token0}")
            logger.debug(f"token1: {override_reserves_token1}")

        if token_in == self.token0:
            reserves_in = (
                override_reserves_token0
                if override_reserves_token0 is not None
                else self.reserves_token0
            )
            reserves_out = (
                override_reserves_token1
                if override_reserves_token1 is not None
                else self.reserves_token1
            )
            fee = self.fee_token0
        elif token_in == self.token1:
            reserves_in = (
                override_reserves_token1
                if override_reserves_token1 is not None
                else self.reserves_token1
            )
            reserves_out = (
                override_reserves_token0
                if override_reserves_token0 is not None
                else self.reserves_token0
            )
            fee = self.fee_token1
        else:
            raise ValueError(
                f"Could not identify token_in: {token_in}! Pool holds: {self.token0} {self.token1}"
            )

        amount_in_with_fee = token_in_quantity * (
            fee.denominator - fee.numerator
        )
        numerator = amount_in_with_fee * reserves_out
        denominator = reserves_in * fee.denominator + amount_in_with_fee

        return numerator // denominator

    def set_swap_target(
        self,
        token_in: Erc20Token,
        token_in_qty: Union[Wei, int],
        token_out: Erc20Token,
        token_out_qty: Union[Wei, int],
        silent: bool = False,
    ) -> None:
        if not (
            (token_in == self.token0 and token_out == self.token1)
            or (token_in == self.token1 and token_out == self.token0)
        ):
            raise ValueError(
                "Tokens must match the two tokens held by this pool!"
            )

        if not silent:
            logger.info(
                f"{token_in} -> {token_out} @ ({token_in_qty} {token_in} = {token_out_qty} {token_out})"
            )

        if token_in == self.token0:
            # calculate the ratio of token0/token1 for swap of token0 -> token1
            self._ratio_token0_in = Decimal(
                (token_in_qty * 10**token_in.decimals)
            ) / Decimal(token_out_qty * 10**token_out.decimals)

        if token_in == self.token1:
            # calculate the ratio of token1/token0 for swap of token1 -> token0
            self._ratio_token1_in = Decimal(
                (token_in_qty * 10**token_in.decimals)
            ) / Decimal(token_out_qty * 10**token_out.decimals)

        self.calculate_tokens_in_from_ratio_out()

    def simulate_swap(
        self,
        token_in: Optional[Erc20Token] = None,
        token_in_quantity: Optional[int] = None,
        token_out: Optional[Erc20Token] = None,
        token_out_quantity: Optional[int] = None,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        TODO
        """

        if token_in_quantity is None and token_out_quantity is None:
            raise ValueError("No quantity was provided")

        if token_in_quantity is not None and token_out_quantity is not None:
            raise ValueError(
                "Provide token_in_quantity or token_out_quantity, not both"
            )

        if token_in and token_out and token_in == token_out:
            raise ValueError("Both tokens are the same!")

        if override_state:
            logger.debug(f"State override: {override_state}")

        if token_in and token_in not in (self.token0, self.token1):
            raise ValueError(
                f"Token not found! token_in = {repr(token_in)}, pool holds {self.token0},{self.token1}"
            )
        if token_out and token_out not in (self.token0, self.token1):
            raise ValueError(
                f"Token not found! token_out = {repr(token_out)}, pool holds {self.token0},{self.token1}"
            )

        if token_in is not None and token_in == self.token0:
            token_out = self.token1
        elif token_in is not None and token_in == self.token1:
            token_out = self.token0

        if token_out is not None and token_out == self.token0:
            token_in = self.token1
        elif token_out is not None and token_out == self.token1:
            token_in = self.token0

        # bugfix: (changed check `token_in_quantity is not None`)
        # swaps with zero amounts (a stupid value, but valid) were falling through
        # both blocks and function was returning None
        if token_in_quantity is not None and token_in is not None:
            token_out_quantity = self.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                # TODO: consolidate into single override_state arg
                override_reserves_token0=override_state.reserves_token0
                if override_state
                else None,
                override_reserves_token1=override_state.reserves_token1
                if override_state
                else None,
            )

            token0_delta = (
                -token_out_quantity
                if token_in is self.token1
                else token_in_quantity
            )
            token1_delta = (
                -token_out_quantity
                if token_in is self.token0
                else token_in_quantity
            )

        # bugfix: (changed check `token_out_quantity is not None`)
        # swaps with zero amounts (a stupid value, but valid) were falling through
        # both blocks and function was returning None
        elif token_out_quantity is not None:
            token_in_quantity = self.calculate_tokens_in_from_tokens_out(
                token_in=token_in,
                token_out=token_out,
                token_out_quantity=token_out_quantity,
                # TODO: consolidate into single override_state arg
                override_reserves_token0=override_state.reserves_token0
                if override_state
                else None,
                override_reserves_token1=override_state.reserves_token1
                if override_state
                else None,
            )

            token0_delta = (
                token_in_quantity
                if token_in == self.token0
                else -token_out_quantity
            )
            token1_delta = (
                token_in_quantity
                if token_in == self.token1
                else -token_out_quantity
            )

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            current_state=self.state,
            future_state=UniswapV2PoolState(
                pool=self,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
            ),
        )

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
        external_token0_reserves: Optional[int] = None,
        external_token1_reserves: Optional[int] = None,
        override_update_method: Optional[str] = None,
        update_block: Optional[int] = None,
    ) -> bool:
        """
        Checks for updated reserve values when set to "polling", otherwise
        if set to "external" assumes that provided reserves are valid
        """

        success = False

        # get the chain height from Brownie if a specific update_block is not provided
        if update_block is None:
            update_block = chain.height

        # discard stale updates, but allow updating the same pool multiple times per block (necessary if sending sync events individually)
        if update_block < self.update_block:
            raise ExternalUpdateError(
                f"Current state recorded at block {self.update_block}, received update for stale block {update_block}"
            )
        else:
            self.update_block = update_block

        if (
            self._update_method == "polling"
            or override_update_method == "polling"
        ):
            try:
                reserves0, reserves1, *_ = self._brownie_contract.getReserves(
                    block_identifier=self.update_block
                )
                if (self.reserves_token0, self.reserves_token1) != (
                    reserves0,
                    reserves1,
                ):
                    self.reserves_token0, self.reserves_token1 = (
                        reserves0,
                        reserves1,
                    )
                    if not silent:
                        logger.info(f"[{self.name}]")
                        if print_reserves:
                            logger.info(
                                f"{self.token0}: {self.reserves_token0}"
                            )
                            logger.info(
                                f"{self.token1}: {self.reserves_token1}"
                            )
                        if print_ratios:
                            logger.info(
                                f"{self.token0}/{self.token1}: {(self.reserves_token0/10**self.token0.decimals) / (self.reserves_token1/10**self.token1.decimals)}"
                            )
                            logger.info(
                                f"{self.token1}/{self.token0}: {(self.reserves_token1/10**self.token1.decimals) / (self.reserves_token0/10**self.token0.decimals)}"
                            )

                    # recalculate possible swaps using the new reserves
                    self.calculate_tokens_in_from_ratio_out()
                    self._update_pool_state()
                    success = True
                else:
                    success = False
            except Exception as e:
                print(
                    f"LiquidityPool: Exception in update_reserves (polling): {e}"
                )
        elif self._update_method == "external":
            if not (
                (
                    external_token0_reserves is not None
                    and external_token1_reserves is not None
                )
            ):
                raise ValueError(
                    "Called update_reserves without providing reserve values for both tokens!"
                )

            # skip follow-up processing if the LP object already has the latest reserves, or if no reserves were provided
            if (
                external_token0_reserves == self.reserves_token0
                and external_token1_reserves == self.reserves_token1
            ):
                self.new_reserves = False
                success = False
            else:
                self.reserves_token0 = external_token0_reserves
                self.reserves_token1 = external_token1_reserves
                self.new_reserves = True
                self._update_pool_state()

            if not silent:
                logger.info(f"[{self.name}]")
                if print_reserves:
                    logger.info(f"{self.token0}: {self.reserves_token0}")
                    logger.info(f"{self.token1}: {self.reserves_token1}")
                if print_ratios:
                    logger.info(
                        f"{self.token0}/{self.token1}: {self.reserves_token0 / self.reserves_token1}"
                    )
                    logger.info(
                        f"{self.token1}/{self.token0}: {self.reserves_token1 / self.reserves_token0}"
                    )
            self.calculate_tokens_in_from_ratio_out()
            success = True
        elif self._update_method == "event":
            raise DeprecationError(
                "The 'event' update method is deprecated. Please update your bot to use the default 'polling' method"
            )
        else:
            success = False

        return success


class CamelotLiquidityPool(LiquidityPool):
    FEE_DENOMINATOR = 100_000

    def _calculate_tokens_out_from_tokens_in_stable_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: Optional[dict] = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        override_reserves_token0: Optional[int] = None
        override_reserves_token1: Optional[int] = None

        if override_state is not None:
            try:
                override_reserves_token0 = override_state["reserves_token0"]
            except KeyError:
                pass
            else:
                logger.debug(f"{override_reserves_token0=}")

            try:
                override_reserves_token1 = override_state["reserves_token1"]
            except KeyError:
                pass
            else:
                logger.debug(f"{override_reserves_token1=}")

        if token_in_quantity <= 0:
            raise ZeroSwapError("token_in_quantity must be positive")

        precision_multiplier_token0 = 10**self.token0.decimals
        precision_multiplier_token1 = 10**self.token1.decimals

        def _k(balance_0, balance_1) -> int:
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
            return 3 * x_0 * (y * y // 10**18) // 10**18 + (
                x_0 * x_0 // 10**18 * x_0 // 10**18
            )

        # fee_percent is stored as a uint16 in the contract, but as a Fraction
        # in this helper, so must be converted.
        #
        # e.g. 0.04% fee = Fraction(1,2500) in the helper, fee = 40 in the
        # contract. To convert, multiply the fraction by the `FEE_DENOMINATOR`,
        # so fee_percent = 1/2500 * 100000 = 40

        fee_percent = (
            self.fee_token0 if token_in is self.token0 else self.fee_token1
        ) * self.FEE_DENOMINATOR

        reserves_token0 = (
            override_reserves_token0
            if override_reserves_token0 is not None
            else self.reserves_token0
        )
        reserves_token1 = (
            override_reserves_token1
            if override_reserves_token1 is not None
            else self.reserves_token1
        )

        # remove fee from amount received
        token_in_quantity -= (
            token_in_quantity * fee_percent // self.FEE_DENOMINATOR
        )
        xy = _k(reserves_token0, reserves_token1)
        reserves_token0 = (
            reserves_token0 * 10**18 // precision_multiplier_token0
        )
        reserves_token1 = (
            reserves_token1 * 10**18 // precision_multiplier_token1
        )
        reserve_a, reserve_b = (
            (reserves_token0, reserves_token1)
            if token_in is self.token0
            else (reserves_token1, reserves_token0)
        )
        token_in_quantity = (
            token_in_quantity * 10**18 // precision_multiplier_token0
            if token_in is self.token0
            else token_in_quantity * 10**18 // precision_multiplier_token1
        )
        y = reserve_b - _get_y(token_in_quantity + reserve_a, xy, reserve_b)

        return (
            y
            * (
                precision_multiplier_token1
                if token_in is self.token0
                else precision_multiplier_token0
            )
            // 10**18
        )

    def __init__(
        self,
        address: str,
        tokens: Optional[List[Erc20Token]] = None,
        name: Optional[str] = None,
        update_method: str = "polling",
        router: Optional[Router] = None,
        abi: Optional[list] = None,
        silent: bool = False,
        update_reserves_on_start: bool = True,
        unload_brownie_contract_after_init: bool = False,
    ) -> None:
        if abi is None:
            abi = CAMELOT_POOL_ABI

        address = Web3.toChecksumAddress(address)

        _contract = Contract.from_abi(
            name=f"CamelotV2 pool: {address}",
            abi=abi,
            address=address,
            persist=False,
        )

        stable_pool: bool = _contract.stableSwap()

        _, _, fee_token0, fee_token1 = _contract.getReserves()
        fee_denominator = _contract.FEE_DENOMINATOR()
        fee_token0 = Fraction(fee_token0, fee_denominator)
        fee_token1 = Fraction(fee_token1, fee_denominator)

        super().__init__(
            address=address,
            tokens=tokens,
            name=name,
            update_method=update_method,
            router=router,
            abi=abi,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            silent=silent,
            update_reserves_on_start=update_reserves_on_start,
            unload_brownie_contract_after_init=unload_brownie_contract_after_init,
        )

        if stable_pool:
            # replace the calculate_tokens_out_from_tokens_in method for stable-only pools
            self.calculate_tokens_out_from_tokens_in = self._calculate_tokens_out_from_tokens_in_stable_swap  # type: ignore
