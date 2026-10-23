"""Fixtures, suite-wide gates, and the declared environment for the Python suite.

Sections, top to bottom: the suite environment (the captured environment every
construction observes, installed through the config-holder seam, with the
per-test ``governed_holder`` companion beside it); collection gates and hooks; the RPC
vocabulary with the anvil/fork/Bot fixtures; the golden record/replay
factories. The README doctest harness lives in the root ``conftest.py``;
offline detection lives in ``tests/offline.py``.
"""

import logging
import os
import shutil
from collections.abc import Callable, Generator, Iterator, Mapping
from pathlib import Path

import pytest
from _pytest.config import Config, Parser
from _pytest.nodes import Item
from dotenv import dotenv_values
from hypothesis import settings

# THE SUITE ENVIRONMENT, DECLARED AS DATA. The declaration and its os.environ
# pin live in tests/suite_env.py, which the ROOT conftest imports before the
# first `degenbot` import: the FFI module init boots the process-wide typed
# BotConfig from the process layers (first-wins), and the engine boot
# re-resolves those layers at every construction (backrun_boot_resources ->
# load_process_config), so the captured-env seam below re-governs holder reads
# but can never re-govern what a construction dials or reads. See
# tests/suite_env.py for the full contract — including why the pin is
# force-set rather than setdefault, and why the declared database path is an
# absent per-session file.
from degenbot._ffi import reinstall_config_holder_for_tests, shutdown_log_drainer
from degenbot.bot import Bot
from degenbot.fork import AnvilFork, ForkLaunchConfig
from degenbot.logging import _LOG_LISTENER, _QUEUED_HANDLER, set_log_level
from tests.golden.oracle import GOLDEN_ROOT, GoldenOracle, _nodeid_to_path
from tests.golden.recorded_pool import RecordedPool
from tests.helpers.bot_factory import make_bot_with_provider
from tests.offline import is_offline
from tests.standalone_anvil import seed as seed_catalog
from tests.suite_env import SUITE_ENV

# Re-govern the holder through the sanctioned test seam. The root conftest
# pinned SUITE_ENV onto os.environ BEFORE the first degenbot import, so the
# module-init snapshot already IS the suite's declaration; this reinstall is
# the same-value re-governance that fails collection loudly if the declared
# file stops loading, and pairs with the per-test `governed_holder` fixture.
reinstall_config_holder_for_tests(SUITE_ENV)


# The per-test counterpart of the install above: import-time governance is
# process-wide and one-shot; this fixture re-governs around individual tests.
@pytest.fixture
def governed_holder() -> Iterator[Callable[[dict[str, str]], None]]:
    """Per-test captured-environment override that re-governs the config holder.

    ``govern({name: value})`` merges the overrides over ``SUITE_ENV`` and
    installs that captured environment into the process-wide typed-config
    holder (and the shell's published verdict). The process environment is
    never written; teardown re-installs the suite's declared snapshot, so a
    test that does not opt in keeps the ``tests/ambient_config.toml`` behavior
    unchanged.

    This is the config-holder test seam: production first-wins is untouched; the seam
    exists so a per-test ``DEGENBOT_RPC_*`` override governs the NEXT
    construction instead of being silently pinned to the process's first
    resolution.

    Yields:
        ``govern(overrides)``, which installs the merged captured environment.

    """

    def govern(overrides: dict[str, str]) -> None:
        reinstall_config_holder_for_tests({**SUITE_ENV, **overrides})

    try:
        yield govern
    finally:
        reinstall_config_holder_for_tests(SUITE_ENV)


# Hypothesis policy: no per-example wall-clock deadline. Fuzz tests here assert
# math/round-trip properties, not timing; the 200ms default only fails
# spuriously under load (FFI first-call cost, GC pauses, scheduler jitter). A
# test that someday asserts a real timing property opts back in with an
# explicit @settings(deadline=...), which overrides this profile.
settings.register_profile("default", deadline=None)
settings.load_profile("default")


