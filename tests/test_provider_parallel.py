"""Test that provider methods release the GIL for parallel execution.

This test verifies that the Rust provider's use of Python::detach()
allows multiple threads to execute RPC calls concurrently, rather than
serializing them behind the GIL.

Key insight: If GIL is held, N parallel calls would be serialized (~N * T).
If GIL is released, N parallel calls should take ~T (single call time).
"""

import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import pytest

from degenbot.degenbot_rs import AlloyProvider

# Use HTTP RPC for meaningful latency measurements
# IPC is too fast (~0.05ms) - thread overhead dominates
HTTP_RPC_URL = "https://ethereum.publicnode.com"


@pytest.fixture(scope="module")
def provider():
    """Create a provider instance for the test module."""
    return AlloyProvider(HTTP_RPC_URL, max_retries=3)


def test_parallel_block_number_calls(provider):
    """Test that multiple get_block_number calls execute in parallel.

    Key insight: If the GIL is held, N parallel calls would be serialized
    and take N * T time. If GIL is released, they should take ~T time.
    """
    # Warm up the connection
    provider.get_block_number()

    # Measure single call time (average of a few calls)
    single_times = []
    for _ in range(3):
        start = time.perf_counter()
        provider.get_block_number()
        single_times.append(time.perf_counter() - start)
    avg_single_time = sum(single_times) / len(single_times)

    # Run parallel calls
    num_calls = 5

    start_parallel = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_calls) as executor:
        futures = [executor.submit(provider.get_block_number) for _ in range(num_calls)]
        parallel_results = [f.result() for f in as_completed(futures)]
    parallel_time = time.perf_counter() - start_parallel

    # Run sequential calls for comparison
    start_sequential = time.perf_counter()
    _sequential_results = [provider.get_block_number() for _ in range(num_calls)]
    sequential_time = time.perf_counter() - start_sequential

    # All results should be valid block numbers
    assert len(parallel_results) == num_calls
    assert all(isinstance(r, int) and r > 0 for r in parallel_results)

    print(f"\nAverage single call time: {avg_single_time * 1000:.1f}ms")
    print(f"Sequential {num_calls} calls: {sequential_time * 1000:.1f}ms")
    print(f"Parallel {num_calls} calls: {parallel_time * 1000:.1f}ms")
    print(f"Speedup: {sequential_time / parallel_time:.1f}x")

    # Key assertions:
    # 1. Parallel should be much faster than sequential
    # 2. Parallel should take close to single call time (not N * single)

    # Parallel should be at least 1.5x faster than sequential
    # (allowing for HTTP connection pool limits and network variability)
    speedup = sequential_time / parallel_time
    assert speedup > 1.5, (
        f"Parallel execution ({parallel_time * 1000:.1f}ms) should be at least 1.5x faster than "
        f"sequential ({sequential_time * 1000:.1f}ms). Got {speedup:.1f}x speedup. "
        "This suggests the GIL is not being released properly."
    )

    # Parallel time should be less than 2x single call time
    # (allows for thread overhead, but not N * single time)
    assert parallel_time < avg_single_time * 2, (
        f"Parallel time ({parallel_time * 1000:.1f}ms) should be close to "
        f"single call time ({avg_single_time * 1000:.1f}ms), not N times it. "
        "This suggests calls are being serialized behind the GIL."
    )


def test_parallel_get_block_calls(provider):
    """Test that multiple get_block calls execute in parallel."""
    # Warm up
    current_block = provider.get_block_number()
    provider.get_block(current_block)

    # Measure single call time
    single_times = []
    for _ in range(3):
        start = time.perf_counter()
        provider.get_block(current_block)
        single_times.append(time.perf_counter() - start)
    avg_single_time = sum(single_times) / len(single_times)

    # Run parallel calls
    num_calls = 5
    block_numbers = [current_block - i for i in range(num_calls)]

    start_parallel = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_calls) as executor:
        futures = {executor.submit(provider.get_block, bn): bn for bn in block_numbers}
        parallel_blocks = [futures[f] for f in as_completed(futures)]
    parallel_time = time.perf_counter() - start_parallel

    # Run sequential for comparison
    start_sequential = time.perf_counter()
    sequential_blocks = [provider.get_block(bn) for bn in block_numbers]
    sequential_time = time.perf_counter() - start_sequential

    print(f"\nBlock fetch single avg: {avg_single_time * 1000:.1f}ms")
    print(f"Sequential {num_calls} blocks: {sequential_time * 1000:.1f}ms")
    print(f"Parallel {num_calls} blocks: {parallel_time * 1000:.1f}ms")
    print(f"Speedup: {sequential_time / parallel_time:.1f}x")

    # All blocks should be fetched successfully
    assert all(b is not None for b in parallel_blocks)
    assert all(b is not None for b in sequential_blocks)

    # Parallel should be at least 2x faster
    speedup = sequential_time / parallel_time
    assert speedup > 2.0, (
        f"Parallel block fetch ({parallel_time * 1000:.1f}ms) should be at least 2x faster than "
        f"sequential ({sequential_time * 1000:.1f}ms). Got {speedup:.1f}x speedup."
    )


