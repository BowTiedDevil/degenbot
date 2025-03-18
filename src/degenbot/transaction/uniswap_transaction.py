# TODO: use tx_payer_is_user to simplify accounting
# TODO: implement "blank" V3 pools and incorporate try/except for all V3 pool manager get_pool calls
# TODO: add state block argument for pool simulation calls
# TODO: instead of appending pool states to list, replace with dict and only return final state

import contextlib
from typing import TYPE_CHECKING, Any, Self, cast

import eth_abi.abi
import pydantic_core
from eth_typing import BlockNumber, ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import (
    DeadlineExpired,
    DegenbotError,
    DegenbotValueError,
    EVMRevertError,
    InsufficientInput,
    InsufficientOutput,
    LeftoverRouterBalance,
    LiquidityPoolError,
    ManagerError,
    PreviousBlockMismatch,
    TransactionError,
    UnknownRouterAddress,
)
from degenbot.logging import logger
from degenbot.managers.erc20_token_manager import Erc20TokenManager
from degenbot.transaction.simulation_ledger import SimulationLedger
from degenbot.types import AbstractSimulationResult, AbstractTransaction
from degenbot.uniswap.abi import UNISWAP_V3_ROUTER2_ABI, UNISWAP_V3_ROUTER_ABI
from degenbot.uniswap.deployments import (
    ROUTER_DEPLOYMENTS,
    UniswapRouterDeployment,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
)
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from degenbot.uniswap.types import (
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)
from degenbot.uniswap.v2_functions import generate_v2_pool_address, get_v2_pools_from_token_path
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool, UnregisteredLiquidityPool
from degenbot.uniswap.v3_functions import decode_v3_path
from degenbot.uniswap.v3_libraries.constants import Q96
from degenbot.uniswap.v3_libraries.full_math import muldiv
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class UniversalRouterSpecialAddress:
    # ref: https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/libraries/Constants.sol
    ETH = get_checksum_address("0x0000000000000000000000000000000000000000")
    MSG_SENDER = get_checksum_address("0x0000000000000000000000000000000000000001")
    ROUTER = get_checksum_address("0x0000000000000000000000000000000000000002")


class UniversalRouterSpecialValues:
    # ref: https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/libraries/Constants.sol
    V2_PAIR_ALREADY_PAID = 0
    USE_CONTRACT_BALANCE = 1 << 255


class V3RouterSpecialAddress:
    # SwapRouter.sol checks for address(0)
    # ref: https://github.com/Uniswap/v3-periphery/blob/main/contracts/SwapRouter.sol
    ROUTER_1 = get_checksum_address("0x0000000000000000000000000000000000000000")

    # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/libraries/Constants.sol
    MSG_SENDER = get_checksum_address("0x0000000000000000000000000000000000000001")
    ROUTER_2 = get_checksum_address("0x0000000000000000000000000000000000000002")


class V3RouterSpecialValues:
    # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/libraries/Constants.sol
    USE_CONTRACT_BALANCE = 0