def pytest_addoption(parser: Parser):
    parser.addoption(
        "--skip-fixture",
        action="store",
        default="",
        help="Comma-separated list of fixture names to skip",
    )
    parser.addoption(
        "--golden-mode",
        action="store",
        default="replay",
        choices=("record", "replay"),
        help=(
            "Golden-oracle mode for on-chain parity tests (tests/golden). "
            "'replay' (default, CI) reads recorded ints with no RPC; "
            "'record' invokes the deferred contract callable against a live fork "
            "and writes the golden file. See docs/architecture/golden-onchain-parity.md."
        ),
    )
    parser.addoption(
        "--golden-root",
        action="store",
        default=str(Path(__file__).resolve().parent / "golden" / "data"),
        help="Root directory for golden-oracle JSON files.",
    )


# Markers that render a fork test a safe no-op in the default offline CI run:
# the live-RPC tiers (online_rpc/base), slow-only (slow), or permanently skipped.
_NON_RUNNING_MARKERS = {"online_rpc", "base", "slow", "skip"}


# The `fork_*` fixtures chain into `bot_*` and per-file bot/token/pool fixtures,
# so a test's transitive `_fixtureinfo.names` (not just `fixturenames`) is the
# reliable signal that it needs a live fork.
def _check_fork_tier_tags(config: Config, items: list[Item]) -> None:
    """Fail collection if a test depends on a ``fork_*`` fixture without a tier tag.

    A fork test must carry ``online_rpc``/``base``/``slow`` so it is deselected
    from the default offline CI (live RPC is unavailable there). The backstop
    fixture guard already makes such a test skip gracefully; this hard gate
    forces an explicit classification so no fork test silently drops out of the
    default run. Any test legitimately needing a live fork must be tagged.
    """
    offenders = []
    for item in items:
        names = getattr(item, "_fixtureinfo", None)
        if names is None:
            continue
        fork_deps = sorted(n for n in names.names_closure if n.startswith("fork_"))
        if not fork_deps:
            continue
        custom = {m.name for m in item.iter_markers()}
        if custom & _NON_RUNNING_MARKERS:
            continue
        offenders.append(f"{item.nodeid}  (forks: {', '.join(fork_deps)})")
    if offenders:
        joined = "\n  ".join(offenders)
        msg = (
            "fork-dependent test(s) are not tagged for offline CI; add @pytest.mark."
            "online_rpc (live) / base / slow so they are deselected from the default "
            f"python-test run:\n  {joined}"
        )
        raise pytest.UsageError(msg)


def pytest_collection_modifyitems(config: Config, items: list[Item]):
    _check_fork_tier_tags(config, items)

    # Deliberately no per-suite timeout headroom: slow-marked suites run
    # against the 300s global default (pyproject.toml) so a breach is a loud
    # failure to investigate, not a budget to raise.

    skip_fixtures: str = config.getoption("--skip-fixture")
    if not skip_fixtures:
        return  # nothing to skip

    # Convert comma-separated string into a set of fixture names
    ignore_fixtures = {name.strip() for name in skip_fixtures.split(",") if name.strip()}

    if not ignore_fixtures:
        return

    remaining_items = []
    deselected_items = []

    for item in items:
        if any(fix in ignore_fixtures for fix in item.fixturenames):
            deselected_items.append(item)
        else:
            remaining_items.append(item)

    if deselected_items:
        items[:] = remaining_items
        config.hook.pytest_deselected(items=deselected_items)


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    """Stop the degenbot log pipeline while the capture streams are still open.

    The queue listener and the Rust log drainer are process-global threads that
    outlive the pytest session: pytest closes its capture streams at
    unconfigure, and any record they forward afterwards dies in the closed
    stream — the historical ``--- Logging error ---`` spray at the tail of
    ``just test-python``. The leaking records come from the ambient fleet
    boot's feed/head-clock tasks, which outlive the test (and the session)
    that booted them. Stopping the listener FIRST drains everything already
    queued into the still-open capture; shutting the Rust drainer down second
    means any late Rust record only lands in the now-inert queue. Idempotent;
    a no-op in the xdist controller (its listener owns no pending records).
    """
    _LOG_LISTENER.stop()
    shutdown_log_drainer()


