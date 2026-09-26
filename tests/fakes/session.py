"""The session-registry seam for an injected ``Bot`` double.

Several tests inject a stand-in for the Rust `Bot` engine so a build path can
be driven without construction I/O. Such a double must still answer identity
questions, because the registries delegate identity to the session.

It answers them by **delegating to a real `Bot` handle** rather than by keeping
a dict of its own. The session's identity authority is the Rust
`SessionObjectRegistry`; a double that deduped in Python would be testing a
second, fake authority, and a test that passed against it would prove nothing
about the seam it is named for. The double's own methods still come from the
test, so only the identity surface is real.
"""

from __future__ import annotations

from typing import Any

from degenbot._ffi import Bot

#: The session-registry surface a `Bot` double must answer, by name. The
#: companion registries call exactly these, so a new registry read surface
#: shows up here as a missing name rather than as a confusing `AttributeError`
#: from inside a registry.
SESSION_REGISTRY_METHODS: tuple[str, ...] = (
    "get_or_create_session_pool",
    "resolve_session_pool",
    "resolve_session_pool_by_address",
    "get_or_create_session_token",
    "resolve_session_token",
    "session_object_counts",
)


def session_registry_methods(*, chain_id: int = 1) -> dict[str, Any]:
    """The session-registry methods an injected ``Bot`` double must carry.

    Returns:
        Keyword arguments to splat into the double's constructor, e.g.
        ``SimpleNamespace(build_v2_pool=..., **session_registry_methods())``.
    """
    real = Bot(chain_id=chain_id)
    return {name: getattr(real, name) for name in SESSION_REGISTRY_METHODS}