def test_parallel_mixed_calls(provider):
    """Test that different provider methods can run in parallel."""
    # Warm up
    provider.get_block_number()
    provider.get_chain_id()
    provider.get_gas_price()

    def make_call(call_type):
        if call_type == "block_number":
            return ("block_number", provider.get_block_number())
        if call_type == "chain_id":
            return ("chain_id", provider.get_chain_id())
        return ("gas_price", provider.get_gas_price())

    call_types = ["block_number", "chain_id", "gas_price"] * 2

    # Sequential execution
    start_sequential = time.perf_counter()
    _sequential_results = [make_call(ct) for ct in call_types]
    sequential_time = time.perf_counter() - start_sequential

    # Parallel execution
    start_parallel = time.perf_counter()
    with ThreadPoolExecutor(max_workers=len(call_types)) as executor:
        futures = [executor.submit(make_call, ct) for ct in call_types]
        parallel_results = [f.result() for f in as_completed(futures)]
    parallel_time = time.perf_counter() - start_parallel

    print(
        f"\nMixed calls - Sequential: {sequential_time * 1000:.1f}ms, "
        f"Parallel: {parallel_time * 1000:.1f}ms"
    )

    # All calls should succeed
    assert len(parallel_results) == len(call_types)

    speedup = sequential_time / parallel_time
    assert speedup > 2.0, (
        f"Parallel mixed calls ({parallel_time * 1000:.1f}ms) should be at least 2x faster than "
        f"sequential ({sequential_time * 1000:.1f}ms). Got {speedup:.1f}x speedup."
    )


def test_parallel_provider_creation():
    """Test that multiple providers can be created in parallel."""
    # Warm up
    AlloyProvider(HTTP_RPC_URL, max_retries=2)

    num_providers = 3

    # Sequential creation
    start_sequential = time.perf_counter()
    _sequential_providers = [
        AlloyProvider(HTTP_RPC_URL, max_retries=2) for _ in range(num_providers)
    ]
    sequential_time = time.perf_counter() - start_sequential

    # Parallel creation
    start_parallel = time.perf_counter()
    with ThreadPoolExecutor(max_workers=num_providers) as executor:
        futures = [executor.submit(AlloyProvider, HTTP_RPC_URL, 2) for _ in range(num_providers)]
        parallel_providers = [f.result() for f in as_completed(futures)]
    parallel_time = time.perf_counter() - start_parallel

    print(
        f"\nProvider creation - Sequential: {sequential_time * 1000:.1f}ms, "
        f"Parallel: {parallel_time * 1000:.1f}ms"
    )

    # All providers should be created successfully
    assert len(parallel_providers) == num_providers
    assert all(hasattr(p, "get_block_number") for p in parallel_providers)

    speedup = sequential_time / parallel_time
    print(f"Speedup: {speedup:.1f}x")

    # Provider creation should show speedup
    assert speedup > 1.5, (
        f"Parallel provider creation ({parallel_time * 1000:.1f}ms) should be faster than "
        f"sequential ({sequential_time * 1000:.1f}ms). Got {speedup:.1f}x speedup."
    )


if __name__ == "__main__":
    # Run tests manually for quick verification
    print("Testing parallel execution with Rust provider...")
    print("=" * 60)

    provider = AlloyProvider(HTTP_RPC_URL, max_retries=3)

    try:
        test_parallel_block_number_calls(provider)
        print("✓ test_parallel_block_number_calls passed")
    except AssertionError as e:
        print(f"✗ test_parallel_block_number_calls failed: {e}")

    try:
        test_parallel_get_block_calls(provider)
        print("✓ test_parallel_get_block_calls passed")
    except AssertionError as e:
        print(f"✗ test_parallel_get_block_calls failed: {e}")

    try:
        test_parallel_mixed_calls(provider)
        print("✓ test_parallel_mixed_calls passed")
    except AssertionError as e:
        print(f"✗ test_parallel_mixed_calls failed: {e}")

    try:
        test_parallel_provider_creation()
        print("✓ test_parallel_provider_creation passed")
    except AssertionError as e:
        print(f"✗ test_parallel_provider_creation failed: {e}")
