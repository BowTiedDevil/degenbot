import logging
import os
from collections.abc import Generator
from pathlib import Path

import dotenv
import pytest
from _pytest.config import Config, Parser
from _pytest.nodes import Item

from degenbot.anvil_fork import AnvilFork
from degenbot.bot import Bot
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.logging import set_log_level
from degenbot.provider import ProviderAdapter
from tests.golden.oracle import GOLDEN_ROOT, GoldenOracle, _nodeid_to_path
from tests.helpers.bot_factory import make_bot_with_provider

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


ARBITRUM_FULL_NODE_HTTP_URI: str = env_values.get(
    "ARBITRUM_FULL_NODE_HTTP_URI",
    "https://arbitrum-one-rpc.publicnode.com",
)
ARBITRUM_FULL_NODE_WS_URI: str = env_values.get(
    "ARBITRUM_FULL_NODE_WS_URI",
    "wss://arbitrum-one-rpc.publicnode.com",
)

BASE_ARCHIVE_NODE_HTTP_URI: str = env_values.get(
    "BASE_ARCHIVE_NODE_HTTP_URI",
    "https://mainnet.base.org",
)
BASE_ARCHIVE_NODE_WS_URI: str = env_values.get(
    "BASE_ARCHIVE_NODE_WS_URI",
    "wss://mainnet.base.org",
)
BASE_FULL_NODE_HTTP_URI: str = env_values.get("BASE_FULL_NODE_HTTP_URI", "https://mainnet.base.org")
BASE_FULL_NODE_WS_URI: str = env_values.get("BASE_FULL_NODE_WS_URI", "wss://mainnet.base.org")

ETHEREUM_ARCHIVE_NODE_HTTP_URI: str = env_values.get(
    "ETHEREUM_ARCHIVE_NODE_HTTP_URI",
    "https://eth.llamarpc.com/",
)
ETHEREUM_ARCHIVE_NODE_WS_URI: str = env_values.get(
    "ETHEREUM_ARCHIVE_NODE_WS_URI",
    "wss://eth.llamarpc.com/",
)
ETHEREUM_FULL_NODE_HTTP_URI: str = env_values.get(
    "ETHEREUM_FULL_NODE_HTTP_URI",
    "https://eth.llamarpc.com/",
)
ETHEREUM_FULL_NODE_WS_URI: str = env_values.get(
    "ETHEREUM_FULL_NODE_WS_URI",
    "wss://eth.llamarpc.com/",
)


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


def pytest_collection_modifyitems(config: Config, items: list[Item]):
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


@pytest.fixture(autouse=True)
def _initialize_and_reset_after_each_test():
    """Before each test, clear/reset global values and singletons"""
    # Global singletons have been removed. Bot-owned connections and registries
    # are scoped to each Bot instance and do not need inter-test resets.
    yield
    # Safety net: dispose any SQLAlchemy engine a test left dangling (a Bot or
    # DatabaseSessionManager constructed inline and never ``close()`` ed). Without
    # this, the Engine's connection pool keeps the ``sqlite3.Connection`` open and
    # surfaces as ``ResourceWarning: unclosed database`` when GC eventually runs
    # (notably at xdist worker teardown).
    DatabaseSessionManager.dispose_all()
    # NOTE: ``AnvilFork.close_all()`` is intentionally NOT called per-test. It
    # reaps *every* live fork indiscriminately, which kills module/session-scoped
    # forks still in use by other tests in the same module — the Anvil process
    # (and its IPC socket) dies between tests, so the next ``.call()`` raises
    # ``OSError: [Errno 9] Bad file descriptor``. Leaked inline forks are reaped
    # by CPython refcounting at test-end via ``__del__``, or — when a failing
    # assertion holds the traceback frame alive and stalls that — by the
    # worker-scoped ``_reap_leaked_anvil_forks`` finalizer below. Per-worker
    # reaping is the right granularity for the PID-budget safety net: each xdist
    # worker has its own ``_LIVE`` weakset, so this still bounds the anvil
    # subprocess count per worker without breaking fixture scope.


