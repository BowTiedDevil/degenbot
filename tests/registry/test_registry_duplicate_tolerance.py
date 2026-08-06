"""Guard-1 tests for 35NMBX: build-path registries are idempotent.

The registration build path is offloaded onto a bounded thread pool
(``asyncio.to_thread``/``run_in_executor``), so two workers can build the SAME
pool or token concurrently (the crawl shares pools across MANY discovered
paths). The public ``add`` API is documented to RAISE on a duplicate
(``test_registry.py::test_adding_pool``/``test_adding_token``), so the build
path must use the idempotent ``get_or_add`` primitive instead: return the
canonical stored item on a duplicate rather than raising, so a distinct path
sharing the pool is never lossily skipped.

These tests pin the ``get_or_add`` contract on the concrete registries the bot
uses (``PoolRegistry``, ``ManagedPoolRegistry``, ``TokenRegistry``) and prove
it is safe under real concurrent worker threads.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass

import pytest

from degenbot.exceptions import DegenbotValueError
from degenbot.registry.pool import ManagedPoolRegistry, PoolRegistry
from degenbot.registry.token import TokenRegistry

CHAIN = 1
ADDR = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
ADDR2 = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
MANAGER = "0x1234567890123456789012345678901234567890"
POOL_ID = bytes(range(32))


@dataclass(frozen=True)
class FakePool:
    address: str


@dataclass(frozen=True)
class FakeManagedPool:
    pool_manager_address: str
    pool_id: bytes


@dataclass(frozen=True)
class FakeToken:
    address: str


def test_pool_registry_get_or_add_returns_canonical_on_duplicate() -> None:
    reg = PoolRegistry()
    first = FakePool(ADDR)
    second = FakePool(ADDR2)

    assert reg.get_or_add(first, CHAIN, ADDR) is first
    got = reg.get_or_add(second, CHAIN, ADDR)  # duplicate -> no raise
    assert got is first  # canonical (first) instance returned
    assert reg.get(CHAIN, ADDR) is first


def test_managed_pool_registry_get_or_add_returns_canonical_on_duplicate() -> None:
    reg = ManagedPoolRegistry()
    first = FakeManagedPool(MANAGER, POOL_ID)
    dup = FakeManagedPool(MANAGER, POOL_ID)

    assert reg.get_or_add(first, CHAIN, MANAGER, POOL_ID) is first
    got = reg.get_or_add(dup, CHAIN, MANAGER, POOL_ID)
    assert got is first
    assert reg.get(CHAIN, MANAGER, POOL_ID) is first


def test_token_registry_get_or_add_returns_canonical_on_duplicate() -> None:
    reg = TokenRegistry()
    first = FakeToken(ADDR)
    dup = FakeToken(ADDR2)

    assert reg.get_or_add(ADDR, CHAIN, first) is first
    got = reg.get_or_add(ADDR, CHAIN, dup)
    assert got is first
    assert reg.get(ADDR, CHAIN) is first


def test_pool_registry_public_add_still_raises_on_duplicate() -> None:
    """Guard 1 must NOT change the documented public contract: direct ``add``
    of an already-registered pool still raises (pinned by test_registry.py)."""
    reg = PoolRegistry()
    reg.add(FakePool(ADDR), CHAIN, ADDR)
    with pytest.raises(DegenbotValueError, match="already registered"):
        reg.add(FakePool(ADDR2), CHAIN, ADDR)


def test_managed_pool_registry_get_or_add_concurrent_no_raise() -> None:
    """Real concurrency: N registration-worker threads race to register the same
    V4 pool via ``get_or_add``. None may raise and exactly one is stored.
    """
    reg = ManagedPoolRegistry()
    n_threads = 8
    barrier = threading.Barrier(n_threads)
    errors: list[BaseException] = []

    def worker() -> None:
        try:
            barrier.wait(timeout=5)
            reg.get_or_add(FakeManagedPool(MANAGER, POOL_ID), CHAIN, MANAGER, POOL_ID)
        except BaseException as exc:  # ruff:ignore[BLE001] - test harness collect-all
            errors.append(exc)

    threads = [threading.Thread(target=worker) for _ in range(n_threads)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=10)

    assert not errors, f"concurrent get_or_add raised: {errors}"
    assert reg.get(CHAIN, MANAGER, POOL_ID) is not None
