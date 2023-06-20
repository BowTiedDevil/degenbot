# TODO: refactor, class has gotten bulky af frfr

import itertools
from pprint import pprint
from typing import Dict, List, Optional, Tuple, Union

import eth_abi
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.exceptions import (
    DegenbotError,
    EVMRevertError,
    LiquidityPoolError,
    ManagerError,
    TransactionError,
)
from degenbot.logging import logger
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.types import TransactionHelper
from degenbot.uniswap import (
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)
from degenbot.uniswap.abi import UNISWAP_V3_ROUTER2_ABI, UNISWAP_V3_ROUTER_ABI
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool
from degenbot.uniswap.v3.functions import decode_v3_path

# Internal dict of known router contracts by chain ID. Pre-populated with mainnet addresses
# New routers can be added via class method `add_router`
_ROUTERS: Dict[int, Dict[str, Dict]] = {
    1: {
        "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F": {
            "name": "Sushiswap: Router",
            "factory_address": {
                2: "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
            },
        },
        "0xf164fC0Ec4E93095b804a4795bBe1e041497b92a": {
            "name": "UniswapV2: Router",
            "factory_address": {
                2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
            },
        },
        "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D": {
            "name": "UniswapV2: Router 2",
            "factory_address": {
                2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
            },
        },
        "0xE592427A0AEce92De3Edee1F18E0157C05861564": {
            "name": "UniswapV3: Router",
            "factory_address": {
                3: "0x1F98431c8aD98523631AE4a59f267346ea31F984"
            },
        },
        "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45": {
            "name": "UniswapV3: Router 2",
            "factory_address": {
                2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                3: "0x1F98431c8aD98523631AE4a59f267346ea31F984",
            },
        },
        "0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B": {
            "name": "Uniswap Universal Router (Old)",
            "factory_address": {
                2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                3: "0x1F98431c8aD98523631AE4a59f267346ea31F984",
            },
        },
        "0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD": {
            "name": "Universal Universal Router (New) ",
            "factory_address": {
                2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                3: "0x1F98431c8aD98523631AE4a59f267346ea31F984",
            },
        },
    }
}

