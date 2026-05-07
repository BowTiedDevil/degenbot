from typing import TYPE_CHECKING, cast

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session

from degenbot.checksum_cache import get_checksum_address
from degenbot.database import db_session
from degenbot.database.models import Erc20TokenTable
from degenbot.exceptions.erc20 import NoPriceOracle
from degenbot.functions import (
    encode_function_calldata,
    raw_call,
)
from degenbot.provider import ProviderAdapter
from degenbot.types.abstract import AbstractErc20Token
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from hexbytes import HexBytes


def get_token_from_database(
    token: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session] = db_session,
) -> Erc20TokenTable | None:
    return session.scalar(
        select(Erc20TokenTable).where(
            Erc20TokenTable.address == token,
            Erc20TokenTable.chain == chain_id,
        )
    )


class Erc20Token(AbstractErc20Token):
    """
    An ERC-20 token contract.

    Constructed from pre-fetched data only. Use ``Bot.build_erc20token()`` to fetch from chain.
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
        self.address = get_checksum_address(address)

        self._state_cache_depth = state_cache_depth
        self._cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]] = {}
        self._cached_total_supply: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )

        self._chain_id = chain_id  # type: ignore[assignment]
        self.name = name
        self.symbol = symbol
        self.decimals = decimals
        self._price_oracle = None
        if oracle_address:
            from degenbot.chainlink import ChainlinkPriceContract

            self._price_oracle = ChainlinkPriceContract(address=oracle_address, chain_id=self.chain_id)

    # -- Cache accessors (dictionary operations, no I/O) --

    def get_cached_balance(self, address: ChecksumAddress, block_number: int) -> int | None:
        return self._cached_balance.get(address, {}).get(block_number)

    def set_cached_balance(self, address: ChecksumAddress, block_number: int, balance: int) -> None:
        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)
        self._cached_balance[address][block_number] = balance

    def get_cached_approval(
        self, block_number: int, owner: ChecksumAddress, spender: ChecksumAddress
    ) -> int | None:
        return self._cached_approval.get((block_number, owner, spender))

    def set_cached_approval(
        self, block_number: int, owner: ChecksumAddress, spender: ChecksumAddress, amount: int
    ) -> None:
        self._cached_approval[block_number, owner, spender] = amount

    def get_cached_total_supply(self, block_number: int) -> int | None:
        return self._cached_total_supply.get(block_number)

    def set_cached_total_supply(self, block_number: int, total_supply: int) -> None:
        self._cached_total_supply[block_number] = total_supply

    # -- RPC static methods (used by Bot.build_erc20token) --

    @staticmethod
    def fetch_name_symbol_decimals_batched(
        address: ChecksumAddress, provider: ProviderAdapter
    ) -> tuple[str, str, int]:
        """Fetch token name, symbol, and decimals via batched RPC calls."""
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
        """Fetch token name via RPC call."""
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
        """Fetch token symbol via RPC call."""
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
        """Fetch token decimals via RPC call."""
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

    @property
    def price(self) -> float:
        if self._price_oracle is None:
            raise NoPriceOracle
        return self._price_oracle.price

    @property
    def chain_id(self) -> int:
        return self._chain_id
