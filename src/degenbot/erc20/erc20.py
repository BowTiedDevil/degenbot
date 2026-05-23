"""Erc20Token: on-chain token with metadata, balance, and approval tracking."""
from typing import TYPE_CHECKING, cast

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3.types import BlockIdentifier

from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models import Erc20TokenTable
from degenbot.exceptions.infrastructure import NoPriceOracle
from degenbot.provider import ProviderAdapter
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.types.abstract import AbstractErc20Token
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from hexbytes import HexBytes


def get_token_from_database(
    token: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session],
) -> Erc20TokenTable | None:
    """Return token from database.

    Returns:
        The computed value.

    """
    return session.scalar(
        select(Erc20TokenTable).where(
            Erc20TokenTable.address == token,
            Erc20TokenTable.chain == chain_id,
        )
    )


UNKNOWN_NAME = "Unknown Token"
UNKNOWN_SYMBOL = "UNKNOWN"
UNKNOWN_DECIMALS = 18


class Erc20Token(AbstractErc20Token):
    """An ERC-20 token contract.

    Constructed from pre-fetched data only. Use ``Bot.build_erc20token()`` to fetch from chain.
    Balance, approval, and total supply queries go through ``Bot.get_token_balance()`` etc.
    """

    def __init__(
        self,
        address: str,
        *,
        name: str,
        symbol: str,
        decimals: int,
        chain_id: ChainId | None = None,
        oracle_address: str | None = None,
        state_cache_depth: int = 8,
    ) -> None:
        """Initialize the instance."""
        self.address = get_checksum_address(address)

        self._state_cache_depth = state_cache_depth
        self._cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]] = {}
        self._cached_total_supply: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )

        self._chain_id = chain_id
        self.name = name
        self.symbol = symbol
        self.decimals = decimals
        self._price_oracle = None
        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(
                address=oracle_address, chain_id=self.chain_id
            )

    # -- Cache accessors (dictionary operations, no I/O) --

    def get_cached_balance(self, address: ChecksumAddress, block_number: int) -> int | None:
        """Return cached balance.

        Returns:
            The computed value.

        """
        cache = self._cached_balance.get(address, BoundedCache(max_items=self._state_cache_depth))
        return cache.get(block_number)

    def set_cached_balance(self, address: ChecksumAddress, block_number: int, balance: int) -> None:
        """Set cached balance."""
        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)
        self._cached_balance[address][block_number] = balance

    def get_cached_approval(
        self, block_number: int, owner: ChecksumAddress, spender: ChecksumAddress
    ) -> int | None:
        """Return cached approval.

        Returns:
            The computed value.

        """
        return self._cached_approval.get((block_number, owner, spender))

    def set_cached_approval(
        self, block_number: int, owner: ChecksumAddress, spender: ChecksumAddress, amount: int
    ) -> None:
        """Set cached approval."""
        self._cached_approval[block_number, owner, spender] = amount

    def get_cached_total_supply(self, block_number: int) -> int | None:
        """Return cached total supply.

        Returns:
            The computed value.

        """
        return self._cached_total_supply.get(block_number)

    def set_cached_total_supply(self, block_number: int, total_supply: int) -> None:
        """Set cached total supply."""
        self._cached_total_supply[block_number] = total_supply

    # -- RPC static methods (used by Bot.build_erc20token) --

    @staticmethod
    def fetch_name_symbol_decimals_batched(
        address: ChecksumAddress, provider: ProviderAdapter
    ) -> tuple[str, str, int]:
        """Fetch token name, symbol, and decimals via batched RPC calls.

        Returns:
            The computed value.

        """
        name_calldata = encode_function_calldata(
            function_prototype="name()",
            function_arguments=None,
        )
        symbol_calldata = encode_function_calldata(
            function_prototype="symbol()",
            function_arguments=None,
        )
        decimals_calldata = encode_function_calldata(
            function_prototype="decimals()",
            function_arguments=None,
        )

        name_result = provider.call(to=address, data=name_calldata)
        symbol_result = provider.call(to=address, data=symbol_calldata)
        decimals_result = provider.call(to=address, data=decimals_calldata)

        (name,) = eth_abi.abi.decode(types=["string"], data=name_result)
        (symbol,) = eth_abi.abi.decode(types=["string"], data=symbol_result)
        (decimals,) = eth_abi.abi.decode(types=["uint256"], data=decimals_result)

        return cast("str", name), cast("str", symbol), cast("int", decimals)

    @staticmethod
    def fetch_name(
        address: ChecksumAddress, provider: ProviderAdapter, func_prototype: str = "name()"
    ) -> str:
        """Fetch token name via RPC call.

        Returns:
            The computed string value.

        """
        result = provider.call(
            to=address,
            data=encode_function_calldata(
                function_prototype=func_prototype,
                function_arguments=None,
            ),
        )

        try:
            (name,) = eth_abi.abi.decode(types=["string"], data=result)
            return cast("str", name)
        except DecodingError:
            (name,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast("HexBytes", name).decode("utf-8", errors="ignore").strip("\x00")

    @staticmethod
    def fetch_symbol(
        address: ChecksumAddress, provider: ProviderAdapter, func_prototype: str = "symbol()"
    ) -> str:
        """Fetch token symbol via RPC call.

        Returns:
            The computed string value.

        """
        result = provider.call(
            to=address,
            data=encode_function_calldata(
                function_prototype=func_prototype,
                function_arguments=None,
            ),
        )

        try:
            (symbol,) = eth_abi.abi.decode(types=["string"], data=result)
            return cast("str", symbol)
        except DecodingError:
            (symbol,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast("HexBytes", symbol).decode("utf-8", errors="ignore").strip("\x00")

    @staticmethod
    def fetch_decimals(
        address: ChecksumAddress, provider: ProviderAdapter, func_prototype: str = "decimals()"
    ) -> int:
        """Fetch token decimals via RPC call.

        Returns:
            The computed integer value.

        """
        (result,) = raw_call(
            provider,
            address=address,
            calldata=encode_function_calldata(
                function_prototype=func_prototype,
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        return cast("int", result)

    @staticmethod
    def fetch_total_supply(
        address: ChecksumAddress,
        provider: ProviderAdapter,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Fetch total supply via RPC call.

        Returns:
            The computed integer value.

        """
        block: int | None = None
        if block_identifier is not None and isinstance(block_identifier, int):
            block = block_identifier

        result = provider.call(
            to=address,
            data=encode_function_calldata(
                function_prototype="totalSupply()",
                function_arguments=None,
            ),
            block=block,
        )
        (total_supply,) = eth_abi.abi.decode(types=["uint256"], data=result)
        return cast("int", total_supply)

    @property
    def price(self) -> float:
        """Price.

        Raises:
            NoPriceOracle: See function documentation.

        """
        if self._price_oracle is None:
            raise NoPriceOracle
        return self._price_oracle.price

    @property
    def chain_id(self) -> int | None:
        """Return chain id."""
        return self._chain_id
