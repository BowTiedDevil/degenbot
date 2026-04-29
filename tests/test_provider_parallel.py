"""Test that provider methods release the GIL for parallel execution.

This test verifies that the Rust provider's use of Python::detach()
allows multiple threads to execute RPC calls concurrently, rather than
serializing them behind the GIL.

Detection method: Track concurrent execution via thread counter.
If GIL is released, multiple threads will be inside the RPC call simultaneously.
If GIL is held, only one thread can execute at a time (max_concurrent == 1).
"""

import threading
from concurrent.futures import ThreadPoolExecutor, as_completed

import pytest
from degenbot.degenbot_rs import AlloyProvider

HTTP_RPC_URL = "https://ethereum.publicnode.com"


@pytest.fixture(scope="module")
def provider():
    """Create a provider instance for the test module."""
    return AlloyProvider(HTTP_RPC_URL, max_retries=3)


def test_parallel_block_number_calls(provider):
    """Test that multiple get_block_number calls execute in parallel.

    Uses a concurrent thread counter to detect GIL release:
    - If GIL is held: only 1 thread can be inside the call at a time
    - If GIL is released: multiple threads execute concurrently
    """
    provider.get_block_number()

    concurrent_count = 0
    max_concurrent = 0
    lock = threading.Lock()

    def call_and_track():
        nonlocal concurrent_count, max_concurrent
        with lock:
            concurrent_count += 1
            max_concurrent = max(max_concurrent, concurrent_count)
        result = provider.get_block_number()
        with lock:
            concurrent_count -= 1
        return result

    num_calls = 5
    with ThreadPoolExecutor(max_workers=num_calls) as executor:
        futures = [executor.submit(call_and_track) for _ in range(num_calls)]
        results = [f.result() for f in as_completed(futures)]

    assert len(results) == num_calls
    assert all(isinstance(r, int) and r > 0 for r in results)

    print(f"\nMax concurrent threads during parallel calls: {max_concurrent}/{num_calls}")

    assert max_concurrent > 1, (
        f"Only {max_concurrent} thread(s) executed concurrently. "
        "Expected multiple threads in-flight during parallel calls, "
        "which indicates GIL is not being released properly."
    )


def test_parallel_get_block_calls(provider):
    """Test that multiple get_block calls execute in parallel."""
    current_block = provider.get_block_number()
    provider.get_block(current_block)

    concurrent_count = 0
    max_concurrent = 0
    lock = threading.Lock()

    def call_and_track(block_num):
        nonlocal concurrent_count, max_concurrent
        with lock:
            concurrent_count += 1
            max_concurrent = max(max_concurrent, concurrent_count)
        result = provider.get_block(block_num)
        with lock:
            concurrent_count -= 1
        return result

    num_calls = 5
    block_numbers = [current_block - i for i in range(num_calls)]

    with ThreadPoolExecutor(max_workers=num_calls) as executor:
        futures = {executor.submit(call_and_track, bn): bn for bn in block_numbers}
        results = [f.result() for f in as_completed(futures)]

    assert all(b is not None for b in results)

    print(f"\nMax concurrent threads during get_block calls: {max_concurrent}/{num_calls}")

    assert max_concurrent > 1, (
        f"Only {max_concurrent} thread(s) executed concurrently. "
        "Expected multiple threads in-flight during parallel calls, "
        "which indicates GIL is not being released properly."
    )


def test_parallel_mixed_calls(provider):
    """Test that different provider methods can run in parallel."""
    provider.get_block_number()
    provider.get_chain_id()
    provider.get_gas_price()

    concurrent_count = 0
    max_concurrent = 0
    lock = threading.Lock()

    def make_call(call_type):
        nonlocal concurrent_count, max_concurrent
        with lock:
            concurrent_count += 1
            max_concurrent = max(max_concurrent, concurrent_count)
        if call_type == "block_number":
            result = ("block_number", provider.get_block_number())
        elif call_type == "chain_id":
            result = ("chain_id", provider.get_chain_id())
        else:
            result = ("gas_price", provider.get_gas_price())
        with lock:
            concurrent_count -= 1
        return result

    call_types = ["block_number", "chain_id", "gas_price"] * 2

    with ThreadPoolExecutor(max_workers=len(call_types)) as executor:
        futures = [executor.submit(make_call, ct) for ct in call_types]
        results = [f.result() for f in as_completed(futures)]

    assert len(results) == len(call_types)

    print(f"\nMax concurrent threads during mixed calls: {max_concurrent}/{len(call_types)}")

    assert max_concurrent > 1, (
        f"Only {max_concurrent} thread(s) executed concurrently. "
        "Expected multiple threads in-flight during parallel calls, "
        "which indicates GIL is not being released properly."
    )


def test_parallel_provider_creation():
    """Test that multiple providers can be created in parallel."""
    AlloyProvider(HTTP_RPC_URL, max_retries=2)

    concurrent_count = 0
    max_concurrent = 0
    lock = threading.Lock()

    def create_and_track():
        nonlocal concurrent_count, max_concurrent
        with lock:
            concurrent_count += 1
            max_concurrent = max(max_concurrent, concurrent_count)
        provider = AlloyProvider(HTTP_RPC_URL, max_retries=2)
        with lock:
            concurrent_count -= 1
        return provider

    num_providers = 3

    with ThreadPoolExecutor(max_workers=num_providers) as executor:
        futures = [executor.submit(create_and_track) for _ in range(num_providers)]
        providers = [f.result() for f in as_completed(futures)]

    assert len(providers) == num_providers
    assert all(hasattr(p, "get_block_number") for p in providers)

    print(f"\nMax concurrent threads during provider creation: {max_concurrent}/{num_providers}")

    assert max_concurrent > 1, (
        f"Only {max_concurrent} thread(s) executed concurrently. "
        "Expected multiple threads in-flight during parallel creation, "
        "which indicates GIL is not being released properly."
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
