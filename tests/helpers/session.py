"""The cockpit's one test construction seam.

:meth:`~degenbot.runner.session.Session.for_test` builds a started session
directly over injected collaborators; this module supplies the standard fake
posture every cockpit test shares, so a test names only the facet it exercises
instead of repeating a thirteen-seam actor list. The record it hands the seam
is the production :class:`~degenbot.runner.session.Actors`, so a test exercises
the same record shape a boot resolves.

The doubles here are the FIXTURE (bot / engine registry / provider / dispatch
state / consumer / builder / scheduler); the subject of a test stays its own
fake, passed as an override.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from degenbot.runner._relay_posture import RelayPosture
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from degenbot.runner.session import Actors, InjectedActors, Session
from tests.fakes.bot import FakeBot
from tests.fakes.engine import FakeEngineRegistry
from tests.fakes.runner_pipelines import StubPipeline
from tests.helpers.boot_actors import inline_registration_scheduler, noop_coro
from tests.helpers.identity_env import identity_env

if TYPE_CHECKING:
    from degenbot.runner._session_watch import SessionWatch

NODE = "ws://localhost:8546"
OPERATOR_ADDRESS = "0x9C56a29c7231974c269E24F9FB3c29203039089E"
OPERATOR_PRIVATE_KEY = "0x" + "a" * 64
EXECUTOR_ADDRESS = "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"

#: The offline relay posture every cockpit test broadcasts through.
OFFLINE_RELAY = RelayPosture(relay_urls=["http://offline-test.relay"])


def offline_cfg(
    *,
    live: bool = True,
    env: dict[str, str] | None = None,
) -> ArbitrageConfig:
    """Build the standard offline cockpit config.

    The session tests inject fakes for the actor trio, so the node URI is never
    connected — it only has to be *present* so ``ArbitrageConfig.build``
    answers. It arrives through the explicit override layer, because the
    installed config reads the environment at FFI module init, long before a
    test could set it.

    Args:
        live: Whether the config is a live (signing) stance.
        env: Extra identity env values overriding the standard operator keys.

    Returns:
        The built config.

    """
    identity = {
        "OPERATOR_ADDRESS": OPERATOR_ADDRESS,
        "OPERATOR_PRIVATE_KEY": OPERATOR_PRIVATE_KEY,
        "EXECUTOR_CONTRACT_ADDRESS": EXECUTOR_ADDRESS,
    }
    identity.update(env or {})
    with identity_env(identity):
        return ArbitrageConfig.build(
            live=live,
            permutation=None,
            rpc=RpcCascadeOverrides(chain_id=1, node=NODE),
        )


class FakeAsyncW3:
    """The session provider double (non-Alloy, so no sim context is built)."""

    async def get_block(self, block_identifier: str):
        return {
            "number": 12_345,
            "baseFeePerGas": 10**9,
            "gasUsed": 0,
            "gasLimit": 30_000_000,
        }

    async def get_transaction_count(self, address: str) -> int:
        return 7

    async def make_request(self, method: str, params: list) -> dict[str, Any]:
        # The session tests do not drive the typed dispatch RPCs; an incidental
        # call answers an empty result.
        return {}

    @property
    def rpc_url(self) -> str:
        return "http://fake:8545"

    def as_async_alloy(self) -> None:
        return None


class FakeDispatcher:
    """The dispatcher-clock surface the block loop and the dispatch leaf touch."""

    def __init__(self, current_block: int = 12_345) -> None:
        self.current_block = current_block
        self.block_times: list[tuple[int, int]] = []

    def record_block_time(self, block_number: int, block_timestamp: int) -> None:
        self.block_times.append((block_number, block_timestamp))

    def block_time_count(self) -> int:
        return len(self.block_times)

    def block_times_oldest(self) -> tuple[int, int]:  # pragma: no cover - gate off
        return (0, 0)

    def advance_block(self, block_number: int) -> None:
        self.current_block = block_number

    def discard_path(self, path_id: int) -> None:
        pass

    def block_timestamp_for(self, block_number: int) -> int | None:
        return 1_700_000_000


@dataclass(frozen=True)
class FakeReadiness:
    """The readiness-view double the run ritual reads for its hosted-arm set."""

    active_backrun_facets: list[str] = field(default_factory=list)
    settlement_active: bool = True


def noop_consumer(*args: Any, **kwargs: Any) -> Any:
    """The consumer double: a coroutine that ends the main loop immediately."""
    return noop_coro()


def noop_builder(**kwargs: Any) -> Any:
    """The path-builder double: a coroutine that completes without doing work."""
    return noop_coro()


def injected_test_actors(**overrides: Any) -> InjectedActors:
    """Build the standard fake DI record; per-test kwargs win over defaults.

    This is the seam for a test that drives the PRODUCTION start path (the boot
    gate, the posture mint, the actor resolution) over fakes — the frozen
    record a real ``BotRunner`` receives.

    Args:
        **overrides: The seams the test replaces (the subject fakes).

    Returns:
        The frozen DI record.

    """
    injected: dict[str, Any] = {
        "bot": FakeBot(),
        "engine_registry": FakeEngineRegistry(),
        "async_w3": FakeAsyncW3(),
        "path_builder": noop_builder,
        "consumer": noop_consumer,
        "pipeline_factory": StubPipeline,
        "scheduler": inline_registration_scheduler,
        "relay_posture": OFFLINE_RELAY,
        "settlement_endpoints": lambda: ["http://relay-a"],
        "readiness": FakeReadiness,
    }
    injected.update(overrides)
    return InjectedActors(**injected)


def test_actors(**overrides: Any) -> Actors:
    """Build the standard resolved actor record (a for_test session's collaborators).

    Args:
        **overrides: The seams the test replaces (the subject fakes).

    Returns:
        The resolved actor record.

    """
    return Actors.from_injected(injected_test_actors(**overrides))


def session_for_test(
    *,
    cfg: ArbitrageConfig | None = None,
    dispatcher: Any = None,
    current_block: int | None = None,
    sim_ctx: Any = None,
    settlement_active: bool = True,
    readiness: Any = None,
    relay_posture: Any = OFFLINE_RELAY,
    session_watch: SessionWatch | None = None,
    install_sigint: bool = False,
    **actor_overrides: Any,
) -> Session:
    """Build the standard started session; per-test kwargs win over defaults.

    Args:
        cfg: The typed config (``None`` = :func:`offline_cfg`).
        dispatcher: The dispatch state (``None`` = a clock at ``current_block``).
        current_block: The block clock (``None`` = the dispatcher's block).
        sim_ctx: The sim context (``None`` = none, i.e. a non-Alloy provider).
        settlement_active: The resolved settlement-arm disposition.
        readiness: The readiness view the run ritual reads.
        relay_posture: The relay posture (``None`` = no-relay posture).
        session_watch: The end-state owner to use (``None`` = a fresh one).
        install_sigint: Whether the session binds the SIGINT handler.
        **actor_overrides: The collaborator seams the test replaces.

    Returns:
        The started session.

    """
    resolved_cfg = cfg if cfg is not None else offline_cfg()
    resolved_dispatcher = dispatcher if dispatcher is not None else FakeDispatcher()
    return Session.for_test(
        resolved_cfg,
        actors=test_actors(**actor_overrides),
        dispatcher=resolved_dispatcher,
        current_block=(
            resolved_dispatcher.current_block if current_block is None else current_block
        ),
        sim_ctx=sim_ctx,
        settlement_active=settlement_active,
        readiness=readiness if readiness is not None else FakeReadiness(),
        relay_posture=relay_posture,
        session_watch=session_watch,
        install_sigint=install_sigint,
    )