@pytest.fixture(scope="session", autouse=True)
def _set_degenbot_logging():
    """Run the suite with Python-side DEBUG records visible on the console queue.

    ``set_log_level`` moves the package logger and the Rust-bridge loggers to
    DEBUG, but a handler's own level is the ceiling every record passes at
    ``callHandlers``: leaving the queued handler at INFO drops DEBUG before it
    can be queued, so the handler must move with the loggers.

    That covers the Python side only. The Rust ``tracing`` ``EnvFilter`` is the
    first gate and defaults to ``info`` for the Python driver, so Rust
    ``debug!`` records cross into Python only when a test raises ``RUST_LOG``
    (or the telemetry level); without it, no handler level can surface them.
    """
    set_log_level(logging.DEBUG)
    _QUEUED_HANDLER.setLevel(logging.DEBUG)


# The harness's endpoint vocabulary: one row per (env-var name, default URI),
# resolved through ``_rpc`` and bound below. Archive and full tiers deliberately
# share a default wherever one public endpoint serves both today (Arbitrum and
# Ethereum via publicnode, Base via mainnet.base.org) -- the table makes that
# sharing visible, and a tier that needs its own endpoint (a real archive node)
# declares it through ``tests.env`` or the process environment. A name absent
# from the table raises at collection, so the vocabulary cannot drift from its
# defaults. The names are imported across
# the suite (``tests/test_anvil_fork.py``, the parity suites) and the ``_FULL_``
# env spellings are an external contract (``executor/ape-config.yaml``), so the
# bindings stay real module-level assignments.
_DEFAULT_RPC_URIS: dict[str, str] = {
    "TEST_ARBITRUM_FULL_NODE_HTTP_URI": "https://arbitrum-one-rpc.publicnode.com",
    "TEST_ARBITRUM_FULL_NODE_WS_URI": "wss://arbitrum-one-rpc.publicnode.com",
    "TEST_BASE_ARCHIVE_NODE_HTTP_URI": "https://mainnet.base.org",
    "TEST_BASE_ARCHIVE_NODE_WS_URI": "wss://mainnet.base.org",
    "TEST_BASE_FULL_NODE_HTTP_URI": "https://mainnet.base.org",
    "TEST_BASE_FULL_NODE_WS_URI": "wss://mainnet.base.org",
    "TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI": "https://ethereum-rpc.publicnode.com",
    "TEST_ETHEREUM_ARCHIVE_NODE_WS_URI": "wss://ethereum-rpc.publicnode.com",
    "TEST_ETHEREUM_FULL_NODE_HTTP_URI": "https://ethereum-rpc.publicnode.com",
    "TEST_ETHEREUM_FULL_NODE_WS_URI": "wss://ethereum-rpc.publicnode.com",
}


_TESTS_ENV_PATH = Path(__file__).resolve().parent.parent / "tests.env"
_TESTS_ENV_VALUES: dict[str, str] = {
    key: value for key, value in dotenv_values(_TESTS_ENV_PATH).items() if value is not None
}


def _declared_value(
    key: str,
    environ: Mapping[str, str],
    file_values: Mapping[str, str],
) -> str | None:
    return environ.get(key) or file_values.get(key) or None


def _resolve_rpc_uri(
    key: str,
    environ: Mapping[str, str],
    file_values: Mapping[str, str],
    defaults: Mapping[str, str],
) -> str:
    """Resolve one endpoint name through the precedence contract.

    Process environment > ``tests.env`` > table default, as ``tests.env.example``
    documents. The table is the visible keyless fallback, not a pin: an endpoint
    is machine topology (a devcontainer reth, a personal archive key), which
    belongs in the gitignored ``tests.env`` or the process environment, never in
    tracked code. An empty declared value is absent, so a half-set override
    cannot shadow a working lower layer. A name absent from the table raises
    before any layer is consulted: the table is the vocabulary, and ambient
    state must not widen it.
    """
    if key not in defaults:
        raise KeyError(key)
    return _declared_value(key, environ, file_values) or defaults[key]


