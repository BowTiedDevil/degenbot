"""Erc20Token: on-chain token with metadata, balance, and approval tracking."""

from typing import TYPE_CHECKING, Any, Self, cast

from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session

from degenbot.abi import AbiDecodeError, decode
from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models import Erc20TokenTable
from degenbot.erc20 import RustErc20Token
from degenbot.exceptions.infrastructure import NoPriceOracle
from degenbot.provider import AlloyProvider
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.types.abstract import AbstractErc20Token
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import BoundedCache
from degenbot.types.rpc_types import BlockIdentifier

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
        ),
    )


UNKNOWN_NAME = "Unknown Token"
UNKNOWN_SYMBOL = "UNKNOWN"
UNKNOWN_DECIMALS = 18


class Erc20Token(AbstractErc20Token):
    """An ERC-20 token contract.

    Constructed from pre-fetched data only. Use ``Bot.build_erc20token()`` to fetch from chain.
    Balance, approval, and total supply queries go through ``Bot.get_token_balance()`` etc.
    """

    # Instance attributes set in `_from_py_token` (the only construction seam —
    # `__init__` raises). Declared at class scope so the type checker tracks
    # them without inline annotations on the classmethod body (red-knot rejects
    # `self.x: T = ...` as `invalid-type-form`).
    _py_token: RustErc20Token
    _state_cache_depth: int
    _cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int]
    _cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]]
    _cached_total_supply: BoundedCache[BlockNumber, int]
    _price_oracle: ChainlinkPriceContract | None

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``Erc20Token`` is a Python companion over a Rust-owned
        ``RustErc20Token`` handle. The handle can only be produced by
        registering token metadata in a ``RustBot`` — there is no way for a
        caller to hand-build one. Use the registered entry points instead:

        - Production: ``Bot.get_token(address)``
        - Tests: ``make_erc20(...)``

        Both register the token metadata in Rust, obtain the ``RustErc20Token``
        handle, and wrap it via :meth:`_from_py_token` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.get_token(address) (production) or make_erc20(...) "
            "(tests) to register the token metadata in Rust and obtain the "
            "RustErc20Token handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_token(
        cls,
        py_token: RustErc20Token,
        *,
        oracle_address: str | None = None,
        state_cache_depth: int = 8,
    ) -> Self:
        """Wrap a Rust-owned ``RustErc20Token`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). Rust
        owns the token metadata (address, name, symbol, decimals, chain_id)
        as a ``TokenEntry``; this companion reads it through ``self._py_token``
        on every access and holds no metadata copy. Price oracle +
        balance/approval/total-supply caches stay Python (I/O constructs that
        cannot move to Rust).

        Only ``Bot.get_token()`` / ``Bot.build_erc20token()`` (production)
        and ``make_erc20`` (tests) should call this — they have already
        registered the token metadata in a ``RustBot`` and obtained the handle.
        ``cls`` is used so subclasses that only set ClassVars inherit this
        seam and produce instances of the subclass.

        Returns:
            A ``cls`` instance wrapping ``py_token``.

        """
        self = cls.__new__(cls)
        self._py_token = py_token

        self._state_cache_depth = state_cache_depth
        self._cached_approval = {}
        self._cached_balance = {}
        self._cached_total_supply = BoundedCache(
            max_items=state_cache_depth,
        )

        self._price_oracle = None
        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(
                address=oracle_address,
                chain_id=self.chain_id,
            )

        return self

    @property
    def address(self) -> ChecksumAddress:
        """Token contract address (EIP-55 checksum).

        Rust holds the address bytes; ``get_checksum_address`` applies the
        codebase-wide EIP-55 display convention.
        """
        return get_checksum_address(self._py_token.address)

    @property
    def name(self) -> str:
        """Token name (read from Rust-owned ``TokenEntry``)."""
        return self._py_token.name

    @property
    def symbol(self) -> str:
        """Token symbol (read from Rust-owned ``TokenEntry``)."""
        return self._py_token.symbol

    @property
    def decimals(self) -> int:
        """Token decimals (read from Rust-owned ``TokenEntry``)."""
        return self._py_token.decimals

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
        self,
        block_number: int,
        owner: ChecksumAddress,
        spender: ChecksumAddress,
    ) -> int | None:
        """Return cached approval.

        Returns:
            The computed value.

        """
        return self._cached_approval.get((block_number, owner, spender))

    def set_cached_approval(
        self,
        block_number: int,
        owner: ChecksumAddress,
        spender: ChecksumAddress,
        amount: int,
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
        address: ChecksumAddress,
        provider: AlloyProvider,
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

        (name,) = decode(types=["string"], data=name_result)
        (symbol,) = decode(types=["string"], data=symbol_result)
        (decimals,) = decode(types=["uint256"], data=decimals_result)

        return cast("str", name), cast("str", symbol), cast("int", decimals)

    @staticmethod
    def fetch_name(
        address: ChecksumAddress,
        provider: AlloyProvider,
        func_prototype: str = "name()",
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
            (name,) = decode(types=["string"], data=result)
            return cast("str", name)
        except AbiDecodeError:
            (name,) = decode(types=["bytes32"], data=result)
            return cast("HexBytes", name).decode("utf-8", errors="ignore").strip("\x00")

    @staticmethod
    def fetch_symbol(
        address: ChecksumAddress,
        provider: AlloyProvider,
        func_prototype: str = "symbol()",
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
            (symbol,) = decode(types=["string"], data=result)
            return cast("str", symbol)
        except AbiDecodeError:
            (symbol,) = decode(types=["bytes32"], data=result)
            return cast("HexBytes", symbol).decode("utf-8", errors="ignore").strip("\x00")

    @staticmethod
    def fetch_decimals(
        address: ChecksumAddress,
        provider: AlloyProvider,
        func_prototype: str = "decimals()",
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
        provider: AlloyProvider,
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
        (total_supply,) = decode(types=["uint256"], data=result)
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
    def chain_id(self) -> int:
        """Chain ID (read from Rust-owned ``TokenEntry``)."""
        return self._py_token.chain_id