class UniswapTransaction(AbstractTransaction):
    @classmethod
    def from_router(cls, router: UniswapRouterDeployment, **kwargs: Any) -> Self:
        return cls(
            chain_id=router.chain_id,
            router_address=router.address,
            **kwargs,
        )

    def __init__(
        self,
        chain_id: int | str,
        router_address: str,
        func_name: str,
        func_params: dict[str, Any],
        tx_hash: HexBytes | bytes | str,
        tx_nonce: int | str,
        tx_value: int | str,
        tx_sender: str,
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
        self.chain_id: int

        self.v2_pool_manager: UniswapV2PoolManager | None = None
        self.v3_pool_manager: UniswapV3PoolManager | None = None

        self.chain_id = int(chain_id, 16) if isinstance(chain_id, str) else chain_id
        self.router_address = get_checksum_address(router_address)
        if self.router_address not in ROUTER_DEPLOYMENTS[self.chain_id]:
            raise UnknownRouterAddress

        router_deployment = ROUTER_DEPLOYMENTS[self.chain_id][self.router_address]

        # Create pool managers for the supported exchanges
        for exchange in router_deployment.exchanges:
            match exchange:
                case UniswapV2ExchangeDeployment():
                    self.v2_pool_manager = UniswapV2PoolManager.get_instance(
                        factory_address=exchange.factory.address, chain_id=self.chain_id
                    ) or UniswapV2PoolManager(
                        factory_address=exchange.factory.address, chain_id=self.chain_id
                    )
                case UniswapV3ExchangeDeployment():
                    self.v3_pool_manager = UniswapV3PoolManager.get_instance(
                        factory_address=exchange.factory.address, chain_id=self.chain_id
                    ) or UniswapV3PoolManager(
                        factory_address=exchange.factory.address, chain_id=self.chain_id
                    )
                case _:
                    raise DegenbotValueError(message=f"Could not identify DEX type for {exchange}")

        self.sender = get_checksum_address(tx_sender)
        self.recipients: set[ChecksumAddress] = set()

        self.hash = HexBytes(tx_hash)
        self.nonce = int(tx_nonce, 16) if isinstance(tx_nonce, str) else tx_nonce
        self.value = int(tx_value, 16) if isinstance(tx_value, str) else tx_value
        self.func_name = func_name
        self.func_params = func_params
        if previous_block_hash := self.func_params.get("previousBlockhash"):
            self.func_previous_block_hash = HexBytes(previous_block_hash)

        self.silent = False

    def _raise_if_past_deadline(
        self, deadline: int, block_number: BlockNumber
    ) -> None:  # pragma: no cover
        block = connection_manager.get_web3(self.chain_id).eth.get_block(block_number)
        block_timestamp = block.get("timestamp")
        if block_timestamp is not None and block_timestamp > deadline:
            raise DeadlineExpired

    def _raise_if_block_hash_mismatch(self, block_hash: HexBytes) -> None:  # pragma: no cover
        logger.info(f"Checking previousBlockhash: {block_hash!r}")
        block = connection_manager.get_web3(self.chain_id).eth.get_block("latest")
        _block_hash = block.get("hash")
        if _block_hash is not None and block_hash != _block_hash:
            raise PreviousBlockMismatch

    @staticmethod
    def _show_pool_states(
        pool: UniswapV2Pool | UniswapV3Pool,
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
        match current_state, future_state, sim_result:
            case UniswapV2PoolState(), UniswapV2PoolState(), UniswapV2PoolSimulationResult():
                logger.info("\t(CURRENT)")
                logger.info(f"\t{pool.token0}: {current_state.reserves_token0}")
                logger.info(f"\t{pool.token1}: {current_state.reserves_token1}")
                logger.info("\t(FUTURE)")
                logger.info(f"\t{pool.token0}: {future_state.reserves_token0}")
                logger.info(f"\t{pool.token1}: {future_state.reserves_token1}")
            case UniswapV3PoolState(), UniswapV3PoolState(), UniswapV3PoolSimulationResult():
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
        pool: UniswapV2Pool,
        recipient: ChecksumAddress | str,
        token_in: Erc20Token,
        amount_in: int,
        amount_out_min: int | None = None,
        first_swap: bool = False,
        last_swap: bool = False,
    ) -> tuple[
        UniswapV2Pool,
        UniswapV2PoolSimulationResult,
    ]:
        assert isinstance(pool, UniswapV2Pool), f"Called _simulate_v2_swap_exact_in on pool {pool}"

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
            self.recipients.add(get_checksum_address(recipient))

        if last_swap and amount_out_min is not None and _amount_out < amount_out_min:
            raise InsufficientOutput(
                minimum=amount_out_min,
                received=_amount_out,
            )

        if not self.silent:
            self._show_pool_states(pool, sim_result)

        return pool, sim_result

    @staticmethod
    def _simulate_v2_add_liquidity(
        pool: UniswapV2Pool,
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
        pool: UniswapV2Pool,
        recipient: ChecksumAddress | str,
        token_in: Erc20Token,
        amount_out: int,
        amount_in_max: int | None = None,
        first_swap: bool = False,
    ) -> tuple[UniswapV2Pool, UniswapV2PoolSimulationResult]:
        assert isinstance(pool, UniswapV2Pool), f"Called _simulate_v2_swap_exact_out on pool {pool}"

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
            raise InsufficientInput(minimum=_amount_in, deposited=amount_in_max)

        if not self.silent:
            self._show_pool_states(pool, sim_result)

        return pool, sim_result

    def _simulate_v3_swap_exact_in(
        self,
        pool: UniswapV3Pool,
        recipient: str,
        token_in: Erc20Token,
        amount_in: int,
        amount_out_min: int | None = None,
        sqrt_price_limit_x96: int | None = None,
        first_swap: bool = False,
    ) -> tuple[UniswapV3Pool, UniswapV3PoolSimulationResult]:
        assert isinstance(pool, UniswapV3Pool), f"Called _simulate_v3_swap_exact_in on pool {pool}"

        self.recipients.add(get_checksum_address(recipient))

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
        except EVMRevertError as exc:
            raise TransactionError(message=f"V3 revert: {exc}") from exc

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
            raise InsufficientOutput(minimum=amount_out_min, received=_amount_out)

        if not self.silent:
            self._show_pool_states(pool, _sim_result)

        return pool, _sim_result

    def _simulate_v3_swap_exact_out(
        self,
        pool: UniswapV3Pool,
        recipient: str,
        token_in: Erc20Token,
        amount_out: int,
        amount_in_max: int | None = None,
        sqrt_price_limit_x96: int | None = None,
        first_swap: bool = False,
        last_swap: bool = False,
    ) -> tuple[UniswapV3Pool, UniswapV3PoolSimulationResult]:
        assert isinstance(pool, UniswapV3Pool), f"Called _simulate_v3_swap_exact_out on pool {pool}"

        self.recipients.add(get_checksum_address(recipient))

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        try:
            _sim_result = pool.simulate_exact_output_swap(
                token_out=token_out,
                token_out_quantity=amount_out,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
            )
        except EVMRevertError as exc:
            raise TransactionError(message=f"V3 revert: {exc}") from exc

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
                raise InsufficientInput(minimum=_amount_in, deposited=amount_in_max)

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
                    _recipient = get_checksum_address(recipient)
                    self.recipients.add(_recipient)

            self.ledger.transfer(
                token=token_out.address,
                amount=_amount_out,
                from_addr=self.router_address,
                to_addr=_recipient,
            )

        if not self.silent:
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
        func_params: dict[str, Any],
        block_number: BlockNumber,
    ) -> None:
        """
        Take a Uniswap V2 / V3 transaction (specified by name and a dictionary
        of arguments to that function) and return a list of pools and state
        dictionaries for all pools used by the transaction
        """

        v2_functions = {
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

        v3_functions = {
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

        universal_router_functions = {
            "execute",
        }

        unhandled_functions = {
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

        no_op_functions = {
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
            universal_router_command_values: dict[int, str | None] = {
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

            unimplemented_universal_router_commands = {
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
                "X2Y2_1155",
                "X2Y2_721",
            }

            command_type_mask = 0x3F
            command = universal_router_command_values[command_type & command_type_mask]

            logger.debug(f"Processing Universal Router command: {command}")

            if TYPE_CHECKING:
                _amount_in: int = 0
                _amount_out: int = 0
                _sim_result: UniswapV2PoolSimulationResult | UniswapV3PoolSimulationResult

            match command:
                case "SWEEP":
                    """
                    This function transfers the current token balance held by the contract to
                    `recipient`
                    """

                    sweep_token_address, sweep_recipient, sweep_amount_min = eth_abi.abi.decode(
                        types=("address", "address", "uint256"),
                        data=inputs,
                    )

                    sweep_recipient = get_checksum_address(sweep_recipient)
                    match sweep_recipient:  # pragma: no cover
                        case UniversalRouterSpecialAddress.MSG_SENDER:
                            sweep_recipient = self.sender
                        case UniversalRouterSpecialAddress.ROUTER:
                            sweep_recipient = self.router_address
                        case _:
                            pass

                    sweep_token_balance = self.ledger.token_balance(
                        self.router_address, sweep_token_address
                    )

                    if sweep_token_balance < sweep_amount_min:
                        raise InsufficientOutput(
                            minimum=sweep_amount_min,
                            received=sweep_token_balance,
                        )

                    self._simulate_sweep(sweep_token_address, sweep_recipient)

                case "PAY_PORTION":
                    """
                    Transfers a portion of the current token balance held by the
                    contract to `recipient`
                    """

                    _pay_portion_token_address, _pay_portion_recipient, _pay_portion_bips = (
                        eth_abi.abi.decode(
                            types=("address", "address", "uint256"),
                            data=inputs,
                        )
                    )

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
                        self.recipients.add(get_checksum_address(_pay_portion_recipient))

                case "TRANSFER":
                    """
                    Transfer an `amount` of `token` to `recipient`
                    """

                    transfer_token, transfer_recipient, transfer_value = eth_abi.abi.decode(
                        types=("address", "address", "uint256"), data=inputs
                    )
                    transfer_recipient = get_checksum_address(transfer_recipient)
                    self.ledger.adjust(
                        address=self.router_address, token=transfer_token, amount=-transfer_value
                    )
                    self.ledger.adjust(
                        address=transfer_recipient, token=transfer_token, amount=transfer_value
                    )

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

                    _tx_recipient, _wrap_amount_min = eth_abi.abi.decode(
                        types=("address", "uint256"),
                        data=inputs,
                    )

                    _recipient = get_checksum_address(_tx_recipient)
                    match _tx_recipient:  # pragma: no cover
                        case UniversalRouterSpecialAddress.MSG_SENDER:
                            _recipient = self.sender
                        case UniversalRouterSpecialAddress.ROUTER:
                            _recipient = self.router_address
                        case _:
                            pass

                    self.ledger.adjust(
                        address=_recipient,
                        token=WRAPPED_NATIVE_TOKENS[self.chain_id],
                        amount=_wrap_amount_min,
                    )

                case "UNWRAP_WETH":
                    """
                    Unwraps a quantity of WETH held by the router to ETH.
                    """

                    _, _unwrap_amount_min = eth_abi.abi.decode(
                        types=("address", "uint256"),
                        data=inputs,
                    )

                    _wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                    _wrapped_token_balance = self.ledger.token_balance(
                        self.router_address, _wrapped_token_address
                    )

                    if _wrapped_token_balance < _unwrap_amount_min:
                        raise InsufficientOutput(
                            minimum=_unwrap_amount_min,
                            received=_wrapped_token_balance,
                        )

                    self._simulate_unwrap(_wrapped_token_address)

                case "V2_SWAP_EXACT_IN":
                    """
                    Decode an exact input swap through Uniswap V2 liquidity pools.

                    Returns: a list of tuples representing the pool object and the
                    final state of the pool after the swap completes.
                    """

                    tx_recipient, tx_amount_in, tx_amount_out_min, tx_path, tx_payer_is_user = (
                        eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "address[]",
                                "bool",
                            ),
                            data=inputs,
                        )
                    )

                    try:
                        if TYPE_CHECKING:
                            assert self.v2_pool_manager is not None
                        pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                    except (LiquidityPoolError, ManagerError):
                        raise TransactionError(
                            message=f"Pools could not be built for all steps in path {tx_path}"
                        ) from None

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

                    tx_recipient, tx_amount_out, tx_amount_in_max, tx_path, tx_payer_is_user = (
                        eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "address[]",
                                "bool",
                            ),
                            data=inputs,
                        )
                    )

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
                            message=f"Pools could not be built for all steps in path {tx_path}"
                        ) from None

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

                    Returns: a list of tuples representing the pool object and the final state of
                    the pool after the swap completes.
                    """

                    tx_recipient, tx_amount_in, tx_amount_out_min, tx_path, tx_payer_is_user = (
                        eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "bytes",
                                "bool",
                            ),
                            data=inputs,
                        )
                    )

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
                        v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
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

                    tx_recipient, tx_amount_out, tx_amount_in_max, tx_path, tx_payer_is_user = (
                        eth_abi.abi.decode(
                            types=(
                                "address",
                                "uint256",
                                "uint256",
                                "bytes",
                                "bool",
                            ),
                            data=inputs,
                        )
                    )

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
                        v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
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
                                    UniswapV2PoolSimulationResult | UniswapV2PoolSimulationResult,
                                )

                            _last_amount_in = max(
                                _last_sim_result.amount1_delta,
                                _last_sim_result.amount0_delta,
                            )

                            if _amount_out != _last_amount_in:
                                raise TransactionError(
                                    message=f"Insufficient swap amount through requested pool {v3_pool}. Needed {_last_amount_in}, received {_amount_out}"  # noqa:E501
                                )

                        self.simulated_pool_states.append((v3_pool, v3_sim_result))

                case _:
                    if command in unimplemented_universal_router_commands:
                        logger.debug(f"UNIMPLEMENTED COMMAND: {command}")
                    else:  # pragma: no cover
                        raise DegenbotValueError(message=f"Invalid command {command}")

        def _process_v3_multicall(
            params: dict[str, Any],
        ) -> None:
            with contextlib.suppress(KeyError):
                self._raise_if_block_hash_mismatch(params["previousBlockhash"])

            for payload in params["data"]:
                payload_func = payload_args = None
                with contextlib.suppress(ValueError):
                    # decode with Router ABI
                    payload_func, payload_args = (
                        Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                        .decode_function_input(payload)
                    )

                with contextlib.suppress(ValueError):
                    # decode with Router2 ABI
                    payload_func, payload_args = (
                        Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                        .decode_function_input(payload)
                    )

                if payload_func is None or payload_args is None:  # pragma: no cover
                    raise DegenbotValueError(message="Failed to decode payload.")

                # special case to handle a multicall encoded within
                # another multicall
                if payload_func.fn_name == "multicall":
                    logger.debug("Unwrapping nested multicall")

                    with contextlib.suppress(KeyError):
                        self._raise_if_block_hash_mismatch(params["previousBlockhash"])

                    for function_input in payload_args["data"]:
                        _func = _params = None
                        with contextlib.suppress(ValueError):
                            _func, _params = (
                                Web3()
                                .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                                .decode_function_input(function_input)
                            )

                        with contextlib.suppress(ValueError):
                            _func, _params = (
                                Web3()
                                .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                                .decode_function_input(function_input)
                            )

                        if _func is None or _params is None:  # pragma: no cover
                            raise DegenbotValueError(
                                message="Failed to decode function parameters."
                            )

                        self._simulate(
                            func_name=_func.fn_name,
                            func_params=_params,
                            block_number=block_number,
                        )

                else:
                    self._simulate(
                        func_name=payload_func.fn_name,
                        func_params=payload_args,
                        block_number=block_number,
                    )

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
                        logger.debug(f"{func_name}: {self.hash.to_0x_hex()=}")

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
                            self._raise_if_past_deadline(
                                tx_deadline,
                                block_number=block_number,
                            )

                        try:
                            pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                        except (LiquidityPoolError, ManagerError):
                            raise TransactionError(
                                message=f"Pools could not be built for all steps in path {tx_path}"
                            ) from None

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
                        logger.debug(f"{func_name}: {self.hash.to_0x_hex()=}")

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

                        with contextlib.suppress(KeyError):
                            tx_deadline = func_params["deadline"]
                            self._raise_if_past_deadline(
                                tx_deadline,
                                block_number=block_number,
                            )

                        try:
                            pools = get_v2_pools_from_token_path(tx_path, self.v2_pool_manager)
                        except (LiquidityPoolError, ManagerError) as exc:  # pragma: no cover
                            raise TransactionError(
                                message=f"Pools could not be built for all steps in path {tx_path}"
                            ) from exc

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
                        logger.debug(f"{func_name}: {self.hash.to_0x_hex()=}")

                        if func_name == "addLiquidity":
                            tx_token_a = get_checksum_address(func_params["tokenA"])
                            tx_token_b = get_checksum_address(func_params["tokenB"])
                            tx_token_amount_a = func_params["amountADesired"]
                            tx_token_amount_b = func_params["amountBDesired"]
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
                        else:
                            tx_token = get_checksum_address(func_params["token"])
                            tx_token_amount = func_params["amountTokenDesired"]
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

                        tx_deadline = func_params["deadline"]
                        self._raise_if_past_deadline(
                            tx_deadline,
                            block_number=block_number,
                        )

                        try:
                            _pool = self.v2_pool_manager.get_pool_from_tokens(
                                token_addresses=(token0_address, token1_address),
                                silent=self.silent,
                            )
                        except ManagerError:
                            token_manager = Erc20TokenManager(chain_id=self.chain_id)
                            _pool = UnregisteredLiquidityPool(
                                address=generate_v2_pool_address(
                                    token_addresses=(
                                        token0_address,
                                        token1_address,
                                    ),
                                    deployer_address=self.v2_pool_manager._factory_address,
                                    init_hash=self.v2_pool_manager._pool_init_hash,
                                ),
                                tokens=[
                                    token_manager.get_erc20token(
                                        token0_address, silent=self.silent
                                    ),
                                    token_manager.get_erc20token(
                                        token1_address, silent=self.silent
                                    ),
                                ],
                            )

                        if TYPE_CHECKING:
                            assert isinstance(_pool, UniswapV2Pool)
                        _sim_result = self._simulate_v2_add_liquidity(
                            pool=_pool,
                            added_reserves_token0=token0_amount,
                            added_reserves_token1=token1_amount,
                            override_state=UniswapV2PoolState(
                                address=_pool.address,
                                reserves_token0=0,
                                reserves_token1=0,
                                block=None,
                            ),
                        )

                        self.simulated_pool_states.append((_pool, _sim_result))

                    case _:
                        raise DegenbotValueError(message=f"Unknown function: {func_name}!")

            except TransactionError:
                # Catch specific subclass exception to prevent nested
                # multicalls from being recursively re-annotated
                # e.g. 'Simulation failed: Simulation failed: {error}'
                raise
            except DegenbotError as e:
                raise TransactionError(message=f"Simulation failed: {e}") from e

        def _process_uniswap_v3_transaction() -> None:
            logger.debug(f"{func_name}: {self.hash.to_0x_hex()=}")

            if TYPE_CHECKING:
                v3_sim_result: UniswapV3PoolSimulationResult
                assert self.v3_pool_manager is not None

            try:
                match func_name:
                    case "multicall":
                        _process_v3_multicall(params=func_params)

                    case "exactInputSingle":
                        match func_params["params"], len(func_params["params"]):
                            case dict(), 7 | 8:
                                tx_token_in_address = func_params["params"]["tokenIn"]
                                tx_token_out_address = func_params["params"]["tokenOut"]
                                tx_fee = func_params["params"]["fee"]
                                tx_recipient = func_params["params"]["recipient"]
                                tx_amount_in = func_params["params"]["amountIn"]
                                tx_amount_out_min = func_params["params"]["amountOutMinimum"]
                                tx_deadline = func_params["params"].get("deadline")
                                tx_sqrt_price_limit_x96 = func_params["params"]["sqrtPriceLimitX96"]
                            case tuple(), 8:
                                # Decode with ISwapRouter ABI
                                # ref: https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
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
                            case tuple(), 7:
                                # Decode with IV3SwapRouter ABI (aka Router2)
                                # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
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
                            case _:
                                raise DegenbotValueError(
                                    message=f"Could not identify function parameters. Got {(func_params['params'])}"  # noqa:E501
                                )

                        if tx_deadline:
                            self._raise_if_past_deadline(
                                tx_deadline,
                                block_number=block_number,
                            )

                        if TYPE_CHECKING:
                            assert isinstance(self.v3_pool_manager, UniswapV3PoolManager)
                        v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
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
                        match func_params["params"], len(func_params["params"]):
                            case dict(), 4 | 5:
                                tx_path = func_params["params"]["path"]
                                tx_recipient = func_params["params"]["recipient"]
                                tx_deadline = func_params["params"].get("deadline")
                                tx_amount_in = func_params["params"]["amountIn"]
                                tx_amount_out_minimum = func_params["params"]["amountOutMinimum"]
                            case tuple(), 5:
                                # Decode with ISwapRouter ABI
                                # ref: https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_deadline,
                                    tx_amount_in,
                                    tx_amount_out_minimum,
                                ) = func_params["params"]
                            case tuple(), 4:
                                # Decode with IV3SwapRouter ABI (aka Router2)
                                # ref: https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_amount_in,
                                    tx_amount_out_minimum,
                                ) = func_params["params"]
                                tx_deadline = None
                            case _:
                                raise DegenbotValueError(
                                    message=f"Could not identify function parameters. Got {(func_params['params'])}"  # noqa:E501
                                )

                        if tx_deadline:
                            self._raise_if_past_deadline(
                                tx_deadline,
                                block_number=block_number,
                            )

                        tx_path_decoded = decode_v3_path(tx_path)

                        if not self.silent:
                            logger.info(f" • path = {tx_path_decoded}")
                            logger.info(f" • recipient = {tx_recipient}")
                            if tx_deadline is not None:
                                logger.info(f" • deadline = {tx_deadline}")
                            logger.info(f" • amountIn = {tx_amount_in}")
                            logger.info(f" • amountOutMinimum = {tx_amount_out_minimum}")

                        last_token_pos = len(tx_path_decoded) - 3
                        token_out_quantity = 0  # this is overridden by the first calc

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

                            v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
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
                        match func_params["params"], len(func_params["params"]):
                            case dict(), 7 | 8:
                                tx_token_in_address = func_params["params"]["tokenIn"]
                                tx_token_out_address = func_params["params"]["tokenOut"]
                                tx_fee = func_params["params"]["fee"]
                                tx_recipient = func_params["params"]["recipient"]
                                tx_deadline = func_params["params"].get("deadline")
                                tx_amount_out = func_params["params"]["amountOut"]
                                tx_amount_in_max = func_params["params"]["amountInMaximum"]
                                tx_sqrt_price_limit_x96 = func_params["params"]["sqrtPriceLimitX96"]
                            case tuple(), 8:
                                # Decode with ISwapRouter ABI
                                # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
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

                            case tuple(), 7:
                                # Decode with IV3SwapRouter ABI (aka Router2)
                                # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
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
                            case _:
                                raise DegenbotValueError(
                                    message=f"Could not identify function parameters. Got {(func_params['params'])}"  # noqa:E501
                                )

                        if tx_deadline:
                            self._raise_if_past_deadline(
                                tx_deadline,
                                block_number=block_number,
                            )

                        if TYPE_CHECKING:
                            assert isinstance(self.v3_pool_manager, UniswapV3PoolManager)
                        v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
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
                            raise InsufficientInput(
                                minimum=amount_deposited,
                                deposited=tx_amount_in_max,
                            )

                    case "exactOutput":
                        match func_params["params"], len(func_params["params"]):
                            case dict(), 4 | 5:
                                tx_path = func_params["params"]["path"]
                                tx_recipient = func_params["params"]["recipient"]
                                tx_deadline = func_params["params"].get("deadline")
                                tx_amount_out = func_params["params"]["amountOut"]
                                tx_amount_in_max = func_params["params"]["amountInMaximum"]
                            case tuple(), 5:
                                # Decode with ISwapRouter ABI
                                # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_deadline,
                                    tx_amount_out,
                                    tx_amount_in_max,
                                ) = func_params["params"]
                            case tuple(), 4:
                                # Decode with IV3SwapRouter ABI (aka Router2)
                                # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                                (
                                    tx_path,
                                    tx_recipient,
                                    tx_amount_out,
                                    tx_amount_in_max,
                                ) = func_params["params"]
                                tx_deadline = None
                            case _:
                                raise DegenbotValueError(
                                    message=f"Could not identify function parameters. Got {(func_params['params'])}"  # noqa:E501
                                )

                        if tx_deadline:
                            self._raise_if_past_deadline(
                                tx_deadline,
                                block_number=block_number,
                            )

                        tx_path_decoded = decode_v3_path(tx_path)

                        if not self.silent:
                            logger.info(f" • path = {tx_path_decoded}")
                            logger.info(f" • recipient = {tx_recipient}")
                            if tx_deadline is not None:
                                logger.info(f" • deadline = {tx_deadline}")
                            logger.info(f" • amountOut = {tx_amount_out}")
                            logger.info(f" • amountInMaximum = {tx_amount_in_max}")

                        # an exact output path is encoded in REVERSE order,
                        # tokenOut is the first position, tokenIn is the second
                        # position. e.g. tokenOut, fee, tokenIn
                        last_token_pos = len(tx_path_decoded) - 3

                        _amount_in = 0
                        first_swap = False

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
                                assert isinstance(self.v3_pool_manager, UniswapV3PoolManager)
                                assert isinstance(tx_token_out_address, str)
                                assert isinstance(tx_token_in_address, str)
                                assert isinstance(tx_fee, int)

                            v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
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
                                _, _last_sim_result = self.simulated_pool_states[-1]

                                if TYPE_CHECKING:
                                    assert isinstance(
                                        _last_sim_result,
                                        UniswapV2PoolSimulationResult
                                        | UniswapV2PoolSimulationResult,
                                    )

                                _last_amount_in = max(
                                    _last_sim_result.amount0_delta,
                                    _last_sim_result.amount1_delta,
                                )

                                if _amount_out != _last_amount_in:
                                    raise InsufficientOutput(
                                        minimum=_last_amount_in,
                                        received=_amount_out,
                                    )

                            self.simulated_pool_states.append((v3_pool, v3_sim_result))

                        # V3 Router enforces a maximum input
                        if first_swap:
                            _sim_result: AbstractSimulationResult
                            _, _sim_result = self.simulated_pool_states[-1]

                            amount_deposited = max(
                                _sim_result.amount0_delta,
                                _sim_result.amount1_delta,
                            )

                            if amount_deposited > tx_amount_in_max:
                                raise InsufficientInput(
                                    minimum=amount_deposited,
                                    deposited=tx_amount_in_max,
                                )

                    case "unwrapWETH9":
                        # TODO: if ETH balances are ever needed, handle the
                        # ETH transfer resulting from this function
                        amount_min = func_params["amountMinimum"]
                        wrapped_token_address = WRAPPED_NATIVE_TOKENS[self.chain_id]
                        wrapped_token_balance = self.ledger.token_balance(
                            self.router_address, wrapped_token_address
                        )
                        if wrapped_token_balance < amount_min:
                            raise InsufficientOutput(
                                minimum=amount_min,
                                received=wrapped_token_balance,
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
                            raise InsufficientOutput(
                                minimum=_amount_in,
                                received=wrapped_token_balance,
                            )

                        self._simulate_unwrap(wrapped_token_address)

                    case "sweepToken":
                        """
                        This function transfers the current token balance
                        held by the contract to `recipient`
                        """

                        tx_token_address = func_params["token"]
                        tx_amount_out_minimum = func_params["amountMinimum"]
                        tx_recipient = func_params.get("recipient")

                        # Router2 ABI omits `recipient`, always uses
                        # `msg.sender`
                        if tx_recipient is None:
                            tx_recipient = self.sender
                        else:
                            self.recipients.add(tx_recipient)

                        _balance = self.ledger.token_balance(self.router_address, tx_token_address)

                        if _balance < tx_amount_out_minimum:
                            raise InsufficientOutput(
                                minimum=tx_amount_out_minimum,
                                received=_balance,
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

                        def get_liquidity_for_amount_0(
                            sqrt_ratio_a_x96: int, sqrt_ratio_b_x96: int, amount0: int
                        ) -> int:
                            if sqrt_ratio_a_x96 > sqrt_ratio_b_x96:
                                (sqrt_ratio_a_x96, sqrt_ratio_b_x96) = (
                                    sqrt_ratio_b_x96,
                                    sqrt_ratio_a_x96,
                                )
                            intermediate = muldiv(sqrt_ratio_a_x96, sqrt_ratio_b_x96, Q96)
                            return muldiv(
                                amount0, intermediate, sqrt_ratio_b_x96 - sqrt_ratio_a_x96
                            )

                        def get_liquidity_for_amount_1(
                            sqrt_ratio_a_x96: int, sqrt_ratio_b_x96: int, amount1: int
                        ) -> int:
                            if sqrt_ratio_a_x96 > sqrt_ratio_b_x96:
                                (sqrt_ratio_a_x96, sqrt_ratio_b_x96) = (
                                    sqrt_ratio_b_x96,
                                    sqrt_ratio_a_x96,
                                )
                            return muldiv(amount1, Q96, sqrt_ratio_b_x96 - sqrt_ratio_a_x96)

                        def get_liquidity_for_amounts(
                            sqrt_ratio_x96: int,
                            sqrt_ratio_a_x96: int,
                            sqrt_ratio_b_x96: int,
                            amount0: int,
                            amount1: int,
                        ) -> int:
                            if sqrt_ratio_a_x96 > sqrt_ratio_b_x96:
                                (sqrt_ratio_a_x96, sqrt_ratio_b_x96) = (
                                    sqrt_ratio_b_x96,
                                    sqrt_ratio_a_x96,
                                )

                            if sqrt_ratio_x96 <= sqrt_ratio_a_x96:
                                liquidity = get_liquidity_for_amount_0(
                                    sqrt_ratio_a_x96, sqrt_ratio_b_x96, amount0
                                )
                            elif sqrt_ratio_x96 < sqrt_ratio_b_x96:
                                liquidity0 = get_liquidity_for_amount_0(
                                    sqrt_ratio_x96, sqrt_ratio_b_x96, amount0
                                )
                                liquidity1 = get_liquidity_for_amount_1(
                                    sqrt_ratio_a_x96, sqrt_ratio_x96, amount1
                                )

                                liquidity = min(liquidity1, liquidity0)
                            else:
                                liquidity = get_liquidity_for_amount_1(
                                    sqrt_ratio_a_x96, sqrt_ratio_b_x96, amount1
                                )

                            return liquidity

                        # Decode inputs
                        """
                        struct IncreaseLiquidityParams {
                            address token0;
                            address token1;
                            uint256 tokenId;
                            uint256 amount0Min;
                            uint256 amount1Min;
                        }
                        """

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

                        positions_contract = connection_manager.get_web3(
                            self.chain_id
                        ).eth.contract(
                            address=get_checksum_address(
                                "0xC36442b4a4522E871399CD717aBDD847Ab11FE88"
                            ),
                            abi=pydantic_core.from_json(
                                """
                                [{"inputs":[{"internalType":"address","name":"_factory","type":"address"},{"internalType":"address","name":"_WETH9","type":"address"},{"internalType":"address","name":"_tokenDescriptor_","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"owner","type":"address"},{"indexed":true,"internalType":"address","name":"approved","type":"address"},{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"Approval","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"owner","type":"address"},{"indexed":true,"internalType":"address","name":"operator","type":"address"},{"indexed":false,"internalType":"bool","name":"approved","type":"bool"}],"name":"ApprovalForAll","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"},{"indexed":false,"internalType":"address","name":"recipient","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"Collect","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"},{"indexed":false,"internalType":"uint128","name":"liquidity","type":"uint128"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"DecreaseLiquidity","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"},{"indexed":false,"internalType":"uint128","name":"liquidity","type":"uint128"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"IncreaseLiquidity","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"from","type":"address"},{"indexed":true,"internalType":"address","name":"to","type":"address"},{"indexed":true,"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"Transfer","type":"event"},{"inputs":[],"name":"DOMAIN_SEPARATOR","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"PERMIT_TYPEHASH","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"WETH9","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"approve","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"}],"name":"balanceOf","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"baseURI","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"pure","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"burn","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"components":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint128","name":"amount0Max","type":"uint128"},{"internalType":"uint128","name":"amount1Max","type":"uint128"}],"internalType":"struct INonfungiblePositionManager.CollectParams","name":"params","type":"tuple"}],"name":"collect","outputs":[{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token0","type":"address"},{"internalType":"address","name":"token1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"uint160","name":"sqrtPriceX96","type":"uint160"}],"name":"createAndInitializePoolIfNecessary","outputs":[{"internalType":"address","name":"pool","type":"address"}],"stateMutability":"payable","type":"function"},{"inputs":[{"components":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"amount0Min","type":"uint256"},{"internalType":"uint256","name":"amount1Min","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"internalType":"struct INonfungiblePositionManager.DecreaseLiquidityParams","name":"params","type":"tuple"}],"name":"decreaseLiquidity","outputs":[{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"getApproved","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint256","name":"amount0Desired","type":"uint256"},{"internalType":"uint256","name":"amount1Desired","type":"uint256"},{"internalType":"uint256","name":"amount0Min","type":"uint256"},{"internalType":"uint256","name":"amount1Min","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"internalType":"struct INonfungiblePositionManager.IncreaseLiquidityParams","name":"params","type":"tuple"}],"name":"increaseLiquidity","outputs":[{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"address","name":"operator","type":"address"}],"name":"isApprovedForAll","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"token0","type":"address"},{"internalType":"address","name":"token1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickLower","type":"int24"},{"internalType":"int24","name":"tickUpper","type":"int24"},{"internalType":"uint256","name":"amount0Desired","type":"uint256"},{"internalType":"uint256","name":"amount1Desired","type":"uint256"},{"internalType":"uint256","name":"amount0Min","type":"uint256"},{"internalType":"uint256","name":"amount1Min","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"internalType":"struct INonfungiblePositionManager.MintParams","name":"params","type":"tuple"}],"name":"mint","outputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"bytes[]","name":"data","type":"bytes[]"}],"name":"multicall","outputs":[{"internalType":"bytes[]","name":"results","type":"bytes[]"}],"stateMutability":"payable","type":"function"},{"inputs":[],"name":"name","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"ownerOf","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"permit","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"positions","outputs":[{"internalType":"uint96","name":"nonce","type":"uint96"},{"internalType":"address","name":"operator","type":"address"},{"internalType":"address","name":"token0","type":"address"},{"internalType":"address","name":"token1","type":"address"},{"internalType":"uint24","name":"fee","type":"uint24"},{"internalType":"int24","name":"tickLower","type":"int24"},{"internalType":"int24","name":"tickUpper","type":"int24"},{"internalType":"uint128","name":"liquidity","type":"uint128"},{"internalType":"uint256","name":"feeGrowthInside0LastX128","type":"uint256"},{"internalType":"uint256","name":"feeGrowthInside1LastX128","type":"uint256"},{"internalType":"uint128","name":"tokensOwed0","type":"uint128"},{"internalType":"uint128","name":"tokensOwed1","type":"uint128"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"refundETH","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"safeTransferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"},{"internalType":"bytes","name":"_data","type":"bytes"}],"name":"safeTransferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermit","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"nonce","type":"uint256"},{"internalType":"uint256","name":"expiry","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermitAllowed","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"nonce","type":"uint256"},{"internalType":"uint256","name":"expiry","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermitAllowedIfNecessary","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"selfPermitIfNecessary","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[{"internalType":"address","name":"operator","type":"address"},{"internalType":"bool","name":"approved","type":"bool"}],"name":"setApprovalForAll","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes4","name":"interfaceId","type":"bytes4"}],"name":"supportsInterface","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"uint256","name":"amountMinimum","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"}],"name":"sweepToken","outputs":[],"stateMutability":"payable","type":"function"},{"inputs":[],"name":"symbol","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"index","type":"uint256"}],"name":"tokenByIndex","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"uint256","name":"index","type":"uint256"}],"name":"tokenOfOwnerByIndex","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"tokenURI","outputs":[{"internalType":"string","name":"","type":"string"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"totalSupply","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"tokenId","type":"uint256"}],"name":"transferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"uint256","name":"amount0Owed","type":"uint256"},{"internalType":"uint256","name":"amount1Owed","type":"uint256"},{"internalType":"bytes","name":"data","type":"bytes"}],"name":"uniswapV3MintCallback","outputs":[],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"uint256","name":"amountMinimum","type":"uint256"},{"internalType":"address","name":"recipient","type":"address"}],"name":"unwrapWETH9","outputs":[],"stateMutability":"payable","type":"function"},{"stateMutability":"payable","type":"receive"}]
                                """  # noqa: E501
                            ),
                        )

                        # Look up liquidity position (NFT positions recorded by
                        # 0xC36442b4a4522E871399CD717aBDD847Ab11FE88)
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

                        v3_pool = self.v3_pool_manager.get_pool_from_tokens_and_fee(
                            token_addresses=(token0, token1),
                            pool_fee=_fee,
                            silent=self.silent,
                        )

                        sqrt_ratio_a_x96 = get_sqrt_ratio_at_tick(_tick_lower)
                        sqrt_ratio_b_x96 = get_sqrt_ratio_at_tick(_tick_upper)

                        current_sqrt_price_x96 = (
                            # TODO: review this, check for earlier pool states that may differ
                            v3_pool.sqrt_price_x96
                        )

                        # Get added liquidity (LiquidityManagement.sol)
                        added_liquidity = get_liquidity_for_amounts(
                            current_sqrt_price_x96,
                            sqrt_ratio_a_x96,
                            sqrt_ratio_b_x96,
                            amount0_desired,
                            amount1_desired,
                        )
                        logger.info(f"{added_liquidity=}")

                        logger.info("Searching for pool in pool states...")
                        for pool, state in self.simulated_pool_states:
                            if pool == v3_pool:
                                if TYPE_CHECKING:
                                    assert isinstance(state, UniswapV3PoolState)
                                    assert isinstance(pool, UniswapV3Pool)
                                logger.info("Found V3 Pool!")
                                pool.simulate_add_liquidity()

                        # Simulate mint
                        ...

            # Wrap exceptions from pool helper simulation attempts
            except LiquidityPoolError as exc:
                raise TransactionError(message=f"Simulation failed: {exc}") from exc

        def _process_uniswap_universal_router_transaction() -> None:
            logger.debug(f"{func_name}: {self.hash.to_0x_hex()=}")

            if func_name != "execute":
                raise DegenbotValueError(
                    message=f"UNHANDLED UNIVERSAL ROUTER FUNCTION: {func_name}"
                )

            try:
                with contextlib.suppress(KeyError):
                    tx_deadline = func_params["deadline"]
                    self._raise_if_past_deadline(
                        tx_deadline,
                        block_number=block_number,
                    )

                tx_commands = func_params["commands"]
                tx_inputs = func_params["inputs"]

                for tx_command, tx_input in zip(tx_commands, tx_inputs, strict=False):
                    _process_universal_router_command(tx_command, tx_input)

            # bugfix: prevents nested multicalls from spamming exception
            # message.
            # e.g. 'Simulation failed: Simulation failed: {error}'
            except TransactionError:
                raise
            except DegenbotError as e:
                raise TransactionError(message=f"Simulation failed: {e}") from e

        if func_name in v2_functions:
            _process_uniswap_v2_transaction()
        elif func_name in v3_functions:
            _process_uniswap_v3_transaction()
        elif func_name in universal_router_functions:
            _process_uniswap_universal_router_transaction()
        elif func_name in unhandled_functions:
            raise TransactionError(
                message=f"Aborting simulation involving un-implemented function: {func_name}"
            )
        elif func_name in no_op_functions:
            logger.debug(f"NON-OP: {func_name}")
        else:
            raise TransactionError(
                message=f"Aborting simulation involving unknown function: {func_name}"
            )

    def simulate(
        self,
        silent: bool = False,
        state_block: BlockNumber | int | None = None,
    ) -> list[
        tuple[UniswapV2Pool, UniswapV2PoolSimulationResult]
        | tuple[UniswapV3Pool, UniswapV3PoolSimulationResult]
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

        self.simulated_pool_states: list[
            tuple[UniswapV2Pool, UniswapV2PoolSimulationResult]
            | tuple[UniswapV3Pool, UniswapV3PoolSimulationResult]
        ] = []

        self._simulate(
            self.func_name,
            self.func_params,
            block_number=(
                connection_manager.get_web3(self.chain_id).eth.get_block_number()
                if state_block is None
                else cast("BlockNumber", state_block)
            ),
        )

        if self.router_address in self.ledger.balances:
            raise LeftoverRouterBalance(balances=self.ledger.balances[self.router_address])

        return self.simulated_pool_states
