"""Python presentation over the Rust session object registry.

The Rust `SessionObjectRegistry` owns canonical identity for one session's
pools and tokens (ADR-006 D5: one `Bot`, one chain, one session). A Python
`PoolRegistry` / `TokenRegistry` is a thin delegate over it: it names an
object, Rust decides whether that name is the identity the session already
holds, and Python keeps the *presentation* object — the pool/token companion
that wraps live Rust state. The two are different things: a session object is
a name, a companion is behaviour, and this module holds the seam between them.

# What stays in Python, and why

The companion cache below is not a second identity authority:

- an entry can only exist for a key **Rust minted** (`SessionObject.key`), so
  Python cannot invent an identity Rust does not hold;
- `add`/`get_or_add` ask Rust to resolve or get-or-create the identity first,
  so the duplicate decision is made once, in one place;
- the cache is keyed by, and only ever looked up with, a key Rust produced —
  no Python code re-derives a key from an address, a chain id, or a family
  tag.

It is a cache because companions are Python values: `release_python_state` and
`close` drop them, and a later build mints a fresh companion for an identity
the session still holds.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

from degenbot.exceptions.base import DegenbotValueError

if TYPE_CHECKING:
    from collections.abc import Iterator

    from degenbot._ffi import Bot, SessionObject


@dataclasses.dataclass(slots=True, frozen=True)
class SessionCompanion[T]:
    """A Python companion paired with the session object that names it.

    The handle is kept so a teardown can act on the *canonical identity* (its
    address, its family) rather than re-deriving it from the companion.
    """

    handle: SessionObject
    item: T


class SessionObjects:
    """Typed adapter over the session-registry seam of one `Bot` handle.

    Every method is one canonical-identity question or get-or-create, with
    arguments already typed: hex strings and family tags in, a session object
    handle (or `None` for a resolve miss) out. No state lives here — the
    registry behind the handle is the session's one registry.
    """

    __slots__ = ("_py_bot",)

    def __init__(self, py_bot: Bot) -> None:
        """Bind to one session's registry."""
        self._py_bot = py_bot

    def get_or_create_pool(
        self,
        *,
        chain_id: int,
        family: str,
        address: str,
        pool_id: bytes | None = None,
    ) -> SessionObject:
        """Return the session's canonical pool object, registering it if new.

        Args:
            chain_id: The session's chain; a different chain is refused.
            family: A registration family tag (`"v3"`, `"balancer-weighted"`, …).
            address: The pool's address, or its `PoolManager` when
                `family` is `"v4"`.
            pool_id: The on-chain V4 pool id; required for `"v4"` and
                refused for every other family.

        Returns:
            The one canonical object for this identity.

        """
        return self._py_bot.get_or_create_session_pool(chain_id, family, address, pool_id)

    def resolve_pool(
        self,
        *,
        chain_id: int,
        family: str,
        address: str,
        pool_id: bytes | None = None,
    ) -> SessionObject | None:
        """Return the session's canonical pool object for this identity, or `None`.

        Registers nothing.

        Returns:
            The object, or None when the session does not hold the identity.

        """
        return self._py_bot.resolve_session_pool(chain_id, family, address, pool_id)

    def resolve_pool_by_address(self, *, chain_id: int, address: str) -> SessionObject | None:
        """Return the session's canonical pool object at `address`, or `None`.

        The family-agnostic read: it names whichever family registered that
        address first in this session. A V4 pool is not address-keyed and is
        never named here.

        Returns:
            The object, or None when the session holds no address-keyed pool
            at that address.

        """
        return self._py_bot.resolve_session_pool_by_address(chain_id, address)

    def get_or_create_token(self, *, chain_id: int, address: str) -> SessionObject:
        """Return the session's canonical token object, registering it if new.

        Returns:
            The one canonical object for this token identity.

        """
        return self._py_bot.get_or_create_session_token(chain_id, address)

    def resolve_token(self, *, chain_id: int, address: str) -> SessionObject | None:
        """Return the session's canonical token object for `address`, or `None`.

        Returns:
            The object, or None when the session does not hold it.

        """
        return self._py_bot.resolve_session_token(chain_id, address)

    def counts(self) -> tuple[int, int]:
        """Count the session's canonical objects.

        Returns:
            ``(pool_count, token_count)`` — one entry per identity in this
            session.

        """
        return self._py_bot.session_object_counts()


class CompanionCache[T]:
    """The presentation objects this registry hands out, keyed by session key.

    Read, store, and drop all take a `SessionObject` handle, so the only keys
    that can enter this cache are the ones the Rust registry minted. A miss is
    `None` and never a `KeyError`, so a registry read reads as a question
    rather than an assertion.
    """

    __slots__ = ("_entries",)

    def __init__(self) -> None:
        """Start empty; the session it mirrors is the caller's registry."""
        self._entries: dict[str, SessionCompanion[T]] = {}

    def resolve(self, handle: SessionObject) -> T | None:
        """Return the companion filed under `handle`'s identity, if any.

        Returns:
            The companion, or None when the identity has none filed.

        """
        entry = self._entries.get(handle.key)
        return None if entry is None else entry.item

    def store(self, handle: SessionObject, item: T) -> None:
        """File `item` under `handle`'s identity, replacing any entry there.

        Callers that must not replace use [`get_or_store`], or check
        [`resolve`] first — this is the unconditional write.
        """
        self._entries[handle.key] = SessionCompanion(handle=handle, item=item)

    def get_or_store(self, handle: SessionObject, item: T) -> T:
        """File `item` unless the identity already has a companion; return the canonical one.

        The build path's concurrent-build primitive: a second worker that built
        the same pool or token gets the companion the first worker registered
        rather than a twin. `setdefault` is a single atomic call under the GIL,
        so racing workers converge on one entry.

        Returns:
            The filed companion — the existing one on a duplicate, `item`
            otherwise.

        """
        entry = self._entries.setdefault(handle.key, SessionCompanion(handle=handle, item=item))
        return entry.item

    def drop(self, handle: SessionObject) -> None:
        """Forget the companion for `handle`'s identity; a no-op when absent."""
        self._entries.pop(handle.key, None)

    def entries(self) -> Iterator[SessionCompanion[T]]:
        """Iterate the filed companions with the handle that names each.

        Returns:
            An iterator over one ``(handle, companion)`` pair per filed
            companion, snapshotted so a caller may mutate the cache while
            iterating.

        """
        return iter(tuple(self._entries.values()))

    def clear(self) -> None:
        """Drop every companion. The session's identities are untouched."""
        self._entries.clear()

    def __len__(self) -> int:
        """Count the filed companions.

        Returns:
            The number of companions filed.

        """
        return len(self._entries)


def pool_family_of(pool: object) -> str:
    """Read the Rust registration-family tag off a pool companion.

    Every pool companion wraps the live Rust handle it was built from, and that
    handle reports the family the core registered it under — the same tag
    vocabulary the session registry keys identity by. Reading the tag off the
    handle (rather than mapping a Python class to a family here) is what keeps
    a Python consumer from carrying a second, drifting notion of "which family
    is this pool".

    Returns:
        The family tag, e.g. `"v3"` or `"balancer-weighted"`.

    Raises:
        DegenbotValueError: The object carries no live handle, so the session
            cannot be told which pool it is.

    """
    handle = getattr(pool, "_py_pool", None)
    family = getattr(handle, "pool_family", None)
    if not isinstance(family, str):
        msg = (
            f"cannot name a session pool for {type(pool).__name__}: it carries no live "
            "pool handle, so its registry family is unknown"
        )
        raise DegenbotValueError(message=msg)
    return family
