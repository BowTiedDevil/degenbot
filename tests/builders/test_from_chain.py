"""Tests for AerodromeV2Builder and CamelotBuilder variant-specific construction.

The I/O that was previously in pool.from_chain() classmethods, and then in
V2PoolBuilder._build_*() methods, is now handled by dedicated builders.
Each builder's build() method fetches common data via V2BuilderBase._fetch_v2_common_data(),
then variant-specific data from chain, then constructs the pool.
"""

from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.builders.aerodrome_v2_builder import AerodromeV2Builder
from degenbot.builders.camelot_builder import CamelotBuilder
from degenbot.builders.context import BuilderContext
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.pool_io import SyncPoolIO
from degenbot.builders.request import BuildPoolRequest
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20 import Erc20Token
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.uniswap.liquidity_pool import LiquidityPool


class _MockProviderError(Exception):
    """Raised when the fake provider has no mock response for a selector."""


class FakeProvider:
    """A fake provider that returns pre-programmed ABI-encoded responses.

    Also supports get_block_number() for state_block resolution.
    """

    def __init__(self, responses: dict[str, bytes]) -> None:
        self._responses = responses

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        selector = data[:4].hex()
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector}"
        raise _MockProviderError(msg)

    def get_block_number(self) -> int:
        return 1


def _selector(signature: str) -> str:
    """Return the 4-byte function selector as a hex string (no 0x prefix)."""
    return Web3.keccak(text=signature)[:4].hex()


def _make_erc20(address: str, chain_id: int = 42161) -> Erc20Token:
    """Create a minimal fake Erc20Token."""
    token = MagicMock(spec=Erc20Token)
    token.address = address
    token.chain_id = chain_id
    return token


AERO_CHAIN_ID = 8453
CAMELOT_CHAIN_ID = 42161


def _make_aerodrome_builder(provider: FakeProvider | None = None) -> AerodromeV2Builder:
    """Create an AerodromeV2Builder with mock dependencies."""
    erc20_builder = MagicMock(spec=Erc20Builder)

    def _build_token(address, *, chain_id=None, silent=False, io=None):
        return _make_erc20(address, chain_id=chain_id or AERO_CHAIN_ID)

    erc20_builder.build = _build_token

    # DB mock: raise on enter so contextlib.suppress skips it
    db = MagicMock(spec=DatabaseSessionManager)
    db.side_effect = RuntimeError("no db")

    ctx = BuilderContext(
        db=db,
        pools=MagicMock(spec=PoolRegistry),
        tokens=MagicMock(spec=TokenRegistry),
        erc20_builder=erc20_builder,
        default_chain_id=AERO_CHAIN_ID,
    )
    return AerodromeV2Builder(ctx)


def _make_camelot_builder(provider: FakeProvider | None = None) -> CamelotBuilder:
    """Create a CamelotBuilder with mock dependencies."""
    erc20_builder = MagicMock(spec=Erc20Builder)

    def _build_token(address, *, chain_id=None, silent=False, io=None):
        return _make_erc20(address, chain_id=chain_id or CAMELOT_CHAIN_ID)

    erc20_builder.build = _build_token

    # DB mock: raise on enter so contextlib.suppress skips it
    db = MagicMock(spec=DatabaseSessionManager)
    db.side_effect = RuntimeError("no db")

    ctx = BuilderContext(
        db=db,
        pools=MagicMock(spec=PoolRegistry),
        tokens=MagicMock(spec=TokenRegistry),
        erc20_builder=erc20_builder,
        default_chain_id=CAMELOT_CHAIN_ID,
    )
    return CamelotBuilder(ctx)


