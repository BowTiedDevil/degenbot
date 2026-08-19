"""Erc20Token: on-chain token with metadata, balance, and approval tracking."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Self

from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.infrastructure import NoPriceOracle
from degenbot.types.abstract import AbstractErc20Token
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress
    from degenbot._ffi import Erc20Token as _TokenHandle
    from degenbot.types.aliases import BlockNumber

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
    _py_token: _TokenHandle
    _state_cache_depth: int
    _cached_approval: dict[tuple[int, ChecksummedAddress, ChecksummedAddress], int]
    _cached_balance: dict[ChecksummedAddress, BoundedCache[BlockNumber, int]]
    _cached_total_supply: BoundedCache[BlockNumber, int]
    _price_oracle: ChainlinkPriceContract | None

    def __init__(self, *args: Any, **kwargs: Any) -> None:  # ruff:ignore[unused-method-argument]
        """Direct construction is forbidden.

        ``Erc20Token`` is a Python companion over a Rust-owned
        ``_TokenHandle`` handle. The handle can only be produced by
        registering token metadata in a ``Bot`` — there is no way for a
        caller to hand-build one. Use the registered entry points instead:

        - Production: ``Bot.get_token(address)``
        - Tests: ``make_erc20(...)``

        Both register the token metadata in Rust, obtain the ``_TokenHandle``
        handle, and wrap it via :meth:`_from_py_token` (mirroring Polars'
        ``_from_pydf`` seam).

        Raises:
            TypeError: Always. Direct construction is not supported.

        """
        msg = (
            f"{type(self).__name__} cannot be constructed directly. "
            "Use Bot.get_token(address) (production) or make_erc20(...) "
            "(tests) to register the token metadata in Rust and obtain the "
            "_TokenHandle handle to wrap."
        )
        raise TypeError(msg)

    @classmethod
    def _from_py_token(
        cls,
        py_token: _TokenHandle,
        *,
        oracle_address: str | None = None,
        state_cache_depth: int = 8,
    ) -> Self:
        """Wrap a Rust-owned ``_TokenHandle`` handle as a Python companion.

        Internal seam (ADR-005, Polars-style ``_from_pydf`` pattern). Rust
        owns the token metadata (address, name, symbol, decimals, chain_id)
        as a ``TokenEntry``; this companion reads it through ``self._py_token``
        on every access and holds no metadata copy. Price oracle +
        balance/approval/total-supply caches stay Python (I/O constructs that
        cannot move to Rust).

        Only ``Bot.get_token()`` / ``Bot.build_erc20token()`` (production)
        and ``make_erc20`` (tests) should call this — they have already
        registered the token metadata in a ``Bot`` and obtained the handle.
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
    def address(self) -> ChecksummedAddress:
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

    def get_cached_balance(self, address: ChecksummedAddress, block_number: int) -> int | None:
        """Return cached balance.

        Returns:
            The computed value.

        """
        cache = self._cached_balance.get(address, BoundedCache(max_items=self._state_cache_depth))
        return cache.get(block_number)

    def set_cached_balance(
        self, address: ChecksummedAddress, block_number: int, balance: int
    ) -> None:
        """Set cached balance."""
        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)
        self._cached_balance[address][block_number] = balance

    def get_cached_approval(
        self,
        block_number: int,
        owner: ChecksummedAddress,
        spender: ChecksummedAddress,
    ) -> int | None:
        """Return cached approval.

        Returns:
            The computed value.

        """
        return self._cached_approval.get((block_number, owner, spender))

    def set_cached_approval(
        self,
        block_number: int,
        owner: ChecksummedAddress,
        spender: ChecksummedAddress,
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
