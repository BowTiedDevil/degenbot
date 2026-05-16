"""Tests for ArbPoolCacheAdapter — auto-registers pools in Rust cache on state update."""

from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage.optimizers.pool_cache_adapter import ArbPoolCacheAdapter
from degenbot.arbitrage.optimizers.solver import ArbSolver
from degenbot.provider import ProviderAdapter
from degenbot.types.pool_protocols import CacheablePool
from tests.helpers.bot_factory import make_bot_with_provider


def test_adapter_requires_cacheable_pool() -> None:
    """Adapter rejects pools that don't implement CacheablePool."""
    solver = MagicMock()
    adapter = ArbPoolCacheAdapter(solver=solver)

    pool = MagicMock()
    # No reserves_for_cache or fee_for_cache
    with pytest.raises(TypeError, match="CacheablePool"):
        adapter.register(pool)


def test_adapter_subscribes_to_pool() -> None:
    """ArbPoolCacheAdapter subscribes to a pool when registered."""
    solver = MagicMock()
    solver.register_pool.side_effect = [1, 2]  # forward_id=1, reverse_id=2
    adapter = ArbPoolCacheAdapter(solver=solver)

    pool = MagicMock(spec=CacheablePool)
    pool.reserves_for_cache.return_value = (1000, 2000)
    pool.fee_for_cache.return_value = Fraction(3, 1000)

    adapter.register(pool)
    pool.subscribe.assert_called_once_with(adapter)


def test_adapter_assigns_pool_id() -> None:
    """ArbPoolCacheAdapter assigns a unique pool ID when registering."""
    solver = MagicMock()
    solver.register_pool.side_effect = [42, 43]
    adapter = ArbPoolCacheAdapter(solver=solver)

    pool = MagicMock(spec=CacheablePool)
    pool.reserves_for_cache.return_value = (1000, 2000)
    pool.fee_for_cache.return_value = Fraction(3, 1000)

    pool_id = adapter.register(pool)
    assert pool_id == 42  # forward_id


def test_adapter_updates_cache_on_notify() -> None:
    """When a pool publishes a state update, the adapter updates the Rust cache."""
    solver = MagicMock()
    solver.register_pool.side_effect = [1, 2]
    adapter = ArbPoolCacheAdapter(solver=solver)

    pool = MagicMock(spec=CacheablePool)
    pool.reserves_for_cache.return_value = (1000, 2000)
    pool.fee_for_cache.return_value = Fraction(3, 1000)

    adapter.register(pool)

    # Simulate state update — reserves change
    pool.reserves_for_cache.return_value = (1500, 2500)

    message = MagicMock()
    adapter.notify(publisher=pool, message=message)

    # update_pool should be called twice (forward + reverse)
    assert solver.update_pool.call_count == 2

    # First call: forward orientation (reserve_in=1500, reserve_out=2500)
    forward_call = solver.update_pool.call_args_list[0]
    assert forward_call[0][0] == 1  # forward_id
    assert forward_call[0][1] == 1500  # reserve_in
    assert forward_call[0][2] == 2500  # reserve_out

    # Second call: reverse orientation (reserve_in=2500, reserve_out=1500)
    reverse_call = solver.update_pool.call_args_list[1]
    assert reverse_call[0][0] == 2  # reverse_id
    assert reverse_call[0][1] == 2500  # reserve_in (swapped)
    assert reverse_call[0][2] == 1500  # reserve_out (swapped)


def test_adapter_registers_both_orientations() -> None:
    """Adapter registers both reserve orientations for a single pool."""
    solver = MagicMock()
    solver.register_pool.side_effect = [1, 2]
    adapter = ArbPoolCacheAdapter(solver=solver)

    pool = MagicMock(spec=CacheablePool)
    pool.reserves_for_cache.return_value = (1000, 2000)
    pool.fee_for_cache.return_value = Fraction(3, 1000)

    adapter.register(pool)

    # register_pool should be called twice (one per orientation)
    assert solver.register_pool.call_count == 2

    # First call: forward (1000, 2000)
    first_call = solver.register_pool.call_args_list[0]
    assert first_call.kwargs["reserve_in"] == 1000
    assert first_call.kwargs["reserve_out"] == 2000

    # Second call: reverse (2000, 1000)
    second_call = solver.register_pool.call_args_list[1]
    assert second_call.kwargs["reserve_in"] == 2000
    assert second_call.kwargs["reserve_out"] == 1000


def test_adapter_get_pool_ids() -> None:
    """get_pool_ids returns (forward_id, reverse_id) tuple."""
    solver = MagicMock()
    solver.register_pool.side_effect = [10, 11]
    adapter = ArbPoolCacheAdapter(solver=solver)

    pool = MagicMock(spec=CacheablePool)
    pool.reserves_for_cache.return_value = (1000, 2000)
    pool.fee_for_cache.return_value = Fraction(3, 1000)

    adapter.register(pool)
    assert adapter.get_pool_ids(pool) == (10, 11)

    # Unregistered pool returns None
    other_pool = MagicMock(spec=CacheablePool)
    assert adapter.get_pool_ids(other_pool) is None


def test_adapter_fork_v2_pool(fork_mainnet_full: AnvilFork) -> None:
    """Integration: register a real V2 pool and verify cache update."""
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    pool = bot.build_pool(
        "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
    )

    solver = ArbSolver()
    adapter = ArbPoolCacheAdapter(solver=solver)

    # Register the pool
    forward_id = adapter.register(pool)
    assert forward_id > 0

    # Both orientations should be in cache
    ids = adapter.get_pool_ids(pool)
    assert ids is not None
    forward_id, reverse_id = ids
    assert reverse_id == forward_id + 1

    # Verify the cache has both entries
    cache = solver.get_pool_cache()
    assert cache.contains(forward_id)
    assert cache.contains(reverse_id)
