"""
Offline tests for ERC20 token comparisons and cache behavior.

Uses I/O-free token construction without requiring a live RPC.
"""

import pytest
from hexbytes import HexBytes

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20.erc20 import Erc20Token

WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")


@pytest.fixture
def offline_wbtc():
    return Erc20Token(
        WBTC_ADDRESS,
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
        chain_id=1,
    )


@pytest.fixture
def offline_weth():
    return Erc20Token(
        WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )


class TestErc20TokenComparisons:
    """Token comparison tests that need real token objects but no live RPC."""

    def test_equality_and_inequality(self, offline_wbtc, offline_weth):
        """ERC20Token compares to addresses and raises on unsupported types."""
        weth = offline_weth
        wbtc = offline_wbtc

        with pytest.raises(AssertionError):
            assert weth == 69

        with pytest.raises(TypeError):
            assert weth < 69

        with pytest.raises(TypeError):
            assert weth > 69

        assert weth != wbtc

        assert weth == weth.address
        assert weth == weth.address.lower()
        assert weth == weth.address.upper()
        assert weth == get_checksum_address(weth.address)
        assert weth == HexBytes(weth.address)
        assert weth == bytes.fromhex(weth.address[2:])

        assert wbtc == wbtc.address
        assert wbtc == wbtc.address.lower()
        assert wbtc == wbtc.address.upper()
        assert wbtc == get_checksum_address(wbtc.address)
        assert wbtc == HexBytes(wbtc.address)

        assert weth > wbtc
        assert weth > wbtc.address
        assert weth > wbtc.address.lower()
        assert weth > wbtc.address.upper()
        assert weth > get_checksum_address(wbtc.address)
        assert weth > HexBytes(wbtc.address)
        assert weth > bytes.fromhex(wbtc.address[2:])

        assert wbtc < weth
        assert wbtc < weth.address
        assert wbtc < weth.address.lower()
        assert wbtc < weth.address.upper()
        assert wbtc < get_checksum_address(weth.address)
        assert wbtc < HexBytes(weth.address)
        assert wbtc < bytes.fromhex(weth.address[2:])


class TestErc20TokenCaches:
    """ERC20Token cache manipulation tests (no RPC calls needed)."""

    def test_cached_total_supply(self, offline_wbtc):
        """Total supply cache round-trips through accessors."""
        wbtc = offline_wbtc

        # Cache is empty initially
        assert wbtc.get_cached_total_supply(block_number=100) is None

        # Set and retrieve
        fake_supply = 69_420_000_000
        wbtc.set_cached_total_supply(block_number=100, total_supply=fake_supply)
        assert wbtc.get_cached_total_supply(block_number=100) == fake_supply

        # Different block is still empty
        assert wbtc.get_cached_total_supply(block_number=200) is None

    def test_cached_name_and_symbol(self, offline_wbtc):
        """Name and symbol are stored from construction."""
        wbtc = offline_wbtc
        assert wbtc.name == "Wrapped BTC"
        assert wbtc.symbol == "WBTC"

    def test_decimals(self, offline_wbtc, offline_weth):
        """Decimals match known token properties."""
        assert offline_wbtc.decimals == 8
        assert offline_weth.decimals == 18
