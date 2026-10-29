"""Session fakes for the two session seams still driven by fakes.

**The session-registry seam for an injected ``Bot`` double.** Several tests
inject a stand-in for the Rust `Bot` engine so a build path can be driven
without construction I/O. Such a double must still answer identity questions,
because the registries delegate identity to the session.

It answers them by **delegating to a real `Bot` handle** rather than by keeping
a dict of its own. The session's identity authority is the Rust
`SessionObjectRegistry`; a double that deduped in Python would be testing a
second, fake authority, and a test that passed against it would prove nothing
about the seam it is named for. The double's own methods still come from the
test, so only the identity surface is real.

**The outcome doubles the renderers read.** :class:`FakeBatchOutcome` carries
the folded counter surface of one drained batch (what the render fold and the
silent-veto predicate read); :class:`FakeDispatchOutcome` carries exactly the
``failures``/``path_infos`` pair the ``[sim-fail]`` renderer reads.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from tests.helpers.seam import Bot

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

    Args:
        chain_id: The chain the real identity handle is built for.

    Returns:
        Keyword arguments to splat into the double's constructor, e.g. a
        scripted engine double carrying ``**session_registry_methods()``.
    """
    real = Bot(chain_id=chain_id)
    return {name: getattr(real, name) for name in SESSION_REGISTRY_METHODS}


@dataclass
class FakeBatchOutcome:
    """The FFI batch-outcome double the render fold and the smoke FSM read."""

    gas_profitable: list[object] = field(default_factory=list)
    gas_unprofitable_count: int = 0
    exception_count: int = 0
    fail_count: int = 0
    candidate_count: int = 0
    suppressed_count: int = 0
    thin_dropped: int = 0
    divergent_dropped: int = 0
    fot_dropped: int = 0
    fail_buckets: dict[str, int] = field(default_factory=dict)
    failures: list[dict[str, Any]] = field(default_factory=list)
    path_infos: dict[int, dict[str, Any]] = field(default_factory=dict)


@dataclass
class FakeDispatchOutcome:
    """The ``DispatchOutcome`` double the ``[sim-fail]`` renderer reads."""

    failures: list[dict[str, Any]] = field(default_factory=list)
    path_infos: dict[int, dict[str, Any]] = field(default_factory=dict)
