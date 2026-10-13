"""Fixtures, suite-wide gates, and the declared environment for the Python suite.

Sections, top to bottom: the suite environment (the ``DEGENBOT_*`` pins every
construction observes, sealed by the config-holder re-govern, with the per-test
``governed_holder`` seam beside it); collection gates and hooks; the RPC
vocabulary with the anvil/fork/Bot fixtures; the golden record/replay
factories. The README doctest harness lives in the root ``conftest.py``;
offline detection lives in ``tests/offline.py``.
"""

import atexit
import logging
import os
import shutil
import tempfile
from collections.abc import Callable, Generator, Iterator
from pathlib import Path

import dotenv
import pytest
from _pytest.config import Config, Parser
from _pytest.nodes import Item
from hypothesis import settings

# THE SUITE ENVIRONMENT, DECLARED IN ONE PLACE. The Rust FFI module init
# installs the process-wide typed BotConfig at import (env >
# $XDG_CONFIG_HOME/$HOME/.config/degenbot/config.toml), and the runner's
# activation gate refuses a boot whose fleet has no active facet. A developer's
# ambient operator config therefore leaks into the suite (one machine fails CI,
# another silently misses the refusal). Because the root conftest (imported
# before this file, for the README harness) imports degenbot, the holder boots
# from whatever the process env held before this file ran — so every pin is
# applied to `os.environ` BEFORE the degenbot imports below, and the holder
# re-govern after them seals the declared environment as the process snapshot.
# Fail-closed: if a declared file does not load, collection errors here rather
# than tests resolving against a bare-env snapshot.
#
# Pin ONE suite-wide ambient config; `setdefault` honors an explicit operator
# override. `tests/ambient_config.toml` activates the mevblocker backrun facet
# only — every other schema key keeps its default.
os.environ.setdefault(
    "DEGENBOT_CONFIG",
    str(Path(__file__).resolve().parent / "ambient_config.toml"),
)

# Hermetic state: the declared default database is the developer's real
# ``~/.local/state/degenbot/db/degenbot.db`` (hundreds of MB). Booting an
# engine reads it, so tests that only need an offline engine pay disk/CPU for
# user state, and the connector-index load runs as a background thread that
# can emit records after pytest closes its capture stream (a swallowed
# ``ValueError: I/O operation on closed file``). Pin an absent, per-session
# path so the connector lane takes its discovery-shut branch; an operator who
# wants the real DB for an on-demand run sets ``DEGENBOT_DB_PATH`` explicitly
# (``setdefault`` honors it). The directory is removed at exit so repeated
# runs leave no state behind.
_pytest_state_dir_ctx = tempfile.TemporaryDirectory(prefix="degenbot-pytest-state-")
atexit.register(_pytest_state_dir_ctx.cleanup)
_pytest_state_dir = _pytest_state_dir_ctx.name
os.environ.setdefault(
    "DEGENBOT_DB_PATH",
    str(Path(_pytest_state_dir) / "degenbot.db"),
)

from degenbot._ffi import reinstall_config_holder_for_tests, shutdown_log_drainer
from degenbot.bot import Bot
from degenbot.fork import AnvilFork, ForkLaunchConfig
from degenbot.logging import _LOG_LISTENER, _QUEUED_HANDLER, set_log_level
from tests.golden.oracle import GOLDEN_ROOT, GoldenOracle, _nodeid_to_path
from tests.golden.recorded_pool import RecordedPool
from tests.helpers.bot_factory import make_bot_with_provider
from tests.offline import is_offline
from tests.standalone_anvil import seed as seed_catalog

# Seal the declared environment: the imports above booted the holder from
# whatever the process env held at that first degenbot import (before this
# file's pins), so re-govern once here through the sanctioned test seam — from
# now on, every construction observes the pins above.
reinstall_config_holder_for_tests()

# ADR-052 D1: a legacy `alembic_version`-marked DB heals itself at open. The
# committed `rust/crates/**/fixtures/*.db` parity fixtures are read-only
# references shared by many tests, so pin the killswitch for the Python suite:
# the Rust `auto_heal_matrix` tests own the heal-at-open behavior. (A test that
# needs the heal can still override the var explicitly.) Unlike the pins above,
# the DB layer reads this name at open time — not the loader at boot — so its
# position relative to the re-govern is free; it lives here to keep the
# declared environment in one place.
os.environ.setdefault("DEGENBOT_DB_AUTO_HEAL", "0")


# The per-test counterpart of the re-govern above: import-time governance is
# process-wide and one-shot; this fixture re-governs around individual tests.
@pytest.fixture
def governed_holder() -> Iterator[Callable[[dict[str, str]], None]]:
    """Per-test ``DEGENBOT_*`` override that re-governs the process config holder.

    ``govern({name: value})`` sets the env names, re-loads env+file, and
    reinstalls the process-wide typed-config holder (and the shell's published
    verdict). Teardown restores the process environment EXACTLY and re-governs
    the holder back to the ambient snapshot, so tests that do not opt in keep
    the ``tests/ambient_config.toml`` behavior unchanged.

    This is the config-holder test seam: production first-wins is untouched; the seam
    exists so per-test ``DEGENBOT_RPC_*`` overrides govern the NEXT
    construction instead of being silently pinned to the process's first
    resolution.

    Yields:
        ``govern(env)``, which applies the env override and re-governs the
        holder.

    """
    saved = dict(os.environ)

    def govern(env: dict[str, str]) -> None:
        for name, value in env.items():
            os.environ[name] = value
        reinstall_config_holder_for_tests()

    try:
        yield govern
    finally:
        for name in list(os.environ):
            if name not in saved:
                del os.environ[name]
        os.environ.update(saved)
        reinstall_config_holder_for_tests()


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
        default=os.environ.get("DEGENBOT_GOLDEN_MODE", "replay"),
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


env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


# The harness's endpoint vocabulary: one row per (env-var name, default URI),
# resolved through ``_rpc`` and bound below. Archive and full tiers deliberately
# share a default wherever one public endpoint serves both today (Arbitrum and
# Ethereum via publicnode, Base via mainnet.base.org) -- the table makes that
# sharing visible, and a tier that needs its own endpoint (a real archive node)
# changes only its row. A name absent from the table raises at collection, so
# the vocabulary cannot drift from its defaults. The names are imported across
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


def _rpc(key: str) -> str:
    """Resolve an RPC URI with precedence: envvar > tests.env > table default.

    ``dotenv.dotenv_values`` reads only the file, so a bare ``env_values.get`` would
    ignore ``os.environ`` entirely — leaving no way for a user to override an RPC
    endpoint without editing the (gitignored) ``tests.env``. Checking ``os.environ``
    first makes ad-hoc overrides (``TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI='…' pytest …``)
    work across machines and CI without touching any file.

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
    return os.environ.get(key, env_values.get(key, _DEFAULT_RPC_URIS[key]))


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
    if shutil.which("anvil") is None:
        pytest.skip("requires the anvil binary (not present in the no-anvil CI job)")
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
    """
    _require_live_node()
    fork = AnvilFork(
        fork_url=fork_url,
        fork_block=fork_block,
        launch=ForkLaunchConfig(
            storage_caching=storage_caching,
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
