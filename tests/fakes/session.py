"""Session fakes for the session seams the tests drive.

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

**The dispatch submit seam.** The runner tests drive
``_submit_batch_records`` through one session shape (``async_w3``, ``cfg``,
``dispatcher``, ``relay_posture``, ``submission_smoke``, ``engine_registry``,
``pipeline``), so :func:`fake_session` builds it once and per-test kwargs win
over the defaults. A field the seam never reads stays unset (``None``) — the
same AttributeError a hand-built session produced.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from degenbot._ffi import Bot
from degenbot.runner._dispatch import SubmissionSmoke

if TYPE_CHECKING:
    from collections.abc import Callable

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
        Keyword arguments to splat into the double's constructor, e.g. a
        scripted engine double carrying ``**session_registry_methods()``.
    """
    real = Bot(chain_id=chain_id)
    return {name: getattr(real, name) for name in SESSION_REGISTRY_METHODS}


# ---------------------------------------------------------------------------
# The dispatch submit-seam session double.
# ---------------------------------------------------------------------------


def opaque_rust_provider() -> object:
    """A fresh opaque stand-in for the Rust provider the seam hands out."""
    return object()


@dataclass
class FakeAsyncW3:
    """The session w3 double: ``as_async_alloy()`` yields an opaque Rust provider."""

    as_async_alloy: Callable[[], object] = opaque_rust_provider


@dataclass
class FakeRelayProvider:
    """An injected relay-provider double; ``as_async_alloy()`` is its only surface."""

    as_async_alloy: Callable[[], object] = opaque_rust_provider


@dataclass
class FakeSessionConfig:
    """The ``cfg`` slice the submit seam reads; mutable, so tests can re-arm."""

    operator_private_key: str = "0x" + "a" * 64
    chain_id: int = 1
    dry_run: bool = False
    inject_executor_code: bool = False


@dataclass
class FakeDispatcher:
    """The dispatcher clock double: the submit seam stamps ``current_block``."""

    current_block: int = 1


@dataclass
class FakeCandidate:
    """A submit candidate double: the fields the record renderer and leaf read."""

    path_id: int = 7
    solve_block: int = 1
    net_profit: int = 0
    gas_used: int = 0
    execute_calldata: object | None = None


@dataclass
class FakeSubmitOutcome:
    """The dispatch outcome double: the candidates cleared for submission."""

    gas_profitable: list[FakeCandidate] = field(default_factory=list)


@dataclass
class FakeSession:
    """The one session double ``_submit_batch_records`` reads."""

    async_w3: FakeAsyncW3 = field(default_factory=FakeAsyncW3)
    cfg: FakeSessionConfig = field(default_factory=FakeSessionConfig)
    dispatcher: FakeDispatcher = field(default_factory=FakeDispatcher)
    relay_posture: object | None = None
    submission_smoke: SubmissionSmoke = field(default_factory=SubmissionSmoke)
    engine_registry: object | None = None
    pipeline: object | None = None


def fake_session(
    *,
    relay_posture: object | None = None,
    current_block: int = 1,
    async_w3: FakeAsyncW3 | None = None,
    submission_smoke: SubmissionSmoke | None = None,
    engine_registry: object | None = None,
    pipeline: object | None = None,
    cfg: FakeSessionConfig | None = None,
    cfg_overrides: dict[str, Any] | None = None,
) -> FakeSession:
    """Build the submit-seam session double; per-test kwargs win over defaults.

    ``cfg_overrides`` carries the one-off config knobs a test re-arms
    (``dry_run``, ``inject_executor_code``, ...).
    """
    resolved_cfg = cfg if cfg is not None else FakeSessionConfig(**(cfg_overrides or {}))
    return FakeSession(
        async_w3=async_w3 if async_w3 is not None else FakeAsyncW3(),
        cfg=resolved_cfg,
        dispatcher=FakeDispatcher(current_block=current_block),
        relay_posture=relay_posture,
        submission_smoke=(
            submission_smoke if submission_smoke is not None else SubmissionSmoke()
        ),
        engine_registry=engine_registry,
        pipeline=pipeline,
    )


# ---------------------------------------------------------------------------
# The runner's dispatch/sim session seams (dispatch assembly, payload merge,
# sim-submit factory) and the outcome doubles their renderers read.
# ---------------------------------------------------------------------------


@dataclass
class FakeRunnerConfig:
    """The ``cfg`` slices the runner's dispatch/sim seams read.

    A field a seam never reads stays ``None`` — extend deliberately, so an
    unexpected read surfaces as a loud ``None`` at the seam that misread it.
    """

    erc6909_profit: bool | None = None
    sim_pipeline_concurrency: int | None = None
    operator_address: str | None = None


@dataclass
class FakeSimContext:
    """The sim-context double: the executor address the merge seam passes."""

    executor_address: str | None = None


@dataclass
class FakeRunnerSession:
    """The session double the runner's dispatch/merge/sim-submit seams read.

    Unlike :class:`FakeSession` (the submit seam), these seams read the
    registry, config, sim context, and dispatcher off one session shape; a
    field the seam under test never reads stays unset (``None``).
    """

    engine_registry: object | None = None
    dispatcher: FakeDispatcher | None = None
    sim_ctx: FakeSimContext | None = None
    cfg: FakeRunnerConfig | None = None


@dataclass
class FakeBatchOutcome:
    """The FFI batch-outcome double the merged-outcome stitch renders."""

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
