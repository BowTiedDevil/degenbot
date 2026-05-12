"""
Tests for V2PoolBuilder's variant-specific construction of Aerodrome and Camelot pools.

The I/O that was previously in pool.from_chain() classmethods is now handled
by the builder, which fetches variant-specific data from chain and passes it
to the pool constructor.
"""

from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.v2_pool_builder import V2PoolBuilder
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20 import Erc20Token
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

# --- Fake provider that returns pre-programmed responses ---


class _MockProviderError(Exception):
    """Raised when the fake provider has no mock response for a selector."""


class FakeProvider:
    """A fake provider that returns pre-programmed ABI-encoded responses."""

    def __init__(self, responses: dict[str, bytes]) -> None:
        """
        responses maps function selector hex strings → ABI-encoded return bytes.
        """
        self._responses = responses

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:  # noqa: ARG002
        # Match by first 4 bytes (function selector)
        selector = data[:4].hex()
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector}"
        raise _MockProviderError(msg)


# Helper: compute the 4-byte function selector
def _selector(signature: str) -> str:
    """Return the 4-byte function selector as a hex string (no 0x prefix)."""
    return Web3.keccak(text=signature)[:4].hex()


def _make_erc20(address: str, chain_id: int = 42161) -> Erc20Token:
    """Create a minimal fake Erc20Token."""
    token = MagicMock(spec=Erc20Token)
    token.address = address
    token.chain_id = chain_id
    return token


def _make_builder(provider: MagicMock | None = None) -> V2PoolBuilder:
    """Create a V2PoolBuilder with mock dependencies."""
    connections = MagicMock(spec=ConnectionManager)
    if provider is not None:
        connections.get_provider.return_value = provider
    return V2PoolBuilder(
        connections=connections,
        db=MagicMock(spec=DatabaseSessionManager),
        pools=MagicMock(spec=PoolRegistry),
        tokens=MagicMock(spec=TokenRegistry),
        erc20_builder=MagicMock(spec=Erc20Builder),
    )


# --- CamelotLiquidityPool construction via builder ---


def _camelot_provider(
    *,
    stable_swap: bool = False,
    fee_denominator: int = 1000,
    fee_token0: int = 3,
    fee_token1: int = 3,
) -> FakeProvider:
    """Build a FakeProvider with Camelot-specific responses."""
    return FakeProvider(
        {
            _selector("stableSwap()"): eth_abi.abi.encode(["bool"], [stable_swap]),
            _selector("FEE_DENOMINATOR()"): eth_abi.abi.encode(
                ["uint256"], [fee_denominator]
            ),
            _selector("token0FeePercent()"): eth_abi.abi.encode(
                ["uint16"], [fee_token0]
            ),
            _selector("token1FeePercent()"): eth_abi.abi.encode(
                ["uint16"], [fee_token1]
            ),
        }
    )


