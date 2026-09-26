"""The Python registries delegate identity to the Rust session registry.

Seam under test: `degenbot.registry.session` (`SessionObjects`,
`CompanionCache`) plus `PoolRegistry` / `ManagedPoolRegistry` / `TokenRegistry`
over a real `degenbot._ffi.Bot` — the same seam the bot's build path uses.

The contract these pin:

- **One identity per session.** Two consumers of one session that name the
  same pool or token get the *same* canonical session object (equal `key`),
  because the Rust `SessionObjectRegistry` answers, not either consumer.
- **One companion per identity.** A duplicate `get_or_add` returns the
  companion already filed; the registry does not grow a second one, and the
  session's object count does not move.
- **Families are identities.** A V3 pool and a Balancer pool at one address are
  two identities, and the address view names the first one registered — which
  is what keeps a per-family Python map from creeping back in as the only way
  to tell them apart.
- **V4 is one store.** `bot.managed_pools` and `bot.pools`' V4 branch are the
  same companion store, so a V4 pool registered through either is one object.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot
from degenbot.exceptions import DegenbotValueError
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.registry.session import SessionObjects, pool_family_of
from tests.fakes.pools import FakeSessionPool
from tests.fakes.tokens import FakeToken

CHAIN_ID = 1
POOL_A = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
POOL_B = "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
V4_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
V4_POOL_ID = bytes.fromhex("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890")


def _session() -> SessionObjects:
    return SessionObjects(Bot(chain_id=CHAIN_ID))


def test_two_consumers_receive_the_same_canonical_pool_identity() -> None:
    """Identity is the session's, so a second consumer of the same session
    resolves the same object — it does not mint its own."""
    session = _session()

    created = session.get_or_create_pool(chain_id=CHAIN_ID, family="v3", address=POOL_A)
    other_consumer = session.resolve_pool(chain_id=CHAIN_ID, family="v3", address=POOL_A)

    assert other_consumer is not None
    assert other_consumer.key == created.key, "one identity, one canonical name"
    assert session.counts() == (1, 0), "a second consumer created no second object"


def test_two_consumers_receive_the_same_canonical_token_identity() -> None:
    session = _session()

    created = session.get_or_create_token(chain_id=CHAIN_ID, address=USDC)
    assert session.resolve_token(chain_id=CHAIN_ID, address=USDC).key == created.key  # type: ignore[union-attr]
    assert session.counts() == (0, 1)


def test_duplicate_construction_does_not_create_a_second_python_companion() -> None:
    """The build path's Guard-1 primitive: a second `get_or_add` of the same
    identity returns the first companion instead of a twin."""
    py_bot = Bot(chain_id=CHAIN_ID)
    pools = PoolRegistry(py_bot=py_bot)

    first = FakeSessionPool(POOL_A, "v3")
    duplicate = FakeSessionPool(POOL_A, "v3")

    assert pools.get_or_add(pool=first, chain_id=CHAIN_ID, pool_address=POOL_A) is first
    assert pools.get_or_add(pool=duplicate, chain_id=CHAIN_ID, pool_address=POOL_A) is first
    assert len(pools) == 1, "one companion for one identity"
    assert pools.get(chain_id=CHAIN_ID, pool_address=POOL_A) is first
    assert pools._session.counts() == (1, 0), "the duplicate did not register a second object"


def test_duplicate_token_construction_does_not_create_a_second_companion() -> None:
    py_bot = Bot(chain_id=CHAIN_ID)
    tokens = TokenRegistry(py_bot=py_bot)

    first = FakeToken(USDC)
    second = FakeToken(WETH)
    assert tokens.get_or_add(token_address=USDC, chain_id=CHAIN_ID, token=first) is first
    assert tokens.get_or_add(token_address=WETH, chain_id=CHAIN_ID, token=second) is second
    assert len(tokens) == 2, "two token identities, two companions"

    duplicate = FakeToken(USDC)
    assert tokens.get_or_add(token_address=USDC, chain_id=CHAIN_ID, token=duplicate) is first
    assert len(tokens) == 2, "the duplicate did not add a companion"
    assert tokens.get(token_address=USDC, chain_id=CHAIN_ID) is first
    assert tokens._session.counts() == (0, 2)


def test_add_raises_on_a_duplicate_but_remove_frees_the_slot() -> None:
    """The documented public contract is unchanged: `add` raises on a second
    registration of the same identity, and `remove` makes the slot addable
    again even though the session keeps the identity."""
    py_bot = Bot(chain_id=CHAIN_ID)
    pools = PoolRegistry(py_bot=py_bot)

    pools.add(pool=FakeSessionPool(POOL_A, "v3"), chain_id=CHAIN_ID, pool_address=POOL_A)
    with pytest.raises(DegenbotValueError, match="already registered"):
        pools.add(pool=FakeSessionPool(POOL_A, "v3"), chain_id=CHAIN_ID, pool_address=POOL_A)

    pools.remove(chain_id=CHAIN_ID, pool_address=POOL_A)
    assert pools.get(chain_id=CHAIN_ID, pool_address=POOL_A) is None
    counts = pools._session.counts()
    assert counts == (1, 0), "removal drops the companion, not the session identity"

    replacement = FakeSessionPool(POOL_A, "v3")
    pools.add(pool=replacement, chain_id=CHAIN_ID, pool_address=POOL_A)
    assert pools.get(chain_id=CHAIN_ID, pool_address=POOL_A) is replacement
    assert pools._session.counts() == (1, 0)


def test_one_address_two_families_are_two_identities() -> None:
    """Family is part of a pool's canonical identity, so a per-family Python map
    is never the only thing telling a V3 pool and a Balancer pool apart."""
    py_bot = Bot(chain_id=CHAIN_ID)
    pools = PoolRegistry(py_bot=py_bot)

    v3_pool = FakeSessionPool(POOL_A, "v3")
    balancer_pool = FakeSessionPool(POOL_A, "balancer-weighted")

    assert pools.get_or_add(pool=v3_pool, chain_id=CHAIN_ID, pool_address=POOL_A) is v3_pool
    second = pools.get_or_add(pool=balancer_pool, chain_id=CHAIN_ID, pool_address=POOL_A)
    assert second is balancer_pool, "the second family is a distinct identity, keeps its companion"
    assert pools._session.counts() == (2, 0)

    # The family-agnostic address view names the identity that claimed the
    # address first, which is the whole contract of an address-only read.
    assert pools.get(chain_id=CHAIN_ID, pool_address=POOL_A) is v3_pool
    assert pool_family_of(balancer_pool) == "balancer-weighted"


def test_a_v4_pool_is_one_companion_across_the_two_registry_views() -> None:
    """`bot.managed_pools` and `bot.pools`' V4 branch are the same store, so the
    V4 branch is not a second identity authority for the session."""
    py_bot = Bot(chain_id=CHAIN_ID)
    managed = ManagedPoolRegistry(py_bot=py_bot)
    pools = PoolRegistry(py_bot=py_bot, managed_pool_registry=managed)

    pool = FakeSessionPool(V4_MANAGER, "v4")
    assert (
        managed.get_or_add(
            pool=pool, chain_id=CHAIN_ID, pool_manager_address=V4_MANAGER, pool_id=V4_POOL_ID
        )
        is pool
    )
    assert (
        pools.get(chain_id=CHAIN_ID, pool_address=V4_MANAGER, pool_id=V4_POOL_ID) is pool
    ), "the V4 pool registered through the managed registry is visible in PoolRegistry"
    assert (
        managed.get(chain_id=CHAIN_ID, pool_manager_address=V4_MANAGER, pool_id=V4_POOL_ID) is pool
    )
    assert len(managed) == 1
    assert pools._session.counts() == (1, 0)

    # A V4 pool is named by its pair, never by the PoolManager address alone.
    assert pools.get(chain_id=CHAIN_ID, pool_address=V4_MANAGER) is None


def test_a_registry_refuses_a_pool_with_no_live_handle() -> None:
    """A companion that carries no live handle cannot be named, so the registry
    says so instead of guessing a family for it."""
    py_bot = Bot(chain_id=CHAIN_ID)
    pools = PoolRegistry(py_bot=py_bot)

    with pytest.raises(DegenbotValueError, match="live pool handle"):
        pools.get_or_add(pool=object(), chain_id=CHAIN_ID, pool_address=POOL_A)  # type: ignore[arg-type]
    assert pools._session.counts() == (0, 0)


def test_a_foreign_chain_is_refused_with_a_clean_message() -> None:
    """A session names one chain, so a foreign `chain_id` is refused — and the
    refusal reads as one sentence rather than a padded source artifact."""
    session = _session()

    with pytest.raises(ValueError, match="is not this session's chain") as excinfo:
        session.get_or_create_pool(chain_id=CHAIN_ID + 1, family="v3", address=POOL_A)

    message = str(excinfo.value)
    assert message == (
        f"chain_id {CHAIN_ID + 1} is not this session's chain ({CHAIN_ID}); "
        "a session object registry is scoped to one chain"
    )
    assert "  " not in message, "the message carries no run of padding"
    assert session.counts() == (0, 0), "a refusal registers nothing"
