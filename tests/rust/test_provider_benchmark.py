"""
Benchmark comparing web3.py HTTPProvider to the new Alloy provider.

These tests measure performance differences between the two providers
for common RPC operations. Run with: pytest tests/rust/test_provider_benchmark.py -v
"""

import time

import pytest
from hexbytes import HexBytes
from web3 import HTTPProvider, Web3

from degenbot.provider import AlloyProvider, LogFilter
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"


class TestProviderBenchmark:
    """Benchmark tests comparing web3.py HTTPProvider to Alloy provider.

    These tests compare performance for common RPC operations including
    block number fetching, log fetching, and provider initialization.
    """

    @pytest.fixture(scope="class")
    def web3_provider(self):
        """Create a web3.py HTTPProvider instance."""
        provider = HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        w3 = Web3(provider)
        yield w3
        # Cleanup
        if hasattr(provider, "close"):
            provider.close()

    @pytest.fixture(scope="class")
    def alloy_provider(self):
        """Create an Alloy provider instance."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        yield provider
        provider.close()

    def _time_function(self, func, iterations=10):
        """Time a function over multiple iterations."""
        times = []
        for _ in range(iterations):
            start = time.perf_counter()
            result = func()
            end = time.perf_counter()
            times.append(end - start)
        return result, times

    def test_benchmark_get_block_number(self, web3_provider, alloy_provider):
        """Benchmark fetching the current block number.

        Compares web3.py HTTPProvider vs Alloy provider for the simple
        eth_blockNumber RPC call.
        """

        def web3_get_block():
            return web3_provider.eth.block_number

        def alloy_get_block():
            return alloy_provider.get_block_number()

        # Run benchmarks
        web3_result, web3_times = self._time_function(web3_get_block, iterations=10)
        alloy_result, alloy_times = self._time_function(alloy_get_block, iterations=10)

        # Calculate statistics
        web3_avg = sum(web3_times) / len(web3_times) * 1000  # Convert to ms
        alloy_avg = sum(alloy_times) / len(alloy_times) * 1000
        speedup = web3_avg / alloy_avg if alloy_avg > 0 else float("inf")

        print(f"\n  web3.py get_block_number: {web3_avg:.2f}ms avg")
        print(f"  Alloy get_block_number: {alloy_avg:.2f}ms avg")
        print(f"  Speedup: {speedup:.2f}x")

        # Verify results are reasonable
        assert isinstance(web3_result, int)
        assert isinstance(alloy_result, int)
        assert web3_result > 0
        assert alloy_result > 0

    def test_benchmark_get_chain_id(self, web3_provider, alloy_provider):
        """Benchmark fetching the chain ID.

        Compares web3.py HTTPProvider vs Alloy provider for the
        eth_chainId RPC call.
        """

        def web3_get_chain():
            return web3_provider.eth.chain_id

        def alloy_get_chain():
            return alloy_provider.get_chain_id()

        # Run benchmarks
        web3_result, web3_times = self._time_function(web3_get_chain, iterations=10)
        alloy_result, alloy_times = self._time_function(alloy_get_chain, iterations=10)

        # Calculate statistics
        web3_avg = sum(web3_times) / len(web3_times) * 1000
        alloy_avg = sum(alloy_times) / len(alloy_times) * 1000
        speedup = web3_avg / alloy_avg if alloy_avg > 0 else float("inf")

        print(f"\n  web3.py get_chain_id: {web3_avg:.2f}ms avg")
        print(f"  Alloy get_chain_id: {alloy_avg:.2f}ms avg")
        print(f"  Speedup: {speedup:.2f}x")

        # Verify results match Ethereum mainnet
        assert web3_result == 1
        assert alloy_result == 1

    def test_benchmark_get_logs_small_range(self, web3_provider, alloy_provider):
        """Benchmark fetching logs for a small block range.

        Compares web3.py HTTPProvider vs Alloy provider for log fetching
        over 100 blocks with WETH contract filter.
        """
        from_block = 18_000_000
        to_block = 18_000_100

        def web3_get_logs():
            return web3_provider.eth.get_logs({
                "fromBlock": from_block,
                "toBlock": to_block,
                "address": WETH_ADDRESS,
            })

        def alloy_get_logs():
            log_filter = LogFilter(
                from_block=from_block,
                to_block=to_block,
                addresses=[WETH_ADDRESS],
            )
            return alloy_provider.get_logs(log_filter)

        # Run benchmarks (fewer iterations for logs as they're slower)
        web3_logs, web3_times = self._time_function(web3_get_logs, iterations=5)
        alloy_logs, alloy_times = self._time_function(alloy_get_logs, iterations=5)

        # Calculate statistics
        web3_avg = sum(web3_times) / len(web3_times) * 1000
        alloy_avg = sum(alloy_times) / len(alloy_times) * 1000
        speedup = web3_avg / alloy_avg if alloy_avg > 0 else float("inf")

        print(f"\n  web3.py get_logs (100 blocks): {web3_avg:.2f}ms avg")
        print(f"  Alloy get_logs (100 blocks): {alloy_avg:.2f}ms avg")
        print(f"  Speedup: {speedup:.2f}x")
        print(f"  Logs returned: {len(web3_logs)} (web3), {len(alloy_logs)} (alloy)")

        # Verify both returned logs
        assert isinstance(web3_logs, list)
        assert isinstance(alloy_logs, list)

    def test_benchmark_get_logs_with_topics(self, web3_provider, alloy_provider):
        """Benchmark fetching logs with topic filtering.

        Compares web3.py HTTPProvider vs Alloy provider for log fetching
        with specific event topic (Transfer events).
        """
        from_block = 18_000_000
        to_block = 18_000_050

        def web3_get_logs():
            return web3_provider.eth.get_logs({
                "fromBlock": from_block,
                "toBlock": to_block,
                "address": WETH_ADDRESS,
                "topics": [TRANSFER_TOPIC],
            })

        def alloy_get_logs():
            log_filter = LogFilter(
                from_block=from_block,
                to_block=to_block,
                addresses=[WETH_ADDRESS],
                topics=[[TRANSFER_TOPIC]],
            )
            return alloy_provider.get_logs(log_filter)

        # Run benchmarks
        web3_logs, web3_times = self._time_function(web3_get_logs, iterations=5)
        alloy_logs, alloy_times = self._time_function(alloy_get_logs, iterations=5)

        # Calculate statistics
        web3_avg = sum(web3_times) / len(web3_times) * 1000
        alloy_avg = sum(alloy_times) / len(alloy_times) * 1000
        speedup = web3_avg / alloy_avg if alloy_avg > 0 else float("inf")

        print(f"\n  web3.py get_logs (with topics): {web3_avg:.2f}ms avg")
        print(f"  Alloy get_logs (with topics): {alloy_avg:.2f}ms avg")
        print(f"  Speedup: {speedup:.2f}x")
        print(f"  Logs returned: {len(web3_logs)} (web3), {len(alloy_logs)} (alloy)")

        # Verify both returned logs
        assert isinstance(web3_logs, list)
        assert isinstance(alloy_logs, list)

    def test_benchmark_provider_initialization(self):
        """Benchmark provider initialization time.

        Compares the time it takes to create and initialize each provider type.
        """

        def web3_init():
            provider = HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
            w3 = Web3(provider)
            # Cleanup
            if hasattr(provider, "close"):
                provider.close()
            return w3

        def alloy_init():
            provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
            provider.close()
            return provider

        # Run benchmarks - single iteration since initialization is one-time cost
        web3_result, web3_times = self._time_function(web3_init, iterations=10)
        alloy_result, alloy_times = self._time_function(alloy_init, iterations=10)

        # Calculate statistics
        web3_avg = sum(web3_times) / len(web3_times) * 1000
        alloy_avg = sum(alloy_times) / len(alloy_times) * 1000
        speedup = web3_avg / alloy_avg if alloy_avg > 0 else float("inf")

        print(f"\n  web3.py initialization: {web3_avg:.2f}ms avg")
        print(f"  Alloy initialization: {alloy_avg:.2f}ms avg")
        print(f"  Speedup: {speedup:.2f}x")

        # Verify initialization succeeded
        assert web3_result is not None
        assert alloy_result is not None


class TestProviderComparison:
    """Direct comparison tests for accuracy and consistency."""

    @pytest.fixture(scope="class")
    def web3_provider(self):
        """Create a web3.py HTTPProvider instance."""
        provider = HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        w3 = Web3(provider)
        yield w3
        if hasattr(provider, "close"):
            provider.close()

    @pytest.fixture(scope="class")
    def alloy_provider(self):
        """Create an Alloy provider instance."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        yield provider
        provider.close()

    def test_block_number_consistency(self, web3_provider, alloy_provider):
        """Verify both providers return the same block number."""
        web3_block = web3_provider.eth.block_number
        alloy_block = alloy_provider.get_block_number()

        # Allow for 1 block difference due to timing
        assert abs(web3_block - alloy_block) <= 1

    def test_chain_id_consistency(self, web3_provider, alloy_provider):
        """Verify both providers return the same chain ID."""
        web3_chain = web3_provider.eth.chain_id
        alloy_chain = alloy_provider.get_chain_id()

        assert web3_chain == alloy_chain == 1

    def test_logs_consistency(self, web3_provider, alloy_provider):
        """Verify both providers return equivalent logs.

        Compares log data from both providers to ensure accuracy.
        """
        from_block = 18_000_000
        to_block = 18_000_010

        # Fetch logs from both providers
        web3_logs = web3_provider.eth.get_logs({
            "fromBlock": from_block,
            "toBlock": to_block,
            "address": WETH_ADDRESS,
        })

        log_filter = LogFilter(
            from_block=from_block,
            to_block=to_block,
            addresses=[WETH_ADDRESS],
        )
        alloy_logs = alloy_provider.get_logs(log_filter)

        # Verify counts match
        assert len(web3_logs) == len(alloy_logs)

        # Verify first and last log match
        if web3_logs:
            web3_first = web3_logs[0]
            alloy_first = alloy_logs[0]

            assert web3_first["address"].lower() == alloy_first.address.lower()
            # Handle HexBytes vs string comparison for topics
            # HexBytes.hex() returns without '0x' prefix, alloy topics have it
            web3_topics = [
                ("0x" + t.hex()) if isinstance(t, HexBytes) else str(t)
                for t in web3_first["topics"]
            ]
            assert web3_topics == alloy_first.topics
            # Handle HexBytes vs bytes comparison for data
            web3_data = (
                web3_first["data"].hex()
                if isinstance(web3_first["data"], HexBytes)
                else web3_first["data"]
            )
            alloy_data = (
                alloy_first.data.hex() if isinstance(alloy_first.data, bytes) else alloy_first.data
            )
            assert web3_data == alloy_data
            assert web3_first["blockNumber"] == alloy_first.block_number