def declared_rpc_uri(key: str) -> str | None:
    """The endpoint the machine declared for ``key``, or ``None`` if none did.

    The ``test-fork`` lane fails closed through this seam: it probes exactly
    what the pytest legs dial (``_rpc`` consults the same layers first) and
    refuses to run against the table's public fallback, which serves the offline
    suite's imports rather than declaring a lane endpoint.
    """
    if key not in _DEFAULT_RPC_URIS:
        raise KeyError(key)
    return _declared_value(key, os.environ, _TESTS_ENV_VALUES)


def _rpc(key: str) -> str:
    """Look up a harness endpoint: process env > ``tests.env`` > table default.

    The names this resolves are the harness's own vocabulary, deliberately kept
    out of the operator namespace: the suite needs two node tiers per chain
    (archive and full), while the operator file's ``[nodes]`` tables hold one
    endpoint per transport per chain, so a second HTTP endpoint for the same
    chain has nowhere to fold into. They are therefore not
    ``DEGENBOT_RPC_{HTTP,WS}_CHAINID_<id>`` names and must not grow a fallback
    onto them — a second namespace that shares a fallback is how the next
    ambiguity starts. ``executor/ape-config.yaml`` reads
    ``${TEST_ETHEREUM_FULL_NODE_HTTP_URI}`` as an external contract, which is why the
    ``_FULL_`` names outlive the suite that defines them.
    """
    return _resolve_rpc_uri(key, os.environ, _TESTS_ENV_VALUES, _DEFAULT_RPC_URIS)


TEST_ARBITRUM_FULL_NODE_HTTP_URI: str = _rpc("TEST_ARBITRUM_FULL_NODE_HTTP_URI")
TEST_ARBITRUM_FULL_NODE_WS_URI: str = _rpc("TEST_ARBITRUM_FULL_NODE_WS_URI")

TEST_BASE_ARCHIVE_NODE_HTTP_URI: str = _rpc("TEST_BASE_ARCHIVE_NODE_HTTP_URI")
TEST_BASE_ARCHIVE_NODE_WS_URI: str = _rpc("TEST_BASE_ARCHIVE_NODE_WS_URI")
TEST_BASE_FULL_NODE_HTTP_URI: str = _rpc("TEST_BASE_FULL_NODE_HTTP_URI")
TEST_BASE_FULL_NODE_WS_URI: str = _rpc("TEST_BASE_FULL_NODE_WS_URI")

TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI: str = _rpc("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI")
TEST_ETHEREUM_ARCHIVE_NODE_WS_URI: str = _rpc("TEST_ETHEREUM_ARCHIVE_NODE_WS_URI")
TEST_ETHEREUM_FULL_NODE_HTTP_URI: str = _rpc("TEST_ETHEREUM_FULL_NODE_HTTP_URI")
TEST_ETHEREUM_FULL_NODE_WS_URI: str = _rpc("TEST_ETHEREUM_FULL_NODE_WS_URI")


class AnvilFactory:
    """Build :class:`degenbot.fork.AnvilFork` instances and own their teardown.

    See the ``anvil_factory`` fixture. Every fork built through the call is
    closed at fixture teardown, even when the test body raised -- a manual
    ``fork.close()`` placed after the assertions never ran on failure and
    leaked the anvil subprocess.
    """

    def __init__(self) -> None:
        self._forks: list[AnvilFork] = []

    def __call__(
        self,
        fork_url: str | None = None,
        *,
        launch: ForkLaunchConfig | None = None,
    ) -> AnvilFork:
        fork = AnvilFork(fork_url=fork_url, launch=launch)
        self._forks.append(fork)
        return fork

    def close(self) -> None:
        """Close every fork this factory built (idempotent)."""
        for fork in self._forks:
            fork.close()
        self._forks.clear()


@pytest.fixture
def anvil_factory() -> Generator[AnvilFactory, None, None]:
    """Factory for tests that spawn their OWN anvil launch configuration.

    Constructor-override and fork-URL tests cannot use the ``standalone_anvil``
    fixture; this gives them the same contract: presence is guarded ONCE here
    (skip cleanly in the no-anvil job instead of failing at spawn), and every
    fork the test built is closed at teardown even when the body raised.

    Usage: ``fork = anvil_factory(fork_url=..., launch=ForkLaunchConfig(...))``
    with the same keyword arguments as :class:`AnvilFork`.
    """
    _require_anvil()
    factory = AnvilFactory()
    yield factory
    factory.close()


