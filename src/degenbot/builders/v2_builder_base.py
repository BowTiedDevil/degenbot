"""V2 builder base class and shared data types."""

from __future__ import annotations

import contextlib
from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.abi import decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import LiquidityPoolTable, UniswapFeeMixin
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.uniswap import resolve_deployer, resolve_v2_init_hash

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from hexbytes import HexBytes

    from degenbot.bot import PyBotIo
    from degenbot.builders.context import BuilderContext
    from degenbot.erc20 import Erc20Token
    from degenbot.types.abstract import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


@dataclass(frozen=True)
class V2CommonData:
    """Data fetched from DB/chain that all V2 variants need.

    Produced by V2BuilderBase._fetch_v2_common_data().
    Consumed by variant-specific build() methods.
    """

    pool_address: ChecksumAddress
    chain_id: ChainId
    factory: ChecksumAddress
    token0_address: ChecksumAddress
    token1_address: ChecksumAddress
    fee_token0: Fraction
    fee_token1: Fraction
    reserves0: int
    reserves1: int
    deployer: str
    init_hash: str
    state_block: int


class V2BuilderBase:
    """Base class for V2-style pool builders.

    Provides shared I/O orchestration (DB lookup, chain fetch,
    token construction, reserve fetch, registry lookup).
    Subclasses implement variant-specific construction and update.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        """Initialize the instance."""
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder
        self._py_bot = ctx.py_bot

    @staticmethod
    def decode_immutable_data(
        factory_result: HexBytes,
        token0_result: HexBytes,
        token1_result: HexBytes,
    ) -> tuple[ChecksumAddress, ChecksumAddress, ChecksumAddress]:
        """Decode raw call results into typed addresses.

        Returns:
            The computed value.

        """
        (factory_raw,) = decode(types=["address"], data=factory_result)
        (token0_raw,) = decode(types=["address"], data=token0_result)
        (token1_raw,) = decode(types=["address"], data=token1_result)
        return (
            get_checksum_address(factory_raw),
            get_checksum_address(token0_raw),
            get_checksum_address(token1_raw),
        )

    @staticmethod
    def extract_db_values(
        pool_from_db: LiquidityPoolTable,
    ) -> tuple[ChecksumAddress, ChecksumAddress, ChecksumAddress, Fraction, Fraction]:
        """Extract factory, token addresses, and fees from a DB row.

        Returns:
            The computed value.

        """
        factory = get_checksum_address(pool_from_db.exchange.factory)
        token0_address = get_checksum_address(pool_from_db.token0.address)
        token1_address = get_checksum_address(pool_from_db.token1.address)
        if isinstance(pool_from_db, UniswapFeeMixin):
            fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
            fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
        else:
            fee_token0 = Fraction(3, 1000)
            fee_token1 = Fraction(3, 1000)
        return factory, token0_address, token1_address, fee_token0, fee_token1

    @staticmethod
    def resolve_deployer_and_init_hash(
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
    ) -> tuple[str, str]:
        """Resolve deployer address and init hash from the Rust JSON resolver.

        Returns:
            The computed value.

        """
        deployer = resolve_deployer(chain_id, factory)
        resolved_init_hash = resolve_v2_init_hash(chain_id, factory)

        return deployer, resolved_init_hash

    @staticmethod
    def _fetch_v2_common_data(
        pool_address: str,
        *,
        chain_id: ChainId,
        state_block: int,
        io: PyBotIo,
    ) -> V2CommonData:
        """Fetch data shared by all V2 variants.

        Returns a frozen dataclass with all values needed
        for variant-specific construction.

        Returns:
            The computed value.

        Raises:
            LiquidityPoolError: If the operation fails.

        """
        pool_address = get_checksum_address(pool_address)

        # Try DB first — route the construction-time read through the Rust
        # `PyBotIo` seam (QVMWQC). `fetch_pool_row` returns the scalar + FK-id
        # columns; the `exchange` / `token0/1` relationships + the V2 subclass
        # fees hydrate via per-FK fetches, mirroring the prior SQLAlchemy
        # lazy-load. The `contextlib.suppress` makes a missing/empty DB a
        # cold-start (no snapshot pools), not an error.
        pool_found_in_db = False
        with contextlib.suppress(Exception):
            pool_row = io.fetch_pool_row(chain_id=chain_id, address=pool_address)
            if pool_row is not None:
                exchange_row = io.fetch_exchange(exchange_id=pool_row.exchange_id)
                token0_row = io.fetch_token_by_id(token_id=pool_row.token0_id)
                token1_row = io.fetch_token_by_id(token_id=pool_row.token1_id)
                if exchange_row is not None and token0_row is not None and token1_row is not None:
                    factory = get_checksum_address(exchange_row.factory)
                    token0_address = get_checksum_address(token0_row.address)
                    token1_address = get_checksum_address(token1_row.address)
                    # The V2 subclass row carries the fees
                    # (UniswapFeeMixin); a fee-bearing subclass has a
                    # non-zero `fee_denominator`. Pools whose subclass
                    # lacks fees (or has no subclass row) fall back to
                    # the 3/1000 default (mirrors the ORM
                    # `isinstance(UniswapFeeMixin)` branch).
                    kind_row = io.fetch_pool_kind(kind=pool_row.kind, pool_id=pool_row.id)
                    if kind_row is not None and kind_row.fee_denominator:
                        fee_token0 = Fraction(kind_row.fee_token0, kind_row.fee_denominator)
                        fee_token1 = Fraction(kind_row.fee_token1, kind_row.fee_denominator)
                    else:
                        fee_token0 = Fraction(3, 1000)
                        fee_token1 = Fraction(3, 1000)
                    pool_found_in_db = True

        # Get factory and token addresses
        if not pool_found_in_db:
            # ADR-005 slice 14e: delegate the 3-call immutable RPC choreography
            # to Rust (PyBotIo is the only executor; the Python parity-gate
            # fallback is retired).
            try:
                factory, token0_address, token1_address = io.fetch_v2_immutable_data(
                    pool_address,
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            # Default fee for V2 pools
            fee_token0 = Fraction(3, 1000)
            fee_token1 = Fraction(3, 1000)

        # Fetch reserves
        try:
            reserves0, reserves1 = io.fetch_v2_reserves(pool_address, block=state_block)
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        deployer, resolved_init_hash = V2BuilderBase.resolve_deployer_and_init_hash(
            chain_id=chain_id,
            factory=factory,
        )

        return V2CommonData(
            pool_address=pool_address,
            chain_id=chain_id,
            factory=factory,
            token0_address=token0_address,
            token1_address=token1_address,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            reserves0=reserves0,
            reserves1=reserves1,
            deployer=deployer,
            init_hash=resolved_init_hash,
            state_block=state_block,
        )

    def _register_pool(
        self,
        pool: AbstractLiquidityPool,
        *,
        chain_id: ChainId,
    ) -> None:
        self._pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

    @staticmethod
    def _log_pool(
        pool: AbstractLiquidityPool,
        *,
        silent: bool,
        token0: Erc20Token,
        token1: Erc20Token,
        reserves0: int,
        reserves1: int,
    ) -> None:
        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {reserves1}")

    @staticmethod
    def _fetch_reserves(
        pool_address: str,
        io: PyBotIo,
        *,
        block_identifier: int,
    ) -> tuple[int, int]:
        """Fetch current reserves from chain via PyBotIo.

        Returns:
            The computed value.

        """
        pool_address = get_checksum_address(pool_address)

        return io.fetch_v2_reserves(pool_address, block=block_identifier)
