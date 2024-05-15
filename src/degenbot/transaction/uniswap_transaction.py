# TODO: use tx_payer_is_user to simplify accounting
# TODO: implement "blank" V3 pools and incorporate try/except for all V3 pool manager get_pool calls
# TODO: add state block argument for pool simulation calls
# TODO: instead of appending pool states to list, replace with dict and only return final state state

from typing import TYPE_CHECKING, Any, Dict, List, Set, Tuple, cast

import eth_abi.abi
from eth_typing import BlockNumber, ChainId, ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3

from .. import config
from ..baseclasses import BaseSimulationResult, BaseTransaction
from ..constants import WRAPPED_NATIVE_TOKENS
from ..erc20_token import Erc20Token
from ..exceptions import (
    DegenbotError,
    EVMRevertError,
    LedgerError,
    LiquidityPoolError,
    ManagerError,
    TransactionError,
)
from ..logging import logger
from ..uniswap.abi import UNISWAP_V3_ROUTER2_ABI, UNISWAP_V3_ROUTER_ABI
from ..uniswap.managers import UniswapV2LiquidityPoolManager, UniswapV3LiquidityPoolManager
from ..uniswap.v2_dataclasses import UniswapV2PoolSimulationResult, UniswapV2PoolState
from ..uniswap.v2_functions import generate_v2_pool_address, get_v2_pools_from_token_path
from ..uniswap.v2_liquidity_pool import LiquidityPool
from ..uniswap.v3_dataclasses import UniswapV3PoolSimulationResult, UniswapV3PoolState
from ..uniswap.v3_functions import decode_v3_path
from ..uniswap.v3_liquidity_pool import V3LiquidityPool
from .simulation_ledger import SimulationLedger

# Internal dict of known router contracts by chain ID. Pre-populated with
# mainnet addresses. New routers can be added by class method `add_router`
_ROUTERS: Dict[
    int,  # chain ID
    Dict[ChecksumAddress, Dict[str, Any]],
]
_ROUTERS = {
    ChainId.ETH: {
        to_checksum_address("0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F"): {
            "name": "Sushiswap: Router",
            "factory_address": {
                2: to_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")
            },
        },
        to_checksum_address("0xf164fC0Ec4E93095b804a4795bBe1e041497b92a"): {
            "name": "UniswapV2: Router",
            "factory_address": {
                2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
            },
        },
        to_checksum_address("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"): {
            "name": "UniswapV2: Router 2",
            "factory_address": {
                2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
            },
        },
        to_checksum_address("0xE592427A0AEce92De3Edee1F18E0157C05861564"): {
            "name": "UniswapV3: Router",
            "factory_address": {
                3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")
            },
        },
        to_checksum_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"): {
            "name": "UniswapV3: Router 2",
            "factory_address": {
                2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
                3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
            },
        },
        to_checksum_address("0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B"): {
            "name": "Uniswap Universal Router",
            "factory_address": {
                2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
                3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
            },
        },
        to_checksum_address("0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD"): {
            "name": "Universal Universal Router (V1_2)",
            "factory_address": {
                2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
                3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
            },
        },
        to_checksum_address("0x3F6328669a86bef431Dc6F9201A5B90F7975a023"): {
            "name": "Universal Universal Router (V1_3)",
            "factory_address": {
                2: to_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"),
                3: to_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984"),
            },
        },
    }
}


class UniversalRouterSpecialAddress:
    # ref: https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/libraries/Constants.sol
    ETH = to_checksum_address("0x0000000000000000000000000000000000000000")
    MSG_SENDER = to_checksum_address("0x0000000000000000000000000000000000000001")
    ROUTER = to_checksum_address("0x0000000000000000000000000000000000000002")


class UniversalRouterSpecialValues:
    # ref: https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/libraries/Constants.sol
    V2_PAIR_ALREADY_PAID = 0
    USE_CONTRACT_BALANCE = 1 << 255


class V3RouterSpecialAddress:
    # SwapRouter.sol checks for address(0)
    # ref: https://github.com/Uniswap/v3-periphery/blob/main/contracts/SwapRouter.sol
    ROUTER_1 = to_checksum_address("0x0000000000000000000000000000000000000000")

    # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/libraries/Constants.sol
    MSG_SENDER = to_checksum_address("0x0000000000000000000000000000000000000001")
    ROUTER_2 = to_checksum_address("0x0000000000000000000000000000000000000002")


class V3RouterSpecialValues:
    # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/libraries/Constants.sol
    USE_CONTRACT_BALANCE = 0


