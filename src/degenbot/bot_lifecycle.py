"""Shared Python-handle teardown for ``Bot``.

``Bot`` is the single-chain facade (ADR-006 D5) with attribute shape
(``pools``, ``tokens``, ``managed_pools``, ``_py_bot``, ``_provider``,
``_io``). This module holds the teardown body in one place so the facade
delegates to it rather than inlining ~15 lines.

Two entry points:

- :func:`release_python_state` — the *mid-lifecycle* handshake: drop the
  pool/token registries once the Rust engine has taken ownership of canonical
  pool state. The Bot keeps running; only the redundant Python caches go.
- :func:`close` — the *end-of-life* teardown: drives :data:`TEARDOWN_STACK`
  in order. Idempotent via a per-instance ``_closed`` flag.

The Rust ``Bot`` is reference-counted; closing a Python wrapper only drops
that wrapper's ref. A running engine that took its own ref (via
``EngineRegistry(bot=bot)`` → ``ArbitrageEngine(py_bot=...)``) is unaffected.

Teardown order is a STACK, not a sequence of statements: each rung is a named
release with its own reason, and :data:`TEARDOWN_STACK` is the one place the
order is stated. The order is load-bearing in both directions — the provider
must be closed before the handles that pin its ``Arc`` are dropped (a live
``Arc`` would let alloy's pubsub service reconnect into a socket that is about
to die), and the caches must go before the provider so a released pool object
can never outlive the connection it was built from. Reading the stack top to
bottom is reading the teardown.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from collections.abc import Callable


class _BotLike(Protocol):
    """Structural shape required by the teardown functions.

    The volatile members are typed :data:`~typing.Any` because teardown
    reaches into registry/provider internals (``pools._reset``,
    ``tokens.reset``, ``_provider.close``) whose precise types
    live in unrelated modules — pulling them in here would create import
    cycles for a structural protocol that only needs call-site shape.
    """

    pools: Any
    tokens: Any
    managed_pools: Any
    _provider: Any
    _py_bot: Any
    _io: Any
    _async_adapter: Any
    _erc20_builder: Any
    _curve_builder: Any
    _closed: bool


def release_python_state(bot: _BotLike) -> None:
    """Drop Python-side pool/token caches once Rust owns canonical state.

    Resets the pool and token registries. Idempotent and safe to call before
    :func:`close`.
    """
    # Drop the pool and token registries (Rust owns canonical state). The
    # mid-lifecycle handoff keeps the Rust ``BotState`` pools — the live pump
    # keeps writing V3 Mint/Burn/Swap through the shared core, so we must NOT
    # unregister (propagate_to_rust=False). Unregistering is end-of-life only
    # (`close`), where the whole Rust state is torn down alongside the bot.
    bot.pools._reset(propagate_to_rust=False)  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]
    bot.tokens.reset()  # type: ignore[attr-defined]


def _close_provider(bot: _BotLike) -> None:
    """Close the provider connection if it exposes close()."""
    provider = bot._provider  # ruff:ignore[private-member-access]
    if hasattr(provider, "close"):
        provider.close()  # type: ignore[attr-defined]


def _drop_bot_handle(bot: _BotLike) -> None:
    """Drop our own Rust-handle and provider refs (the engine keeps its own Bot ref)."""
    bot._py_bot = None  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]
    bot._provider = None  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]


def _drop_io_seams(bot: _BotLike) -> None:
    """Drop the I/O seam (`BotIo`) and the on-demand async adapter."""
    # Both hold a strong reference to the shared provider's `Arc<dyn Provider>`;
    # leaving them live pins that Arc — and if the provider's backing endpoint
    # is a locally-owned anvil fork that is torn down next, alloy's pubsub
    # service reconnects into the now-dead socket, logging
    # `Reconnection attempt N/10 …` for up to 10 backoff attempts. Dropping
    # them here lets the provider's pubsub shut down cleanly
    # ("request channel closed") before any consumer of the endpoint dies.
    bot._io = None  # ruff:ignore[private-member-access]
    bot._async_adapter = None  # ruff:ignore[private-member-access]


def _drop_holder_refs(bot: _BotLike) -> None:
    """Drop the registries + builders + ctx that each hold a strong ref to `_py_bot`."""
    # PoolRegistry → Bot, TokenRegistry, BuilderContext → Bot. Rust `Bot` is
    # refcounted; until EVERY Python holder is dropped the rust core stays alive
    # and keeps its `ConstructionIo`'s `Arc<dyn Provider>` — the same
    # reconnect-into-a-dead-socket hazard as `_io`. Dropping all of them lets the
    # rust `Bot`/`BotState` release the provider when the last one goes.
    bot.pools = None
    bot.tokens = None
    bot.managed_pools = None
    bot._erc20_builder = None  # ruff:ignore[private-member-access]
    bot._curve_builder = None  # ruff:ignore[private-member-access]


@dataclass(frozen=True, slots=True)
class TeardownRung:
    """One rung of the close stack: a named, ordered release."""

    name: str
    release: Callable[[_BotLike], None]


#: The end-of-life teardown, in the order it must run (see the module docstring
#: for why each edge points the way it does).
TEARDOWN_STACK: tuple[TeardownRung, ...] = (
    TeardownRung("python_state", release_python_state),
    TeardownRung("provider", _close_provider),
    TeardownRung("bot_handle", _drop_bot_handle),
    TeardownRung("io_seams", _drop_io_seams),
    TeardownRung("holder_refs", _drop_holder_refs),
)


def close(bot: _BotLike) -> None:
    """End-of-life teardown: drive :data:`TEARDOWN_STACK` once, in order.

    Idempotent — safe to call directly and again from a context manager's
    ``__exit__``/``__aexit__``. Composes :func:`release_python_state`, so it
    is also safe after an explicit mid-lifecycle ``release_python_state`` call.
    """
    if getattr(bot, "_closed", False):
        return
    bot._closed = True  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]

    for rung in TEARDOWN_STACK:
        rung.release(bot)