def _require_anvil() -> None:
    """Skip a test that spawns the `anvil` binary when it is not installed.

    Guards the ``standalone_anvil`` and ``anvil_factory`` fixtures (the
    latter covers every test that spawns its own launch configuration), so
    tests skip cleanly in the no-anvil CI job instead of failing at spawn.
    """
    if shutil.which("anvil") is None:
        pytest.skip("requires the anvil binary (not present in the no-anvil CI job)")


@pytest.fixture
def standalone_anvil() -> Generator[AnvilFork, None, None]:
    """A non-forking standalone anvil seeded with canonical contracts (no upstream RPC).

    Uses :class:`degenbot.fork.AnvilFork` with ``fork_url=None`` + a fixed chain id,
    seeded via :mod:`tests.standalone_anvil.seed` (compiled bytecode at fixed
    addresses + a funded EOA). Provider/anvil-backed tests that only need to
    exercise the RPC/contract seams run against it locally AND in the anvil CI
    job with no external network. See ``tests/standalone_anvil/seed.py`` for the
    fixed addresses.
    """
    _require_anvil()
    # A default funded dev account (anvil account 0) is kept so tests that need
    # to send a real transaction (e.g. emitting a log) can: anvil unlocks dev
    # accounts server-side, so eth_sendTransaction needs no client-side signing
    # (emitter: tests/standalone_anvil/emit.py).
    fork = AnvilFork(launch=ForkLaunchConfig(chain_id=seed_catalog.CHAIN_ID))
    seed_catalog.seed(fork)
    fork.mine()  # advance past genesis so get_block_number() > 0
    yield fork
    fork.close()


def _require_live_node() -> None:
    """Skip a live-network test in the constrained (no-RPC, no-anvil) CI runner.

    Guards the root ``fork_*`` fixtures: any test that chains off them (bot_*, per-file
    bot/token/pool fixtures, …) skips transitively via ``pytest.skip``, so a fork/anvil
    test can never fail offline CI. Local dev (RPC + anvil reachable) runs them normally.
    """
    if is_offline():
        pytest.skip("requires live RPC/anvil (not available in offline CI)")