class UniswapTransaction(BaseTransaction):
    class LeftoverRouterBalance(LedgerError):
        pass

    @classmethod
    def add_chain(cls, chain_id: int) -> None:
        try:
            _ROUTERS[chain_id]
        except Exception:
            _ROUTERS[chain_id] = {}

    @classmethod
    def add_router(cls, chain_id: int, router_address: str, router_dict: Dict[Any, Any]) -> None:
        """
        Add a new router address for a given chain ID.

        The `router_dict` argument should contain at minimum the following key-value pairs:
            - 'name': [str]
            - 'factory_address': {
                [int]: [address],
                [int]: [address],
            }

            The dicts inside 'factory_address' are keyed by the Uniswap version associated with their contracts, e.g.
            router_dict = {
                'name': 'SomeDEX',
                'factory_address': {
                    2: '0x...',
                    3: '0x...',
                }
            }
        """
        router_address = to_checksum_address(router_address)

        for key in [
            "name",
            "factory_address",
        ]:
            if key not in router_dict:
                raise ValueError(f"{key} not found in router_dict")

        try:
            _ROUTERS[chain_id][router_address]
        except Exception:
            _ROUTERS[chain_id][router_address] = router_dict

    @classmethod
    def add_wrapped_token(cls, chain_id: int, token_address: str) -> None:
        """
        Add a wrapped token address for a given chain ID.

        The method checksums the token address.
        """

        _token_address = to_checksum_address(token_address)

        try:
            WRAPPED_NATIVE_TOKENS[chain_id]
        except KeyError:
            WRAPPED_NATIVE_TOKENS[chain_id] = _token_address

    def __init__(
        self,
        chain_id: int | str,
        tx_hash: HexBytes | bytes | str,
        tx_nonce: int | str,
        tx_value: int | str,
        tx_sender: str,
        func_name: str,
        func_params: Dict[str, Any],
        router_address: str,
    ):
        """
        Build a standalone representation of a transaction submitted to a known Uniswap-based
        router contract.

        Supported contracts:
            Uniswap V2 Router
            Uniswap V2 Router 2
            Uniswap V3 Router
            Uniswap V3 Router 2
            Uniswap Universal Router
        """

        # @dev The `self.ledger` is used to track token balances for all
        # addresses involved in the swap. A positive balance represents
        # a pre-swap deposit, a negative balance represents an outstanding
        # withdrawal. A full transaction should end with a positive balance of
        # the desired output token, credited to `self.sender` or one or more
        # recipients, which are collected in the `self.to` set.

        # @dev Some routers have special flags that signify the swap amount
        # should be looked up at the time of the swap, as opposed to a
        # specified amount at the time the transaction is built. The ledger is
        # used to look up the balance at any point inside the swap, and at the
        # end to confirm that all balances have been accounted for.

        self.ledger = SimulationLedger()

        self.chain_id = int(chain_id, 16) if isinstance(chain_id, str) else chain_id
        self.routers = _ROUTERS[self.chain_id]
        self.sender = to_checksum_address(tx_sender)
        self.recipients: Set[ChecksumAddress] = set()

        router_address = to_checksum_address(router_address)
        if router_address not in self.routers:
            raise ValueError(f"Router address {router_address} unknown!")

        self.router_address = router_address

        self.v2_pool_manager: UniswapV2LiquidityPoolManager | None = None
        self.v3_pool_manager: UniswapV3LiquidityPoolManager | None = None

        try:
            self.v2_pool_manager = UniswapV2LiquidityPoolManager(
                factory_address=self.routers[router_address]["factory_address"][2]
            )
        except KeyError:
            pass

        try:
            self.v3_pool_manager = UniswapV3LiquidityPoolManager(
                factory_address=self.routers[router_address]["factory_address"][3]
            )
        except KeyError:
            pass

        self.hash = HexBytes(tx_hash)
        self.nonce = int(tx_nonce, 16) if isinstance(tx_nonce, str) else tx_nonce
        self.value = int(tx_value, 16) if isinstance(tx_value, str) else tx_value
        self.func_name = func_name
        self.func_params = func_params
        if previous_block_hash := self.func_params.get("previousBlockhash"):
            self.func_previous_block_hash = HexBytes(previous_block_hash)

        self.silent = False

    def _raise_if_past_deadline(self, deadline: int) -> None:
        if (
            self.state_block is not None
            and config.get_web3().eth.get_block(self.state_block)["timestamp"] > deadline
        ):
            raise TransactionError("Deadline expired")

    def _raise_if_block_hash_mismatch(self, block_hash: HexBytes) -> None:
        logger.info(f"Checking previousBlockhash: {block_hash!r}")
        if config.get_web3().eth.get_block("latest")["hash"] != block_hash:
            raise TransactionError("Previous block hash mismatch")

    def _show_pool_states(
        self,
        pool: LiquidityPool | V3LiquidityPool,
        sim_result: UniswapV2PoolSimulationResult | UniswapV3PoolSimulationResult,
    ) -> None:
        current_state = sim_result.initial_state
        future_state = sim_result.final_state

        # amount out is negative
        if sim_result.amount0_delta < sim_result.amount1_delta:
            token_in = pool.token1
            token_out = pool.token0
            amount_in = sim_result.amount1_delta
            amount_out = -sim_result.amount0_delta
        else:
            token_in = pool.token0
            token_out = pool.token1
            amount_in = sim_result.amount0_delta
            amount_out = -sim_result.amount1_delta

        logger.info(f"Simulating swap through pool: {pool}")
        logger.info(f"\t{amount_in} {token_in} -> {amount_out} {token_out}")
        match sim_result:
            case UniswapV2PoolSimulationResult():
                if TYPE_CHECKING:
                    assert isinstance(current_state, UniswapV2PoolState)
                    assert isinstance(future_state, UniswapV2PoolState)
                logger.info("\t(CURRENT)")
                logger.info(f"\t{pool.token0}: {current_state.reserves_token0}")
                logger.info(f"\t{pool.token1}: {current_state.reserves_token1}")
                logger.info("\t(FUTURE)")
                logger.info(f"\t{pool.token0}: {future_state.reserves_token0}")
                logger.info(f"\t{pool.token1}: {future_state.reserves_token1}")
            case UniswapV3PoolSimulationResult():
                if TYPE_CHECKING:
                    assert isinstance(current_state, UniswapV3PoolState)
                    assert isinstance(future_state, UniswapV3PoolState)
                logger.info(f"\t{amount_in} {token_in} -> {amount_out} {token_out}")
                logger.info("\t(CURRENT)")
                logger.info(f"\tprice={current_state.sqrt_price_x96}")
                logger.info(f"\tliquidity={current_state.liquidity}")
                logger.info(f"\ttick={current_state.tick}")
                logger.info("\t(FUTURE)")
                logger.info(f"\tprice={future_state.sqrt_price_x96}")
                logger.info(f"\tliquidity={future_state.liquidity}")
                logger.info(f"\ttick={future_state.tick}")

    def _simulate_v2_swap_exact_in(
        self,
        pool: LiquidityPool,
        recipient: ChecksumAddress | str,
        token_in: Erc20Token,
        amount_in: int,
        amount_out_min: int | None = None,
        first_swap: bool = False,
        last_swap: bool = False,
    ) -> Tuple[
        LiquidityPool,
        UniswapV2PoolSimulationResult,
    ]:
        assert isinstance(pool, LiquidityPool), f"Called _simulate_v2_swap_exact_in on pool {pool}"

        silent = self.silent

        if token_in not in pool.tokens:
            raise ValueError(f"Token {token_in} not found in pool {pool}")

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        if amount_in in (
            UniversalRouterSpecialValues.USE_CONTRACT_BALANCE,
            V3RouterSpecialValues.USE_CONTRACT_BALANCE,
        ):
            _balance = self.ledger.token_balance(self.router_address, token_in)

            self.ledger.transfer(
                token=token_in,
                amount=_balance,
                to_addr=pool.address,
                from_addr=self.router_address,
            )
            amount_in = self.ledger.token_balance(pool.address, token_in)

        sim_result = pool.simulate_exact_input_swap(
            token_in=token_in,
            token_in_quantity=amount_in,
        )

        _amount_out = -min(
            sim_result.amount0_delta,
            sim_result.amount1_delta,
        )

        if first_swap and not self.ledger.token_balance(pool.address, token_in):
            # credit the router if there is a zero balance
            if not self.ledger.token_balance(self.router_address, token_in):
                self.ledger.adjust(self.router_address, token_in.address, amount_in)
            self.ledger.transfer(
                token_in,
                amount=amount_in,
                from_addr=self.router_address,
                to_addr=pool.address,
            )

        # process the swap
        self.ledger.adjust(pool.address, token_in.address, -amount_in)
        self.ledger.adjust(pool.address, token_out.address, _amount_out)

        # transfer to the recipient
        self.ledger.transfer(
            token=token_out,
            amount=_amount_out,
            from_addr=pool.address,
            to_addr=recipient,
        )

        if last_swap:
            self.recipients.add(to_checksum_address(recipient))

        if last_swap and amount_out_min is not None and _amount_out < amount_out_min:
            raise TransactionError(
                f"Insufficient output for swap! {_amount_out} {token_out} received, {amount_out_min} required"
            )

        if not silent:
            self._show_pool_states(pool, sim_result)

        return pool, sim_result

    def _simulate_v2_add_liquidity(
        self,
        pool: LiquidityPool,
        added_reserves_token0: int,
        added_reserves_token1: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        return pool.simulate_add_liquidity(
            added_reserves_token0=added_reserves_token0,
            added_reserves_token1=added_reserves_token1,
            override_state=override_state,
        )

    def _simulate_v2_swap_exact_out(
        self,
        pool: LiquidityPool,
        recipient: ChecksumAddress | str,
        token_in: Erc20Token,
        amount_out: int,
        amount_in_max: int | None = None,
        first_swap: bool = False,
    ) -> Tuple[LiquidityPool, UniswapV2PoolSimulationResult]:
        assert isinstance(pool, LiquidityPool), f"Called _simulate_v2_swap_exact_out on pool {pool}"

        silent = self.silent

        if token_in not in pool.tokens:
            raise ValueError(f"Token {token_in} not found in pool {pool}")

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        sim_result = pool.simulate_exact_output_swap(
            token_out=token_out,
            token_out_quantity=amount_out,
        )

        _amount_in = max(
            sim_result.amount0_delta,
            sim_result.amount1_delta,
        )

        if first_swap:
            # transfer the input token amount from the sender to the first pool
            self.ledger.transfer(
                token=token_in.address,
                amount=_amount_in,
                from_addr=self.sender,
                to_addr=pool.address,
            )

        # process the swap
        self.ledger.adjust(pool.address, token_in.address, -_amount_in)
        self.ledger.adjust(pool.address, token_out.address, amount_out)

        # transfer the output token from the pool to the recipient
        self.ledger.transfer(
            token=token_out.address,
            amount=amount_out,
            from_addr=pool.address,
            to_addr=recipient,
        )

        if first_swap and amount_in_max is not None and _amount_in > amount_in_max:
            raise TransactionError(f"Required input {_amount_in} exceeds maximum {amount_in_max}")

        if not silent:
            self._show_pool_states(pool, sim_result)

        return pool, sim_result

    def _simulate_v3_swap_exact_in(
        self,
        pool: V3LiquidityPool,
        recipient: str,
        token_in: Erc20Token,
        amount_in: int,
        amount_out_min: int | None = None,
        sqrt_price_limit_x96: int | None = None,
        first_swap: bool = False,
    ) -> Tuple[V3LiquidityPool, UniswapV3PoolSimulationResult]:
        assert isinstance(
            pool, V3LiquidityPool
        ), f"Called _simulate_v3_swap_exact_in on pool {pool}"

        silent = self.silent

        self.recipients.add(to_checksum_address(recipient))

        if token_in not in pool.tokens:
            raise ValueError(f"Token {token_in} not found in pool {pool}")

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        if amount_in in (
            UniversalRouterSpecialValues.USE_CONTRACT_BALANCE,
            V3RouterSpecialValues.USE_CONTRACT_BALANCE,
        ):
            amount_in = self.ledger.token_balance(self.router_address, token_in.address)

        # the swap may occur after wrapping ETH, in which case amountIn will
        # be already set. If not, credit the router (user transfers the input)
        if first_swap and not self.ledger.token_balance(self.router_address, token_in.address):
            self.ledger.adjust(
                self.router_address,
                token_in.address,
                amount_in,
            )

        try:
            _sim_result = pool.simulate_exact_input_swap(
                token_in=token_in,
                token_in_quantity=amount_in,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
        except EVMRevertError as e:
            raise TransactionError(f"V3 revert: {e}")

        _amount_out = -min(
            _sim_result.amount0_delta,
            _sim_result.amount1_delta,
        )

        self.ledger.adjust(
            self.router_address,
            token_in.address,
            -amount_in,
        )

        logger.debug(f"{recipient=}")
        match recipient:
            case UniversalRouterSpecialAddress.MSG_SENDER:
                recipient = self.sender
            case (
                UniversalRouterSpecialAddress.ROUTER
                | V3RouterSpecialAddress.ROUTER_1
                | V3RouterSpecialAddress.ROUTER_2
            ):
                recipient = self.router_address
            case _:
                pass

        self.ledger.adjust(
            recipient,
            token_out.address,
            _amount_out,
        )

        if amount_out_min is not None and _amount_out < amount_out_min:
            raise TransactionError(
                f"Insufficient output for swap! {_amount_out} {token_out} received, {amount_out_min} required"
            )

        if not silent:
            self._show_pool_states(pool, _sim_result)

        return pool, _sim_result

    def _simulate_v3_swap_exact_out(
        self,
        pool: V3LiquidityPool,
        recipient: str,
        token_in: Erc20Token,
        amount_out: int,
        amount_in_max: int | None = None,
        sqrt_price_limit_x96: int | None = None,
        first_swap: bool = False,
        last_swap: bool = False,
    ) -> Tuple[V3LiquidityPool, UniswapV3PoolSimulationResult]:
        assert isinstance(
            pool, V3LiquidityPool
        ), f"Called _simulate_v3_swap_exact_out on pool {pool}"

        silent = self.silent

        self.recipients.add(to_checksum_address(recipient))

        if token_in not in pool.tokens:
            raise ValueError(f"Token {token_in} not found in pool {pool}")

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        try:
            _sim_result = pool.simulate_exact_output_swap(
                token_out=token_out,
                token_out_quantity=amount_out,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
        except EVMRevertError as e:
            raise TransactionError(f"V3 revert: {e}")

        _amount_in = max(
            _sim_result.amount0_delta,
            _sim_result.amount1_delta,
        )
        _amount_out = -min(
            _sim_result.amount0_delta,
            _sim_result.amount1_delta,
        )

        # adjust the post-swap balances for each token
        self.ledger.adjust(
            self.router_address,
            token_in,
            -_amount_in,
        )
        self.ledger.adjust(
            self.router_address,
            token_out,
            _amount_out,
        )

        if first_swap:
            # Exact output swaps proceed in reverse order, so the last
            # iteration will show a negative balance of the input token,
            # which must be accounted for.

            router_input_token_balance = self.ledger.token_balance(self.router_address, token_in)

            # Check for a balance:
            #   - If zero or positive, take no action
            #   - If negative, adjust with the assumption the user has paid
            #     that amount with the transaction call
            if router_input_token_balance < 0:
                self.ledger.adjust(
                    self.router_address,
                    token_in,
                    _amount_in,
                )

            if amount_in_max is not None and amount_in_max < _amount_in:
                raise TransactionError(
                    f"Insufficient input for exact output swap! {_amount_in} {token_in} required, {amount_in_max} provided"
                )

        if last_swap:
            match recipient:
                case UniversalRouterSpecialAddress.MSG_SENDER:
                    _recipient = self.sender
                case (
                    UniversalRouterSpecialAddress.ROUTER
                    | V3RouterSpecialAddress.ROUTER_1
                    | V3RouterSpecialAddress.ROUTER_2
                ):
                    _recipient = self.router_address
                case _:
                    _recipient = to_checksum_address(recipient)
                    self.recipients.add(_recipient)

            self.ledger.transfer(
                token=token_out.address,
                amount=_amount_out,
                from_addr=self.router_address,
                to_addr=_recipient,
            )

        if not silent:
            self._show_pool_states(pool, _sim_result)

        return pool, _sim_result

    def _simulate_unwrap(self, wrapped_token: str) -> None:
        logger.debug(f"Unwrapping {wrapped_token}")

        wrapped_token_balance = self.ledger.token_balance(self.router_address, wrapped_token)

        self.ledger.adjust(
            self.router_address,
            wrapped_token,
            -wrapped_token_balance,
        )

    def _simulate_sweep(self, token: str, recipient: str) -> None:
        logger.debug(f"Sweeping {token} to {recipient}")

        token_balance = self.ledger.token_balance(self.router_address, token)
        self.ledger.adjust(
            self.router_address,
            token,
            -token_balance,
        )
        self.ledger.adjust(
            recipient,
            token,
            token_balance,
        )

    def _simulate(
        self,
        func_name: str,
        func_params: Dict[str, Any],
    ) -> None:
        """
        Take a Uniswap V2 / V3 transaction (specified by name and a dictionary
        of arguments to that function) and return a list of pools and state
        dictionaries for all pools used by the transaction
        """

        V2_FUNCTIONS = {
            "addLiquidity",
            "addLiquidityETH",
            "swapExactTokensForETH",
            "swapExactTokensForETHSupportingFeeOnTransferTokens",
            "swapExactETHForTokens",
            "swapExactETHForTokensSupportingFeeOnTransferTokens",
            "swapExactTokensForTokens",
            "swapExactTokensForTokensSupportingFeeOnTransferTokens",
            "swapTokensForExactETH",
            "swapTokensForExactTokens",
            "swapETHForExactTokens",
        }

        V3_FUNCTIONS = {
            "exactInputSingle",
            "exactInput",
            "exactOutputSingle",
            "exactOutput",
            "increaseLiquidity",
            "multicall",
            "sweepToken",
            "unwrapWETH9",
            "unwrapWETH9WithFee",
            "wrapETH",
        }

        UNIVERSAL_ROUTER_FUNCTIONS = {
            "execute",
        }

        UNHANDLED_FUNCTIONS = {
            # TODO: handle these
            "removeLiquidity",
            "removeLiquidityETH",
            "removeLiquidityETHWithPermit",
            "removeLiquidityETHSupportingFeeOnTransferTokens",
            "removeLiquidityETHWithPermitSupportingFeeOnTransferTokens",
            "removeLiquidityWithPermit",
            "sweepTokenWithFee",
            # V3 multicall functions
            # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/base/ApproveAndCall.sol
            "mint",
        }

        NO_OP_FUNCTIONS = {
            # These functions do not affect the pool state.
            # ---
            # ref: https://docs.uniswap.org/contracts/v3/reference/periphery/interfaces/IPeripheryPayments#refundeth
            "refundETH",
            # ---
            #
            # EIP-2612 token permit functions
            # ref: https://docs.uniswap.org/contracts/v3/reference/periphery/base/SelfPermit
            "selfPermit",
            "selfPermitAllowed",
            "selfPermitAllowedIfNecessary",
            "selfPermitIfNecessary",
            # ---
            # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/base/PeripheryPaymentsExtended.sol
            "pull",
        }

        def _process_universal_router_command(
            command_type: int,
            inputs: bytes,
        ) -> None:
            # ref: https://github.com/Uniswap/universal-router/blob/main/contracts/libraries/Commands.sol
            UNIVERSAL_ROUTER_COMMAND_VALUES: Dict[int, str | None] = {
                0x00: "V3_SWAP_EXACT_IN",
                0x01: "V3_SWAP_EXACT_OUT",
                0x02: "PERMIT2_TRANSFER_FROM",
                0x03: "PERMIT2_PERMIT_BATCH",
                0x04: "SWEEP",
                0x05: "TRANSFER",
                0x06: "PAY_PORTION",
                0x07: None,  # COMMAND_PLACEHOLDER
                0x08: "V2_SWAP_EXACT_IN",
                0x09: "V2_SWAP_EXACT_OUT",
                0x0A: "PERMIT2_PERMIT",
                0x0B: "WRAP_ETH",
                0x0C: "UNWRAP_WETH",
                0x0D: "PERMIT2_TRANSFER_FROM_BATCH",
                0x0E: "BALANCE_CHECK_ERC20",
                0x0F: None,  # COMMAND_PLACEHOLDER
                0x10: "SEAPORT_V1_5",
                0x11: "LOOKS_RARE_V2",
                0x12: "NFTX",
                0x13: "CRYPTOPUNKS",
                0x14: "LOOKS_RARE_1155",  # dropped in V1_3
                0x15: "OWNER_CHECK_721",
                0x16: "OWNER_CHECK_1155",
                0x17: "SWEEP_ERC721",
                0x18: "X2Y2_721",
                0x19: "SUDOSWAP",
                0x1A: "NFT20",
                0x1B: "X2Y2_1155",
                0x1C: "FOUNDATION",
                0x1D: "SWEEP_ERC1155",
                0x1E: "ELEMENT_MARKET",
                0x1F: None,  # COMMAND_PLACEHOLDER
                0x20: "SEAPORT_V1_4",
                0x21: "EXECUTE_SUB_PLAN",
                0x22: "APPROVE_ERC20",
                0x23: None,  # COMMAND_PLACEHOLDER
                0x24: None,  # COMMAND_PLACEHOLDER
                0x25: None,  # COMMAND_PLACEHOLDER
                0x26: None,  # COMMAND_PLACEHOLDER
                0x27: None,  # COMMAND_PLACEHOLDER
                0x28: None,  # COMMAND_PLACEHOLDER
                0x29: None,  # COMMAND_PLACEHOLDER
                0x2A: None,  # COMMAND_PLACEHOLDER
                0x2B: None,  # COMMAND_PLACEHOLDER
                0x2C: None,  # COMMAND_PLACEHOLDER
                0x2D: None,  # COMMAND_PLACEHOLDER
                0x2E: None,  # COMMAND_PLACEHOLDER
                0x2F: None,  # COMMAND_PLACEHOLDER
                0x30: None,  # COMMAND_PLACEHOLDER
                0x31: None,  # COMMAND_PLACEHOLDER
                0x32: None,  # COMMAND_PLACEHOLDER
                0x33: None,  # COMMAND_PLACEHOLDER
                0x34: None,  # COMMAND_PLACEHOLDER
                0x35: None,  # COMMAND_PLACEHOLDER
                0x36: None,  # COMMAND_PLACEHOLDER
                0x37: None,  # COMMAND_PLACEHOLDER
                0x38: None,  # COMMAND_PLACEHOLDER
                0x39: None,  # COMMAND_PLACEHOLDER
                0x3A: None,  # COMMAND_PLACEHOLDER
                0x3B: None,  # COMMAND_PLACEHOLDER
                0x3C: None,  # COMMAND_PLACEHOLDER
                0x3D: None,  # COMMAND_PLACEHOLDER
                0x3E: None,  # COMMAND_PLACEHOLDER
                0x3F: None,  # COMMAND_PLACEHOLDER
            }

            UNIMPLEMENTED_UNIVERAL_ROUTER_COMMANDS = {
                "APPROVE_ERC20",
                "BALANCE_CHECK_ERC20",
                "CRYPTOPUNKS",
                "ELEMENT_MARKET",
                "EXECUTE_SUB_PLAN",
                "FOUNDATION",
                "LOOKS_RARE_1155",
                "LOOKS_RARE_721",
                "NFT20",
                "NFTX",
                "OWNER_CHECK_1155",
                "OWNER_CHECK_721",
                "PERMIT2_PERMIT_BATCH",
                "PERMIT2_PERMIT",
                "PERMIT2_TRANSFER_FROM_BATCH",
                "PERMIT2_TRANSFER_FROM",
                "SEAPORT_V2",
                "SEAPORT",
                "SUDOSWAP",
                "SWEEP_ERC1155",
                "SWEEP_ERC721",
                "TRANSFER",
                "X2Y2_1155",
                "X2Y2_721",
            }

            COMMAND_TYPE_MASK = 0x3F
            command = UNIVERSAL_ROUTER_COMMAND_VALUES[command_type & COMMAND_TYPE_MASK]

            logger.debug(f"Processing Universal Router command: {command}")

            if TYPE_CHECKING:
                _amount_in: int = 0
                _amount_out: int = 0
                _sim_result: UniswapV2PoolSimulationResult | UniswapV3PoolSimulationResult

            match command:
                case "SWEEP":
                    """
                    This function transfers the current token balance held by the contract to `recipient`
                    """

                    try:
                        (
                            sweep_token_address,
                            sweep_recipient,
                            sweep_amount_min,
                        ) = eth_abi.abi.decode(
                            types=("address", "address", "uint256"),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    match sweep_recipient:
                        case UniversalRouterSpecialAddress.MSG_SENDER:
                            sweep_recipient = self.sender
                        case UniversalRouterSpecialAddress.ROUTER:
                            sweep_recipient = self.router_address

                    sweep_token_balance = self.ledger.token_balance(
                        self.router_address, sweep_token_address
                    )

                    if sweep_token_balance < sweep_amount_min:
                        raise TransactionError(
                            f"Requested sweep of min. {sweep_amount_min} WETH, received {sweep_token_balance}"
                        )

                    self._simulate_sweep(sweep_token_address, sweep_recipient)

                case "PAY_PORTION":
                    """
                    Transfers a portion of the current token balance held by the
                    contract to `recipient`
                    """

                    try:
                        (
                            _pay_portion_token_address,
                            _pay_portion_recipient,
                            _pay_portion_bips,
                        ) = eth_abi.abi.decode(
                            types=("address", "address", "uint256"),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    # ref: https://docs.uniswap.org/contracts/universal-router/technical-reference#pay_portion
                    # ref: https://github.com/Uniswap/universal-router/blob/main/contracts/libraries/Constants.sol
                    # TODO: refactor if ledger needs to support ETH balances
                    if _pay_portion_token_address == UniversalRouterSpecialAddress.ETH:
                        logger.info("PAY_PORTION called with ETH shorthand")

                    else:
                        sweep_token_balance = self.ledger.token_balance(
                            self.router_address, _pay_portion_token_address
                        )
                        self.ledger.transfer(
                            _pay_portion_token_address,
                            sweep_token_balance * _pay_portion_bips // 10_000,
                            self.router_address,
                            _pay_portion_recipient,
                        )
                        self.recipients.add(to_checksum_address(_pay_portion_recipient))

                case "WRAP_ETH":
                    """
                    This function wraps a quantity of ETH to WETH and transfers it
                    to `recipient`.

                    The mainnet WETH contract only implements the `deposit` method,
                    so `recipient` will always be the router address.

                    Some L2s and side chains implement a `depositTo` method, so
                    `recipient` is evaluated before adjusting the ledger balance.
                    """

                    tx_recipient: ChecksumAddress
                    try:
                        _tx_recipient, _wrap_amount_min = eth_abi.abi.decode(
                            types=("address", "uint256"),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    match _tx_recipient:
                        case UniversalRouterSpecialAddress.ROUTER:
                            _recipient = self.router_address
                        case UniversalRouterSpecialAddress.MSG_SENDER:
                            _recipient = self.sender
                        case _:
                            _recipient = to_checksum_address(_tx_recipient)

                    # if tx_recipient == UniversalRouterSpecialAddress.ROUTER:
                    #     _recipient = self.router_address
                    # else:
                    #     _recipient = tx_recipient

                    _wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]

                    self.ledger.adjust(
                        _recipient,
                        _wrapped_token_address,
                        _wrap_amount_min,
                    )

                case "UNWRAP_WETH":
                    """
                    This function unwraps a quantity of WETH to ETH.

                    ETH is currently untracked by the ledger, so `recipient` is
                    unused.
                    """

                    # TODO: process ETH balance in ledger if needed

                    try:
                        _unwrap_recipient, _unwrap_amount_min = eth_abi.abi.decode(
                            types=("address", "uint256"),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    _wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                    _wrapped_token_balance = self.ledger.token_balance(
                        self.router_address, _wrapped_token_address
                    )

                    if _wrapped_token_balance < _unwrap_amount_min:
                        raise TransactionError(
                            f"Requested unwrap of min. {_unwrap_amount_min} WETH, received {_wrapped_token_balance}"
                        )

                    self._simulate_unwrap(_wrapped_token_address)

                case "V2_SWAP_EXACT_IN":
                    """
                    Decode an exact input swap through Uniswap V2 liquidity pools.

                    Returns: a list of tuples representing the pool object and the
                    final state of the pool after the swap completes.
                    """

                    try:
                        (
                            tx_recipient,
                            tx_amount_in,
                            tx_amount_out_min,
                            tx_path,
                            tx_payer_is_user,
                        ) = eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "address[]",
                                "bool",
                            ),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    try:
                        if TYPE_CHECKING:
                            assert self.v2_pool_manager is not None
                        pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                    except (LiquidityPoolError, ManagerError):
                        raise TransactionError(
                            f"LiquidityPool could not be built for all steps in path {tx_path}"
                        )

                    last_pool_pos = len(tx_path) - 2

                    for pool_pos, pool in enumerate(pools):
                        first_swap = pool_pos == 0
                        last_swap = pool_pos == last_pool_pos

                        token_in = pool.token0 if tx_path[pool_pos] == pool.token0 else pool.token1

                        _amount_in = tx_amount_in if first_swap else _amount_out

                        _recipient = tx_recipient if last_swap else pools[pool_pos + 1].address

                        match _recipient:
                            case UniversalRouterSpecialAddress.MSG_SENDER:
                                _recipient = self.sender
                            case (
                                UniversalRouterSpecialAddress.ROUTER
                                | V3RouterSpecialAddress.ROUTER_1
                                | V3RouterSpecialAddress.ROUTER_2
                            ):
                                _recipient = self.router_address

                        _, _sim_result = self._simulate_v2_swap_exact_in(
                            pool=pool,
                            recipient=_recipient,
                            token_in=token_in,
                            amount_in=_amount_in,
                            amount_out_min=tx_amount_out_min if last_swap else None,
                            first_swap=first_swap,
                            last_swap=last_swap,
                        )

                        _amount_out = -min(_sim_result.amount0_delta, _sim_result.amount1_delta)

                        self.simulated_pool_states.append((pool, _sim_result))

                case "V2_SWAP_EXACT_OUT":
                    """
                    Decode an exact output swap through Uniswap V2 liquidity pools.

                    Returns: a list of tuples representing the pool object and the
                    final state of the pool after the swap completes.
                    """

                    try:
                        (
                            tx_recipient,
                            tx_amount_out,
                            tx_amount_in_max,
                            tx_path,
                            tx_payer_is_user,
                        ) = eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "address[]",
                                "bool",
                            ),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    match tx_recipient:
                        case UniversalRouterSpecialAddress.ROUTER:
                            tx_recipient = self.router_address
                        case UniversalRouterSpecialAddress.MSG_SENDER:
                            tx_recipient = self.sender
                        case _:
                            pass

                    try:
                        if TYPE_CHECKING:
                            assert self.v2_pool_manager is not None
                        pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                    except (LiquidityPoolError, ManagerError):
                        raise TransactionError(
                            f"LiquidityPool could not be built for all steps in path {tx_path}"
                        )

                    last_pool_pos = len(pools) - 1

                    for pool_pos, pool in enumerate(pools[::-1]):
                        first_swap = pool_pos == last_pool_pos
                        last_swap = pool_pos == 0

                        _amount_out = tx_amount_out if last_swap else _amount_in
                        _amount_in_max = tx_amount_in_max if first_swap else None

                        _recipient = (
                            tx_recipient if last_swap else pools[::-1][pool_pos - 1].address
                        )

                        # get the input token by proceeding backwards from
                        # the 2nd to last position,
                        # e.g. for a token0 -> token1 -> token2 -> token3
                        # swap proceeding through pool0 -> pool1 -> pool2,
                        # - the last swap (pool_pos = 0) will have the input token
                        #   in the second to last position (index -2, token2)
                        # - the middle swap (pool_pos = 1) will have input token
                        #   in the third to last position (index -3, token1)
                        # - the first swap (pool_pos = 2) will have input token
                        #   in the four to last position (index -4, token0)
                        _token_in = (
                            pool.token0 if pool.token0 == tx_path[-2 - pool_pos] else pool.token1
                        )

                        _, _v2_sim_result = self._simulate_v2_swap_exact_out(
                            pool=pool,
                            recipient=_recipient,
                            token_in=_token_in,
                            amount_out=_amount_out,
                            amount_in_max=_amount_in_max,
                            first_swap=first_swap,
                        )

                        _amount_in = max(
                            _v2_sim_result.amount0_delta,
                            _v2_sim_result.amount1_delta,
                        )

                        self.simulated_pool_states.append((pool, _v2_sim_result))

                case "V3_SWAP_EXACT_IN":
                    """
                    Decode an exact input swap through Uniswap V3 liquidity pools.

                    Returns: a list of tuples representing the pool object and the final state of the pool after the swap completes.
                    """

                    try:
                        (
                            tx_recipient,
                            tx_amount_in,
                            tx_amount_out_min,
                            tx_path,
                            tx_payer_is_user,
                        ) = eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "bytes",
                                "bool",
                            ),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    tx_path_decoded = decode_v3_path(tx_path)

                    # decode the path - tokenIn is the first position, fee is the
                    # second position, tokenOut is the third position.
                    # paths can be an arbitrary length, but address & fee values
                    # are always interleaved. e.g. tokenIn, fee, tokenOut, fee,
                    last_token_pos = len(tx_path_decoded) - 3

                    for token_pos in range(
                        0,
                        len(tx_path_decoded) - 2,
                        2,
                    ):
                        tx_token_in_address = tx_path_decoded[token_pos]
                        fee = tx_path_decoded[token_pos + 1]
                        tx_token_out_address = tx_path_decoded[token_pos + 2]

                        if TYPE_CHECKING:
                            assert isinstance(tx_token_in_address, str)
                            assert isinstance(fee, int)
                            assert isinstance(tx_token_out_address, str)

                        first_swap = token_pos == 0
                        last_swap = token_pos == last_token_pos

                        if TYPE_CHECKING:
                            assert self.v3_pool_manager is not None
                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(
                                tx_token_in_address,
                                tx_token_out_address,
                            ),
                            pool_fee=fee,
                            silent=self.silent,
                        )

                        _recipient = (
                            tx_recipient if last_swap else UniversalRouterSpecialAddress.ROUTER
                        )
                        _token_in = (
                            v3_pool.token0
                            if v3_pool.token0 == tx_token_in_address
                            else v3_pool.token1
                        )
                        _amount_in = tx_amount_in if first_swap else _amount_out
                        _amount_out_min = tx_amount_out_min if last_swap else None

                        v3_sim_result: UniswapV3PoolSimulationResult
                        _, v3_sim_result = self._simulate_v3_swap_exact_in(
                            pool=v3_pool,
                            recipient=_recipient,
                            token_in=_token_in,
                            amount_in=_amount_in,
                            amount_out_min=_amount_out_min,
                            first_swap=first_swap,
                        )

                        _amount_out = -min(
                            v3_sim_result.amount0_delta,
                            v3_sim_result.amount1_delta,
                        )

                        self.simulated_pool_states.append((v3_pool, v3_sim_result))

                case "V3_SWAP_EXACT_OUT":
                    """
                    Decode an exact output swap through Uniswap V3 liquidity pools.

                    Returns: a list of tuples representing the pool object and the
                    final state of the pool after the swap completes.
                    """

                    try:
                        (
                            tx_recipient,
                            tx_amount_out,
                            tx_amount_in_max,
                            tx_path,
                            tx_payer_is_user,
                        ) = eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "bytes",
                                "bool",
                            ),
                            data=inputs,
                        )
                    except Exception:
                        raise ValueError(f"Could not decode input for {command}")

                    tx_path_decoded = decode_v3_path(tx_path)

                    # An exact output path is encoded in REVERSE order,
                    # tokenOut is the first position, tokenIn is the second
                    # position. e.g. tokenOut, fee, tokenIn
                    last_token_pos = len(tx_path_decoded) - 3

                    for token_pos in range(
                        0,
                        len(tx_path_decoded) - 2,
                        2,
                    ):
                        tx_token_out_address = tx_path_decoded[token_pos]
                        fee = tx_path_decoded[token_pos + 1]
                        tx_token_in_address = tx_path_decoded[token_pos + 2]

                        if TYPE_CHECKING:
                            assert isinstance(tx_token_in_address, str)
                            assert isinstance(tx_token_out_address, str)
                            assert isinstance(fee, int)

                        first_swap = token_pos == last_token_pos
                        last_swap = token_pos == 0

                        if TYPE_CHECKING:
                            assert self.v3_pool_manager is not None
                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(
                                tx_token_in_address,
                                tx_token_out_address,
                            ),
                            pool_fee=fee,
                            silent=self.silent,
                        )

                        _recipient = (
                            tx_recipient if last_swap else UniversalRouterSpecialAddress.ROUTER
                        )
                        _token_in = (
                            v3_pool.token0
                            if v3_pool.token0 == tx_token_in_address
                            else v3_pool.token1
                        )
                        _amount_out = tx_amount_out if last_swap else _amount_in
                        _amount_in_max = tx_amount_in_max if first_swap else None

                        _, v3_sim_result = self._simulate_v3_swap_exact_out(
                            pool=v3_pool,
                            recipient=_recipient,
                            token_in=_token_in,
                            amount_out=_amount_out,
                            amount_in_max=_amount_in_max,
                            first_swap=first_swap,
                            last_swap=last_swap,
                        )

                        _amount_in = max(
                            v3_sim_result.amount0_delta,
                            v3_sim_result.amount1_delta,
                        )

                        # check that the output of each intermediate swap meets
                        # the input for the next swap
                        if not last_swap:
                            # pool states are appended to `future_pool_states`
                            # so the previous swap will be in the last position
                            _, _last_sim_result = self.simulated_pool_states[-1]

                            if TYPE_CHECKING:
                                assert isinstance(
                                    _last_sim_result,
                                    (UniswapV2PoolSimulationResult, UniswapV2PoolSimulationResult),
                                )

                            _last_amount_in = max(
                                _last_sim_result.amount1_delta,
                                _last_sim_result.amount0_delta,
                            )

                            if _amount_out != _last_amount_in:
                                raise TransactionError(
                                    f"Insufficient swap amount through requested pool {v3_pool}. Needed {_last_amount_in}, received {_amount_out}"
                                )

                        self.simulated_pool_states.append((v3_pool, v3_sim_result))

                case _:
                    if command in UNIMPLEMENTED_UNIVERAL_ROUTER_COMMANDS:
                        logger.debug(f"UNIMPLEMENTED COMMAND: {command}")
                    else:  # pragma: no cover
                        raise ValueError(f"Invalid command {command}")

        def _process_v3_multicall(
            params: Dict[str, Any],
        ) -> None:
            try:
                self._raise_if_block_hash_mismatch(params["previousBlockhash"])
            except KeyError:
                pass

            for payload in params["data"]:
                try:
                    # decode with Router ABI
                    payload_func, payload_args = (
                        Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                        .decode_function_input(payload)
                    )
                except Exception:
                    pass

                try:
                    # decode with Router2 ABI
                    payload_func, payload_args = (
                        Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                        .decode_function_input(payload)
                    )
                except Exception:
                    pass

                # special case to handle a multicall encoded within
                # another multicall
                if payload_func.fn_name == "multicall":
                    logger.debug("Unwrapping nested multicall")

                    try:
                        self._raise_if_block_hash_mismatch(params["previousBlockhash"])
                    except KeyError:
                        pass

                    for payload in payload_args["data"]:
                        try:
                            _func, _params = (
                                Web3()
                                .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                                .decode_function_input(payload)
                            )
                        except Exception:
                            pass

                        try:
                            _func, _params = (
                                Web3()
                                .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                                .decode_function_input(payload)
                            )
                        except Exception:
                            pass

                        try:
                            self._simulate(
                                func_name=_func.fn_name,
                                func_params=_params,
                            )
                        except Exception as e:
                            raise ValueError(f"Could not decode nested multicall: {e}") from e
                else:
                    try:
                        self._simulate(
                            func_name=payload_func.fn_name,
                            func_params=payload_args,
                        )
                    except TransactionError:
                        raise
                    except Exception as e:
                        import traceback

                        traceback.print_exc()
                        raise ValueError(f"Could not decode multicall: {e}")

        def _process_uniswap_v2_transaction() -> None:
            if TYPE_CHECKING:
                _amount_in: int = 0
                _amount_out: int = 0
                assert self.v2_pool_manager is not None

            try:
                match func_name:
                    case (
                        "swapExactTokensForETH"
                        | "swapExactTokensForETHSupportingFeeOnTransferTokens"
                        | "swapExactETHForTokens"
                        | "swapExactETHForTokensSupportingFeeOnTransferTokens"
                        | "swapExactTokensForTokens"
                        | "swapExactTokensForTokensSupportingFeeOnTransferTokens"
                    ):
                        logger.debug(f"{func_name}: {self.hash.hex()=}")

                        try:
                            tx_amount_in = func_params["amountIn"]
                        except KeyError:
                            tx_amount_in = self.value  # 'swapExactETHForTokens'

                        tx_amount_out_min = func_params["amountOutMin"]
                        tx_path = func_params["path"]

                        tx_recipient = func_params["to"]
                        if tx_recipient == UniversalRouterSpecialAddress.ROUTER:
                            tx_recipient = self.router_address

                        try:
                            tx_deadline = func_params["deadline"]
                        except KeyError:
                            pass
                        else:
                            self._raise_if_past_deadline(tx_deadline)

                        try:
                            pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                        except (LiquidityPoolError, ManagerError):
                            raise TransactionError(
                                f"LiquidityPool could not be built for all steps in path {tx_path}"
                            )

                        last_pool_pos = len(tx_path) - 2

                        for pool_pos, pool in enumerate(pools):
                            first_swap = pool_pos == 0
                            last_swap = pool_pos == last_pool_pos

                            _token_in = (
                                pool.token0 if tx_path[pool_pos] == pool.token0 else pool.token1
                            )

                            _amount_in = tx_amount_in if first_swap else _amount_out

                            _recipient = tx_recipient if last_swap else pools[pool_pos + 1].address

                            _, sim_result = self._simulate_v2_swap_exact_in(
                                pool=pool,
                                recipient=_recipient,
                                token_in=_token_in,
                                amount_in=_amount_in,
                                amount_out_min=tx_amount_out_min if last_swap else None,
                                first_swap=first_swap,
                                last_swap=last_swap,
                            )

                            _amount_out = -min(sim_result.amount0_delta, sim_result.amount1_delta)

                            self.simulated_pool_states.append((pool, sim_result))

                    case (
                        "swapTokensForExactETH"
                        | "swapTokensForExactTokens"
                        | "swapETHForExactTokens"
                    ):
                        logger.debug(f"{func_name}: {self.hash.hex()=}")

                        tx_amount_out = func_params["amountOut"]
                        try:
                            tx_amount_in_max = func_params["amountInMax"]
                        except KeyError:
                            tx_amount_in_max = self.value  # 'swapETHForExactTokens'
                        tx_path = func_params["path"]

                        tx_recipient = func_params["to"]
                        if tx_recipient == UniversalRouterSpecialAddress.ROUTER:
                            tx_recipient = self.router_address
                        else:
                            self.recipients.add(tx_recipient)

                        try:
                            tx_deadline = func_params["deadline"]
                        except KeyError:
                            pass
                        else:
                            self._raise_if_past_deadline(tx_deadline)

                        try:
                            pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                        except (LiquidityPoolError, ManagerError):
                            raise TransactionError(
                                f"LiquidityPool could not be built for all steps in path {tx_path}"
                            )

                        last_pool_pos = len(pools) - 1

                        for pool_pos, pool in enumerate(pools[::-1]):
                            first_swap = pool_pos == last_pool_pos
                            last_swap = pool_pos == 0

                            _amount_out = tx_amount_out if last_swap else _amount_in
                            _amount_in_max = tx_amount_in_max if first_swap else None

                            _recipient = (
                                tx_recipient if last_swap else pools[::-1][pool_pos - 1].address
                            )

                            # get the input token by proceeding backwards from
                            # the 2nd to last position,
                            # e.g. for a token0 -> token1 -> token2 -> token3
                            # swap proceeding through pool0 -> pool1 -> pool2,
                            # - the last swap (pool_pos = 0) will have the input token
                            #   in the second to last position (index -2, token2)
                            # - the middle swap (pool_pos = 1) will have input token
                            #   in the third to last position (index -3, token1)
                            # - the first swap (pool_pos = 2) will have input token
                            #   in the four to last position (index -4, token0)
                            _token_in = (
                                pool.token0
                                if pool.token0 == tx_path[-2 - pool_pos]
                                else pool.token1
                            )

                            _, _sim_result = self._simulate_v2_swap_exact_out(
                                pool=pool,
                                recipient=_recipient,
                                token_in=_token_in,
                                amount_out=_amount_out,
                                amount_in_max=_amount_in_max,
                                first_swap=first_swap,
                            )

                            _amount_in = max(
                                _sim_result.amount0_delta,
                                _sim_result.amount1_delta,
                            )

                            if first_swap:
                                self.ledger.adjust(
                                    self.sender,
                                    WRAPPED_NATIVE_TOKENS[self.chain_id],
                                    _amount_in,
                                )

                            self.simulated_pool_states.append((pool, _sim_result))

                    case "addLiquidity" | "addLiquidityETH":
                        logger.debug(f"{func_name}: {self.hash.hex()=}")

                        if func_name == "addLiquidity":
                            tx_token_a = to_checksum_address(func_params["tokenA"])
                            tx_token_b = to_checksum_address(func_params["tokenB"])
                            tx_token_amount_a = func_params["amountADesired"]
                            tx_token_amount_b = func_params["amountBDesired"]
                            tx_token_amount_a_min = func_params["amountAMin"]  # noqa: F841
                            tx_token_amount_b_min = func_params["amountBMin"]  # noqa: F841
                            token0_address, token1_address = (
                                (tx_token_a, tx_token_b)
                                if tx_token_a < tx_token_b
                                else (tx_token_b, tx_token_a)
                            )
                            token0_amount, token1_amount = (
                                (tx_token_amount_a, tx_token_amount_b)
                                if tx_token_a < tx_token_b
                                else (tx_token_amount_b, tx_token_amount_a)
                            )
                        elif func_name == "addLiquidityETH":
                            tx_token = to_checksum_address(func_params["token"])
                            tx_token_amount = func_params["amountTokenDesired"]
                            tx_token_amount_min = func_params["amountTokenMin"]  # noqa: F841
                            tx_eth_min = func_params["amountETHMin"]
                            _wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                            token0_address, token1_address = (
                                (_wrapped_token_address, tx_token)
                                if _wrapped_token_address < tx_token
                                else (tx_token, _wrapped_token_address)
                            )
                            token0_amount, token1_amount = (
                                (tx_eth_min, tx_token_amount)
                                if _wrapped_token_address < tx_token
                                else (tx_token_amount, tx_eth_min)
                            )

                        tx_to = func_params["to"]  # noqa: F841
                        tx_deadline = func_params["deadline"]
                        self._raise_if_past_deadline(tx_deadline)

                        try:
                            _pool = self.v2_pool_manager.get_pool(
                                token_addresses=(token0_address, token1_address),
                                silent=self.silent,
                            )
                        except ManagerError:
                            _pool = LiquidityPool(
                                address=generate_v2_pool_address(
                                    token_addresses=(
                                        token0_address,
                                        token1_address,
                                    ),
                                    factory_address=self.v2_pool_manager._factory_address,
                                    init_hash=self.v2_pool_manager._factory_init_hash,
                                ),
                                tokens=[
                                    self.v2_pool_manager._token_manager.get_erc20token(
                                        token0_address
                                    ),
                                    self.v2_pool_manager._token_manager.get_erc20token(
                                        token1_address
                                    ),
                                ],
                                factory_address=self.v2_pool_manager._factory_address,
                                factory_init_hash=self.v2_pool_manager._factory_init_hash,
                                empty=True,
                                silent=self.silent,
                            )

                        _sim_result = self._simulate_v2_add_liquidity(
                            pool=_pool,
                            added_reserves_token0=token0_amount,
                            added_reserves_token1=token1_amount,
                            override_state=UniswapV2PoolState(
                                pool=_pool.address,
                                reserves_token0=0,
                                reserves_token1=0,
                            ),
                        )

                        self.simulated_pool_states.append((_pool, _sim_result))

                    case _:
                        raise ValueError(f"Unknown function: {func_name}!")

            except TransactionError:
                # Catch specific subclass exception to prevent nested
                # multicalls from being recursively re-annotated
                # e.g. 'Simulation failed: Simulation failed: {error}'
                raise
            except DegenbotError as e:
                raise TransactionError(f"Simulation failed: {e}") from e

        def _process_uniswap_v3_transaction() -> None:
            logger.debug(f"{func_name}: {self.hash.hex()=}")

            if TYPE_CHECKING:
                v3_sim_result: UniswapV3PoolSimulationResult
                assert self.v3_pool_manager is not None

            silent = self.silent

            try:
                match func_name:
                    case "multicall":
                        _process_v3_multicall(params=func_params)

                    case "exactInputSingle":
                        # Extract parameters from the dict results of web3py v6
                        if isinstance(func_params["params"], dict):
                            tx_token_in_address = func_params["params"]["tokenIn"]
                            tx_token_out_address = func_params["params"]["tokenOut"]
                            tx_fee = func_params["params"]["fee"]
                            tx_recipient = func_params["params"]["recipient"]
                            tx_amount_in = func_params["params"]["amountIn"]
                            tx_amount_out_min = func_params["params"]["amountOutMinimum"]
                            tx_deadline = func_params["params"].get("deadline")
                            tx_sqrt_price_limit_x96 = func_params["params"]["sqrtPriceLimitX96"]
                        # Extract parameters from the tuple results of web3py v5
                        elif isinstance(func_params["params"], tuple):
                            # Decode with ISwapRouter ABI
                            # ref: https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                            if len(func_params["params"]) == 8:
                                (
                                    tx_token_in_address,
                                    tx_token_out_address,
                                    tx_fee,
                                    tx_recipient,
                                    tx_deadline,
                                    tx_amount_in,
                                    tx_amount_out_min,
                                    tx_sqrt_price_limit_x96,
                                ) = func_params["params"]

                            # Decode with IV3SwapRouter ABI (aka Router2)
                            # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                            elif len(func_params["params"]) == 7:
                                (
                                    tx_token_in_address,
                                    tx_token_out_address,
                                    tx_fee,
                                    tx_recipient,
                                    tx_amount_in,
                                    tx_amount_out_min,
                                    tx_sqrt_price_limit_x96,
                                ) = func_params["params"]
                                tx_deadline = None
                            else:
                                raise ValueError(
                                    f"Could not extract parameters for function {func_name} with parameters {func_params['params']}"
                                )
                        else:
                            raise ValueError(
                                f'Could not identify type for function params. Expected tuple or dict, got {type(func_params["params"])}'
                            )

                        if tx_deadline:
                            self._raise_if_past_deadline(tx_deadline)

                        if TYPE_CHECKING:
                            assert isinstance(self.v3_pool_manager, UniswapV3LiquidityPoolManager)
                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(
                                tx_token_in_address,
                                tx_token_out_address,
                            ),
                            pool_fee=tx_fee,
                            silent=self.silent,
                        )

                        _, v3_sim_result = self._simulate_v3_swap_exact_in(
                            pool=v3_pool,
                            recipient=tx_recipient,
                            token_in=(
                                v3_pool.token0
                                if v3_pool.token0 == tx_token_in_address
                                else v3_pool.token1
                            ),
                            amount_in=tx_amount_in,
                            amount_out_min=tx_amount_out_min,
                            first_swap=True,
                        )

                        self.simulated_pool_states.append((v3_pool, v3_sim_result))

                        token_out_quantity = -min(
                            v3_sim_result.amount1_delta,
                            v3_sim_result.amount0_delta,
                        )

                    case "exactInput":
                        # Extract parameters from the dict results of web3py v6
                        if isinstance(func_params["params"], dict):
                            tx_path = func_params["params"]["path"]
                            tx_recipient = func_params["params"]["recipient"]
                            tx_deadline = func_params["params"].get("deadline")
                            tx_amount_in = func_params["params"]["amountIn"]
                            tx_amount_out_minimum = func_params["params"]["amountOutMinimum"]
                        # Extract parameters from the tuple results of web3py v5
                        elif isinstance(func_params["params"], tuple):
                            # Decode with ISwapRouter ABI
                            # ref: https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                            if len(func_params["params"]) == 5:
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_deadline,
                                    tx_amount_in,
                                    tx_amount_out_minimum,
                                ) = func_params["params"]

                            # Decode with IV3SwapRouter ABI (aka Router2)
                            # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                            elif len(func_params["params"]) == 4:
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_amount_in,
                                    tx_amount_out_minimum,
                                ) = func_params["params"]
                                tx_deadline = None
                            else:
                                raise ValueError(
                                    f"Could not extract parameters for function {func_name} with parameters {func_params['params']}"
                                )
                        else:
                            raise ValueError(
                                f'Could not identify type for function params. Expected tuple or dict, got {type(func_params["params"])}'
                            )

                        if tx_deadline:
                            self._raise_if_past_deadline(tx_deadline)

                        tx_path_decoded = decode_v3_path(tx_path)

                        if not silent:
                            logger.info(f" • path = {tx_path_decoded}")
                            logger.info(f" • recipient = {tx_recipient}")
                            try:
                                tx_deadline
                            except Exception:
                                pass
                            else:
                                logger.info(f" • deadline = {tx_deadline}")
                            logger.info(f" • amountIn = {tx_amount_in}")
                            logger.info(f" • amountOutMinimum = {tx_amount_out_minimum}")

                        last_token_pos = len(tx_path_decoded) - 3

                        for token_pos in range(
                            0,
                            len(tx_path_decoded) - 2,
                            2,
                        ):
                            tx_token_in_address = tx_path_decoded[token_pos]
                            tx_fee = tx_path_decoded[token_pos + 1]
                            tx_token_out_address = tx_path_decoded[token_pos + 2]

                            if TYPE_CHECKING:
                                assert isinstance(tx_token_in_address, str)
                                assert isinstance(tx_fee, int)
                                assert isinstance(tx_token_out_address, str)

                            first_swap = token_pos == 0
                            last_swap = token_pos == last_token_pos

                            if TYPE_CHECKING:
                                assert isinstance(
                                    self.v3_pool_manager, UniswapV3LiquidityPoolManager
                                )
                            v3_pool = self.v3_pool_manager.get_pool(
                                token_addresses=(
                                    tx_token_in_address,
                                    tx_token_out_address,
                                ),
                                pool_fee=tx_fee,
                                silent=self.silent,
                            )

                            _, v3_sim_result = self._simulate_v3_swap_exact_in(
                                pool=v3_pool,
                                recipient=tx_recipient
                                if last_swap
                                else UniversalRouterSpecialAddress.ROUTER,
                                token_in=v3_pool.token0
                                if v3_pool.token0 == tx_token_in_address
                                else v3_pool.token1,
                                amount_in=tx_amount_in if token_pos == 0 else token_out_quantity,
                                # only apply minimum output to the last swap
                                amount_out_min=tx_amount_out_minimum if last_swap else None,
                                first_swap=first_swap,
                            )

                            self.simulated_pool_states.append((v3_pool, v3_sim_result))

                            token_out_quantity = -min(
                                v3_sim_result.amount0_delta,
                                v3_sim_result.amount1_delta,
                            )

                    case "exactOutputSingle":
                        # Extract parameters from the dict results of web3py v6
                        if isinstance(func_params["params"], dict):
                            tx_token_in_address = func_params["params"]["tokenIn"]
                            tx_token_out_address = func_params["params"]["tokenOut"]
                            tx_fee = func_params["params"]["fee"]
                            tx_recipient = func_params["params"]["recipient"]
                            tx_deadline = func_params["params"].get("deadline")
                            tx_amount_out = func_params["params"]["amountOut"]
                            tx_amount_in_max = func_params["params"]["amountInMaximum"]
                            tx_sqrt_price_limit_x96 = func_params["params"]["sqrtPriceLimitX96"]
                        # Extract parameters from the tuple results of web3py v5
                        elif isinstance(func_params["params"], tuple):
                            # Decode with ISwapRouter ABI
                            # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                            if len(func_params["params"]) == 8:
                                (
                                    tx_token_in_address,
                                    tx_token_out_address,
                                    tx_fee,
                                    tx_recipient,
                                    tx_deadline,
                                    tx_amount_out,
                                    tx_amount_in_max,
                                    tx_sqrt_price_limit_x96,
                                ) = func_params["params"]

                            # Decode with IV3SwapRouter ABI (aka Router2)
                            # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                            elif len(func_params["params"]) == 7:
                                (
                                    tx_token_in_address,
                                    tx_token_out_address,
                                    tx_fee,
                                    tx_recipient,
                                    tx_amount_out,
                                    tx_amount_in_max,
                                    tx_sqrt_price_limit_x96,
                                ) = func_params["params"]
                                tx_deadline = None
                            else:
                                raise ValueError(
                                    f"Could not extract parameters for function {func_name} with parameters {func_params['params']}"
                                )
                        else:
                            raise ValueError(
                                f'Could not identify type for function params. Expected tuple or dict, got {type(func_params["params"])}'
                            )

                        if tx_deadline:
                            self._raise_if_past_deadline(tx_deadline)

                        if TYPE_CHECKING:
                            assert isinstance(self.v3_pool_manager, UniswapV3LiquidityPoolManager)
                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(
                                tx_token_in_address,
                                tx_token_out_address,
                            ),
                            pool_fee=tx_fee,
                            silent=self.silent,
                        )

                        _, v3_sim_result = self._simulate_v3_swap_exact_out(
                            pool=v3_pool,
                            recipient=tx_recipient,
                            token_in=v3_pool.token0
                            if v3_pool.token0 == tx_token_in_address
                            else v3_pool.token1,
                            amount_out=tx_amount_out,
                            amount_in_max=tx_amount_in_max,
                            first_swap=True,
                            last_swap=True,
                        )

                        self.simulated_pool_states.append((v3_pool, v3_sim_result))

                        amount_deposited = max(
                            v3_sim_result.amount0_delta,
                            v3_sim_result.amount1_delta,
                        )

                        if amount_deposited > tx_amount_in_max:
                            raise TransactionError(
                                f"Maximum input exceeded. Specified {tx_amount_in_max}, {amount_deposited} required."
                            )

                    case "exactOutput":
                        # Extract parameters from the dict results of web3py v6
                        if isinstance(func_params["params"], dict):
                            tx_path = func_params["params"]["path"]
                            tx_recipient = func_params["params"]["recipient"]
                            tx_deadline = func_params["params"].get("deadline")
                            tx_amount_out = func_params["params"]["amountOut"]
                            tx_amount_in_max = func_params["params"]["amountInMaximum"]
                        # Extract parameters from the tuple results of web3py v5
                        elif isinstance(func_params["params"], tuple):
                            # Decode with ISwapRouter ABI
                            # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                            if len(func_params["params"]) == 5:
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_deadline,
                                    tx_amount_out,
                                    tx_amount_in_max,
                                ) = func_params["params"]
                            # Decode with IV3SwapRouter ABI (aka Router2)
                            # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                            elif len(func_params["params"]) == 4:
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_amount_out,
                                    tx_amount_in_max,
                                ) = func_params["params"]
                                tx_deadline = None
                            else:
                                raise ValueError(
                                    f"Could not extract parameters for function {func_name} with parameters {func_params['params']}"
                                )
                        else:
                            raise ValueError(
                                f'Could not identify type for function params. Expected tuple or dict, got {type(func_params["params"])}'
                            )

                        if tx_deadline:
                            self._raise_if_past_deadline(tx_deadline)

                        tx_path_decoded = decode_v3_path(tx_path)

                        if not silent:
                            logger.info(f" • path = {tx_path_decoded}")
                            logger.info(f" • recipient = {tx_recipient}")
                            try:
                                tx_deadline
                            except Exception:
                                pass
                            else:
                                logger.info(f" • deadline = {tx_deadline}")
                            logger.info(f" • amountOut = {tx_amount_out}")
                            logger.info(f" • amountInMaximum = {tx_amount_in_max}")

                        # an exact output path is encoded in REVERSE order,
                        # tokenOut is the first position, tokenIn is the second
                        # position. e.g. tokenOut, fee, tokenIn
                        last_token_pos = len(tx_path_decoded) - 3

                        _amount_in = 0

                        for token_pos in range(
                            0,
                            len(tx_path_decoded) - 2,
                            2,
                        ):
                            tx_token_out_address = tx_path_decoded[token_pos]
                            tx_fee = tx_path_decoded[token_pos + 1]
                            tx_token_in_address = tx_path_decoded[token_pos + 2]

                            first_swap = token_pos == last_token_pos
                            last_swap = token_pos == 0

                            if TYPE_CHECKING:
                                assert isinstance(
                                    self.v3_pool_manager, UniswapV3LiquidityPoolManager
                                )
                            v3_pool = self.v3_pool_manager.get_pool(
                                token_addresses=(
                                    tx_token_in_address,
                                    tx_token_out_address,
                                ),
                                pool_fee=tx_fee,
                                silent=self.silent,
                            )

                            _recipient = (
                                tx_recipient if last_swap else UniversalRouterSpecialAddress.ROUTER
                            )
                            _token_in = (
                                v3_pool.token0
                                if v3_pool.token0 == tx_token_in_address
                                else v3_pool.token1
                            )
                            _amount_out = tx_amount_out if last_swap else _amount_in
                            _amount_in_max = tx_amount_in_max if first_swap else None

                            _, v3_sim_result = self._simulate_v3_swap_exact_out(
                                pool=v3_pool,
                                recipient=_recipient,
                                token_in=_token_in,
                                amount_out=_amount_out,
                                amount_in_max=_amount_in_max,
                                first_swap=first_swap,
                                last_swap=last_swap,
                            )

                            _amount_in = max(
                                v3_sim_result.amount0_delta,
                                v3_sim_result.amount1_delta,
                            )

                            _amount_out = -min(
                                v3_sim_result.amount0_delta,
                                v3_sim_result.amount1_delta,
                            )

                            # check that the output of each intermediate swap meets
                            # the input for the next swap
                            if not last_swap:
                                # pool states are appended to `future_pool_states`
                                # so the previous swap will be in the last position
                                (
                                    _,
                                    _last_sim_result,
                                ) = self.simulated_pool_states[-1]

                                if TYPE_CHECKING:
                                    assert isinstance(
                                        _last_sim_result,
                                        (
                                            UniswapV2PoolSimulationResult,
                                            UniswapV2PoolSimulationResult,
                                        ),
                                    )

                                _last_amount_in = max(
                                    _last_sim_result.amount0_delta,
                                    _last_sim_result.amount1_delta,
                                )

                                if _amount_out != _last_amount_in:
                                    raise TransactionError(
                                        f"Insufficient swap amount through requested pool {v3_pool}. Needed {_last_amount_in}, received {_amount_out}"
                                    )

                            self.simulated_pool_states.append((v3_pool, v3_sim_result))

                        # V3 Router enforces a maximum input
                        if first_swap:
                            _sim_result: BaseSimulationResult
                            _, _sim_result = self.simulated_pool_states[-1]

                            amount_deposited = max(
                                _sim_result.amount0_delta,
                                _sim_result.amount1_delta,
                            )

                            if amount_deposited > tx_amount_in_max:
                                raise TransactionError(
                                    f"Maximum input exceeded. Specified {tx_amount_in_max}, {amount_deposited} required."
                                )

                    case "unwrapWETH9":
                        # TODO: if ETH balances are ever needed, handle the
                        # ETH transfer resulting from this function
                        amountMin = func_params["amountMinimum"]
                        wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                        wrapped_token_balance = self.ledger.token_balance(
                            self.router_address, wrapped_token_address
                        )
                        if wrapped_token_balance < amountMin:
                            raise TransactionError(
                                f"Requested unwrap of min. {amountMin} WETH, received {wrapped_token_balance}"
                            )

                        self._simulate_unwrap(wrapped_token_address)

                    case "unwrapWETH9WithFee":
                        # TODO: if ETH balances are ever needed, handle the
                        # two ETH transfers resulting from this function
                        _amount_in = func_params["amountMinimum"]
                        _recipient = func_params["recipient"]
                        _fee_bips = func_params["feeBips"]
                        _fee_recipient = func_params["feeRecipient"]

                        wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                        wrapped_token_balance = self.ledger.token_balance(
                            self.router_address, wrapped_token_address
                        )
                        if wrapped_token_balance < _amount_in:
                            raise TransactionError(
                                f"Requested unwrap of min. {_amount_in} WETH, received {wrapped_token_balance}"
                            )

                        self._simulate_unwrap(wrapped_token_address)

                    case "sweepToken":
                        """
                        This function transfers the current token balance
                        held by the contract to `recipient`
                        """

                        try:
                            tx_token_address = func_params["token"]
                            tx_amount_out_minimum = func_params["amountMinimum"]
                            tx_recipient = func_params.get("recipient")
                        except Exception as e:
                            print(e)
                        else:
                            # Router2 ABI omits `recipient`, always uses
                            # `msg.sender`
                            if tx_recipient is None:
                                tx_recipient = self.sender
                            else:
                                self.recipients.add(tx_recipient)

                        _balance = self.ledger.token_balance(self.router_address, tx_token_address)

                        if _balance < tx_amount_out_minimum:
                            raise TransactionError(
                                f"Requested sweep of min. {tx_amount_out_minimum} {tx_token_address}, received {_balance}"
                            )

                        self._simulate_sweep(tx_token_address, tx_recipient)

                    case "wrapETH":
                        _wrapped_token_amount = func_params["value"]
                        _wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                        self.ledger.adjust(
                            self.router_address,
                            _wrapped_token_address,
                            _wrapped_token_amount,
                        )

                    case "increaseLiquidity":
                        import json
                        from degenbot.uniswap.v3_libraries import FullMath, TickMath, constants

                        def getLiquidityForAmount0(
                            sqrtRatioAX96: int, sqrtRatioBX96: int, amount0: int
                        ) -> int:
                            if sqrtRatioAX96 > sqrtRatioBX96:
                                (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96)
                            intermediate = FullMath.mulDiv(
                                sqrtRatioAX96, sqrtRatioBX96, constants.Q96
                            )
                            return FullMath.mulDiv(
                                amount0, intermediate, sqrtRatioBX96 - sqrtRatioAX96
                            )

                        def getLiquidityForAmount1(
                            sqrtRatioAX96: int, sqrtRatioBX96: int, amount1: int
                        ) -> int:
                            if sqrtRatioAX96 > sqrtRatioBX96:
                                (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96)
                            return FullMath.mulDiv(
                                amount1, constants.Q96, sqrtRatioBX96 - sqrtRatioAX96
                            )

                        def getLiquidityForAmounts(
                            sqrtRatioX96: int,
                            sqrtRatioAX96: int,
                            sqrtRatioBX96: int,
                            amount0: int,
                            amount1: int,
                        ) -> int:
                            if sqrtRatioAX96 > sqrtRatioBX96:
                                (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96)

                            if sqrtRatioX96 <= sqrtRatioAX96:
                                liquidity = getLiquidityForAmount0(
                                    sqrtRatioAX96, sqrtRatioBX96, amount0
                                )
                            elif sqrtRatioX96 < sqrtRatioBX96:
                                liquidity0 = getLiquidityForAmount0(
                                    sqrtRatioX96, sqrtRatioBX96, amount0
                                )
                                liquidity1 = getLiquidityForAmount1(
                                    sqrtRatioAX96, sqrtRatioX96, amount1
                                )

                                liquidity = liquidity0 if liquidity0 < liquidity1 else liquidity1
                            else:
                                liquidity = getLiquidityForAmount1(
                                    sqrtRatioAX96, sqrtRatioBX96, amount1
                                )

                            return liquidity

                        # struct IncreaseLiquidityParams {
                        #     address token0;
                        #     address token1;
                        #     uint256 tokenId;
                        #     uint256 amount0Min;
                        #     uint256 amount1Min;
                        # }

                        # Decode inputs

                        logger.info(f"{func_params=}")

                        token0 = func_params["params"]["token0"]
                        token1 = func_params["params"]["token1"]
                        token_id = func_params["params"]["tokenId"]
                        amount0_min = func_params["params"]["amount0Min"]
                        amount0_desired = self.ledger.token_balance(self.router_address, token0)
                        amount1_min = func_params["params"]["amount1Min"]
                        amount1_desired = self.ledger.token_balance(self.router_address, token1)

                        logger.info(f"{token0=}")
                        logger.info(f"{token1=}")
                        logger.info(f"{token_id=}")
                        logger.info(f"{amount0_min=}")
                        logger.info(f"{amount1_min=}")
                        logger.info(f"{amount0_desired=}")
                        logger.info(f"{amount1_desired=}")

                        positions_contract = config.get_web3().eth.contract(
                            address=to_checksum_address(
                                "0xC36442b4a4522E871399CD717aBDD847Ab11FE88"
                            ),
                            abi=json.loads(
                                """
                                [{"inputs":[{"internalType":"address","name":"_factory","type":"address"},{"internalType":"address","name":"_WETH9","type":"address"},{"internalType":"address","name":"_tokenDescriptor_","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"owner","type":"address"},{"indexed":true,"internalType":"address","name":"approved","type":"address"},{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"Approval","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"owner","type":"address"},{"indexed":true,"internalType":"address","name":"operator","type":"address"},{"indexed":false,"internalType":"bool","name":"approved","type":"bool"}],"name":"ApprovalForAll","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"},{"indexed":false,"internalType":"address","name":"recipient","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"Collect","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"},{"indexed":false,"internalType":"uint128","name":"liquidity","type":"uint128"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"DecreaseLiquidity","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"},{"indexed":false,"internalType":"uint128","name":"liquidity","type":"uint128"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"IncreaseLiquidity","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"from","type":"address"},{"indexed":true,"internalType":"address","name":"to","type":"address"},{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"Transfer","type":"event"},{"inputs":[],"name":"DOMAIN_SEPARATOR","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"PERMIT_TYPEHASH","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"WETH9","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"approve","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"}],"name":"balanceOf","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"baseURI","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"pure","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"burn","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"components":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint128","name":"amount0Max","type":"uint128"},{"internalType":"uint128","name":"amount1Max","type":"uint128"}],"internalType":"struct INonfungiblePositionManager.CollectParams","name":"params","type":"tuple"}],"name":"collect","outputs":[{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token0","type":"address"},{"internalType":"address","name":"token1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"uint160","name":"sqrtPriceX96","type":"uint160"}],"name":"createAndInitializePoolIfNecessary","outputs":[{"internalType":"address","name":"pool","type":"address"}],"stateMutability":"payable","type":"function"},{"inputs":[{"components":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"amount0Min","type":"uint256"},{"internalType":"uint256","name":"amount1Min","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"internalType":"struct INonfungiblePositionManager.DecreaseLiquidityParams","name":"params","type":"tuple"}],"name":"decreaseLiquidity","outputs":[{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"getApproved","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint256","name":"amount0Desired","type":"uint256"},{"internalType":"uint256","name":"amount1Desired","type":"uint256"},{"internalType":"uint256","name":"amount0Min","type":"uint256"},{"internalType":"uint256","name":"amount1Min","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"internalType":"struct INonfungiblePositionManager.IncreaseLiquidityParams","name":"params","type":"tuple"}],"name":"increaseLiquidity","outputs":[{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"address","name":"operator","type":"address"}],"name":"isApprovedForAll","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"token0","type":"address"},{"internalType":"address","name":"token1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickLower","type":"int24"},{"internalType":"int24","name":"tickUpper","type":"int24"},{"internalType":"uint256","name":"amount0Desired","type":"uint256"},{"internalType":"uint256","name":"amount1Desired","type":"uint256"},{"internalType":"uint256","name":"amount0Min","type":"uint256"},{"internalType":"uint256","name":"amount1Min","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"internalType":"struct INonfungiblePositionManager.MintParams","name":"params","type":"tuple"}],"name":"mint","outputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"bytes[]","name":"data","type":"bytes[]"}],"name":"multicall","outputs":[{"internalType":"bytes[]","name":"results","type":"bytes[]"}],"stateMutability":"payable","type":"function"},{"inputs":[],"name":"name","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"ownerOf","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"permit","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"positions","outputs":[{"internalType":"uint96","name":"nonce","type":"uint96"},{"internalType":"address","name":"operator","type":"address"},{"internalType":"address","name":"token0","type":"address"},{"internalType":"address","name":"token1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickLower","type":"int24"},{"internalType":"int24","name":"tickUpper","type":"int24"},{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"feeGrowthInside0LastX128","type":"uint256"},{"internalType":"uint256","name":"feeGrowthInside1LastX128","type":"uint256"},{"internalType":"uint128","name":"tokensOwed0","type":"uint128"},{"internalType":"uint128","name":"tokensOwed1","type":"uint128"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"refundETH","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"safeTransferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"bytes","name":"_data","type":"bytes"}],"name":"safeTransferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermit","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"nonce","type":"uint256"},{"internalType":"uint256","name":"expiry","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermitAllowed","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"nonce","type":"uint256"},{"internalType":"uint256","name":"expiry","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermitAllowedIfNecessary","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermitIfNecessary","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"operator","type":"address"},{"internalType":"bool","name":"approved","type":"bool"}],"name":"setApprovalForAll","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes4","name":"interfaceId","type":"bytes4"}],"name":"supportsInterface","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"amountMinimum","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"}],"name":"sweepToken","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[],"name":"symbol","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"index","type":"uint256"}],"name":"tokenByIndex","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"uint256","name":"index","type":"uint256"}],"name":"tokenOfOwnerByIndex","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"tokenURI","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"totalSupply","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"transferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"uint256","name":"amount0Owed","type":"uint256"},{"internalType":"uint256","name":"amount1Owed","type":"uint256"},{"internalType":"bytes","name":"data","type":"bytes"}],"name":"uniswapV3MintCallback","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"uint256","name":"amountMinimum","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"}],"name":"unwrapWETH9","outputs":[],"stateMutability":"payable","type":"function"},{"stateMutability":"payable","type":"receive"}]
                                """
                            ),
                        )

                        # Look up liquidity position (NFT positions recorded by 0xC36442b4a4522E871399CD717aBDD847Ab11FE88)
                        (
                            _nonce,
                            _operator,
                            _token0,
                            _token1,
                            _fee,
                            _tick_lower,
                            _tick_upper,
                            _liquidity,
                            *_,
                        ) = positions_contract.functions.positions(token_id).call()
                        logger.info(f"{_tick_lower=}")
                        logger.info(f"{_tick_upper=}")

                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(token0, token1),
                            pool_fee=_fee,
                            silent=self.silent,
                        )

                        sqrtRatioAX96 = TickMath.getSqrtRatioAtTick(_tick_lower)
                        sqrtRatioBX96 = TickMath.getSqrtRatioAtTick(_tick_upper)

                        current_sqrt_price_x96 = (
                            # TODO: review this, check for earlier pool states that may differ
                            v3_pool.sqrt_price_x96
                        )

                        # Get added liquidity (LiquidityManagement.sol)
                        added_liquidity = getLiquidityForAmounts(
                            current_sqrt_price_x96,
                            sqrtRatioAX96,
                            sqrtRatioBX96,
                            amount0_desired,
                            amount1_desired,
                        )
                        logger.info(f"{added_liquidity=}")

                        logger.info("Searching for pool in pool states...")
                        for pool, state in self.simulated_pool_states:
                            if pool == v3_pool:
                                if TYPE_CHECKING:
                                    assert isinstance(state, UniswapV3PoolState)
                                    assert isinstance(pool, V3LiquidityPool)
                                logger.info("Found V3 Pool!")
                                pool.simulate_add_liquidity()

                        # Simulate mint
                        ...

            # Catch and re-raise without special handling. These are raised by this class, so short-circuit if one has bubbled up.
            # This prevents nested multicalls from adding redundant strings to exception message.
            # e.g. 'Simulation failed: Simulation failed: Simulation failed: Simulation failed: {error}'
            except TransactionError:
                raise
            # Catch errors from pool helper simulation attempts
            except DegenbotError as e:
                raise TransactionError(f"Simulation failed: {e}") from e

        def _process_uniswap_universal_router_transaction() -> None:
            logger.debug(f"{func_name}: {self.hash.hex()=}")

            if func_name != "execute":
                raise ValueError(f"UNHANDLED UNIVERSAL ROUTER FUNCTION: {func_name}")

            try:
                try:
                    tx_deadline = func_params["deadline"]
                except KeyError:
                    pass
                else:
                    self._raise_if_past_deadline(tx_deadline)

                tx_commands = func_params["commands"]
                tx_inputs = func_params["inputs"]

                for command, input in zip(tx_commands, tx_inputs):
                    _process_universal_router_command(command, input)

            # bugfix: prevents nested multicalls from spamming exception
            # message.
            # e.g. 'Simulation failed: Simulation failed: {error}'
            except TransactionError:
                raise
            except DegenbotError as e:
                raise TransactionError(f"Simulation failed: {e}") from e

        if func_name in V2_FUNCTIONS:
            _process_uniswap_v2_transaction()
        elif func_name in V3_FUNCTIONS:
            _process_uniswap_v3_transaction()
        elif func_name in UNIVERSAL_ROUTER_FUNCTIONS:
            _process_uniswap_universal_router_transaction()
        elif func_name in UNHANDLED_FUNCTIONS:
            logger.debug(f"TODO: {func_name}")
            raise TransactionError(
                f"Aborting simulation involving un-implemented function: {func_name}"
            )
        elif func_name in NO_OP_FUNCTIONS:
            logger.debug(f"NON-OP: {func_name}")
        else:
            raise ValueError(f"UNHANDLED: {func_name}")

    def simulate(
        self,
        silent: bool = False,
        state_block: BlockNumber | int | None = None,
    ) -> List[
        Tuple[LiquidityPool, UniswapV2PoolSimulationResult]
        | Tuple[V3LiquidityPool, UniswapV3PoolSimulationResult]
    ]:
        """
        Execute a simulation of a transaction, using the attributes
        stored in the constructor.

        Defers simulation to the `_simulate` method, which may recurse
        as needed for nested multicalls.

        Performs a final accounting check of addresses in `self.ledger`
        ledger, excluding the `msg.sender` and `recipient` addresses.
        """

        self.silent = silent

        self.state_block: BlockNumber
        if state_block is None:
            self.state_block = config.get_web3().eth.get_block_number()
        else:
            self.state_block = cast(BlockNumber, state_block)

        self.simulated_pool_states: List[
            Tuple[LiquidityPool, UniswapV2PoolSimulationResult]
            | Tuple[V3LiquidityPool, UniswapV3PoolSimulationResult]
        ] = []

        try:
            self._simulate(
                self.func_name,
                self.func_params,
            )
        except ValueError as e:
            raise TransactionError(e)

        if self.router_address in self.ledger._balances:
            raise self.LeftoverRouterBalance(
                "Unaccounted router balance", self.ledger._balances[self.router_address]
            )

        # if not silent:
        #     logger.info(f"{self.simulated_pool_states=}")

        return self.simulated_pool_states
