import itertools
from pprint import pprint
from typing import Dict, List, Optional, Set, Tuple, Union

import eth_abi
from brownie import chain  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.exceptions import (
    DegenbotError,
    EVMRevertError,
    LiquidityPoolError,
    ManagerError,
    TransactionError,
    LedgerError,
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

# Internal dict of known router contracts by chain ID. Pre-populated with
# mainnet addresses. New routers can be added by class method `add_router`
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

# Internal dict of known wrapped token contracts by chain ID.
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


def _raise_if_expired(deadline: int):
    if chain.time() > deadline:
        raise TransactionError("Deadline expired")


def get_v2_pools_from_token_path(
    tx_path,
    pool_manager: UniswapV2LiquidityPoolManager,
) -> List[LiquidityPool]:
    return [
        pool_manager.get_pool(
            token_addresses=token_addresses,
            silent=True,
        )
        for token_addresses in itertools.pairwise(tx_path)
    ]


class SimulationLedger:
    """
    Ledger for tracking token balances across a set of addresses
    """

    def __init__(self):
        # entries are recorded as a dict-of-dicts, keyed by address, then by token address
        self._balances: Dict[
            ChecksumAddress,  # address holding balance
            Dict[
                ChecksumAddress,  # token address
                int,  # balance
            ],
        ] = dict()

    def adjust(
        self,
        address: Union[str, ChecksumAddress],
        token: Union[Erc20Token, str, ChecksumAddress],
        amount: int,
    ) -> None:
        """
        Modify the balance for a given address and token.

        The amount can be positive (credit) or negative (debit).

        The method checksums all addresses.
        """

        _token_address: ChecksumAddress
        if isinstance(token, Erc20Token):
            _token_address = token.address
        elif isinstance(token, str):
            _token_address = Web3.toChecksumAddress(token)
        elif isinstance(token, ChecksumAddress):
            _token_address = token
        else:
            raise ValueError(
                f"Expected token type Erc20Token, str, or ChecksumAddress. Was {type(token)}"
            )

        _address = Web3.toChecksumAddress(address)

        address_balance: Dict[ChecksumAddress, int]
        try:
            address_balance = self._balances[_address]
        except KeyError:
            address_balance = {}
            self._balances[_address] = address_balance

        logger.debug(
            f"BALANCE: {_address} {'+' if amount > 0 else ''}{amount} {_token_address}"
        )

        try:
            address_balance[_token_address]
        except KeyError:
            address_balance[_token_address] = 0
        finally:
            address_balance[_token_address] += amount
            if address_balance[_token_address] == 0:
                del address_balance[_token_address]
            if not address_balance:
                del self._balances[_address]

    def token_balance(
        self,
        address: Union[str, ChecksumAddress],
        token: Union[Erc20Token, str, ChecksumAddress],
    ) -> int:
        """
        Get the balance for a given address and token.

        The method checksums all addresses.
        """

        _address = Web3.toChecksumAddress(address)

        if isinstance(token, Erc20Token):
            _token_address = token.address
        elif isinstance(token, str):
            _token_address = Web3.toChecksumAddress(token)
        elif isinstance(token, ChecksumAddress):
            _token_address = token
        else:
            raise ValueError(
                f"Expected token type Erc20Token, str, or ChecksumAddress. Was {type(token)}"
            )

        address_balances: Dict[ChecksumAddress, int]
        try:
            address_balances = self._balances[_address]
        except KeyError:
            address_balances = {}

        return address_balances.get(_token_address, 0)

    def transfer(
        self,
        token: Union[Erc20Token, str, ChecksumAddress],
        amount: int,
        from_addr: Union[ChecksumAddress, str],
        to_addr: Union[ChecksumAddress, str],
    ) -> None:
        if isinstance(token, Erc20Token):
            _token_address = token.address
        elif isinstance(token, str):
            _token_address = Web3.toChecksumAddress(token)
        elif isinstance(token, ChecksumAddress):
            _token_address = token
        else:
            raise ValueError(
                f"Expected token type Erc20Token, str, or ChecksumAddress. Was {type(token)}"
            )

        self.adjust(
            address=from_addr,
            token=_token_address,
            amount=-amount,
        )
        self.adjust(
            address=to_addr,
            token=_token_address,
            amount=amount,
        )


class UniswapTransaction(TransactionHelper):
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

    def __init__(
        self,
        chain_id: Union[int, str],
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

        # The `self.ledger` dictionary maintains a ledger of token balances for all addresses involved in the swap.
        # A positive balance represents a pre-swap deposit, a negative balance represents an outstanding withdrawal.
        # A full transaction should end with a positive balance of the desired output token, credited to `self.sender`.
        #
        # @dev Some routers have special flags that signify the swap amount should be looked up at the time of the swap,
        # as opposed to a specified amount at the time the transaction is built. The ledger is used to look up the balance
        # at any point inside the swap, and at the end to confirm that all balances have been accounted for.
        self.ledger = SimulationLedger()
        self.chain_id = (
            int(chain_id, 16) if isinstance(chain_id, str) else chain_id
        )
        self.routers = _ROUTERS[self.chain_id]
        self.sender = Web3.toChecksumAddress(tx_sender)
        self.to: Set[ChecksumAddress] = set()

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

        self.hash = tx_hash
        self.nonce = (
            int(tx_nonce, 16) if isinstance(tx_nonce, str) else tx_nonce
        )
        self.value = (
            int(tx_value, 16) if isinstance(tx_value, str) else tx_value
        )
        self.func_name = func_name
        self.func_params = func_params
        if previous_block_hash := self.func_params.get("previousBlockhash"):
            self.func_previous_block_hash = previous_block_hash.hex()

    def _simulate_v2_swap_exact_in(
        self,
        pool: LiquidityPool,
        recipient: Union[str, ChecksumAddress],
        token_in: Erc20Token,
        amount_in: int,
        amount_out_min: Optional[int] = None,
        first_swap: bool = False,
        last_swap: bool = False,
        silent: bool = False,
    ) -> Tuple[LiquidityPool, Dict]:
        """
        TBD
        """

        if token_in not in [pool.token0, pool.token1]:
            raise ValueError(f"Token {token_in} not found in pool {pool}")

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        future_state = pool.simulate_swap(
            token_in=token_in,
            token_in_quantity=amount_in,
        )

        _amount_out: int = -min(
            future_state["amount0_delta"],
            future_state["amount1_delta"],
        )

        if (
            amount_in == _UNIVERSAL_ROUTER_CONTRACT_BALANCE_FLAG
            or amount_in == _V3_ROUTER2_CONTRACT_BALANCE_FLAG
        ):
            _balance = self.ledger.token_balance(self.router_address, token_in)

            self.ledger.transfer(
                token=token_in,
                amount=_balance,
                to_addr=pool.address,
                from_addr=self.router_address,
            )
            amount_in = self.ledger.token_balance(pool.address, token_in)

        if first_swap and not self.ledger.token_balance(
            pool.address, token_in
        ):
            # credit the router if there is a zero balance
            if not self.ledger.token_balance(self.router_address, token_in):
                self.ledger.adjust(
                    self.router_address, token_in.address, amount_in
                )
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
            self.to.add(Web3.toChecksumAddress(recipient))

        if not silent:
            current_state = pool.state
            logger.info(f"Simulating swap through pool: {pool}")
            logger.info(
                f"\t{amount_in} {token_in} -> {_amount_out} {token_out}"
            )
            logger.info("\t(CURRENT)")
            logger.info(f"\t{pool.token0}: {current_state['reserves_token0']}")
            logger.info(f"\t{pool.token1}: {current_state['reserves_token1']}")
            logger.info(f"\t(FUTURE)")
            logger.info(f"\t{pool.token0}: {future_state['reserves_token0']}")
            logger.info(f"\t{pool.token1}: {future_state['reserves_token1']}")

        if (
            last_swap
            and amount_out_min is not None
            and _amount_out < amount_out_min
        ):
            raise TransactionError(
                f"Insufficient output for swap! {_amount_out} {token_out} received, {amount_out_min} required"
            )

        return pool, future_state

    def _simulate_v2_swap_exact_out(
        self,
        pool: LiquidityPool,
        recipient: Union[str, ChecksumAddress],
        token_in: Erc20Token,
        amount_out: int,
        amount_in_max: Optional[int] = None,
        first_swap: bool = False,
        last_swap: bool = False,
        silent: bool = False,
    ) -> Tuple[LiquidityPool, Dict]:
        """
        TBD
        """

        if token_in not in [pool.token0, pool.token1]:
            raise ValueError(f"Token {token_in} not found in pool {pool}")

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        future_state = pool.simulate_swap(
            token_out=token_out,
            token_out_quantity=amount_out,
        )

        _amount_in: int = max(
            future_state["amount0_delta"],
            future_state["amount1_delta"],
        )

        if first_swap:
            # transfer the input token amount from the sender to the first pool
            logger.info("FIRST SWAP")
            self.ledger.transfer(
                token=token_in.address,
                amount=_amount_in,
                from_addr=self.sender,
                to_addr=pool.address,
            )

        # TODO: handle recipient address handling within router-level logic
        if last_swap:
            logger.info("LAST SWAP")
            if recipient in [
                _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
            ]:
                _recipient = self.router_address
            else:
                _recipient = self.sender
        else:
            _recipient = Web3.toChecksumAddress(recipient)

        if not silent:
            current_state = pool.state
            logger.info(f"Simulating swap through pool: {pool}")
            logger.info(
                f"\t{_amount_in} {token_in} -> {amount_out} {token_out}"
            )
            logger.info("\t(CURRENT)")
            logger.info(f"\t{pool.token0}: {current_state['reserves_token0']}")
            logger.info(f"\t{pool.token1}: {current_state['reserves_token1']}")
            logger.info(f"\t(FUTURE)")
            logger.info(f"\t{pool.token0}: {future_state['reserves_token0']}")
            logger.info(f"\t{pool.token1}: {future_state['reserves_token1']}")

        # process the swap
        self.ledger.adjust(pool.address, token_in.address, -_amount_in)
        self.ledger.adjust(pool.address, token_out.address, amount_out)

        # transfer the output token from the pool to the recipient
        self.ledger.transfer(
            token=token_out.address,
            amount=amount_out,
            from_addr=pool.address,
            to_addr=_recipient,
        )

        if (
            first_swap
            and amount_in_max is not None
            and _amount_in > amount_in_max
        ):
            raise TransactionError(
                f"Required input {_amount_in} exceeds maximum {amount_in_max}"
            )

        return pool, future_state

    def _simulate_v3_swap_exact_in(
        self,
        pool: V3LiquidityPool,
        recipient: str,
        token_in: Erc20Token,
        amount_in: int,
        amount_out_min: Optional[int] = None,
        sqrt_price_limit_x96: Optional[int] = None,
        silent: bool = False,
        first_swap: bool = False,
    ) -> Tuple[V3LiquidityPool, Dict]:
        """
        TBD
        """

        token_out_quantity: int

        self.to.add(Web3.toChecksumAddress(recipient))

        if token_in not in [pool.token0, pool.token1]:
            raise ValueError

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        if (
            amount_in == _UNIVERSAL_ROUTER_CONTRACT_BALANCE_FLAG
            or amount_in == _V3_ROUTER2_CONTRACT_BALANCE_FLAG
        ):
            amount_in = self.ledger.token_balance(
                self.router_address, token_in.address
            )

        # the swap may occur after wrapping ETH, in which case amountIn will be already set.
        # if not, credit the router (user will send as part of contract call)
        if first_swap and not self.ledger.token_balance(
            self.router_address, token_in.address
        ):
            self.ledger.adjust(
                self.router_address,
                token_in.address,
                amount_in,
            )

        try:
            final_state = pool.simulate_swap(
                token_in=token_in,
                token_in_quantity=amount_in,
                sqrt_price_limit=sqrt_price_limit_x96,
            )
        except EVMRevertError as e:
            raise TransactionError(f"V3 revert: {e}")

        token_out_quantity = -min(
            final_state["amount0_delta"],
            final_state["amount1_delta"],
        )

        self.ledger.adjust(
            self.router_address,
            token_in.address,
            -amount_in,
        )

        logger.debug(f"{recipient=}")
        if recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
            recipient = self.sender
        elif recipient in [
            _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
            _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
        ]:
            recipient = self.router_address

        self.ledger.adjust(
            recipient,
            token_out.address,
            token_out_quantity,
        )

        if not silent:
            current_state = pool.state
            logger.info(f"Predicting output of swap through pool: {pool}")
            logger.info(
                f"\t{amount_in} {token_in} -> {token_out_quantity} {token_out}"
            )
            logger.info("\t(CURRENT)")
            logger.info(f"\tprice={current_state['sqrt_price_x96']}")
            logger.info(f"\tliquidity={current_state['liquidity']}")
            logger.info(f"\ttick={current_state['tick']}")
            logger.info(f"\t(FUTURE)")
            logger.info(f"\tprice={final_state['sqrt_price_x96']}")
            logger.info(f"\tliquidity={final_state['liquidity']}")
            logger.info(f"\ttick={final_state['tick']}")

        if amount_out_min is not None and token_out_quantity < amount_out_min:
            raise TransactionError(
                f"Insufficient output for swap! {token_out_quantity} {token_out} received, {amount_out_min} required"
            )

        return pool, final_state

    def _simulate_v3_swap_exact_out(
        self,
        pool: V3LiquidityPool,
        recipient: str,
        token_in: Erc20Token,
        amount_out: int,
        amount_in_max: Optional[int] = None,
        sqrt_price_limit_x96: Optional[int] = None,
        #
        silent: bool = False,
        first_swap: bool = False,
        last_swap: bool = False,
    ) -> Tuple[V3LiquidityPool, Dict]:
        """
        TBD
        """

        token_out: Erc20Token
        _amount_in: int
        _amount_out: int

        self.to.add(Web3.toChecksumAddress(recipient))

        if token_in not in [pool.token0, pool.token1]:
            raise ValueError

        token_out = pool.token1 if token_in == pool.token0 else pool.token0

        logger.info(f"{amount_out=}")
        logger.info(f"{amount_in_max=}")

        try:
            final_state = pool.simulate_swap(
                token_out=token_out,
                token_out_quantity=amount_out,
                sqrt_price_limit=sqrt_price_limit_x96,
            )
        except EVMRevertError as e:
            raise TransactionError(f"V3 revert: {e}")

        _amount_in = max(
            final_state["amount0_delta"],
            final_state["amount1_delta"],
        )
        _amount_out = -min(
            final_state["amount0_delta"],
            final_state["amount1_delta"],
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

        # Exact output swaps proceed in reverse order, so the last iteration will show a negative balance
        # of the input token, which must be accounted for.
        #
        # Check for a balance:
        #   - If zero, take no action
        #   - If non-zero, adjust with the assumption the user has paid that amount with the transaction call
        #
        if first_swap:
            swap_input_balance = self.ledger.token_balance(
                self.router_address, token_in.address
            )
            if swap_input_balance:
                self.ledger.adjust(
                    self.router_address,
                    token_in,
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
                _recipient = Web3.toChecksumAddress(recipient)
                self.to.add(_recipient)

            self.ledger.transfer(
                token=token_out.address,
                amount=_amount_out,
                from_addr=self.router_address,
                to_addr=_recipient,
            )

        if not silent:
            current_state = pool.state
            logger.info(f"Predicting output of swap through pool: {pool}")
            logger.info(
                f"\t{_amount_in} {token_in} -> {_amount_out} {token_out}"
            )
            logger.info("\t(CURRENT)")
            logger.info(f"\tprice={current_state['sqrt_price_x96']}")
            logger.info(f"\tliquidity={current_state['liquidity']}")
            logger.info(f"\ttick={current_state['tick']}")
            logger.info(f"\t(FUTURE)")
            logger.info(f"\tprice={final_state['sqrt_price_x96']}")
            logger.info(f"\tliquidity={final_state['liquidity']}")
            logger.info(f"\ttick={final_state['tick']}")

        if amount_in_max is not None and amount_in_max < _amount_in:
            raise TransactionError(
                f"Insufficient input for exact output swap! {_amount_in} {token_in} required, {amount_in_max} provided"
            )

        return pool, final_state

    def _simulate_unwrap(self, wrapped_token: str):
        logger.info(f"Unwrapping {wrapped_token}")

        wrapped_token_balance = self.ledger.token_balance(
            self.router_address, wrapped_token
        )

        self.ledger.adjust(
            self.router_address,
            wrapped_token,
            -wrapped_token_balance,
        )

    def _simulate_sweep(self, token: str, recipient: str):
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
        func_params: dict,
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

        V2_FUNCTIONS = {
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
            "multicall",
            "sweepToken",
            "unwrapWETH9",
            "unwrapWETH9WithFee",
        }

        UNIVERSAL_ROUTER_FUNCTIONS = {
            "execute",
        }

        # TODO: handle these
        UNHANDLED_FUNCTIONS = {
            "addLiquidity",
            "addLiquidityETH",
            "removeLiquidity",
            "removeLiquidityETH",
            "removeLiquidityETHWithPermit",
            "removeLiquidityETHSupportingFeeOnTransferTokens",
            "removeLiquidityETHWithPermitSupportingFeeOnTransferTokens",
            "removeLiquidityWithPermit",
            "sweepTokenWithFee",
        }

        NO_OP_FUNCTIONS = {
            "refundETH",
            "selfPermit",
            "selfPermitAllowed",
        }

        def _process_universal_router_dispatch(
            command_type: int,
            inputs: bytes,
            silent: bool,
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

            _amount_in: int
            _amount_out: int

            pool_state: Dict
            _future_pool_states: List[
                Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]
            ] = []

            if command in [
                "PERMIT2_TRANSFER_FROM",
                "PERMIT2_PERMIT_BATCH",
                "TRANSFER",
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

            elif command == "PAY_PORTION":
                """
                Transfers a portion of the current token balance held by the contract to `recipient`
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    _token, _recipient, bips = eth_abi.decode(
                        ["address", "address", "uint256"], inputs
                    )
                except:
                    raise ValueError(f"Could not decode input for {command}")

                # shorthand for ETH
                if _token == 0:
                    return

                _balance = self.ledger.token_balance(
                    self.router_address, _token
                )
                self.ledger.transfer(
                    _token,
                    _balance * bips // 10_000,
                    self.router_address,
                    _recipient,
                )
                self.to.add(Web3.toChecksumAddress(_recipient))

            elif command == "SWEEP":
                """
                This function transfers the current token balance held by the contract to `recipient`
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    token, tx_recipient, amountMin = eth_abi.decode(
                        ["address", "address", "uint256"], inputs
                    )
                except:
                    raise ValueError(f"Could not decode input for {command}")

                if tx_recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                    tx_recipient = self.sender

                _balance = self.ledger.token_balance(
                    self.router_address, token
                )

                if _balance < amountMin:
                    raise TransactionError(
                        f"Requested sweep of min. {amountMin} WETH, received {_balance}"
                    )

                self._simulate_sweep(token, tx_recipient)

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
                    tx_recipient, amountMin = eth_abi.decode(
                        ["address", "uint256"], inputs
                    )
                except:
                    raise ValueError(f"Could not decode input for {command}")

                if tx_recipient == _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG:
                    _recipient = self.router_address
                else:
                    _recipient = tx_recipient

                self.ledger.adjust(
                    _recipient,
                    wrapped_token_address,
                    amountMin,
                )

            elif command == "UNWRAP_WETH":
                """
                This function unwraps a quantity of WETH to ETH.

                ETH is currently untracked by the `self.ledger` ledger, so `recipient` is unused.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    tx_recipient, amountMin = eth_abi.decode(
                        ["address", "uint256"], inputs
                    )
                except:
                    raise ValueError(f"Could not decode input for {command}")

                wrapped_token_address = _WRAPPED_NATIVE_TOKENS[self.chain_id]
                wrapped_token_balance = self.ledger.token_balance(
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
                        tx_recipient,
                        tx_amount_in,
                        tx_amount_out_min,
                        tx_path,
                        tx_payer_is_user,
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
                    raise ValueError(f"Could not decode input for {command}")

                try:
                    pools = get_v2_pools_from_token_path(
                        tx_path, self.v2_pool_manager
                    )
                except (LiquidityPoolError, ManagerError):
                    raise TransactionError(
                        f"LiquidityPool could not be built for all steps in path {tx_path}"
                    )

                last_pool_pos = len(tx_path) - 2

                for pool_pos, pool in enumerate(pools):
                    first_swap = pool_pos == 0
                    last_swap = pool_pos == last_pool_pos

                    token_in = (
                        pool.token0
                        if tx_path[pool_pos] == pool.token0
                        else pool.token1
                    )

                    _amount_in = tx_amount_in if first_swap else _amount_out

                    _recipient = (
                        tx_recipient
                        if last_swap
                        else pools[pool_pos + 1].address
                    )

                    if _recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                        _recipient = self.sender
                    elif _recipient in [
                        _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                        _V3_ROUTER_CONTRACT_ADDRESS_FLAG,
                    ]:
                        _recipient = self.router_address

                    _, pool_state = self._simulate_v2_swap_exact_in(
                        pool=pool,
                        recipient=_recipient,
                        token_in=token_in,
                        amount_in=_amount_in,
                        amount_out_min=tx_amount_out_min
                        if last_swap
                        else None,
                        first_swap=first_swap,
                        last_swap=last_swap,
                        silent=silent,
                    )

                    _amount_out = -min(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
                    )

                    _future_pool_states.append((pool, pool_state))

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
                        tx_recipient,
                        tx_amount_out,
                        tx_amount_in_max,
                        tx_path,
                        tx_payer_is_user,
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
                    raise ValueError(f"Could not decode input for {command}")

                if tx_recipient == _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG:
                    tx_recipient = self.router_address
                elif tx_recipient == _UNIVERSAL_ROUTER_MSG_SENDER_ADDRESS_FLAG:
                    tx_recipient = self.sender

                _future_pool_states = []

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    pools = get_v2_pools_from_token_path(
                        tx_path, self.v2_pool_manager
                    )
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
                        tx_recipient
                        if last_swap
                        else pools[::-1][pool_pos - 1].address
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

                    _, pool_state = self._simulate_v2_swap_exact_out(
                        pool=pool,
                        recipient=_recipient,
                        token_in=_token_in,
                        amount_out=_amount_out,
                        amount_in_max=_amount_in_max,
                        first_swap=first_swap,
                        last_swap=last_swap,
                        silent=silent,
                    )

                    _amount_in = max(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
                    )

                    _future_pool_states.append((pool, pool_state))

                return _future_pool_states

            elif command == "V3_SWAP_EXACT_IN":
                """
                Decode an exact input swap through Uniswap V3 liquidity pools.

                Returns: a list of tuples representing the pool object and the final state of the pool after the swap completes.
                """

                if not silent:
                    logger.info(f"{func_name}: {self.hash}")

                try:
                    (
                        tx_recipient,
                        tx_amount_in,
                        tx_amount_out_min,
                        tx_path,
                        tx_payer_is_user,
                    ) = eth_abi.decode(
                        ["address", "uint256", "uint256", "bytes", "bool"],
                        inputs,
                    )
                except:
                    raise ValueError(f"Could not decode input for {command}")

                tx_path_decoded = decode_v3_path(tx_path)

                # decode the path - tokenIn is the first position, fee is the second position, tokenOut is the third position
                # paths can be an arbitrary length, but address & fee values are always interleaved
                # e.g. tokenIn, fee, tokenOut, fee,
                last_token_pos = len(tx_path_decoded) - 3

                for token_pos in range(
                    0,
                    len(tx_path_decoded) - 2,
                    2,
                ):
                    tx_token_in_address = tx_path_decoded[token_pos]
                    assert isinstance(tx_token_in_address, str)
                    fee = tx_path_decoded[token_pos + 1]
                    assert isinstance(fee, int)
                    tx_token_out_address = tx_path_decoded[token_pos + 2]
                    assert isinstance(tx_token_out_address, str)

                    first_swap = token_pos == 0
                    last_swap = token_pos == last_token_pos

                    v3_pool = self.v3_pool_manager.get_pool(
                        token_addresses=(
                            tx_token_in_address,
                            tx_token_out_address,
                        ),
                        pool_fee=fee,
                    )

                    _recipient = (
                        tx_recipient
                        if last_swap
                        else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG
                    )
                    _token_in = (
                        v3_pool.token0
                        if v3_pool.token0.address == tx_token_in_address
                        else v3_pool.token1
                    )
                    _amount_in = tx_amount_in if first_swap else _amount_out
                    _amount_out_min = tx_amount_out_min if last_swap else None

                    _, pool_state = self._simulate_v3_swap_exact_in(
                        pool=v3_pool,
                        recipient=_recipient,
                        token_in=_token_in,
                        amount_in=_amount_in,
                        amount_out_min=_amount_out_min,
                        silent=silent,
                        first_swap=first_swap,
                    )

                    _amount_out = -min(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
                    )

                    _future_pool_states.append((v3_pool, pool_state))

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
                        tx_recipient,
                        tx_amount_out,
                        tx_amount_in_max,
                        tx_path,
                        tx_payer_is_user,
                    ) = eth_abi.decode(
                        ["address", "uint256", "uint256", "bytes", "bool"],
                        inputs,
                    )
                except:
                    raise ValueError(f"Could not decode input for {command}")

                tx_path_decoded = decode_v3_path(tx_path)

                # an exact output path is encoded in REVERSE order,
                # tokenOut is the first position, tokenIn is the second position
                # e.g. tokenOut, fee, tokenIn
                last_token_pos = len(tx_path_decoded) - 3

                for token_pos in range(
                    0,
                    len(tx_path_decoded) - 2,
                    2,
                ):
                    tx_token_out_address = tx_path_decoded[token_pos]
                    assert isinstance(tx_token_out_address, str)
                    fee = tx_path_decoded[token_pos + 1]
                    assert isinstance(fee, int)
                    tx_token_in_address = tx_path_decoded[token_pos + 2]
                    assert isinstance(tx_token_in_address, str)

                    first_swap = token_pos == last_token_pos
                    last_swap = token_pos == 0

                    v3_pool = self.v3_pool_manager.get_pool(
                        token_addresses=(
                            tx_token_in_address,
                            tx_token_out_address,
                        ),
                        pool_fee=fee,
                    )

                    _recipient = (
                        tx_recipient
                        if last_swap
                        else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG
                    )
                    _token_in = (
                        v3_pool.token0
                        if v3_pool.token0.address == tx_token_in_address
                        else v3_pool.token1
                    )
                    _amount_out = tx_amount_out if last_swap else _amount_in
                    _amount_in_max = tx_amount_in_max if first_swap else None

                    _, pool_state = self._simulate_v3_swap_exact_out(
                        pool=v3_pool,
                        recipient=_recipient,
                        token_in=_token_in,
                        amount_out=_amount_out,
                        amount_in_max=_amount_in_max,
                        silent=silent,
                        first_swap=first_swap,
                        last_swap=last_swap,
                    )

                    _amount_in = max(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
                    )

                    # check that the output of each intermediate swap meets
                    # the input for the next swap
                    if not last_swap:
                        # pool states are appended to `future_pool_states`
                        # so the previous swap will be in the last position
                        _, _last_swap_state = _future_pool_states[-1]

                        _last_amount_in = max(
                            _last_swap_state["amount0_delta"],
                            _last_swap_state["amount1_delta"],
                        )

                        if _amount_out != _last_amount_in:
                            raise TransactionError(
                                f"Insufficient swap amount through requested pool {v3_pool}. Needed {_last_amount_in}, received {_amount_out}"
                            )

                    _future_pool_states.append((v3_pool, pool_state))

                return _future_pool_states

            else:
                raise ValueError(f"Invalid command {command}")

        def _process_v3_multicall(
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
                            raise ValueError(
                                f"Could not decode nested multicall: {e}"
                            ) from e
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
                    except TransactionError:
                        raise
                    except Exception as e:
                        import traceback

                        traceback.print_exc()
                        raise ValueError(f"Could not decode multicall: {e}")

            return _future_pool_states

        def _process_uniswap_v2_router_transaction() -> (
            List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]
        ):
            _future_pool_states: List[
                Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]
            ] = []
            _amount_in: int
            _amount_out: int

            try:
                if func_name in (
                    "swapExactTokensForETH",
                    "swapExactTokensForETHSupportingFeeOnTransferTokens",
                    "swapExactETHForTokens",
                    "swapExactETHForTokensSupportingFeeOnTransferTokens",
                    "swapExactTokensForTokens",
                    "swapExactTokensForTokensSupportingFeeOnTransferTokens",
                ):
                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")

                    try:
                        tx_amount_in = func_params["amountIn"]
                    except KeyError:
                        tx_amount_in = self.value  # 'swapExactETHForTokens'

                    tx_amount_out_min = func_params["amountOutMin"]
                    tx_path = func_params["path"]
                    tx_recipient = func_params["to"]
                    try:
                        tx_deadline = func_params["deadline"]
                    except KeyError:
                        pass
                    else:
                        _raise_if_expired(tx_deadline)

                    try:
                        pools = get_v2_pools_from_token_path(
                            tx_path, self.v2_pool_manager
                        )
                    except (LiquidityPoolError, ManagerError):
                        raise TransactionError(
                            f"LiquidityPool could not be built for all steps in path {tx_path}"
                        )

                    last_pool_pos = len(tx_path) - 2

                    for pool_pos, pool in enumerate(pools):
                        first_swap = pool_pos == 0
                        last_swap = pool_pos == last_pool_pos

                        _token_in = (
                            pool.token0
                            if tx_path[pool_pos] == pool.token0
                            else pool.token1
                        )

                        _amount_in = (
                            tx_amount_in if first_swap else _amount_out
                        )

                        _recipient = (
                            tx_recipient
                            if last_swap
                            else pools[pool_pos + 1].address
                        )

                        _, pool_state = self._simulate_v2_swap_exact_in(
                            pool=pool,
                            recipient=_recipient,
                            token_in=_token_in,
                            amount_in=_amount_in,
                            amount_out_min=tx_amount_out_min
                            if last_swap
                            else None,
                            first_swap=first_swap,
                            last_swap=last_swap,
                            silent=silent,
                        )

                        _amount_out = -min(
                            pool_state["amount0_delta"],
                            pool_state["amount1_delta"],
                        )

                        _future_pool_states.append((pool, pool_state))

                    return _future_pool_states

                elif func_name in (
                    "swapTokensForExactETH",
                    "swapTokensForExactTokens",
                    "swapETHForExactTokens",
                ):
                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")

                    _future_pool_states = []

                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")

                    tx_amount_out = func_params["amountOut"]
                    try:
                        tx_amount_in_max = func_params["amountInMax"]
                    except KeyError:
                        tx_amount_in_max = (
                            self.value
                        )  # 'swapETHForExactTokens'
                    tx_path = func_params["path"]
                    tx_recipient = func_params["to"]
                    try:
                        tx_deadline = func_params["deadline"]
                    except KeyError:
                        pass
                    else:
                        _raise_if_expired(tx_deadline)

                    try:
                        pools = get_v2_pools_from_token_path(
                            tx_path, self.v2_pool_manager
                        )
                    except (LiquidityPoolError, ManagerError):
                        raise TransactionError(
                            f"LiquidityPool could not be built for all steps in path {tx_path}"
                        )

                    last_pool_pos = len(pools) - 1

                    for pool_pos, pool in enumerate(pools[::-1]):
                        first_swap = pool_pos == last_pool_pos
                        last_swap = pool_pos == 0

                        _amount_out = (
                            tx_amount_out if last_swap else _amount_in
                        )
                        _amount_in_max = (
                            tx_amount_in_max if first_swap else None
                        )

                        _recipient = (
                            tx_recipient
                            if last_swap
                            else pools[::-1][pool_pos - 1].address
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

                        _, pool_state = self._simulate_v2_swap_exact_out(
                            pool=pool,
                            recipient=_recipient,
                            token_in=_token_in,
                            amount_out=_amount_out,
                            amount_in_max=_amount_in_max,
                            first_swap=first_swap,
                            last_swap=last_swap,
                            silent=silent,
                        )

                        _amount_in = max(
                            pool_state["amount0_delta"],
                            pool_state["amount1_delta"],
                        )

                        _future_pool_states.append((pool, pool_state))

                    return _future_pool_states

            # bugfix: prevents nested multicalls from spamming exception message
            # e.g. 'Simulation failed: Simulation failed: {error}'
            except TransactionError:
                raise
            # catch generic DegenbotError (non-fatal) and re-raise as TransactionError
            except DegenbotError as e:
                raise TransactionError(f"Simulation failed: {e}") from e
            else:
                return future_pool_states

        def _process_uniswap_v3_router_transaction() -> (
            List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]
        ):
            try:
                if func_name == "multicall":
                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")
                    future_pool_states.extend(
                        _process_v3_multicall(
                            params=func_params, silent=silent
                        )
                    )

                elif func_name == "exactInputSingle":
                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")

                    # decode with Router ABI
                    # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                    try:
                        (
                            tx_token_in_address,
                            tx_token_out_address,
                            tx_fee,
                            tx_recipient,
                            tx_deadline,
                            tx_amount_in,
                            tx_amount_out_min,
                            sqrt_price_limit_x96,
                        ) = func_params["params"]
                    except:
                        pass
                    else:
                        _raise_if_expired(tx_deadline)

                    # decode with Router2 ABI
                    # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                    try:
                        (
                            tx_token_in_address,
                            tx_token_out_address,
                            tx_fee,
                            tx_recipient,
                            tx_amount_in,
                            tx_amount_out_min,
                            sqrt_price_limit_x96,
                        ) = func_params["params"]
                    except:
                        pass

                    v3_pool = self.v3_pool_manager.get_pool(
                        token_addresses=(
                            tx_token_in_address,
                            tx_token_out_address,
                        ),
                        pool_fee=tx_fee,
                    )

                    _, pool_state = self._simulate_v3_swap_exact_in(
                        pool=v3_pool,
                        recipient=tx_recipient,
                        token_in=v3_pool.token0
                        if v3_pool.token0.address == tx_token_in_address
                        else v3_pool.token1,
                        amount_in=tx_amount_in,
                        amount_out_min=tx_amount_out_min,
                        silent=silent,
                        first_swap=True,
                    )

                    future_pool_states.append((v3_pool, pool_state))

                    token_out_quantity = -min(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
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
                            tx_path,
                            tx_recipient,
                            tx_deadline,
                            tx_amount_in,
                            tx_amount_out_minimum,
                        ) = func_params["params"]
                    except:
                        pass
                    else:
                        _raise_if_expired(tx_deadline)

                    # from IV3SwapRouter.sol - https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                    try:
                        (
                            tx_path,
                            tx_recipient,
                            tx_amount_in,
                            tx_amount_out_minimum,
                        ) = func_params["params"]
                    except:
                        pass

                    tx_path_decoded = decode_v3_path(tx_path)

                    if not silent:
                        logger.info(f" • path = {tx_path_decoded}")
                        logger.info(f" • recipient = {tx_recipient}")
                        try:
                            tx_deadline
                        except:
                            pass
                        else:
                            logger.info(f" • deadline = {tx_deadline}")
                        logger.info(f" • amountIn = {tx_amount_in}")
                        logger.info(
                            f" • amountOutMinimum = {tx_amount_out_minimum}"
                        )

                    last_token_pos = len(tx_path_decoded) - 3

                    for token_pos in range(
                        0,
                        len(tx_path_decoded) - 2,
                        2,
                    ):
                        tx_token_in_address = tx_path_decoded[token_pos]
                        assert isinstance(tx_token_in_address, str)
                        tx_fee = tx_path_decoded[token_pos + 1]
                        assert isinstance(tx_fee, int)
                        tx_token_out_address = tx_path_decoded[token_pos + 2]
                        assert isinstance(tx_token_out_address, str)

                        first_swap = token_pos == 0
                        last_swap = token_pos == last_token_pos

                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(
                                tx_token_in_address,
                                tx_token_out_address,
                            ),
                            pool_fee=tx_fee,
                        )

                        _, pool_state = self._simulate_v3_swap_exact_in(
                            pool=v3_pool,
                            recipient=tx_recipient
                            if last_swap
                            else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG,
                            token_in=v3_pool.token0
                            if v3_pool.token0.address == tx_token_in_address
                            else v3_pool.token1,
                            amount_in=tx_amount_in
                            if token_pos == 0
                            else token_out_quantity,
                            # only apply minimum output to the last swap
                            amount_out_min=tx_amount_out_minimum
                            if last_swap
                            else None,
                            silent=silent,
                            first_swap=first_swap,
                        )

                        future_pool_states.append((v3_pool, pool_state))

                        token_out_quantity = -min(
                            pool_state["amount0_delta"],
                            pool_state["amount1_delta"],
                        )

                elif func_name == "exactOutputSingle":
                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")

                    # decode with Router ABI
                    # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
                    try:
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
                    except:
                        pass
                    else:
                        _raise_if_expired(tx_deadline)

                    # decode with Router2 ABI
                    # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
                    try:
                        (
                            tx_token_in_address,
                            tx_token_out_address,
                            tx_fee,
                            tx_recipient,
                            tx_amount_out,
                            tx_amount_in_max,
                            tx_sqrt_price_limit_x96,
                        ) = func_params["params"]
                    except:
                        pass

                    v3_pool = self.v3_pool_manager.get_pool(
                        token_addresses=(
                            tx_token_in_address,
                            tx_token_out_address,
                        ),
                        pool_fee=tx_fee,
                    )

                    _, pool_state = self._simulate_v3_swap_exact_out(
                        pool=v3_pool,
                        recipient=tx_recipient,
                        token_in=v3_pool.token0
                        if v3_pool.token0.address == tx_token_in_address
                        else v3_pool.token1,
                        amount_out=tx_amount_out,
                        amount_in_max=tx_amount_in_max,
                        silent=silent,
                        first_swap=True,
                        last_swap=True,
                    )

                    future_pool_states.append((v3_pool, pool_state))

                    amount_deposited = max(
                        pool_state["amount0_delta"],
                        pool_state["amount1_delta"],
                    )
                    if amount_deposited > tx_amount_in_max:
                        raise TransactionError(
                            f"Maximum input exceeded. Specified {tx_amount_in_max}, {amount_deposited} required."
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
                            tx_path,
                            tx_recipient,
                            tx_deadline,
                            tx_amount_out,
                            tx_amount_in_max,
                        ) = func_params["params"]
                    except Exception as e:
                        pass
                    else:
                        _raise_if_expired(tx_deadline)

                    # Router2 ABI
                    try:
                        (
                            tx_path,
                            tx_recipient,
                            tx_amount_out,
                            tx_amount_in_max,
                        ) = func_params["params"]
                    except Exception as e:
                        pass

                    tx_path_decoded = decode_v3_path(tx_path)

                    if not silent:
                        logger.info(f" • path = {tx_path_decoded}")
                        logger.info(f" • recipient = {tx_recipient}")
                        try:
                            tx_deadline
                        except:
                            pass
                        else:
                            logger.info(f" • deadline = {tx_deadline}")
                        logger.info(f" • amountOut = {tx_amount_out}")
                        logger.info(f" • amountInMaximum = {tx_amount_in_max}")

                    # an exact output path is encoded in REVERSE order,
                    # tokenOut is the first position, tokenIn is the second position
                    # e.g. tokenOut, fee, tokenIn
                    last_token_pos = len(tx_path_decoded) - 3

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

                        v3_pool = self.v3_pool_manager.get_pool(
                            token_addresses=(
                                tx_token_in_address,
                                tx_token_out_address,
                            ),
                            pool_fee=tx_fee,
                        )

                        _recipient = (
                            tx_recipient
                            if last_swap
                            else _UNIVERSAL_ROUTER_CONTRACT_ADDRESS_FLAG
                        )
                        _token_in = (
                            v3_pool.token0
                            if v3_pool.token0.address == tx_token_in_address
                            else v3_pool.token1
                        )
                        _amount_out = (
                            tx_amount_out if last_swap else _amount_in
                        )
                        _amount_in_max = (
                            tx_amount_in_max if first_swap else None
                        )

                        _, pool_state = self._simulate_v3_swap_exact_out(
                            pool=v3_pool,
                            recipient=_recipient,
                            token_in=_token_in,
                            amount_out=_amount_out,
                            amount_in_max=_amount_in_max,
                            silent=silent,
                            first_swap=first_swap,
                            last_swap=last_swap,
                        )

                        _amount_in: int = max(
                            pool_state["amount0_delta"],
                            pool_state["amount1_delta"],
                        )

                        _amount_out = -min(
                            pool_state["amount0_delta"],
                            pool_state["amount1_delta"],
                        )

                        # check that the output of each intermediate swap meets
                        # the input for the next swap
                        if not last_swap:
                            # pool states are appended to `future_pool_states`
                            # so the previous swap will be in the last position
                            _, _last_swap_state = future_pool_states[-1]

                            _last_amount_in = max(
                                _last_swap_state["amount0_delta"],
                                _last_swap_state["amount1_delta"],
                            )

                            if _amount_out != _last_amount_in:
                                raise TransactionError(
                                    f"Insufficient swap amount through requested pool {v3_pool}. Needed {_last_amount_in}, received {_amount_out}"
                                )

                        future_pool_states.append((v3_pool, pool_state))

                    # V3 Router enforces a maximum input
                    if first_swap:
                        _, _pool_state = future_pool_states[0]

                        amount_deposited = max(
                            _pool_state["amount0_delta"],
                            _pool_state["amount1_delta"],
                        )

                        if amount_deposited > tx_amount_in_max:
                            raise TransactionError(
                                f"Maximum input exceeded. Specified {tx_amount_in_max}, {amount_deposited} required."
                            )

                elif func_name == "unwrapWETH9":
                    # TODO: if ETH balances are ever needed, handle the ETH
                    # transfer resulting from this function
                    amountMin = func_params["amountMinimum"]
                    wrapped_token_address = _WRAPPED_NATIVE_TOKENS[
                        self.chain_id
                    ]
                    wrapped_token_balance = self.ledger.token_balance(
                        self.router_address, wrapped_token_address
                    )
                    if wrapped_token_balance < amountMin:
                        raise TransactionError(
                            f"Requested unwrap of min. {amountMin} WETH, received {wrapped_token_balance}"
                        )

                    self._simulate_unwrap(wrapped_token_address)

                elif func_name == "unwrapWETH9WithFee":
                    # TODO: if ETH balances are ever needed, handle the two ETH
                    # transfers resulting from this function
                    _amount_in = func_params["amountMinimum"]
                    _recipient = func_params["recipient"]
                    _fee_bips = func_params["feeBips"]
                    _fee_recipient = func_params["feeRecipient"]

                    wrapped_token_address = _WRAPPED_NATIVE_TOKENS[
                        self.chain_id
                    ]
                    wrapped_token_balance = self.ledger.token_balance(
                        self.router_address, wrapped_token_address
                    )
                    if wrapped_token_balance < _amount_in:
                        raise TransactionError(
                            f"Requested unwrap of min. {_amount_in} WETH, received {wrapped_token_balance}"
                        )

                    self._simulate_unwrap(wrapped_token_address)

                elif func_name == "sweepToken":
                    """
                    This function transfers the current token balance held by the contract to `recipient`
                    """

                    if not silent:
                        logger.info(f"{func_name}: {self.hash}")

                    try:
                        tx_token_address = func_params["token"]
                        tx_amount_out_minimum = func_params["amountMinimum"]
                        tx_recipient = func_params.get("recipient")
                    except Exception as e:
                        print(e)
                    else:
                        # Router2 ABI omits `recipient`, always uses `msg.sender`
                        if tx_recipient is None:
                            tx_recipient = self.sender

                    _balance = self.ledger.token_balance(
                        self.router_address, tx_token_address
                    )

                    if _balance < tx_amount_out_minimum:
                        raise TransactionError(
                            f"Requested sweep of min. {tx_amount_out_minimum} {tx_token_address}, received {_balance}"
                        )

                    self._simulate_sweep(tx_token_address, tx_recipient)

            # bugfix: prevents nested multicalls from spamming exception message
            # e.g. 'Simulation failed: Simulation failed: {error}'
            except TransactionError:
                raise
            # catch generic DegenbotError (non-fatal) and re-raise as TransactionError
            except DegenbotError as e:
                raise TransactionError(f"Simulation failed: {e}") from e
            else:
                return future_pool_states

        def _process_uniswap_universal_router_transaction() -> (
            List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]
        ):
            _future_pool_states: List[
                Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]
            ] = []

            if func_name != "execute":
                raise ValueError(
                    f"UNHANDLED UNIVERSAL ROUTER FUNCTION: {func_name}"
                )

            if not silent:
                logger.info(f"{func_name}: {self.hash}")

            try:
                try:
                    tx_deadline = func_params["deadline"]
                except KeyError:
                    pass
                else:
                    _raise_if_expired(tx_deadline)

                tx_commands = func_params["commands"]
                tx_inputs = func_params["inputs"]

                for command, input in zip(tx_commands, tx_inputs):
                    _future_pool_states.append(
                        _process_universal_router_dispatch(
                            command,
                            input,
                            silent=silent,
                        )
                    )

            # bugfix: prevents nested multicalls from spamming exception message
            # e.g. 'Simulation failed: Simulation failed: {error}'
            except TransactionError:
                raise
            # catch generic DegenbotError (non-fatal) and re-raise as TransactionError
            except DegenbotError as e:
                raise TransactionError(f"Simulation failed: {e}") from e
            else:
                return _future_pool_states

        if func_name in V2_FUNCTIONS:
            future_pool_states.extend(
                _process_uniswap_v2_router_transaction(),
            )
        elif func_name in V3_FUNCTIONS:
            future_pool_states.extend(
                _process_uniswap_v3_router_transaction(),
            )
        elif func_name in UNIVERSAL_ROUTER_FUNCTIONS:
            future_pool_states.extend(
                _process_uniswap_universal_router_transaction(),
            )
        elif func_name in UNHANDLED_FUNCTIONS:
            # TODO: add prediction for these functions
            logger.debug(f"TODO: {func_name}")
            raise TransactionError(
                f"Aborting simulation involving un-implemented function: {func_name}"
            )
        elif func_name in NO_OP_FUNCTIONS:
            logger.debug(f"NON-OP: {func_name}")
            pass
        else:
            # logger.info(f"UNHANDLED: {func_name}")
            raise ValueError(f"UNHANDLED: {func_name}")

        return future_pool_states

    def simulate(
        self,
        silent: bool = False,
    ) -> List[Tuple[Union[LiquidityPool, V3LiquidityPool], Dict]]:
        """
        Execute a simulation of a transaction, using the attributes stored in the constructor.

        Defers simulation to the `_simulate` method, which may recurse as needed for nested multicalls.

        Performs a final accounting check of addresses in `self.ledger` ledger, excluding the `msg.sender` and `recipient` addresses.
        """

        result = self._simulate(
            self.func_name,
            self.func_params,
            silent,
        )

        if set(self.ledger._balances) - set([self.sender]) - self.to:
            pprint(self.ledger._balances)
            raise LedgerError("UNACCOUNTED BALANCE FOUND!")

        return result
