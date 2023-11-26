from bisect import bisect_left
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Dict, Iterable, List, Optional, Set, Tuple, Union
from warnings import warn

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from web3.contract import Contract

from .. import config
from ..baseclasses import PoolHelper
from ..erc20_token import Erc20Token
from ..exceptions import (
    DeprecationError,
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
    ZeroSwapError,
)
from ..logging import logger
from ..manager import AllPools, Erc20TokenHelperManager
from .abi import CAMELOT_POOL_ABI, UNISWAP_V2_POOL_ABI
from .mixins import CamelotStablePoolMixin, Subscriber, SubscriptionMixin
from .v2_dataclasses import UniswapV2PoolSimulationResult, UniswapV2PoolState
from .v2_functions import generate_v2_pool_address


class LiquidityPool(SubscriptionMixin, PoolHelper):
    """
    Represents a Uniswap V2 liquidity pool
    """

    uniswap_version = 2

    __slots__: Tuple[str, ...] = (
        "_pool_state_archive",
        # "_ratio_token0_in",
        # "_ratio_token1_in",
        "_state_lock",
        "_subscribers",
        "_update_method",
        "abi",
        "address",
        "factory",
        "fee_token0",
        "fee_token1",
        "fee",
        "name",
        "new_reserves",
        "reserves_token0",
        "reserves_token1",
        "router",
        "state",
        "token0",
        "token1",
        # "token0_max_swap",
        # "token1_max_swap",
        "update_block",
    )

    def __init__(
        self,
        address: Union[ChecksumAddress, str],
        tokens: Optional[List["Erc20Token"]] = None,
        name: Optional[str] = None,
        update_method: str = "polling",
        abi: Optional[list] = None,
        factory_address: Optional[str] = None,
        factory_init_hash: Optional[str] = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Union[Fraction, Iterable[Fraction]] = Fraction(3, 1000),
        silent: bool = False,
        update_reserves_on_start: Optional[bool] = None,  # deprecated
        unload_brownie_contract_after_init: Optional[bool] = None,  # deprecated
        state_block: Optional[int] = None,
        empty: bool = False,
    ) -> None:
        """
        Create a new `LiquidityPool` object for interaction with a Uniswap
        V2 pool.

        Arguments
        ---------
        address : str
            Address for the deployed pool contract.
        tokens : List[Erc20Token], optional
            "Erc20Token" objects for the tokens held by the deployed pool.
        name : str, optional
            Name of the contract, e.g. "DAI-WETH".
        update_method : str
            A string that sets the method used to fetch updates to the pool.
            Can be "polling", which fetches updates from the chain object
            using the contract object, or "external" which relies on updates
            being provided from outside the object.
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
        fee : Fraction | (Fraction, Fraction)
            The swap fee imposed by the pool. Defaults to `Fraction(3,1000)`
            which is equivalent to 0.3%. For split-fee pools of unequal value,
            provide a tuple or list with the token0 fee in the first position,
            and the token1 fee in the second.
        silent : bool
            Suppress status output.
        state_block: int, optional
            Fetch initial state values from the chain at a particular block
            height. Defaults to the latest block if omitted.
        empty: bool
            Set to `True` to initialize the pool without initial values
            retrieved from chain, and skipping some validation. Useful for
            simulating transactions through pools that do not exist.
        """

        if empty and any([address is None, factory_address is None, tokens is None]):
            raise ValueError(
                "Empty LiquidityPool cannot be created without pool, factory, and token addresses"
            )

        self._state_lock = Lock()

        if unload_brownie_contract_after_init is not None:  # pragma: no cover
            warn("unload_brownie_contract_after_init has been deprecated and is ignored.")

        if update_reserves_on_start is not None:  # pragma: no cover
            warn("update_reserves_on_start has been deprecated in favor of `empty` argument.")

        self.address: ChecksumAddress = to_checksum_address(address)
        self.abi = abi if abi is not None else UNISWAP_V2_POOL_ABI

        _w3 = config.get_web3()
        _w3_contract = self._w3_contract

        if factory_address is not None and factory_init_hash is None:
            raise ValueError(f"Init hash not provided for factory {factory_address}")

        if factory_address:
            self.factory = to_checksum_address(factory_address)

        if isinstance(fee, Iterable):
            self.fee_token0, self.fee_token1 = fee
            if not isinstance(self.fee_token0, Fraction) or not isinstance(
                self.fee_token1, Fraction
            ):
                raise TypeError(
                    f"LP fee was not correctly passed! "
                    f"Expected '{Fraction().__class__.__name__}', "
                    f"was '{self.fee_token0.__class__.__name__}' and '{self.fee_token1.__class__.__name__}'"
                )
        else:
            self.fee_token0 = fee
            self.fee_token1 = fee
            if not isinstance(fee, Fraction):
                raise TypeError(
                    f"LP fee was not correctly passed! "
                    f"Expected '{Fraction().__class__.__name__}', "
                    f"was '{fee.__class__.__name__}'"
                )

        self._update_method = update_method
        # self._ratio_token0_in: Optional[Decimal] = None
        # self._ratio_token1_in: Optional[Decimal] = None
        # self.token0_max_swap = 0
        # self.token1_max_swap = 0
        self.new_reserves = False

        if empty:
            self.update_block = 1
        else:
            self.update_block = (
                state_block if state_block is not None else _w3.eth.get_block_number()
            )
            self.factory = _w3_contract.functions.factory().call()

        chain_id = 1 if empty else _w3.eth.chain_id

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(f"Expected 2 tokens, found {len(tokens)}")
            self.token0 = min(tokens)
            self.token1 = max(tokens)
        else:
            _token_manager = Erc20TokenHelperManager(chain_id)
            self.token0 = _token_manager.get_erc20token(
                address=_w3_contract.functions.token0().call(),
                silent=silent,
            )
            self.token1 = _token_manager.get_erc20token(
                address=_w3_contract.functions.token1().call(),
                silent=silent,
            )

        if factory_address is not None and factory_init_hash is not None:
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
            # if self.fee is not None:
            # fee_string = f"{100*self.fee.numerator/self.fee.denominator:.2f}"
            # if self.fee_token0 is not None and self.fee_token1 is not None:
            if self.fee_token0 != self.fee_token1:
                fee_string = f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}/{100*self.fee_token1.numerator/self.fee_token1.denominator:.2f}"
            elif self.fee_token0 == self.fee_token1:
                fee_string = f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}"

            self.name = f"{self.token0}-{self.token1} (V2, {fee_string}%)"

        self.reserves_token0: int = 0
        self.reserves_token1: int = 0

        if not empty:
            (
                self.reserves_token0,
                self.reserves_token1,
                *_,
            ) = _w3_contract.functions.getReserves().call(block_identifier=self.update_block)

        if TYPE_CHECKING:
            assert isinstance(self.reserves_token0, int)
            assert isinstance(self.reserves_token1, int)

        if self._update_method == "event":  # pragma: no cover
            raise ValueError(
                "The 'event' update method is inaccurate and unsupported, please update your bot to use the default 'polling' method"
            )

        self.state = UniswapV2PoolState(
            pool=self,
            reserves_token0=self.reserves_token0,
            reserves_token1=self.reserves_token1,
        )
        self._pool_state_archive: Dict[int, UniswapV2PoolState] = {
            0: UniswapV2PoolState(
                pool=self,
                reserves_token0=0,
                reserves_token1=0,
            ),
            self.update_block: self.state,
        }  # WIP

        AllPools(chain_id)[self.address] = self

        self._subscribers: Set[Subscriber] = set()

        if not silent:
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}")
            logger.info(f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}")

    def __eq__(self, other) -> bool:
        if issubclass(type(other), PoolHelper):
            return self.address == other.address
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            raise NotImplementedError

    def __getstate__(self) -> dict:
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        dropped_attributes = (
            "_state_lock",
            "_subscribers",
        )

        with self._state_lock:
            return {
                attr_name: getattr(self, attr_name, None)
                for attr_name in self.__slots__
                if attr_name not in dropped_attributes
            }

    def __hash__(self):
        return hash(self.address)

    def __repr__(self):  # pragma: no cover
        return f"LiquidityPool(address={self.address}, token0={self.token0}, token1={self.token1})"

    def __setstate__(self, state: Dict):
        for attr_name, attr_value in state.items():
            setattr(self, attr_name, attr_value)

    def __str__(self):
        return self.name

    @property
    def _w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(
            address=self.address,
            abi=self.abi,
        )

    def _update_pool_state(self):
        with self._state_lock:
            self.state = UniswapV2PoolState(
                pool=self,
                reserves_token0=self.reserves_token0,
                reserves_token1=self.reserves_token1,
            )
        self._notify_subscribers()

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

        if token_in not in (self.token0, self.token1):
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
        token_out: "Erc20Token",
        token_in: Optional["Erc20Token"] = None,
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

        if token_in is not None:
            warn(
                "The use of token_in is deprecated and will be removed in the future. Please modify your calling code to specify token_out instead."
            )

        if (
            override_state is None
            and override_reserves_token0 is not None
            and override_reserves_token1 is not None
        ):
            override_state = UniswapV2PoolState(
                pool=self,
                reserves_token0=override_reserves_token0,
                reserves_token1=override_reserves_token1,
            )
            warn(
                "Overriding individual reserves is deprecated in favor of a single state override via override_state. The individual overrides have been transformed in-place, but this will be removed in a future release."
            )

        if override_state:
            # if override_reserves_token0 or override_reserves_token1:
            #     raise ValueError(
            #         "Provide a single override via `override_state` or individual reserves."
            #     )
            override_reserves_token0 = override_state.reserves_token0
            override_reserves_token1 = override_state.reserves_token1
            logger.debug("Reserve overrides applied:")
            logger.debug(f"token0: {override_reserves_token0}")
            logger.debug(f"token1: {override_reserves_token1}")

            # if (override_reserves_token0 and not override_reserves_token1) or (
            #     not override_reserves_token0 and override_reserves_token1
            # ):
            #     raise ValueError("Must provide reserve override values for both tokens")

        # if token_in is not None:
        #     if token_in == self.token0:
        #         reserves_in = (
        #             override_reserves_token0
        #             if override_reserves_token0 is not None
        #             else self.reserves_token0
        #         )
        #         fee = self.fee_token0
        #         reserves_out = (
        #             override_reserves_token1
        #             if override_reserves_token1 is not None
        #             else self.reserves_token1
        #         )
        #     elif token_in == self.token1:
        #         reserves_in = (
        #             override_reserves_token1
        #             if override_reserves_token1 is not None
        #             else self.reserves_token1
        #         )
        #         reserves_out = (
        #             override_reserves_token0
        #             if override_reserves_token0 is not None
        #             else self.reserves_token0
        #         )
        #         fee = self.fee_token1
        #     else:
        #         raise ValueError(
        #             f"Could not identify token_in: {token_in}! This pool holds: {self.token0} {self.token1}"
        #         )
        # elif token_out is not None:
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
            raise ValueError(
                f"Could not identify token_out: {token_out}! This pool holds: {self.token0} {self.token1}"
            )

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"
            )

        numerator = reserves_in * token_out_quantity * fee.denominator
        denominator = (reserves_out - token_out_quantity) * (fee.denominator - fee.numerator)
        return numerator // denominator + 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: "Erc20Token",
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

        if (
            override_state is None
            and override_reserves_token0 is not None
            and override_reserves_token1 is not None
        ):  # pragma: no cover
            warn(
                "Overriding individual reserves is deprecated in favor of a single state override via override_state. The individual overrides have been transformed in-place, but this will be removed in a future release."
            )
            override_state = UniswapV2PoolState(
                pool=self,
                reserves_token0=override_reserves_token0,
                reserves_token1=override_reserves_token1,
            )

        if override_state:
            override_reserves_token0 = override_state.reserves_token0
            override_reserves_token1 = override_state.reserves_token1
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

        amount_in_with_fee = token_in_quantity * (fee.denominator - fee.numerator)
        numerator = amount_in_with_fee * reserves_out
        denominator = reserves_in * fee.denominator + amount_in_with_fee

        return numerator // denominator

    def restore_state_before_block(
        self,
        block: int,
    ) -> None:
        """
        Restore the last pool state recorded prior to a target block.

        Use this method to maintain consistent state data following a chain
        re-organization.
        """

        # Find the index for the most recent pool state PRIOR to the requested
        # block number.
        #
        # e.g. Calling restore_state_before_block(block=104) for a pool with
        # states at blocks 100, 101, 102, 103, 104. `bisect_left()` returns
        # block_index=3, since block 104 is at index=4. The state held at
        # index=3 is for block 103.
        # block_index = self._pool_state_archive.bisect_left(block)

        with self._state_lock:
            known_blocks = list(self._pool_state_archive.keys())
            block_index = bisect_left(known_blocks, block)

            if block_index == 0:
                raise NoPoolStateAvailable(f"No pool state known prior to block {block}")

            # The last known state already meets the criterion, so return early
            if block_index == len(known_blocks):
                return

            # Remove states at and after the specified block
            for block in known_blocks[block_index:]:
                del self._pool_state_archive[block]

            restored_block, restored_state = list(self._pool_state_archive.items())[-1]

            # Set mutable values to match state
            self.reserves_token0 = restored_state.reserves_token0
            self.reserves_token1 = restored_state.reserves_token1
            self.state = restored_state
            self.update_block = restored_block

        self._update_pool_state()

    def set_swap_target(self, *args, **kwargs):
        warn(
            "set_swap_target has been deprecated. Please convert your code to use the calculate_tokens_in_from_ratio_out method directly."
        )

    def simulate_add_liquidity(
        self,
        added_reserves_token0: int,
        added_reserves_token1: int,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> UniswapV2PoolSimulationResult:
        if override_state:
            logger.debug(f"State override: {override_state}")

        reserves_token0 = override_state.reserves_token0 if override_state else self.reserves_token0
        reserves_token1 = override_state.reserves_token1 if override_state else self.reserves_token1

        return UniswapV2PoolSimulationResult(
            amount0_delta=added_reserves_token0,
            amount1_delta=added_reserves_token1,
            current_state=self.state,
            future_state=UniswapV2PoolState(
                pool=self,
                reserves_token0=reserves_token0 + added_reserves_token0,
                reserves_token1=reserves_token1 + added_reserves_token1,
            ),
        )

    def simulate_remove_liquidity(
        self,
        removed_reserves_token0: int,
        removed_reserves_token1: int,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> UniswapV2PoolSimulationResult:
        if override_state:
            logger.debug(f"State override: {override_state}")

        reserves_token0 = override_state.reserves_token0 if override_state else self.reserves_token0
        reserves_token1 = override_state.reserves_token1 if override_state else self.reserves_token1

        return UniswapV2PoolSimulationResult(
            amount0_delta=-removed_reserves_token0,
            amount1_delta=-removed_reserves_token1,
            current_state=self.state,
            future_state=UniswapV2PoolState(
                pool=self,
                reserves_token0=reserves_token0 - removed_reserves_token0,
                reserves_token1=reserves_token1 - removed_reserves_token1,
            ),
        )

    def simulate_swap(
        self,
        token_in: Optional["Erc20Token"] = None,
        token_in_quantity: Optional[int] = None,
        token_out: Optional["Erc20Token"] = None,
        token_out_quantity: Optional[int] = None,
        override_state: Optional[UniswapV2PoolState] = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        TODO
        """

        if token_in_quantity is None and token_out_quantity is None:
            raise ValueError("No quantity was provided")

        if token_in_quantity is not None and token_out_quantity is not None:
            raise ValueError("Provide token_in_quantity or token_out_quantity, not both")

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

        if token_in is not None and token_in_quantity is not None:
            token_out_quantity = self.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                # WIP: consolidate into single override_state arg
                override_state=override_state,
            )
            token0_delta = -token_out_quantity if token_in is self.token1 else token_in_quantity
            token1_delta = -token_out_quantity if token_in is self.token0 else token_in_quantity

        elif token_out is not None and token_out_quantity is not None:
            token_in_quantity = self.calculate_tokens_in_from_tokens_out(
                token_out=token_out,
                token_out_quantity=token_out_quantity,
                # WIP: consolidate into single override_state arg
                override_state=override_state,
            )
            token0_delta = token_in_quantity if token_in == self.token0 else -token_out_quantity
            token1_delta = token_in_quantity if token_in == self.token1 else -token_out_quantity

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

        _w3_contract = self._w3_contract

        updates = False

        # Fetch the chain height if a specific update_block is not provided
        if update_block is None:
            update_block = config.get_web3().eth.get_block_number()

        # discard stale updates, but allow updating the same pool multiple times per block (necessary if sending sync events individually)
        if update_block < self.update_block:
            raise ExternalUpdateError(
                f"Current state recorded at block {self.update_block}, received update for stale block {update_block}"
            )
        else:
            self.update_block = update_block

        if self._update_method == "polling" or override_update_method == "polling":
            try:
                (
                    reserves0,
                    reserves1,
                    *_,
                ) = _w3_contract.functions.getReserves().call(block_identifier=self.update_block)
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
                            logger.info(f"{self.token0}: {self.reserves_token0}")
                            logger.info(f"{self.token1}: {self.reserves_token1}")
                        if print_ratios:
                            logger.info(
                                f"{self.token0}/{self.token1}: {(self.reserves_token0/10**self.token0.decimals) / (self.reserves_token1/10**self.token1.decimals)}"
                            )
                            logger.info(
                                f"{self.token1}/{self.token0}: {(self.reserves_token1/10**self.token1.decimals) / (self.reserves_token0/10**self.token0.decimals)}"
                            )

                    self._update_pool_state()
                    self._pool_state_archive[update_block] = self.state
                    updates = True
                else:
                    updates = False
            except Exception as e:
                print(f"LiquidityPool: Exception in update_reserves (polling): {e}")
        elif self._update_method == "external":
            if not (external_token0_reserves is not None and external_token1_reserves is not None):
                raise ValueError(
                    "Called update_reserves without providing reserve values for both tokens!"
                )

            # Skip follow-up processing if no updated values were found
            if (
                external_token0_reserves == self.reserves_token0
                and external_token1_reserves == self.reserves_token1
            ):
                self.new_reserves = False
                updates = False
            else:
                self.reserves_token0 = external_token0_reserves
                self.reserves_token1 = external_token1_reserves
                self.new_reserves = True
                self._update_pool_state()
                self._pool_state_archive[update_block] = self.state

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
        elif self._update_method == "event":  # pragma: no cover
            raise DeprecationError(
                "The 'event' update method is deprecated. Please update your bot to use the default 'polling' method"
            )
        else:  # pragma: no cover
            raise ValueError(f"Update method {self._update_method} is not recognized.")

        return updates


class CamelotLiquidityPool(CamelotStablePoolMixin, LiquidityPool):
    def __init__(
        self,
        address: str,
        tokens: Optional[List["Erc20Token"]] = None,
        name: Optional[str] = None,
        update_method: str = "polling",
        abi: Optional[list] = None,
        silent: bool = False,
        update_reserves_on_start: Optional[bool] = None,  # deprecated
        unload_brownie_contract_after_init: Optional[bool] = None,  # deprecated
    ) -> None:
        if unload_brownie_contract_after_init is not None:  # pragma: no cover
            warn(
                "unload_brownie_contract_after_init is no longer needed and is "
                "ignored. Remove constructor argument to stop seeing this "
                "message."
            )

        if update_reserves_on_start is not None:  # pragma: no cover
            warn("update_reserves_on_start has been deprecated.")

        address = to_checksum_address(address)

        if abi is None:
            abi = CAMELOT_POOL_ABI

        _w3 = config.get_web3()
        _w3_contract = config.get_web3().eth.contract(address=address, abi=abi)

        state_block = _w3.eth.block_number

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
            abi=abi,
            fee=(fee_token0, fee_token1),
            silent=silent,
            state_block=state_block,
        )

        self.stable_swap: bool = _w3_contract.functions.stableSwap().call()

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_reserves_token0: Optional[int] = None,  # TODO: drop after removing in superclass
        override_reserves_token1: Optional[int] = None,  # TODO: drop after removing in superclass
        override_state: Optional[UniswapV2PoolState] = None,
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
