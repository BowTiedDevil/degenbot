"""The per-call ``Bot`` for the I/O-free pool factories.

The pool factories mint a FRESH ``Bot`` per call by default, so every built
pool is fully isolated. Pools are MUTABLE, and the register entries are now
get-or-register (a same-address + same-family + same-identity build answers the
EXISTING handle), so a shared *default* bot would silently reuse one test's
mutated pool in another. Isolation stays the default; this helper is that
default in ONE place, so the factories carry no inline ``Bot()`` fallback.

A caller that WANTS to share one bot — a read-only parity / keys-diff build, or
the engine's shared core — passes ``py_bot=`` explicitly. That is the
dependency-injection seam, not a workaround.
"""

from __future__ import annotations

from tests.helpers.seam import Bot


def new_pool_bot() -> Bot:
    """A fresh, isolated ``Bot`` for one pool-factory call.

    The ``chain_id=0`` construction default is deliberate for these
    synthetic fixtures: a factory pool is not a chain deployment, and
    chain 1 would run the real CREATE2/deployment verification against
    synthetic addresses. A fixture that needs real chain facts (the native
    token's metadata, ADR-097 D2) passes its own chain-scoped ``py_bot=``.
    """
    return Bot()


__all__ = ["new_pool_bot"]
