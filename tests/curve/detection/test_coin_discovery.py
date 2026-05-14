"""Tests for Curve pool coin discovery."""

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.coin_discovery import discover_coins
from degenbot.functions import encode_function_calldata

# Well-known token addresses for tests
USDC = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
DAI = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
USDT = get_checksum_address("0xdAC17F958D2ee523a2206206994597C13D831ec7")
ZERO_ADDR = "0x0000000000000000000000000000000000000000"
POOL_ADDR = get_checksum_address("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

# Selectors
COINS_UINT256 = encode_function_calldata("coins(uint256)", [0])[:4]
COINS_INT128 = encode_function_calldata("coins(int128)", [0])[:4]
BALANCES_UINT256 = encode_function_calldata("balances(uint256)", [0])[:4]
BALANCES_INT128 = encode_function_calldata("balances(int128)", [0])[:4]


def _encode_addr(addr: str) -> bytes:
    return eth_abi.abi.encode(["address"], [addr])


def _encode_uint256(val: int) -> bytes:
    return eth_abi.abi.encode(["uint256"], [val])


class TestDiscoverCoinsUint256:
    """Coin discovery when the pool uses coins(uint256)."""

    def testTwoCoinPool(self):
        """A 2-coin pool returning USDC and DAI with balances."""
        coins = [USDC, DAI]
        balances = [1_000_000, 2_000_000]

        def handle_coins_uint256(to: str, data: bytes, block: int) -> bytes:
            # Decode the index from the calldata (after 4-byte selector)
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            if idx < len(coins):
                return _encode_addr(coins[idx])
            return _encode_addr(ZERO_ADDR)

        def handle_balances_uint256(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            if idx < len(balances):
                return _encode_uint256(balances[idx])
            return _encode_uint256(0)

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_UINT256: handle_coins_uint256,
            BALANCES_UINT256: handle_balances_uint256,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)

        assert len(result.token_addresses) == 2
        assert result.token_addresses[0] == USDC
        assert result.token_addresses[1] == DAI
        assert result.balances == (1_000_000, 2_000_000)
        assert result.coin_prototype == "coins(uint256)"
        assert result.balance_prototype == "balances(uint256)"

    def testThreeCoinPool(self):
        """A 3-coin pool (like the Curve tripool)."""
        coins = [DAI, USDC, USDT]

        def handle_coins_uint256(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            if idx < len(coins):
                return _encode_addr(coins[idx])
            return _encode_addr(ZERO_ADDR)

        def handle_balances_uint256(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            return _encode_uint256(1000 * (idx + 1))

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_UINT256: handle_coins_uint256,
            BALANCES_UINT256: handle_balances_uint256,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)

        assert len(result.token_addresses) == 3
        assert result.balances == (1000, 2000, 3000)


class TestDiscoverCoinsInt128:
    """Coin discovery when the pool uses coins(int128)."""

    def testTwoCoinInt128Pool(self):
        """A pool that reverts on coins(uint256) but works with coins(int128)."""
        coins = [DAI, USDC]

        def handle_coins_int128(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["int128"], data[4:])
            if idx < len(coins):
                return _encode_addr(coins[idx])
            return _encode_addr(ZERO_ADDR)

        def handle_balances_int128(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["int128"], data[4:])
            return _encode_uint256(5000)

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_INT128: handle_coins_int128,
            BALANCES_INT128: handle_balances_int128,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)

        assert result.coin_prototype == "coins(int128)"
        assert result.balance_prototype == "balances(int128)"
        assert len(result.token_addresses) == 2

    def testUint256FailsThenInt128Works(self):
        """Pool that reverts on uint256, works on int128 — confirms fallback."""
        coins = [DAI, USDC]

        call_count = {"uint256_tries": 0}

        def handle_coins_uint256(to: str, data: bytes, block: int) -> bytes:
            call_count["uint256_tries"] += 1
            raise Exception("revert")

        def handle_coins_int128(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["int128"], data[4:])
            if idx < len(coins):
                return _encode_addr(coins[idx])
            return _encode_addr(ZERO_ADDR)

        def handle_balances_int128(to: str, data: bytes, block: int) -> bytes:
            return _encode_uint256(999)

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_UINT256: handle_coins_uint256,
            COINS_INT128: handle_coins_int128,
            BALANCES_INT128: handle_balances_int128,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)

        assert result.coin_prototype == "coins(int128)"
        assert call_count["uint256_tries"] >= 1


class TestDiscoverCoinsEdgeCases:
    """Edge cases in coin discovery."""

    def testStopsAtZeroAddress(self):
        """Coin discovery stops when it encounters a zero address."""
        coins = [DAI, USDC]  # Only 2 coins; index 2 returns zero

        def handle_coins_uint256(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            if idx < len(coins):
                return _encode_addr(coins[idx])
            return _encode_addr(ZERO_ADDR)

        def handle_balances_uint256(to: str, data: bytes, block: int) -> bytes:
            return _encode_uint256(100)

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_UINT256: handle_coins_uint256,
            BALANCES_UINT256: handle_balances_uint256,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)
        assert len(result.token_addresses) == 2

    def testStopsAtRevert(self):
        """Coin discovery stops when a coin call reverts after prototype is known."""
        coins = [DAI, USDC]

        def handle_coins_uint256(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            if idx >= len(coins):
                raise Exception("revert")
            return _encode_addr(coins[idx])

        def handle_balances_uint256(to: str, data: bytes, block: int) -> bytes:
            return _encode_uint256(100)

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_UINT256: handle_coins_uint256,
            BALANCES_UINT256: handle_balances_uint256,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)
        assert len(result.token_addresses) == 2

    def testNoCoinsAtAll(self):
        """Both uint256 and int128 revert on the first call returns empty result."""
        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({})  # No handlers — all calls revert

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)
        assert len(result.token_addresses) == 0
        assert len(result.balances) == 0

    def testBalanceRevertStopsIteration(self):
        """If a balance call reverts, iteration stops at that point.

        Note: the coin address for the current index is already appended
        before the balance is fetched. A balance revert breaks the loop,
        so the token list may be longer than the balance list.
        """
        coins = [DAI, USDC, USDT]
        balance_calls = {"count": 0}

        def handle_coins_uint256(to: str, data: bytes, block: int) -> bytes:
            (idx,) = eth_abi.abi.decode(["uint256"], data[4:])
            if idx < len(coins):
                return _encode_addr(coins[idx])
            return _encode_addr(ZERO_ADDR)

        def handle_balances_uint256(to: str, data: bytes, block: int) -> bytes:
            balance_calls["count"] += 1
            if balance_calls["count"] > 1:
                msg = "balance revert"
                raise Exception(msg)
            return _encode_uint256(100)

        from tests.curve.detection.fake_provider import make_fake_curve_provider

        provider = make_fake_curve_provider({
            COINS_UINT256: handle_coins_uint256,
            BALANCES_UINT256: handle_balances_uint256,
        })

        result = discover_coins(provider, POOL_ADDR, block_identifier=18_000_000)
        # Coin 0 (DAI) + balance is fetched. Coin 1 (USDC) is appended
        # but its balance reverts, breaking the loop.
        assert len(result.token_addresses) == 2
        assert len(result.balances) == 1