# Internal dict of known wrapped token contracts by chain ID. Pre-populated with mainnet addresses
_WRAPPED_NATIVE_TOKENS = {1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"}

# see https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/libraries/Constants.sol
_UNIVERSAL_ROUTER_CONTRACT_BALANCE_FLAG = 1 << 255
_UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG = (
    "0x0000000000000000000000000000000000000002"
)
_UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG = (
    "0x0000000000000000000000000000000000000001"
)
_V3_ROUTER_CONTRACT_ADDRESS_FLAG = "0x0000000000000000000000000000000000000000"
_V3_ROUTER2_CONTRACT_BALANCE_FLAG = 0


class UniswapTransaction(TransactionHelper):
    def __init__(
        self,
        chain_id: int,
        tx_hash: str,
        tx_nonce: Union[int, str],
        tx_value: Union[int, str],
        tx_sender: str,
        func_name: str,
        func_params: dict,
        router_address: str,
    ):
        """
        Build a standalone representation of a transaction submitted to a known Uniswap-based router contract address.

        Supported addresses:
            - 0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F (Sushiswap Router)
            - 0xf164fC0Ec4E93095b804a4795bBe1e041497b92a (Uniswap V2 Router)
            - 0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D (Uniswap V2 Router 2)
            - 0xE592427A0AEce92De3Edee1F18E0157C05861564 (Uniswap V3 Router)
            - 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45 (Uniswap V3 Router 2)
            - 0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B (Uniswap Universal Router)
        """

        router_address = Web3.toChecksumAddress(router_address)

        self.routers = _ROUTERS[chain_id]

        # The `self.balance` dictionary maintains a ledger of token balances for all addresses involved in the swap.
        # A positive balance represents a pre-swap deposit, a negative balance represents an outstanding withdrawal.
        # A full transaction should end with a positive balance of the desired output token, credited to `self.sender`.
        #
        # @dev Some routers have special flags that signify the swap amount should be looked up at the time of the swap,
        # as opposed to a specified amount at the time the transaction is built. The ledger is used to look up the balance
        # at any point inside the swap, and at the end to confirm that all balances have been accounted for.
        self.balance: Dict[str, Dict[str, int]] = {}
        self.chain_id = chain_id
        self.sender = Web3.toChecksumAddress(tx_sender)
        self.to: Optional[Union[ChecksumAddress, str]] = None

        if router_address not in self.routers:
            raise ValueError(f"Router address {router_address} unknown!")

        self.router_address = router_address

        try:
            self.v2_pool_manager = UniswapV2LiquidityPoolManager(
                factory_address=self.routers[router_address][
                    "factory_address"
                ][2]
            )
        except:
            pass

        try:
            self.v3_pool_manager = UniswapV3LiquidityPoolManager(
                factory_address=self.routers[router_address][
                    "factory_address"
                ][3]
            )
        except:
            pass

        self.token_manager = Erc20TokenHelperManager()
        self.hash = tx_hash
        self.nonce = (
            int(tx_nonce, 16) if isinstance(tx_nonce, str) else tx_nonce
        )
        self.value = (
            int(tx_value, 16) if isinstance(tx_value, str) else tx_value
        )
        self.func_name = func_name
        self.func_params = func_params
        self.func_deadline = func_params.get("deadline")
        if hash := self.func_params.get("previousBlockhash"):
            self.func_previous_block_hash = hash.hex()

    def _simulate_unwrap(self, wrapped_token: str):
        logger.info(f"Unwrapping {wrapped_token}")

        wrapped_token_balance = self._get_balance(
            self.router_address, wrapped_token
        )

        self._adjust_balance(
            self.router_address,
            wrapped_token,
            -wrapped_token_balance,
        )

    def _simulate_sweep(self, token: str, recipient: str):
        logger.debug(f"Sweeping {token} to {recipient}")

        token_balance = self._get_balance(self.router_address, token)
        self._adjust_balance(
            self.router_address,
            token,
            -token_balance,
        )
        self._adjust_balance(
            recipient,
            token,
            token_balance,
        )

    def _adjust_balance(self, address: str, token: str, amount: int) -> None:
        """
        Modify the balance for a given address and token.

        The amount can be positive (credit) or negative (debit).

        The method checksums all addresses.
        """

        _token = Web3.toChecksumAddress(token)
        _address = Web3.toChecksumAddress(address)

        address_balance: Dict[str, int]
        try:
            address_balance = self.balance[_address]
        except KeyError:
            address_balance = {}
            self.balance[_address] = address_balance

        logger.debug(
            f"ADJUSTING BALANCE FOR ADDRESS {_address}: {'+' if amount > 0 else ''}{amount} {_token}:  "
        )

        try:
            address_balance[_token]
        except KeyError:
            address_balance[_token] = 0
        finally:
            address_balance[_token] += amount
            if address_balance[_token] == 0:
                del address_balance[_token]
            if not address_balance:
                del self.balance[_address]

    def _get_balance(self, address: str, token: str) -> int:
        """
        Get the balance for a given address and token.

        The method checksums all addresses.
        """

        _address = Web3.toChecksumAddress(address)
        _token = Web3.toChecksumAddress(token)

        address_balances: Dict[str, int]
        try:
            address_balances = self.balance[_address]
        except KeyError:
            address_balances = {}

        _token = Web3.toChecksumAddress(_token)
        return address_balances.get(_token, 0)

    @classmethod
    def add_router(cls, chain_id: int, router_address: str, router_dict: dict):
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
        router_address = Web3.toChecksumAddress(router_address)

        for key in [
            "name",
            "factory_address",
        ]:
            if key not in router_dict:
                raise ValueError(f"{key} not found in router_dict")

        try:
            _ROUTERS[chain_id][router_address]
        except:
            _ROUTERS[chain_id][router_address] = router_dict
        else:
            raise ValueError("Router address already known!")

    @classmethod
    def add_wrapped_token(cls, chain_id: int, token_address: str):
        """
        Add a wrapped token address for a given chain ID.

        The method checksums the token address.
        """

        _token_address = Web3.toChecksumAddress(token_address)

        try:
            _WRAPPED_NATIVE_TOKENS[chain_id]
        except KeyError:
            _WRAPPED_NATIVE_TOKENS[chain_id] = _token_address
        else:
            raise ValueError(
                f"Token address {_WRAPPED_NATIVE_TOKENS[chain_id]} already set for chain ID {chain_id}!"
            )

    def _simulate(
        self,
        func_name: Optional[str] = None,
        func_params: Optional[Dict] = None,
        silent: bool = False,
    ) -> List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]:
        """
        Take a Uniswap V2 / V3 transaction (specified by name and a dictionary of arguments
        to that function) and return a list of pools and state dictionaries for all hops
        associated with the transaction
        """

        future_pool_states: List[
            Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]
        ] = []
        v2_pool: LiquidityPool
        v3_pool: V3LiquidityPool
        pool_state: dict

        def _simulate_universal_router_dispatch(
            command_type: int,
            inputs: bytes,
        ):
            _UNIVERSAL_ROUTER_COMMANDS = {
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
                0x0D: "ERMIT2_TRANSFER_FROM_BATCH",
                0x0E: "BALANCE_CHECK_ERC20",
                0x0F: None,  # COMMAND_PLACEHOLDER
                0x10: "SEAPORT",
                0x11: "LOOKS_RARE_721",
                0x12: "NFTX",
                0x13: "CRYPTOPUNKS",
                0x14: "LOOKS_RARE_1155",
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
                0x20: "EXECUTE_SUB_PLAN",
                0x21: "SEAPORT_V2",
            }

            COMMAND_TYPE_MASK = 0x3F
            command = _UNIVERSAL_ROUTER_COMMANDS[
                command_type & COMMAND_TYPE_MASK
            ]

            logger.info(command)

            pool_state: Dict
            _future_pool_states: List[
                Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]
            ] = []

            if command in [
                "PERMIT2_TRANSFER_FROM",
                "PERMIT2_PERMIT_BATCH",
                "TRANSFER",
                "PAY_PORTION",
                "PERMIT2_PERMIT",
                "PERMIT2_TRANSFER_FROM_BATCH",
                "BALANCE_CHECK_ERC20",
                "SEAPORT",
                "LOOKS_RARE_721",
                "NFTX",
                "CRYPTOPUNKS",
                "LOOKS_RARE_1155",
                "OWNER_CHECK_721",
                "OWNER_CHECK_1155",
                "SWEEP_ERC721",
                "X2Y2_721",
                "SUDOSWAP",
                "NFT20",
                "X2Y2_1155",
                "FOUNDATION",
                "SWEEP_ERC1155",
                "ELEMENT_MARKET",
                "EXECUTE_SUB_PLAN",
                "SEAPORT_V2",
            ]:
                logger.debug(f"{command}: NOT IMPLEMENTED")

            elif command == "SWEEP":
                """
                This function transfers the current token balance held by the contract to `recipient`
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    token, recipient, amountMin = eth_abi.decode(
                        ["address", "address", "uint256"], inputs
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                if recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                    recipient = self.sender

                _balance = self._get_balance(self.router_address, token)

                if _balance < amountMin:
                    raise TransactionError(
                        f"Requested sweep of min. {amountMin} WETH, received {_balance}"
                    )

                self._simulate_sweep(token, recipient)

            elif command == "WRAP_ETH":
                """
                This function wraps a quantity of ETH to WETH and transfers it to `recipient`.

                The mainnet WETH contract only implements the `deposit` method, so `recipient` will always be the router address.

                Some L2s and side chains implement a `depositTo` method, so `recipient` is evaluated before adjusting the ledger balance.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                wrapped_token_address = _WRAPPED_NATIVE_TOKENS[self.chain_id]

                try:
                    recipient, amountMin = eth_abi.decode(
                        ["address", "uint256"], inputs
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                if recipient == _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG:
                    _recipient = self.router_address
                else:
                    _recipient = recipient

                self._adjust_balance(
                    _recipient,
                    wrapped_token_address,
                    amountMin,
                )

            elif command == "UNWRAP_WETH":
                """
                This function unwraps a quantity of WETH to ETH.

                ETH is currently untracked by the `self.balance` ledger, so `recipient` is unused.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    recipient, amountMin = eth_abi.decode(
                        ["address", "uint256"], inputs
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                wrapped_token_address = _WRAPPED_NATIVE_TOKENS[self.chain_id]
                wrapped_token_balance = self._get_balance(
                    self.router_address, wrapped_token_address
                )

                if wrapped_token_balance < amountMin:
                    raise TransactionError(
                        f"Requested unwrap of min. {amountMin} WETH, received {wrapped_token_balance}"
                    )

                self._simulate_unwrap(wrapped_token_address)

            elif command == "V2_SWAP_EXACT_IN":
                """
                Decode an exact input swap through Uniswap V2 liquidity pools.

                Returns: a list of tuples representing the pool object and the final state of the pool after the swap completes.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    (
                        recipient,
                        amountIn,
                        amountOutMin,
                        path,
                        payerIsUser,
                    ) = eth_abi.decode(
                        [
                            "address",
                            "uint256",
                            "uint256",
                            "address[]",
                            "bool",
                        ],
                        inputs,
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                func_params = {
                    "amountIn": amountIn,
                    "amountOutMin": amountOutMin,
                    "path": path,
                    "to": recipient,
                }

                # TODO: convert simulation for V2 to single pool

                _future_pool_states.extend(
                    _simulate_v2_swap_exact_in(func_params, silent=silent)
                )

                return _future_pool_states

            elif command == "V2_SWAP_EXACT_OUT":
                """
                Decode an exact output swap through Uniswap V2 liquidity pools.

                Returns: a list of tuples representing the pool object and the final state of the pool after the swap completes.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    (
                        recipient,
                        amountOut,
                        amountInMax,
                        path,
                        payerIsUser,
                    ) = eth_abi.decode(
                        [
                            "address",
                            "uint256",
                            "uint256",
                            "address[]",
                            "bool",
                        ],
                        inputs,
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                func_params = {
                    "amountOut": amountOut,
                    "amountInMax": amountInMax,
                    "path": path,
                    "to": recipient,
                }

                _future_pool_states.extend(
                    _simulate_v2_swap_exact_out(func_params, silent=silent)
                )

                return _future_pool_states

            elif command == "V3_SWAP_EXACT_IN":
                """
                Decode an exact input swap through Uniswap V3 liquidity pools.

                Returns: a list of tuples representing the pool object and the final state of the pool after the swap completes.
                """

                # TODO: handle multi-pool swaps within the method, instead of single-pool

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    (
                        recipient,
                        amountIn,
                        amountOutMin,
                        path,
                        payerIsUser,
                    ) = eth_abi.decode(
                        ["address", "uint256", "uint256", "bytes", "bool"],
                        inputs,
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                exactInputParams_path_decoded = decode_v3_path(path)

                # decode the path - tokenIn is the first position, fee is the second position, tokenOut is the third position
                # paths can be an arbitrary length, but address & fee values are always interleaved
                # e.g. tokenIn, fee, tokenOut, fee,
                last_token_pos = len(exactInputParams_path_decoded) - 3

                for token_pos in range(
                    0,
                    len(exactInputParams_path_decoded) - 2,
                    2,
                ):
                    tokenIn = exactInputParams_path_decoded[token_pos]
                    fee = exactInputParams_path_decoded[token_pos + 1]
                    tokenOut = exactInputParams_path_decoded[token_pos + 2]

                    first_swap = token_pos == 0
                    last_swap = token_pos == last_token_pos

                    v3_pool, pool_state = _simulate_v3_swap_exact_in(
                        # manually craft the `params` dict
                        params={
                            "params": (
                                tokenIn,
                                tokenOut,
                                fee,
                                # use amountIn for the first swap, otherwise take the output
                                # amount of the last swap (always negative so we can check
                                # for the min without knowing the token positions)
                                amountIn
                                if token_pos == 0
                                else token_out_quantity,
                                # only apply minimum output to the last swap
                                amountOutMin if last_swap else None,
                                recipient
                                if last_swap
                                else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                            )
                        },
                        silent=silent,
                        first_swap=first_swap,
                    )
                    _future_pool_states.append((v3_pool, pool_state))

                    token_out_quantity: int = -min(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
                    )

                return _future_pool_states

            elif command == "V3_SWAP_EXACT_OUT":
                """
                Decode an exact output swap through Uniswap V3 liquidity pools.

                Returns: a list of tuples representing the pool object and the final state of the pool after the swap completes.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    (
                        recipient,
                        amountOut,
                        amountInMax,
                        path,
                        payerIsUser,
                    ) = eth_abi.decode(
                        ["address", "uint256", "uint256", "bytes", "bool"],
                        inputs,
                    )
                except:
                    raise TransactionError(
                        f"Could not decode input for {command}"
                    )

                exactOutputParams_path_decoded = decode_v3_path(path)

                # the path is encoded in REVERSE order, so we decode from start to finish
                # tokenOut is the first position, tokenIn is the second position
                # e.g. tokenOut, fee, tokenIn
                last_token_pos = len(exactOutputParams_path_decoded) - 3

                for token_pos in range(
                    0,
                    len(exactOutputParams_path_decoded) - 2,
                    2,
                ):
                    tokenOut = exactOutputParams_path_decoded[token_pos]
                    fee = exactOutputParams_path_decoded[token_pos + 1]
                    tokenIn = exactOutputParams_path_decoded[token_pos + 2]

                    first_swap = token_pos == last_token_pos
                    last_swap = token_pos == 0

                    logger.debug(f"{first_swap=}")
                    logger.debug(f"{last_swap=}")

                    v3_pool, pool_state = _simulate_v3_swap_exact_out(
                        params={
                            "params": (
                                tokenIn,
                                tokenOut,
                                fee,
                                # use amountOut for the last swap, otherwise take
                                # the input amount of the previous swap
                                # (always positive so we can check without
                                # knowing the token positions)
                                amountOut
                                if last_swap
                                else max(
                                    pool_state["amount0_delta"],
                                    pool_state["amount1_delta"],
                                ),
                                amountInMax if first_swap else None,
                                recipient
                                if last_swap
                                else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                            )
                        },
                        silent=silent,
                        first_swap=first_swap,
                        last_swap=last_swap,
                    )

                    _future_pool_states.append((v3_pool, pool_state))

                # plogger.info(self.balance)
                return _future_pool_states

            else:
                raise TransactionError(f"Invalid command {command}")

        def _simulate_v2_swap_exact_in(
            params: dict,
            unwrapped_input: Optional[bool] = False,
            silent: bool = False,
        ) -> List[Tuple[LiquidityPool, Dict]]:
            """
            TBD
            """

            # TODO: convert simulation for V2 to single pool?

            token_in_object: Erc20Token
            token_out_object: Erc20Token
            token_in_quantity: int
            token_out_quantity: int
            v2_pool_objects: List[LiquidityPool] = []
            _future_pool_states: List[Tuple[LiquidityPool, Dict]] = []

            for token_addresses in itertools.pairwise(params["path"]):
                try:
                    pool_helper: LiquidityPool = self.v2_pool_manager.get_pool(
                        token_addresses=token_addresses,
                        silent=silent,
                    )
                except (LiquidityPoolError, ManagerError):
                    raise TransactionError(
                        f"LiquidityPool could not be built for token pair {token_addresses[0]} - {token_addresses[1]}"
                    )
                else:
                    v2_pool_objects.append(pool_helper)

            # the pool manager creates Erc20Token objects in the code block above,
            # so calls to `get_erc20token` will return the previously-created helper
            token_in_object = self.token_manager.get_erc20token(
                address=params["path"][0],
                silent=silent,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )

            if unwrapped_input:
                token_in_quantity = self.value
            else:
                token_in_quantity = params["amountIn"]

            logger.info(f"{token_in_quantity=}")
            last_pool_pos = len(v2_pool_objects) - 1
            recipient = params["to"]
            self.to = Web3.toChecksumAddress(recipient)

            for i, v2_pool in enumerate(v2_pool_objects):
                # i == 0 for first pool in path, take from 'path' in func_params
                # otherwise, set token_in equal to token_out from previous iteration
                # and token_out equal to the other token held by the pool
                token_in_object = (
                    token_in_object if i == 0 else token_out_object
                )

                token_out_object = (
                    v2_pool.token0
                    if token_in_object is v2_pool.token1
                    else v2_pool.token1
                )

                # use the transaction input for the first swap, otherwise take the output from the last iteration
                token_in_quantity = (
                    token_in_quantity if i == 0 else token_out_quantity
                )

                first_swap = i == 0
                last_swap = i == last_pool_pos

                if first_swap:
                    # if the router and the pool have a zero balance, credit it to the router
                    # (the user calling for the swap transfers the input)
                    if not self._get_balance(
                        self.router_address, token_in_object.address
                    ) and not self._get_balance(
                        v2_pool.address, token_in_object.address
                    ):
                        self._adjust_balance(
                            self.router_address,
                            token_in_object.address,
                            token_in_quantity,
                        )

                    if (
                        token_in_quantity
                        == _UNIVERSAL_ROUTER_CONTRACT_BALANCE_FLAG
                    ):
                        token_in_quantity = max(
                            self._get_balance(
                                self.router_address, token_in_object.address
                            ),
                            self._get_balance(
                                v2_pool.address, token_in_object.address
                            ),
                        )

                    router_balance = self._get_balance(
                        self.router_address, token_in_object.address
                    )
                    pool_balance = self._get_balance(
                        v2_pool.address, token_in_object.address
                    )

                    logger.info("V2 SWAP: FIRST POOL")
                    logger.info(f"{router_balance=}")
                    logger.info(f"{pool_balance=}")

                    if pool_balance < token_in_quantity:
                        difference = token_in_quantity - pool_balance
                        self._adjust_balance(
                            self.router_address,
                            token_in_object.address,
                            -difference,
                        )
                        self._adjust_balance(
                            v2_pool.address,
                            token_in_object.address,
                            difference,
                        )

                future_state = v2_pool.simulate_swap(
                    token_in=token_in_object,
                    token_in_quantity=token_in_quantity,
                )

                token_out_quantity = -min(
                    future_state["amount0_delta"],
                    future_state["amount1_delta"],
                )

                _future_pool_states.append(
                    (
                        v2_pool,
                        future_state,
                    )
                )

                # adjust the post-swap balances for each token
                self._adjust_balance(
                    v2_pool.address,
                    token_in_object.address,
                    -token_in_quantity,
                )

                logger.debug(f"{first_swap=}")
                logger.debug(f"{last_swap=}")

                if last_swap:
                    if recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                        _recipient = self.sender
                    elif recipient in [
                        _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                        _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
                    ]:
                        _recipient = self.router_address
                    else:
                        _recipient = recipient
                else:
                    _recipient = v2_pool_objects[i + 1].address

                self._adjust_balance(
                    _recipient,
                    token_out_object.address,
                    token_out_quantity,
                )

                if not silent:
                    current_state = v2_pool.state
                    logger.info(f"Simulating swap through pool: {v2_pool}")
                    logger.info(
                        f"\t{token_in_quantity} {token_in_object} -> {token_out_quantity} {token_out_object}"
                    )
                    logger.info("\t(CURRENT)")
                    logger.info(
                        f"\t{v2_pool.token0}: {current_state['reserves_token0']}"
                    )
                    logger.info(
                        f"\t{v2_pool.token1}: {current_state['reserves_token1']}"
                    )
                    logger.info(f"\t(FUTURE)")
                    logger.info(
                        f"\t{v2_pool.token0}: {future_state['reserves_token0']}"
                    )
                    logger.info(
                        f"\t{v2_pool.token1}: {future_state['reserves_token1']}"
                    )

            token_out_quantity_min = params["amountOutMin"]
            if token_out_quantity < token_out_quantity_min:
                raise TransactionError(
                    f"Insufficient output for swap! {token_out_quantity} {token_out_object} received, {token_out_quantity_min} required"
                )

            return _future_pool_states

        def _simulate_v2_swap_exact_out(
            params: dict,
            unwrapped_input: Optional[bool] = False,
            silent: bool = False,
        ) -> List[Tuple[LiquidityPool, Dict]]:
            """
            TBD
            """

            # TODO: convert simulation for V2 to single pool?

            token_in_object: Erc20Token
            token_out_object: Erc20Token
            token_in_quantity: int
            token_out_quantity: int
            pool_objects: List[LiquidityPool] = []
            _future_pool_states: List[Tuple[LiquidityPool, Dict]] = []

            for token_addresses in itertools.pairwise(params["path"]):
                try:
                    pool_helper: LiquidityPool = self.v2_pool_manager.get_pool(
                        token_addresses=token_addresses,
                        silent=silent,
                    )
                except (LiquidityPoolError, ManagerError):
                    raise TransactionError(
                        f"Liquidity pool could not be built for token pair {token_addresses[0]} - {token_addresses[1]}"
                    )
                else:
                    pool_objects.append(pool_helper)

            # the pool manager creates Erc20Token objects as it works,
            # so calls to `get_erc20token` will return the previously-created helper
            token_out_object = self.token_manager.get_erc20token(
                address=params["path"][-1],
                silent=silent,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )
            token_out_quantity = params["amountOut"]
            last_pool_pos = len(pool_objects) - 1
            recipient = params["to"]
            self.to = Web3.toChecksumAddress(recipient)

            # work through the pools backwards, since the swap will execute at a defined output, with input floating
            for i, v2_pool in enumerate(pool_objects[::-1]):
                token_out_quantity = (
                    token_out_quantity if i == 0 else token_in_quantity
                )

                # i == 0 for last pool in path, take from 'path' in func_params
                # otherwise, set token_out equal to token_in from previous iteration
                # and token_in equal to the other token held by the pool
                token_out_object = (
                    token_out_object if i == 0 else token_in_object
                )

                token_in_object = (
                    v2_pool.token0
                    if token_out_object is v2_pool.token1
                    else v2_pool.token1
                )

                first_swap = i == last_pool_pos
                last_swap = i == 0

                future_state = v2_pool.simulate_swap(
                    token_out=token_out_object,
                    token_out_quantity=token_out_quantity,
                )

                token_in_quantity = max(
                    future_state["amount0_delta"],
                    future_state["amount1_delta"],
                )

                _future_pool_states.append(
                    (
                        v2_pool,
                        future_state,
                    )
                )

                # adjust the pool balances for each token
                self._adjust_balance(
                    v2_pool.address,
                    token_in_object.address,
                    -token_in_quantity,
                )
                self._adjust_balance(
                    v2_pool.address,
                    token_out_object.address,
                    token_out_quantity,
                )

                # logger.info(f"{recipient=}")

                if first_swap:
                    # transfer the input token amount from the sender to the first pool
                    logger.info("FIRST SWAP")
                    self._adjust_balance(
                        self.sender,
                        token_in_object.address,
                        -token_in_quantity,
                    )
                    self._adjust_balance(
                        v2_pool.address,
                        token_in_object.address,
                        token_in_quantity,
                    )

                if last_swap:
                    logger.info("LAST SWAP")
                    if recipient in [
                        _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                        _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
                    ]:
                        _recipient = self.router_address
                    else:
                        _recipient = self.sender
                else:
                    # send tokens to the next pool
                    _recipient = pool_objects[::-1][i - 1].address

                # logger.info(f"{_recipient=}")

                # transfer the output token from the pool to the recipient
                self._adjust_balance(
                    v2_pool.address,
                    token_out_object.address,
                    -token_out_quantity,
                )
                self._adjust_balance(
                    _recipient,
                    token_out_object.address,
                    token_out_quantity,
                )

                if not silent:
                    current_state = v2_pool.state
                    logger.info(f"Simulating swap through pool: {v2_pool}")
                    logger.info(
                        f"\t{token_in_quantity} {token_in_object} -> {token_out_quantity} {token_out_object}"
                    )
                    logger.info("\t(CURRENT)")
                    logger.info(
                        f"\t{v2_pool.token0}: {current_state['reserves_token0']}"
                    )
                    logger.info(
                        f"\t{v2_pool.token1}: {current_state['reserves_token1']}"
                    )
                    logger.info(f"\t(FUTURE)")
                    logger.info(
                        f"\t{v2_pool.token0}: {future_state['reserves_token0']}"
                    )
                    logger.info(
                        f"\t{v2_pool.token1}: {future_state['reserves_token1']}"
                    )

            if unwrapped_input:
                swap_in_quantity = self.value
            else:
                swap_in_quantity = params["amountInMax"]

            if swap_in_quantity < token_in_quantity:
                raise TransactionError(
                    f"Insufficient input for exact output swap! {swap_in_quantity} {token_in_object} provided, {token_in_quantity} required"
                )

            return _future_pool_states

        def _simulate_v3_multicall(
            params,
            silent: bool = False,
        ) -> List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]:
            """
            TBD
            """

            _future_pool_states: List[
                Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]
            ] = []

            for payload in params["data"]:
                try:
                    # decode with Router ABI
                    payload_func, payload_args = (
                        Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                        .decode_function_input(payload)
                    )
                except:
                    pass

                try:
                    # decode with Router2 ABI
                    payload_func, payload_args = (
                        Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                        .decode_function_input(payload)
                    )
                except:
                    pass

                # special case to handle a multicall encoded within another multicall
                if payload_func.fn_name == "multicall":
                    if not silent:
                        logger.info("Unwrapping nested multicall")

                    for payload in payload_args["data"]:
                        try:
                            _func, _params = (
                                Web3()
                                .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                                .decode_function_input(payload)
                            )
                        except:
                            pass

                        try:
                            _func, _params = (
                                Web3()
                                .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                                .decode_function_input(payload)
                            )
                        except:
                            pass

                        try:
                            # simulate each payload individually and append its result to future_pool_states
                            _future_pool_states.extend(
                                self._simulate(
                                    func_name=_func.fn_name,
                                    func_params=_params,
                                    silent=silent,
                                )
                            )
                        except Exception as e:
                            raise TransactionError(
                                f"Could not decode nested multicall: {e}"
                            )
                else:
                    try:
                        # simulate each payload individually and append its result to future_pool_states
                        _future_pool_states.extend(
                            self._simulate(
                                func_name=payload_func.fn_name,
                                func_params=payload_args,
                                silent=silent,
                            )
                        )
                    except Exception as e:
                        raise TransactionError(
                            f"Could not decode multicall: {e}"
                        )

            return _future_pool_states

        def _simulate_v3_swap_exact_in(
            params: dict,
            silent: bool = False,
            first_swap: bool = False,
        ) -> Tuple[V3LiquidityPool, Dict]:
            """
            TBD
            """

            token_in_object: Erc20Token
            token_out_object: Erc20Token
            token_in_quantity: int
            token_out_quantity: int
            token_in_address: str
            token_out_address: str

            # decode with Router ABI
            # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
            try:
                (
                    token_in_address,
                    token_out_address,
                    fee,
                    recipient,
                    deadline,
                    token_in_quantity,
                    token_out_quantity_min,
                    sqrt_price_limit_x96,
                ) = params["params"]
            except:
                pass

            # decode with Router2 ABI
            # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
            try:
                (
                    token_in_address,
                    token_out_address,
                    fee,
                    recipient,
                    token_in_quantity,
                    token_out_quantity_min,
                    sqrt_price_limit_x96,
                ) = params["params"]
            except:
                pass

            # decode values from a manually-built exactInput swap
            try:
                (
                    token_in_address,
                    token_out_address,
                    fee,
                    token_in_quantity,
                    token_out_quantity_min,
                    recipient,
                ) = params["params"]
            except:
                pass

            self.to = Web3.toChecksumAddress(recipient)

            try:
                # get the V3 pool involved in the swap
                v3_pool = self.v3_pool_manager.get_pool(
                    token_addresses=(token_in_address, token_out_address),
                    pool_fee=fee,
                    silent=silent,
                )
            except (LiquidityPoolError, ManagerError) as e:
                raise TransactionError(
                    f"Could not get pool (via tokens {token_in_address} & {token_out_address}): {e}"
                )
            except:
                raise

            try:
                token_in_object = self.token_manager.get_erc20token(
                    address=token_in_address,
                    silent=silent,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
                token_out_object = self.token_manager.get_erc20token(
                    address=token_out_address,
                    silent=silent,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
            except Exception as e:
                print(e)
                print(type(e))
                raise

            logger.info(f"{token_in_quantity=}")

            if token_in_quantity == _UNIVERSAL_ROUTER_CONTRACT_BALANCE_FLAG:
                token_in_quantity = self._get_balance(
                    self.router_address, token_in_object.address
                )

            # the swap may occur after wrapping ETH, in which case amountIn will be already set.
            # if not, credit the router (user will send as part of contract call)
            if first_swap and not self._get_balance(
                self.router_address, token_in_object.address
            ):
                self._adjust_balance(
                    self.router_address,
                    token_in_object.address,
                    token_in_quantity,
                )

            try:
                final_state = v3_pool.simulate_swap(
                    token_in=token_in_object,
                    token_in_quantity=token_in_quantity,
                )
            except EVMRevertError as e:
                raise TransactionError(f"Simulated V3 revert: {e}")

            token_out_quantity = -min(
                final_state["amount0_delta"], final_state["amount1_delta"]
            )

            self._adjust_balance(
                self.router_address,
                token_in_object.address,
                -token_in_quantity,
            )

            logger.debug(f"{recipient=}")
            if recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                recipient = self.sender
            elif recipient in [
                _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
            ]:
                recipient = self.router_address

            self._adjust_balance(
                recipient,
                token_out_object.address,
                token_out_quantity,
            )

            if not silent:
                current_state = v3_pool.state
                logger.info(
                    f"Predicting output of swap through pool: {v3_pool}"
                )
                logger.info(
                    f"\t{token_in_quantity} {token_in_object} -> {token_out_quantity} {token_out_object}"
                )
                logger.info("\t(CURRENT)")
                logger.info(f"\tprice={current_state['sqrt_price_x96']}")
                logger.info(f"\tliquidity={current_state['liquidity']}")
                logger.info(f"\ttick={current_state['tick']}")
                logger.info(f"\t(FUTURE)")
                logger.info(f"\tprice={final_state['sqrt_price_x96']}")
                logger.info(f"\tliquidity={final_state['liquidity']}")
                logger.info(f"\ttick={final_state['tick']}")

            if (
                token_out_quantity_min is not None
                and token_out_quantity < token_out_quantity_min
            ):
                raise TransactionError(
                    f"Insufficient output for swap! {token_out_quantity} {token_out_object} received, {token_out_quantity_min} required"
                )

            return v3_pool, final_state

        def _simulate_v3_swap_exact_out(
            params: dict,
            silent: bool = False,
            first_swap: bool = False,
            last_swap: bool = False,
        ) -> Tuple[V3LiquidityPool, Dict]:
            """
            TBD
            """

            token_in_object: Erc20Token
            token_out_object: Erc20Token
            token_in_quantity: int
            token_out_quantity: int

            sqrtPriceLimitX96 = None
            amountInMaximum = None

            # decode with Router ABI
            # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    recipient,
                    deadline,
                    amountOut,
                    amountInMaximum,
                    sqrtPriceLimitX96,
                ) = params["params"]
            except:
                pass

            # decode with Router2 ABI
            # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    recipient,
                    amountOut,
                    amountInMaximum,
                    sqrtPriceLimitX96,
                ) = params["params"]
            except:
                pass

            # decode values from exactOutput (hand-crafted)
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    amountOut,
                    amountInMaximum,
                    recipient,
                ) = params["params"]
            except:
                pass

            self.to = Web3.toChecksumAddress(recipient)

            try:
                v3_pool = self.v3_pool_manager.get_pool(
                    token_addresses=(tokenIn, tokenOut),
                    pool_fee=fee,
                    silent=silent,
                )
            except (LiquidityPoolError, ManagerError) as e:
                raise TransactionError(f"Could not get pool (via tokens): {e}")
            except:
                raise

            try:
                token_in_object = self.token_manager.get_erc20token(
                    address=tokenIn,
                    silent=silent,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
                token_out_object = self.token_manager.get_erc20token(
                    address=tokenOut,
                    silent=silent,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
            except Exception as e:
                print(e)
                print(type(e))
                raise

            current_state = v3_pool.state

            try:
                final_state = v3_pool.simulate_swap(
                    token_out=token_out_object,
                    token_out_quantity=amountOut,
                    sqrt_price_limit=sqrtPriceLimitX96,
                )
            except EVMRevertError as e:
                raise TransactionError(
                    f"V3 operation could not be simulated: {e}"
                )

            token_in_quantity = max(
                final_state["amount0_delta"],
                final_state["amount1_delta"],
            )
            token_out_quantity = -min(
                final_state["amount0_delta"],
                final_state["amount1_delta"],
            )

            # adjust the post-swap balances for each token
            self._adjust_balance(
                self.router_address,
                token_in_object.address,
                -token_in_quantity,
            )
            self._adjust_balance(
                self.router_address,
                token_out_object.address,
                token_out_quantity,
            )

            # Exact output swaps proceed in reverse order, so the last iteration will show a negative balance
            # of the input token, which must be accounted for.
            #
            # Check for a balance:
            #   - If zero, take no action
            #   - If non-zero, adjust with the assumption the user has paid that amount with the transaction call
            #
            if first_swap:
                swap_input_balance = self._get_balance(
                    self.router_address, token_in_object.address
                )
                if swap_input_balance:
                    self._adjust_balance(
                        self.router_address,
                        token_in_object.address,
                        -swap_input_balance,
                    )

            # logger.info(f"{recipient=}")

            if last_swap:
                if recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                    _recipient = self.sender
                elif recipient in [
                    _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                    _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
                ]:
                    _recipient = self.router_address
                else:
                    _recipient = recipient
                    self.to = Web3.toChecksumAddress(recipient)

                # logger.debug(f"{_recipient=}")
                # logger.debug(f"{self.to=}")

                self._adjust_balance(
                    self.router_address,
                    token_out_object.address,
                    -token_out_quantity,
                )
                self._adjust_balance(
                    _recipient,
                    token_out_object.address,
                    token_out_quantity,
                )

            if not silent:
                logger.info(
                    f"Predicting output of swap through pool: {v3_pool}"
                )
                logger.info(
                    f"\t{token_in_quantity} {token_in_object} -> {token_out_quantity} {token_out_object}"
                )
                logger.info("\t(CURRENT)")
                logger.info(f"\tprice={current_state['sqrt_price_x96']}")
                logger.info(f"\tliquidity={current_state['liquidity']}")
                logger.info(f"\ttick={current_state['tick']}")
                logger.info(f"\t(FUTURE)")
                logger.info(f"\tprice={final_state['sqrt_price_x96']}")
                logger.info(f"\tliquidity={final_state['liquidity']}")
                logger.info(f"\ttick={final_state['tick']}")

            if (
                amountInMaximum is not None
                and amountInMaximum < token_in_quantity
            ):
                raise TransactionError(
                    f"Insufficient input for exact output swap! {token_in_quantity} {token_in_object} required, {amountInMaximum} provided"
                )

            return v3_pool, final_state

        if func_name is None:
            func_name = self.func_name

        if func_params is None:
            func_params = self.func_params

        try:
            # -----------------------------------------------------
            # UniswapV2 functions
            # -----------------------------------------------------
            if func_name in (
                "swapExactTokensForETH",
                "swapExactTokensForETHSupportingFeeOnTransferTokens",
            ):
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v2_swap_exact_in(func_params, silent=silent)
                )

            elif func_name in (
                "swapExactETHForTokens",
                "swapExactETHForTokensSupportingFeeOnTransferTokens",
            ):
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v2_swap_exact_in(
                        func_params, unwrapped_input=True, silent=silent
                    )
                )

            elif func_name in [
                "swapExactTokensForTokens",
                "swapExactTokensForTokensSupportingFeeOnTransferTokens",
            ]:
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v2_swap_exact_in(func_params, silent=silent)
                )

            elif func_name in ("swapTokensForExactETH"):
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v2_swap_exact_out(
                        params=func_params, silent=silent
                    )
                )

            elif func_name in ("swapTokensForExactTokens"):
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v2_swap_exact_out(
                        params=func_params, silent=silent
                    )
                )

            elif func_name in ("swapETHForExactTokens"):
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v2_swap_exact_out(
                        params=func_params, unwrapped_input=True, silent=silent
                    )
                )

            # -----------------------------------------------------
            # UniswapV3 functions
            # -----------------------------------------------------
            elif func_name == "multicall":
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.extend(
                    _simulate_v3_multicall(params=func_params, silent=silent)
                )

            elif func_name == "exactInputSingle":
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.append(
                    _simulate_v3_swap_exact_in(
                        params=func_params, silent=silent, first_swap=True
                    )
                )

            elif func_name == "exactInput":
                """
                TBD
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                # from ISwapRouter.sol - https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                try:
                    (
                        path,
                        recipient,
                        deadline,
                        amount_in,
                        amount_out_minimum,
                    ) = func_params["params"]
                except:
                    pass

                # from IV3SwapRouter.sol - https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                try:
                    (
                        path,
                        recipient,
                        amount_in,
                        amount_out_minimum,
                    ) = func_params["params"]
                except:
                    pass

                path_decoded = decode_v3_path(path)

                if not silent:
                    logger.info(f" • path = {path_decoded}")
                    logger.info(f" • recipient = {recipient}")
                    try:
                        deadline
                    except:
                        pass
                    else:
                        logger.info(f" • deadline = {deadline}")
                    logger.info(f" • amountIn = {amount_in}")
                    logger.info(f" • amountOutMinimum = {amount_out_minimum}")

                last_token_pos = len(path_decoded) - 3

                for token_pos in range(
                    0,
                    len(path_decoded) - 2,
                    2,
                ):
                    token_in_address = path_decoded[token_pos]
                    fee = path_decoded[token_pos + 1]
                    token_out_address = path_decoded[token_pos + 2]

                    first_swap = token_pos == 0
                    last_swap = token_pos == last_token_pos

                    v3_pool, pool_state = _simulate_v3_swap_exact_in(
                        params={
                            "params": (
                                token_in_address,
                                token_out_address,
                                fee,
                                # use amountIn for the first swap, otherwise take the output
                                # amount of the last swap (always negative so we can check
                                # for the min without knowing the token positions)
                                amount_in
                                if first_swap
                                else -min(
                                    pool_state["amount0_delta"],
                                    pool_state["amount1_delta"],
                                ),
                                # only apply minimum output to the last swap
                                amount_out_minimum if last_swap else None,
                                recipient
                                if last_swap
                                else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                            )
                        },
                        silent=silent,
                        first_swap=first_swap,
                    )
                    future_pool_states.append((v3_pool, pool_state))

            elif func_name == "exactOutputSingle":
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")
                future_pool_states.append(
                    _simulate_v3_swap_exact_out(
                        params=func_params,
                        silent=silent,
                        first_swap=True,
                        last_swap=True,
                    )
                )

            elif func_name == "exactOutput":
                """
                TBD
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                # Router ABI
                try:
                    (
                        path,
                        recipient,
                        deadline,
                        amount_out,
                        amount_in_maximum,
                    ) = func_params["params"]
                except Exception as e:
                    pass

                # Router2 ABI
                try:
                    (
                        path,
                        recipient,
                        amount_out,
                        amount_in_maximum,
                    ) = func_params["params"]
                except Exception as e:
                    pass

                path_decoded = decode_v3_path(path)

                if not silent:
                    logger.info(f" • path = {path_decoded}")
                    logger.info(f" • recipient = {recipient}")
                    try:
                        deadline
                    except:
                        pass
                    else:
                        logger.info(f" • deadline = {deadline}")
                    logger.info(f" • amountOut = {amount_out}")
                    logger.info(f" • amountInMaximum = {amount_in_maximum}")

                # the path is encoded in REVERSE order, so we decode from start to finish
                # tokenOut is the first position, tokenIn is the second position
                # e.g. tokenOut, fee, tokenIn
                last_token_pos = len(path_decoded) - 3

                for token_pos in range(
                    0,
                    len(path_decoded) - 2,
                    2,
                ):
                    token_out_address = path_decoded[token_pos]
                    fee = path_decoded[token_pos + 1]
                    token_in_address = path_decoded[token_pos + 2]

                    first_swap = token_pos == last_token_pos
                    last_swap = token_pos == 0

                    logger.debug(f"{first_swap=}")
                    logger.debug(f"{last_swap=}")

                    v3_pool, pool_state = _simulate_v3_swap_exact_out(
                        params={
                            "params": (
                                token_in_address,
                                token_out_address,
                                fee,
                                # use amountOut for the last swap (token_pos == 0),
                                # otherwise take the input amount of the previous swap
                                # (always positive so we can check for the max without
                                # knowing the token positions)
                                amount_out
                                if last_swap
                                else max(
                                    pool_state["amount0_delta"],
                                    pool_state["amount1_delta"],
                                ),
                                # only apply maximum input to the last swap
                                amount_in_maximum if first_swap else None,
                                recipient
                                if last_swap
                                else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                            )
                        },
                        silent=silent,
                        first_swap=first_swap,
                        last_swap=last_swap,
                    )

                    future_pool_states.append((v3_pool, pool_state))

            elif func_name == "unwrapWETH9":
                wrapped_token_address = _WRAPPED_NATIVE_TOKENS[self.chain_id]
                self._simulate_unwrap(wrapped_token_address)

            elif func_name == "sweepToken":
                """
                This function transfers the current token balance held by the contract to `recipient`
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    token_address = func_params["token"]
                    amount_out_minimum = func_params["amountMinimum"]
                    recipient = func_params.get("recipient")
                except Exception as e:
                    print(e)
                else:
                    # Router2 ABI omits `recipient`, always uses `msg.sender`
                    if recipient is None:
                        recipient = self.sender

                _balance = self._get_balance(
                    self.router_address, token_address
                )

                if _balance < amount_out_minimum:
                    raise ValueError(
                        f"Requested sweep of min. {amount_out_minimum} {token_address}, received {_balance}"
                    )

                self._simulate_sweep(token_address, recipient)

            # -----------------------------------------------------
            # Universal Router functions
            # -----------------------------------------------------
            elif func_name == "execute":
                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                commands = func_params["commands"]
                inputs = func_params["inputs"]
                # not used?
                # deadline = func_params.get("deadline")

                for command, input in zip(commands, inputs):
                    if result := _simulate_universal_router_dispatch(
                        command, input
                    ):
                        future_pool_states.extend(result)

            elif func_name in (
                "addLiquidity",
                "addLiquidityETH",
                "removeLiquidity",
                "removeLiquidityETH",
                "removeLiquidityETHWithPermit",
                "removeLiquidityETHSupportingFeeOnTransferTokens",
                "removeLiquidityETHWithPermitSupportingFeeOnTransferTokens",
                "removeLiquidityWithPermit",
            ):
                # TODO: add prediction for these functions
                logger.debug(f"TODO: {func_name}")
                raise TransactionError(
                    f"Aborting simulation involving un-implemented function {func_name}"
                )

            elif func_name in (
                "refundETH",
                "selfPermit",
                "selfPermitAllowed",
            ):
                # ignore, these functions do not affect future pool states
                pass

            else:
                logger.info(f"\tUNHANDLED function: {func_name}")

        # catch generic DegenbotError (non-fatal), everything else will escape
        except DegenbotError as e:
            raise TransactionError(f"Simulation failed: {e}") from e
        else:
            return future_pool_states

    def simulate(
        self,
        func_name: Optional[str] = None,
        func_params: Optional[Dict] = None,
        silent: bool = False,
    ) -> List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]:
        """
        Execute a simulation of a transaction, using the attributes stored in the constructor.

        Defers simulation to the `_simulate` method, which may recurse as needed for nested multicalls.

        Performs a final accounting check of addresses in `self.balance` ledger, excluding the `msg.sender` and `recipient` addresses.
        """

        result = self._simulate(func_name, func_params, silent)
        if set(self.balance) - set([self.sender, self.to]):
            logger.info("UNACCOUNTED BALANCE FOUND!")
            pprint(self.balance)
            import sys

            # hard-quit as a debugging technique to identify transactions that are not balanced
            sys.exit()
        return result