def _fork(
    *,
    fork_url: str,
    storage_caching: bool,
    fork_block: int | None = None,
    extra_opts: tuple[str, ...] = (),
) -> Generator[AnvilFork, None, None]:
    """Shared body behind the ``fork_*`` fixtures: guard, launch, yield, close.

    ``fork_block`` is the archive tier's indirect-param target (``None`` forks
    at head, like every full-tier fixture -- ``AnvilFork``'s default);
    ``extra_opts`` carries the chain's anvil flags after the universal
    ``--accounts=0``.

    A pinned ``fork_block`` always forces ``storage_caching=False``. The anvil
    fork storage cache is one on-disk file per block
    (``~/.foundry/cache/rpc/<chain>/<block>/storage.json``) shared by every
    anvil instance, so an entry written while the archive node could not answer
    consistently for that block persists and makes every later fork at that
    block serve state the block's header never had. A pinned block's state is
    immutable, so the cache buys only repeat-spawn latency and costs
    correctness; head forks keep it.
    """
    _require_live_node()
    fork = AnvilFork(
        fork_url=fork_url,
        fork_block=fork_block,
        launch=ForkLaunchConfig(
            storage_caching=storage_caching and fork_block is None,
            anvil_opts=["--accounts=0", *extra_opts],
        ),
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_arbitrum_full() -> Generator[AnvilFork, None, None]:
    yield from _fork(
        fork_url=TEST_ARBITRUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )


@pytest.fixture
def fork_base_archive(request: pytest.FixtureRequest) -> Generator[AnvilFork, None, None]:
    """An AnvilFork using the default mainnet archive node. To fork from a specific block,
    parametrize the test with an indirect parameter for this fixture, e.g.:

    ```
        @pytest.mark.parametrize("fork_base_archive", [block_number], indirect=True)
        def test_using_fork(fork_base_archive: AnvilFork): ...
    ```

    """
    yield from _fork(
        fork_url=TEST_BASE_ARCHIVE_NODE_HTTP_URI,
        fork_block=getattr(request, "param", None),
        storage_caching=True,
        extra_opts=("--optimism",),
    )


@pytest.fixture
def fork_base_full() -> Generator[AnvilFork, None, None]:
    yield from _fork(
        fork_url=TEST_BASE_FULL_NODE_HTTP_URI,
        storage_caching=False,
        extra_opts=("--optimism",),
    )


@pytest.fixture
def fork_mainnet_archive(request: pytest.FixtureRequest) -> Generator[AnvilFork, None, None]:
    """An AnvilFork using the default mainnet archive node. To fork from a specific block,
    parametrize the test with an indirect parameter for this fixture, e.g.:

    ```
        @pytest.mark.parametrize("fork_mainnet_archive", [block_number], indirect=True)
        def test_using_fork(fork_mainnet_archive: AnvilFork): ...
    ```

    """
    yield from _fork(
        fork_url=TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        fork_block=getattr(request, "param", None),
        storage_caching=True,
    )


@pytest.fixture
def fork_mainnet_full() -> Generator[AnvilFork, None, None]:
    yield from _fork(
        fork_url=TEST_ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
    )


@pytest.fixture
def bot_mainnet_full(fork_mainnet_full: AnvilFork) -> Generator[Bot, None, None]:
    """Provide a Bot with the mainnet full fork's provider registered."""
    provider = fork_mainnet_full.provider
    bot = make_bot_with_provider(provider)
    yield bot
    bot.close()


@pytest.fixture
def bot_mainnet_archive(fork_mainnet_archive: AnvilFork) -> Generator[Bot, None, None]:
    """Provide a Bot with the mainnet archive fork's provider registered."""
    provider = fork_mainnet_archive.provider
    bot = make_bot_with_provider(provider)
    yield bot
    bot.close()


def _golden_target(request: pytest.FixtureRequest) -> tuple[Path, str]:
    """Per-test golden JSON path + mode shared by both golden factories.

    When --golden-root is the default, GOLDEN_ROOT already encodes it; honour
    an explicit override by rewriting the root anchor too.
    """
    mode: str = request.config.getoption("--golden-mode")
    root = Path(request.config.getoption("--golden-root"))
    rel = _nodeid_to_path(request.node.nodeid, GOLDEN_ROOT).relative_to(GOLDEN_ROOT)
    return root / rel, mode


@pytest.fixture
def golden_factory(request: pytest.FixtureRequest):
    """Factory for :class:`tests.golden.oracle.GoldenOracle` (L2 golden seam).

    Yields a callable ``bind(chain_id, block_number)`` returning a GoldenOracle
    bound to a per-test JSON file derived from ``request.node.nodeid``. Mode is
    driven by ``--golden-mode`` (replay by default, record to (re)populate the
    file against a pinned fork). See ``docs/architecture/golden-onchain-parity.md``.

    Example::

        def test_x(golden_factory):
            golden = golden_factory(chain_id=1, block_number=17_600_000)
            res = golden.check("key", contract=lambda: quoter.fn...call())
            if res.reverted:
                continue
            assert calc == res.value
    """
    path, mode = _golden_target(request)

    def bind(*, chain_id: int, block_number: int) -> GoldenOracle:
        return GoldenOracle(path=path, chain_id=chain_id, block_number=block_number, mode=mode)

    return bind


@pytest.fixture
def recorded_pool_factory(request: pytest.FixtureRequest):
    """Factory for :class:`tests.golden.recorded_pool.RecordedPool` (T0 pool-state golden seam).

    Yields a callable ``bind(chain_id, block_number)`` returning a ``RecordedPool``
    bound to a per-test JSON file derived from ``request.node.nodeid``. Mode is driven
    by ``--golden-mode`` (replay by default, record to (re)populate the file against a
    pinned fork). See ``docs/architecture/golden-onchain-parity.md``.
    """
    path, mode = _golden_target(request)

    def bind(*, chain_id: int, block_number: int) -> RecordedPool:
        return RecordedPool(path=path, chain_id=chain_id, block=block_number, mode=mode)

    return bind
