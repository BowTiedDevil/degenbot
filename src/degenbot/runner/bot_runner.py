"""Settlement-arbitrage runtime driver facade (``BotRunner``).

``BotRunner`` is the operator shell over one
:class:`~degenbot.runner.session.Session`: the session owns the config, the
resolved actors, the dispatcher, the block clock, the pipelines, and the
lifecycle/state machines, while this class exposes the verbs an operator (or
the CLI) drives.

    start():              build the actors, subscribe -> backfill -> verify
                          config (``EngineRegistry.start``, stops at Backfilled,
                          pre-resume)
    run():                the run ritual (:mod:`~degenbot.runner._run_ritual`)
                          drives consumer-attach -> watch-attach -> ``resume()``
                          -> registration -> main loop
    enqueue_path():       add ONE specific path to a running session
    trigger_discovery():  bounded one-shot discovery over a running session
    shutdown():           stop the Rust pump (any phase, idempotent)

The driver is ``stays-python`` (asyncio loop, SIGINT, deployment policy): it
controls the Rust engine but owns no pool state (ADR-003: ``Bot`` is the
single state owner; ADR-006: ``Bot`` is the per-chain orchestrator). The
start/run sequencing above is Rust-owned in ``EngineDriver``; this facade
delegates path registration to :mod:`~degenbot.runner.build_paths` and the
main loop to :mod:`~degenbot.runner._consume`. The Python/Rust split is
documented in ``docs/architecture/rust-settlement-bot-parity.md``.

Every earlier per-seam accessor (the actor trio, the pipelines, the readiness
view, the scheduler, the watchdog) is gone: those are the session's fields, and
a caller that needs one reads it there.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Self

from degenbot.runner.session import InjectedActors, Session

if TYPE_CHECKING:
    from degenbot.runner._session_watch import SessionEndVerdict
    from degenbot.runner.config import ArbitrageConfig


class BotRunner:
    """The operator verbs of one settlement-arbitrage pump session.

    Usage (production)::

        cfg = ArbitrageConfig.build(live=not dry_run, permutation=args.permutation)
        async with BotRunner(cfg) as runner:
            await runner.run()

    The session is built by ``start()``; ``run()`` schedules
    discovery+registration as a background task (through the session's
    scheduler) and enters the main loop immediately, so the state-trim runs on
    registration completion (inside the scheduled hand-off), not on the
    main-loop entry path. A fatal verification error still crashes loudly
    through the cross-task channel.

    Testability seams (mirrors ``EngineRegistry``'s ``engine=`` seam): the
    ``actors`` record (:class:`~degenbot.runner.session.InjectedActors`)
    injects the actor trio, the module functions, and the scheduler; a cockpit
    test that wants a ready session without the boot I/O builds one with
    :meth:`~degenbot.runner.session.Session.for_test` instead.
    """

    def __init__(
        self,
        cfg: ArbitrageConfig,
        *,
        actors: InjectedActors | None = None,
        install_sigint: bool = True,
    ) -> None:
        """Build the session shell over the config + the injectable actor seams.

        Args:
            cfg: The typed config the session resolves its actors from.
            actors: The DI record naming the seams to adopt verbatim
                (``None`` = every seam from ``cfg``).
            install_sigint: Whether the session binds the SIGINT handler
                (tests pass ``False`` to avoid a process-global handler).

        """
        # Strategy admission is host-owned (ADR-057): the runner carries no
        # Python-side arm gate. The host boot registers each configured facet
        # and `enable_strategy` surfaces the typed refusal
        # (`UnknownStrategyError` / `UnconfiguredStrategyError`), so there is
        # one admission authority and no env check that can disagree with the
        # host's configured-ness.
        self.session = Session(
            cfg=cfg,
            injected=actors if actors is not None else InjectedActors(),
            install_sigint=install_sigint,
        )

    async def start(self) -> Self:
        """Build the session's actors and run the engine's pre-resume ritual.

        Returns:
            The started runner.

        """
        await self.session.start()
        return self

    async def run(self) -> SessionEndVerdict:
        """Run the cockpit main loop until the consumer task ends.

        Returns:
            The session's end verdict (the session watch's ranking over how
            the run ended).

        """
        return await self.session.run()

    async def enqueue_path(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Add ONE specific path to a running session.

        Args:
            path_steps: The path's steps (the pipeline's enqueue shape).
            directions: Per-step swap directions (``None`` = resolve them).

        """
        await self.session.enqueue_path(path_steps, directions)

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep over a running session.

        Args:
            bound: The maximum number of paths the sweep processes.

        Returns:
            The number of paths processed by the sweep.

        """
        return await self.session.trigger_discovery(bound=bound)

    async def shutdown(self) -> None:
        """Signal the Rust core to stop the pump (best-effort, any phase)."""
        await self.session.shutdown()

    async def __aenter__(self) -> Self:
        """Start the pump, then hand the started runner to the ``async with`` block.

        Returns:
            The started runner.

        """
        await self.start()
        return self

    async def __aexit__(self, *exc: object) -> None:
        """Best-effort teardown; never suppresses.

        Args:
            *exc: The in-flight exception triple (unused; never suppressed).

        """
        await self.session.close()
