"""Tests for async V2 pool builder (Plan 048, Slice 3)."""

from __future__ import annotations

from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
import pytest
from hexbytes import HexBytes
from web3 import Web3

from degenbot.builders.async_context import AsyncBuilderContext
from degenbot.builders.async_erc20_builder import AsyncErc20Builder
from degenbot.builders.async_v2_pool_builder import AsyncV2PoolBuilder
from degenbot.builders.pool_io import AsyncPoolIO
from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.database.models.pools import UniswapFeeMixin
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20 import Erc20Token
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

# --- Fake async provider ---


def _selector(signature: str) -> str:
    """Return the 4-byte function selector as a hex string (no 0x prefix)."""
    return Web3.keccak(text=signature)[:4].hex()


class FakeAsyncProvider:
    """Async fake that returns pre-programmed ABI-encoded responses."""

    def __init__(self, responses: dict[str, bytes]) -> None:
        self._responses = responses

    async def call(
        self, *, to: str, data: bytes, block: int | None = None  # noqa: ARG002
    ) -> HexBytes:
        selector = data[:4].hex()
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector}"
        raise ValueError(msg)

    async def get_block_number(self) -> int:
        return 1


def _v2_common_responses(
    *,
    factory: str = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
    token0: str = "0x0000000000000000000000000000000000000002",
    token1: str = "0x0000000000000000000000000000000000000003",
    reserves0: int = 1000,
    reserves1: int = 2000,
) -> dict[str, bytes]:
    """Build responses for common V2 data (factory, token0, token1, getReserves)."""
    return {
        _selector("factory()"): eth_abi.abi.encode(["address"], [factory]),
        _selector("token0()"): eth_abi.abi.encode(["address"], [token0]),
        _selector("token1()"): eth_abi.abi.encode(["address"], [token1]),
        _selector("getReserves()"): eth_abi.abi.encode(
            ["uint256", "uint256"], [reserves0, reserves1]
        ),
    }


class TestV2BuilderBaseDecodeHelpers:
    """Shared pure decode helpers on V2BuilderBase."""

    def test_decode_immutable_data(self) -> None:
        """decode_immutable_data decodes raw call results into typed addresses."""
        factory_result = eth_abi.abi.encode(
            ["address"], ["0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"]
        )
        token0_result = eth_abi.abi.encode(
            ["address"], ["0x0000000000000000000000000000000000000002"]
        )
        token1_result = eth_abi.abi.encode(
            ["address"], ["0x0000000000000000000000000000000000000003"]
        )

        factory, token0, token1 = V2BuilderBase.decode_immutable_data(
            factory_result=HexBytes(factory_result),
            token0_result=HexBytes(token0_result),
            token1_result=HexBytes(token1_result),
        )

        assert factory == "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        assert token0 == "0x0000000000000000000000000000000000000002"
        assert token1 == "0x0000000000000000000000000000000000000003"

    def test_extract_db_values_with_fee(self) -> None:
        """extract_db_values extracts factory, tokens, and fees from a UniswapFeeMixin DB row."""
        pool_from_db = MagicMock()
        pool_from_db.exchange.factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        pool_from_db.token0.address = "0x0000000000000000000000000000000000000002"
        pool_from_db.token1.address = "0x0000000000000000000000000000000000000003"
        pool_from_db.fee_token0 = 30
        pool_from_db.fee_token1 = 30
        pool_from_db.fee_denominator = 10000

        # Make isinstance check for UniswapFeeMixin work
        pool_from_db.__class__ = type(
            "FakeFeeMixin",
            (MagicMock, UniswapFeeMixin),
            {},
        )

        factory, token0, token1, fee0, fee1 = V2BuilderBase.extract_db_values(
            pool_from_db,
        )
        assert factory == "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        assert token0 == "0x0000000000000000000000000000000000000002"
        assert token1 == "0x0000000000000000000000000000000000000003"
        assert fee0 == Fraction(30, 10000)
        assert fee1 == Fraction(30, 10000)


class TestAsyncV2PoolBuilder:
    """AsyncV2PoolBuilder builds a UniswapV2Pool using AsyncPoolIO."""

    @pytest.mark.asyncio
    async def test_async_v2_builder_builds_pool(self) -> None:
        """AsyncV2PoolBuilder.build() produces a UniswapV2Pool using async I/O."""
        provider = FakeAsyncProvider(_v2_common_responses())
        io = AsyncPoolIO(provider)

        # Builder with mock dependencies — all I/O goes through io=
        erc20_builder = MagicMock(spec=AsyncErc20Builder)

        async def _build_token(address, *, chain_id=None, silent=False, io=None):  # noqa: ARG001, RUF029
            token = MagicMock(spec=Erc20Token)
            token.address = address
            token.chain_id = chain_id or 1
            return token

        erc20_builder.build = _build_token

        db = MagicMock(spec=DatabaseSessionManager)
        db.side_effect = RuntimeError("no db")

        ctx = AsyncBuilderContext(
            db=db,
            pools=MagicMock(spec=PoolRegistry),
            tokens=MagicMock(spec=TokenRegistry),
            erc20_builder=erc20_builder,
            default_chain_id=1,
        )

        builder = AsyncV2PoolBuilder(ctx)
        pool = await builder.build(
            "0x0000000000000000000000000000000000000001",
            chain_id=1,
            silent=True,
            io=io,
        )
        assert isinstance(pool, UniswapV2Pool)
        assert pool.reserves_token0 == 1000
        assert pool.reserves_token1 == 2000