@pytest.fixture(scope="session", autouse=True)
def _reap_leaked_anvil_forks():
    """Reap any AnvilFork still live at worker/session exit.

    Safety net for leaked inline constructions (a test that built an AnvilFork
    directly and never ``close()`` ed it). Under CPython refcounting these are
    normally reaped by ``__del__`` the instant the test's local goes out of
    scope; the exception is a failing assertion, which keeps the traceback frame
    — and its locals — alive until the worker tears down. Without this finalizer
    those stragglers (each anvil process ~27 pids) accumulate across failed tests
    under xdist fan-out and can exhaust the container ``pids.max``.

    Runs once per worker (xdist gives each worker its own session) *after* all
    its tests finish, so it cannot clobber module/session-scoped forks mid-run.
    """
    yield
    AnvilFork.close_all()


@pytest.fixture(scope="session", autouse=True)
def _set_degenbot_logging():
    """Set the logging level to DEBUG for the test run.

    Covers both the package logger and the Rust ``log::`` bridge (pyo3-log)
    loggers, so ``log::debug!`` records from the Rust extension are visible in
    the test run too.
    """
    set_log_level(logging.DEBUG)


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
    mode: str = request.config.getoption("--golden-mode")
    root = Path(request.config.getoption("--golden-root"))
    # When --golden-root is the default, GOLDEN_ROOT already encodes it; honour
    # an explicit override by rewriting the root anchor too.
    rel = _nodeid_to_path(request.node.nodeid, GOLDEN_ROOT).relative_to(GOLDEN_ROOT)
    path = root / rel

    def bind(*, chain_id: int, block_number: int) -> GoldenOracle:
        return GoldenOracle(path=path, chain_id=chain_id, block_number=block_number, mode=mode)

    return bind


@pytest.fixture
def fork_arbitrum_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=ARBITRUM_FULL_NODE_HTTP_URI,
        ipc_provider_kwargs={"timeout": None},
        storage_caching=False,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_base_archive(request: pytest.FixtureRequest) -> Generator[AnvilFork, None, None]:
    """An AnvilFork using the default mainnet archive node. To fork from a specific block,
    parametrize the test with an indirect parameter for this fixture, e.g.:

    ```
        @pytest.mark.parametrize("fork_base_archive", [block_number], indirect=True)
        def test_using_fork(fork_base_archive: AnvilFork): ...
    ```
    """
    block_number = getattr(request, "param", None)

    fork = AnvilFork(
        fork_url=BASE_ARCHIVE_NODE_HTTP_URI,
        storage_caching=True,
        fork_block=block_number,
        ipc_provider_kwargs={"timeout": None},
        anvil_opts=["--accounts=0", "--optimism"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_base_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=BASE_FULL_NODE_HTTP_URI,
        storage_caching=False,
        anvil_opts=["--accounts=0", "--optimism"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_mainnet_archive(request: pytest.FixtureRequest) -> Generator[AnvilFork, None, None]:
    """An AnvilFork using the default mainnet archive node. To fork from a specific block,
    parametrize the test with an indirect parameter for this fixture, e.g.:

    ```
        @pytest.mark.parametrize("fork_mainnet_archive", [block_number], indirect=True)
        def test_using_fork(fork_mainnet_archive: AnvilFork): ...
    ```
    """
    block_number = getattr(request, "param", None)

    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        storage_caching=True,
        fork_block=block_number,
        ipc_provider_kwargs={"timeout": None},
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_mainnet_full() -> Generator[AnvilFork, None, None]:
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        ipc_provider_kwargs={"timeout": None},
        storage_caching=False,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture
def bot_mainnet_full(fork_mainnet_full: AnvilFork) -> Generator[Bot, None, None]:
    """Provide a Bot with the mainnet full fork's provider registered."""
    provider = ProviderAdapter.from_web3(fork_mainnet_full.w3)
    bot = make_bot_with_provider(provider)
    yield bot
    bot.close()


@pytest.fixture
def bot_mainnet_archive(fork_mainnet_archive: AnvilFork) -> Generator[Bot, None, None]:
    """Provide a Bot with the mainnet archive fork's provider registered."""
    provider = ProviderAdapter.from_web3(fork_mainnet_archive.w3)
    bot = make_bot_with_provider(provider)
    yield bot
    bot.close()