class TestCamelotBuilderConstruction:
    """Test V2PoolBuilder._build_camelot() method."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
    TOKEN0_ADDRESS = "0x0000000000000000000000000000000000000002"
    TOKEN1_ADDRESS = "0x0000000000000000000000000000000000000003"
    FACTORY_ADDRESS = "0x6EcCab422D763aC031210895C81787E87B43A652"

    def test_build_camelot_returns_camelot_pool(self) -> None:
        """_build_camelot should return a CamelotLiquidityPool instance."""
        builder = _make_builder(_camelot_provider())
        pool = builder._build_camelot(
            pool_address=self.POOL_ADDRESS,
            pool_class=CamelotLiquidityPool,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_camelot_provider(),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert isinstance(pool, CamelotLiquidityPool)
        assert isinstance(pool, UniswapV2Pool)

    def test_build_camelot_fetches_camelot_specific_state(self) -> None:
        """_build_camelot should fetch stableSwap, FEE_DENOMINATOR, and fee percents."""
        builder = _make_builder()
        pool = builder._build_camelot(
            pool_address=self.POOL_ADDRESS,
            pool_class=CamelotLiquidityPool,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_camelot_provider(
                stable_swap=True,
                fee_denominator=1000,
                fee_token0=5,
                fee_token1=7,
            ),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert pool.stable_swap is True
        assert pool.fee_denominator == 1000
        assert pool.fee_token0 == Fraction(5, 1000)
        assert pool.fee_token1 == Fraction(7, 1000)

    def test_build_camelot_stable_swap_false(self) -> None:
        """_build_camelot with stable_swap=False should set the attribute accordingly."""
        builder = _make_builder()
        pool = builder._build_camelot(
            pool_address=self.POOL_ADDRESS,
            pool_class=CamelotLiquidityPool,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_camelot_provider(stable_swap=False),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert pool.stable_swap is False

    def test_build_camelot_sets_fee_as_fraction(self) -> None:
        """_build_camelot should set fee_token0/fee_token1 as Fractions."""
        builder = _make_builder()
        pool = builder._build_camelot(
            pool_address=self.POOL_ADDRESS,
            pool_class=CamelotLiquidityPool,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_camelot_provider(fee_denominator=10000, fee_token0=30, fee_token1=30),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert isinstance(pool.fee_token0, Fraction)
        assert isinstance(pool.fee_token1, Fraction)
        assert pool.fee_token0 == Fraction(30, 10000)
        assert pool.fee_token1 == Fraction(30, 10000)

    def test_build_camelot_passes_deployer_address(self) -> None:
        """_build_camelot should forward deployer_address to the constructor."""
        builder = _make_builder()
        pool = builder._build_camelot(
            pool_address=self.POOL_ADDRESS,
            pool_class=CamelotLiquidityPool,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_camelot_provider(),
            state_block=1,
            deployer="0x0000000000000000000000000000000000000004",
        )
        assert pool.deployer == "0x0000000000000000000000000000000000000004"


# --- AerodromeV2Pool construction via builder ---


def _aerodrome_provider(
    *,
    stable: bool = False,
    fee: int = 30,
) -> FakeProvider:
    """Build a FakeProvider with Aerodrome-specific responses."""
    return FakeProvider(
        {
            _selector("stable()"): eth_abi.abi.encode(["bool"], [stable]),
            _selector("getFee(address,bool)"): eth_abi.abi.encode(
                ["uint256"], [fee]
            ),
        }
    )


class TestAerodromeV2BuilderConstruction:
    """Test V2PoolBuilder._build_aerodrome_v2() method."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
    TOKEN0_ADDRESS = "0x0000000000000000000000000000000000000002"
    TOKEN1_ADDRESS = "0x0000000000000000000000000000000000000003"
    FACTORY_ADDRESS = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"

    def test_build_aerodrome_v2_returns_pool(self) -> None:
        """_build_aerodrome_v2 should return an AerodromeV2Pool instance."""
        builder = _make_builder()
        pool = builder._build_aerodrome_v2(
            pool_address=self.POOL_ADDRESS,
            pool_class=AerodromeV2Pool,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_aerodrome_provider(),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert isinstance(pool, AerodromeV2Pool)

    def test_build_aerodrome_v2_fetches_stable_and_fee(self) -> None:
        """_build_aerodrome_v2 should fetch stable and fee from chain."""
        builder = _make_builder()
        pool = builder._build_aerodrome_v2(
            pool_address=self.POOL_ADDRESS,
            pool_class=AerodromeV2Pool,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_aerodrome_provider(stable=True, fee=50),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert pool.stable is True
        assert pool.fee == Fraction(50, 10_000)

    def test_build_aerodrome_v2_volatile_pool(self) -> None:
        """_build_aerodrome_v2 with stable=False should create a volatile pool."""
        builder = _make_builder()
        pool = builder._build_aerodrome_v2(
            pool_address=self.POOL_ADDRESS,
            pool_class=AerodromeV2Pool,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_aerodrome_provider(stable=False, fee=30),
            state_block=1,
            deployer=self.FACTORY_ADDRESS,
        )
        assert pool.stable is False
        assert pool.fee == Fraction(30, 10_000)

    def test_build_aerodrome_v2_passes_deployer_address(self) -> None:
        """_build_aerodrome_v2 should forward deployer_address to the constructor."""
        builder = _make_builder()
        pool = builder._build_aerodrome_v2(
            pool_address=self.POOL_ADDRESS,
            pool_class=AerodromeV2Pool,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves0=1000,
            reserves1=2000,
            provider=_aerodrome_provider(),
            state_block=1,
            deployer="0x0000000000000000000000000000000000000004",
        )
        assert pool.deployer_address == "0x0000000000000000000000000000000000000004"
