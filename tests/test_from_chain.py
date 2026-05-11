"""
Tests for `from_chain` classmethod on pool classes that need custom construction
logic (Camelot, AerodromeV2).

The `from_chain` classmethod encapsulates the class-specific chain fetches that
Bot otherwise has to hard-code as branches in build_v2_pool.
"""

from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.erc20 import Erc20Token
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


# --- CamelotLiquidityPool.from_chain ---


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


class TestCamelotFromChain:
    """Test CamelotLiquidityPool.from_chain classmethod."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
    TOKEN0_ADDRESS = "0x0000000000000000000000000000000000000002"
    TOKEN1_ADDRESS = "0x0000000000000000000000000000000000000003"
    FACTORY_ADDRESS = "0x6EcCab422D763aC031210895C81787E87B43A652"

    def test_from_chain_exists(self) -> None:
        """CamelotLiquidityPool should have a from_chain classmethod."""
        assert hasattr(CamelotLiquidityPool, "from_chain")
        assert callable(CamelotLiquidityPool.from_chain)

    def test_from_chain_returns_camelot_pool(self) -> None:
        """from_chain should return a CamelotLiquidityPool instance."""
        pool = CamelotLiquidityPool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_camelot_provider(),
            state_block=1,
        )
        assert isinstance(pool, CamelotLiquidityPool)
        assert isinstance(pool, UniswapV2Pool)

    def test_from_chain_fetches_camelot_specific_state(self) -> None:
        """from_chain should fetch stableSwap, FEE_DENOMINATOR, and fee percents."""
        pool = CamelotLiquidityPool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_camelot_provider(
                stable_swap=True,
                fee_denominator=1000,
                fee_token0=5,
                fee_token1=7,
            ),
            state_block=1,
        )
        assert pool.stable_swap is True
        assert pool.fee_denominator == 1000
        assert pool.fee_token0 == Fraction(5, 1000)
        assert pool.fee_token1 == Fraction(7, 1000)

    def test_from_chain_stable_swap_false(self) -> None:
        """from_chain with stable_swap=False should set the attribute accordingly."""
        pool = CamelotLiquidityPool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_camelot_provider(stable_swap=False),
            state_block=1,
        )
        assert pool.stable_swap is False

    def test_from_chain_sets_fee_as_fraction(self) -> None:
        """from_chain should set fee_token0/fee_token1 as Fractions."""
        pool = CamelotLiquidityPool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_camelot_provider(fee_denominator=10000, fee_token0=30, fee_token1=30),
            state_block=1,
        )
        assert isinstance(pool.fee_token0, Fraction)
        assert isinstance(pool.fee_token1, Fraction)
        assert pool.fee_token0 == Fraction(30, 10000)
        assert pool.fee_token1 == Fraction(30, 10000)

    def test_from_chain_passes_deployer_address(self) -> None:
        """from_chain should forward deployer_address to the constructor."""
        pool = CamelotLiquidityPool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS),
            token1=_make_erc20(self.TOKEN1_ADDRESS),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_camelot_provider(),
            state_block=1,
            deployer_address="0x0000000000000000000000000000000000000004",
        )
        assert pool.deployer == "0x0000000000000000000000000000000000000004"


# --- AerodromeV2Pool.from_chain ---


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


class TestAerodromeV2FromChain:
    """Test AerodromeV2Pool.from_chain classmethod."""

    POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
    TOKEN0_ADDRESS = "0x0000000000000000000000000000000000000002"
    TOKEN1_ADDRESS = "0x0000000000000000000000000000000000000003"
    FACTORY_ADDRESS = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"

    def test_from_chain_exists(self) -> None:
        """AerodromeV2Pool should have a from_chain classmethod."""
        assert hasattr(AerodromeV2Pool, "from_chain")
        assert callable(AerodromeV2Pool.from_chain)

    def test_from_chain_returns_aerodrome_v2_pool(self) -> None:
        """from_chain should return an AerodromeV2Pool instance."""
        pool = AerodromeV2Pool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_aerodrome_provider(),
            state_block=1,
        )
        assert isinstance(pool, AerodromeV2Pool)

    def test_from_chain_fetches_stable_and_fee(self) -> None:
        """from_chain should fetch stable and fee from chain."""
        pool = AerodromeV2Pool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_aerodrome_provider(stable=True, fee=50),
            state_block=1,
        )
        assert pool.stable is True
        assert pool.fee == Fraction(50, 10_000)

    def test_from_chain_volatile_pool(self) -> None:
        """from_chain with stable=False should create a volatile pool."""
        pool = AerodromeV2Pool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_aerodrome_provider(stable=False, fee=30),
            state_block=1,
        )
        assert pool.stable is False
        assert pool.fee == Fraction(30, 10_000)

    def test_from_chain_passes_deployer_address(self) -> None:
        """from_chain should forward deployer_address to the constructor."""
        pool = AerodromeV2Pool.from_chain(
            address=self.POOL_ADDRESS,
            token0=_make_erc20(self.TOKEN0_ADDRESS, chain_id=8453),
            token1=_make_erc20(self.TOKEN1_ADDRESS, chain_id=8453),
            factory=self.FACTORY_ADDRESS,
            reserves_token0=1000,
            reserves_token1=2000,
            provider=_aerodrome_provider(),
            state_block=1,
            deployer_address="0x0000000000000000000000000000000000000004",
        )
        assert pool.deployer_address == "0x0000000000000000000000000000000000000004"


# --- V2 pools WITHOUT from_chain should not break ---


class TestStandardV2NoFromChain:
    """Standard V2 pools don't need from_chain — they use the standard constructor."""

    def test_uniswap_v2_has_no_from_chain(self) -> None:
        """UniswapV2Pool should not have from_chain (not needed)."""
        assert not hasattr(UniswapV2Pool, "from_chain")

    def test_aerodrome_v3_has_no_from_chain(self) -> None:
        """AerodromeV3Pool doesn't need from_chain (same constructor as UniswapV3)."""
        assert not hasattr(AerodromeV3Pool, "from_chain")