def _v2_common_responses(
    *,
    factory: str = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
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


def _aerodrome_provider(
    *,
    stable: bool = False,
    fee: int = 30,
    reserves0: int = 1000,
    reserves1: int = 2000,
) -> FakeProvider:
    """Build a FakeProvider with Aerodrome-specific responses."""
    responses = _v2_common_responses(reserves0=reserves0, reserves1=reserves1)
    responses[_selector("stable()")] = eth_abi.abi.encode(["bool"], [stable])
    responses[_selector("getFee(address,bool)")] = eth_abi.abi.encode(["uint256"], [fee])
    return FakeProvider(responses)


class TestAerodromeV2BuilderWithPoolIO:
    """AerodromeV2Builder.build() via PoolIO."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"

    def test_build_uses_io(self) -> None:
        """When io= is passed to build(), the builder uses it for I/O."""
        provider = _aerodrome_provider()
        io = SyncPoolIO(provider)
        builder = _make_aerodrome_builder()
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert isinstance(pool, AerodromeV2Pool)


class TestAerodromeV2BuilderConstruction:
    """Test AerodromeV2Builder.build() method."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
    FACTORY_ADDRESS = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"

    def test_build_aerodrome_v2_returns_pool(self) -> None:
        """build() should return an AerodromeV2Pool instance."""
        provider = _aerodrome_provider()
        io = SyncPoolIO(provider)
        builder = _make_aerodrome_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert isinstance(pool, AerodromeV2Pool)

    def test_build_aerodrome_v2_fetches_stable_and_fee(self) -> None:
        """build() should fetch stable and fee from chain."""
        provider = _aerodrome_provider(stable=True, fee=50)
        io = SyncPoolIO(provider)
        builder = _make_aerodrome_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert pool.stable is True
        assert pool.fee == Fraction(50, 10_000)

    def test_build_aerodrome_v2_volatile_pool(self) -> None:
        """build() with stable=False should create a volatile pool."""
        provider = _aerodrome_provider(stable=False, fee=30)
        io = SyncPoolIO(provider)
        builder = _make_aerodrome_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert pool.stable is False
        assert pool.fee == Fraction(30, 10_000)


def _camelot_provider(
    *,
    stable_swap: bool = False,
    fee_denominator: int = 1000,
    fee_token0: int = 3,
    fee_token1: int = 3,
    reserves0: int = 1000,
    reserves1: int = 2000,
) -> FakeProvider:
    """Build a FakeProvider with Camelot-specific responses."""
    responses = _v2_common_responses(
        factory="0x6EcCab422D763aC031210895C81787E87B43A652",
        reserves0=reserves0,
        reserves1=reserves1,
    )
    responses[_selector("stableSwap()")] = eth_abi.abi.encode(["bool"], [stable_swap])
    responses[_selector("FEE_DENOMINATOR()")] = eth_abi.abi.encode(["uint256"], [fee_denominator])
    responses[_selector("token0FeePercent()")] = eth_abi.abi.encode(["uint16"], [fee_token0])
    responses[_selector("token1FeePercent()")] = eth_abi.abi.encode(["uint16"], [fee_token1])
    return FakeProvider(responses)


class TestCamelotBuilderConstruction:
    """Test CamelotBuilder.build() method."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
    FACTORY_ADDRESS = "0x6EcCab422D763aC031210895C81787E87B43A652"

    def test_build_camelot_returns_camelot_pool(self) -> None:
        """build() should return a CamelotLiquidityPool instance."""
        provider = _camelot_provider()
        io = SyncPoolIO(provider)
        builder = _make_camelot_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert isinstance(pool, CamelotLiquidityPool)
        assert isinstance(pool, LiquidityPool)

    def test_build_camelot_fetches_camelot_specific_state(self) -> None:
        """build() should fetch stableSwap, FEE_DENOMINATOR, and fee percents."""
        provider = _camelot_provider(
            stable_swap=True, fee_denominator=1000, fee_token0=5, fee_token1=7
        )
        io = SyncPoolIO(provider)
        builder = _make_camelot_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert pool.stable_swap is True
        assert pool.fee_denominator == 1000
        assert pool.fee_token0 == Fraction(5, 1000)
        assert pool.fee_token1 == Fraction(7, 1000)

    def test_build_camelot_stable_swap_false(self) -> None:
        """build() with stable_swap=False should set the attribute accordingly."""
        provider = _camelot_provider(stable_swap=False)
        io = SyncPoolIO(provider)
        builder = _make_camelot_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert pool.stable_swap is False

    def test_build_camelot_sets_fee_as_fraction(self) -> None:
        """build() should set fee_token0/fee_token1 as Fractions."""
        provider = _camelot_provider(fee_denominator=10000, fee_token0=30, fee_token1=30)
        io = SyncPoolIO(provider)
        builder = _make_camelot_builder(provider)
        pool = builder.build(self.POOL_ADDRESS, io=io, request=BuildPoolRequest(silent=True))
        assert isinstance(pool.fee_token0, Fraction)
        assert isinstance(pool.fee_token1, Fraction)
        assert pool.fee_token0 == Fraction(30, 10000)
        assert pool.fee_token1 == Fraction(30, 10000)
