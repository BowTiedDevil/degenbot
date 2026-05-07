import contextlib
import warnings
from typing import TYPE_CHECKING, cast

import eth_abi.abi
import sqlalchemy.exc
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3 import AsyncBaseProvider, AsyncWeb3, Web3
from web3.exceptions import Web3Exception
from web3.types import BlockIdentifier

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import async_connection_manager, connection_manager
from degenbot.database import db_session
from degenbot.database.models import Erc20TokenTable
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.erc20 import NoPriceOracle
from degenbot.functions import (
    encode_function_calldata,
    get_number_for_block_identifier,
    get_number_for_block_identifier_async,
    raw_call,
)
from degenbot.logging import logger
from degenbot.provider import AsyncProviderAdapter, ProviderAdapter
from degenbot.registry import token_registry
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

    Supports two construction modes:
    - **I/O-free mode** (preferred): pass ``name``, ``symbol``, ``decimals`` explicitly.
      No provider, database, or registry access occurs.
    - **Legacy I/O mode** (deprecated): pass ``provider`` and omit ``name``/``symbol``/``decimals``.
      Token metadata is fetched from the database or on-chain. This mode will be removed in a
      future release — use ``Bot.build_erc20token()`` instead.
    """

    UNKNOWN_NAME = "Unknown"
    UNKNOWN_SYMBOL = "UNKN"
    UNKNOWN_DECIMALS = 18

    def __init__(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        name: str | None = None,
        symbol: str | None = None,
        decimals: int | None = None,
        oracle_address: str | None = None,
        provider: ProviderAdapter | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        self.address = get_checksum_address(address)

        # Shared state (both paths)
        self._state_cache_depth = state_cache_depth
        self._cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]] = {}
        self._cached_total_supply: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )

        if name is not None and symbol is not None and decimals is not None:
            # ── I/O-free path ──
            self._chain_id = chain_id  # type: ignore[assignment]
            self.name = name
            self.symbol = symbol
            self.decimals = decimals
            self._price_oracle = None
            return

        # ── Legacy I/O path (deprecated) ──
        warnings.warn(
            "Constructing Erc20Token without pre-fetched name/symbol/decimals is deprecated. "
            "Use Bot.build_erc20token() instead.",
            DeprecationWarning,
            stacklevel=2,
        )

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id

        # Use injected provider, or fall back to global connection_manager
        self._provider = provider if provider is not None else self.provider

        token_from_db = get_token_from_database(
            token=self.address,
            chain_id=self.chain_id,
        )

        self.decimals = self.UNKNOWN_DECIMALS
        self.name = self.UNKNOWN_NAME
        self.symbol = self.UNKNOWN_SYMBOL

        # Attempt to load values from the DB
        if token_from_db is not None:
            if token_from_db.decimals is not None:
                self.decimals = token_from_db.decimals
            if token_from_db.name is not None:
                self.name = token_from_db.name
            if token_from_db.symbol is not None:
                self.symbol = token_from_db.symbol

        # Look up values from the contract if all defaults are still set
        if (
            self.decimals == self.UNKNOWN_DECIMALS
            and self.name == self.UNKNOWN_NAME
            and self.symbol == self.UNKNOWN_SYMBOL
        ):
            prov = self._provider

            if not prov.get_code(self.address):
                raise DegenbotValueError(message="No contract deployed at this address")

            try:
                self.name, self.symbol, self.decimals = self.fetch_name_symbol_decimals_batched(
                    address=self.address, provider=prov
                )
            except (Web3Exception, DecodingError):
                for func_prototype in ("name()", "NAME()"):
                    try:
                        self.name = self.fetch_name(
                            address=self.address, provider=prov, func_prototype=func_prototype
                        )
                    except (Web3Exception, DecodingError):
                        continue
                    else:
                        break

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        self.symbol = self.fetch_symbol(
                            address=self.address, provider=prov, func_prototype=func_prototype
                        )
                    except (Web3Exception, DecodingError):
                        continue
                    else:
                        break

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        self.decimals = self.fetch_decimals(
                            address=self.address, provider=prov, func_prototype=func_prototype
                        )
                    except (Web3Exception, DecodingError):
                        continue
                    else:
                        break

            if (
                token_from_db is not None
                and token_from_db.name is None
                and token_from_db.symbol is None
                and token_from_db.decimals is None
            ):
                with contextlib.suppress(sqlalchemy.exc.SQLAlchemyError), db_session() as session:
                    token_from_db.decimals = self.decimals
                    token_from_db.name = self.name
                    token_from_db.symbol = self.symbol
                    session.commit()

        self._price_oracle = None
        if oracle_address:
            from degenbot.chainlink import ChainlinkPriceContract

            self._price_oracle = ChainlinkPriceContract(address=oracle_address, chain_id=self.chain_id)

        token_registry.add(token_address=self.address, chain_id=self.chain_id, token=self)

        if not silent:  # pragma: no cover
            logger.info(f"• {self.symbol} ({self.name})")

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

    # -- Legacy I/O methods (deprecated, kept for backward compat) --

    def get_approval(
        self,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.\n\n        .. deprecated:: Use Bot.get_token_approval() instead.
        """
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.provider,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_approval[block_number, owner, spender]

        approval: int
        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.provider.call(
                to=self.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )
        self._cached_approval[block_number, owner, spender] = approval
        return approval

    async def get_approval_async(
        self,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.\n\n        .. deprecated:: Use AsyncBot.get_token_approval() instead.
        """
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else await get_number_for_block_identifier_async(
                block_identifier,
                self.async_w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_approval[block_number, owner, spender]

        approval: int
        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await self.async_provider.call(
                to=self.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )
        self._cached_approval[block_number, owner, spender] = approval
        return approval

    def get_balance(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address.\n\n        .. deprecated:: Use Bot.get_token_balance() instead.
        """
        address = get_checksum_address(address)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.provider,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_balance[address][block_number]

        balance: int
        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.provider.call(
                to=self.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[address]),
                block=block_number,
            ),
        )

        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)

        self._cached_balance[address][block_number] = balance
        return balance

    async def get_balance_async(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address.\n\n        .. deprecated:: Use AsyncBot.get_token_balance() instead.
        """
        address = get_checksum_address(address)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else await get_number_for_block_identifier_async(
                block_identifier,
                self.async_w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_balance[address][block_number]

        balance: int
        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await self.async_provider.call(
                to=self.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[address]),
                block=block_number,
            ),
        )

        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)

        self._cached_balance[address][block_number] = balance
        return balance

    def get_total_supply(self, block_identifier: BlockIdentifier | None = None) -> int:
        """Retrieve the total supply for this token.\n\n        .. deprecated:: Use Bot.get_token_total_supply() instead.
        """
        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.provider,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_total_supply[block_number]

        total_supply: int
        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.provider.call(
                to=self.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )
        self._cached_total_supply[block_number] = total_supply
        return total_supply

    async def get_total_supply_async(self, block_identifier: BlockIdentifier | None = None) -> int:
        """Retrieve the total supply for this token.\n\n        .. deprecated:: Use AsyncBot.get_token_total_supply() instead.
        """
        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else await get_number_for_block_identifier_async(
                block_identifier,
                self.async_w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_total_supply[block_number]

        total_supply: int
        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await self.async_provider.call(
                to=self.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )
        self._cached_total_supply[block_number] = total_supply
        return total_supply

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, symbol='{self.symbol}', name='{self.name}', decimals={self.decimals})"  # noqa:E501

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def price(self) -> float:
        if self._price_oracle is None:
            raise NoPriceOracle
        return self._price_oracle.price

    @property
    def provider(self) -> ProviderAdapter:
        return connection_manager.get_provider(self.chain_id)

    @property
    def async_w3(self) -> AsyncWeb3[AsyncBaseProvider]:
        return async_connection_manager.get_web3(self.chain_id)

    @property
    def async_provider(self) -> AsyncProviderAdapter:
        return async_connection_manager.get_provider(self.chain_id)
