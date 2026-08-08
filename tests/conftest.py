import logging
import os
import shutil
from collections.abc import Generator
from pathlib import Path

import dotenv
import pytest
from _pytest.config import Config, Parser
from _pytest.nodes import Item

from degenbot.bot import Bot
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.fork import AnvilFork
from degenbot.logging import set_log_level
from tests.golden.oracle import GOLDEN_ROOT, GoldenOracle, _nodeid_to_path
from tests.golden.recorded_pool import RecordedPool
from tests.helpers.bot_factory import make_bot_with_provider
from tests.offline import is_offline
from tests.standalone_anvil import seed as seed_catalog

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


def _rpc(key: str, default: str) -> str:
    """Resolve an RPC URI with precedence: envvar > tests.env > built-in default.

    ``dotenv.dotenv_values`` reads only the file, so a bare ``env_values.get`` would
    ignore ``os.environ`` entirely — leaving no way for a user to override an RPC
    endpoint without editing the (gitignored) ``tests.env``. Checking ``os.environ``
    first makes ad-hoc overrides (``ETHEREUM_ARCHIVE_NODE_HTTP_URI='…' pytest …``)
    work across machines and CI without touching any file.
    """
    return os.environ.get(key, env_values.get(key, default))


ARBITRUM_FULL_NODE_HTTP_URI: str = _rpc(
    "ARBITRUM_FULL_NODE_HTTP_URI",
    "https://arbitrum-one-rpc.publicnode.com",
)
ARBITRUM_FULL_NODE_WS_URI: str = _rpc(
    "ARBITRUM_FULL_NODE_WS_URI",
    "wss://arbitrum-one-rpc.publicnode.com",
)

BASE_ARCHIVE_NODE_HTTP_URI: str = _rpc(
    "BASE_ARCHIVE_NODE_HTTP_URI",
    "https://mainnet.base.org",
)
BASE_ARCHIVE_NODE_WS_URI: str = _rpc(
    "BASE_ARCHIVE_NODE_WS_URI",
    "wss://mainnet.base.org",
)
BASE_FULL_NODE_HTTP_URI: str = _rpc(
    "BASE_FULL_NODE_HTTP_URI",
    "https://mainnet.base.org",
)
BASE_FULL_NODE_WS_URI: str = _rpc(
    "BASE_FULL_NODE_WS_URI",
    "wss://mainnet.base.org",
)

ETHEREUM_ARCHIVE_NODE_HTTP_URI: str = _rpc(
    "ETHEREUM_ARCHIVE_NODE_HTTP_URI",
    "https://ethereum-rpc.publicnode.com",
)
ETHEREUM_ARCHIVE_NODE_WS_URI: str = _rpc(
    "ETHEREUM_ARCHIVE_NODE_WS_URI",
    "wss://ethereum-rpc.publicnode.com",
)
ETHEREUM_FULL_NODE_HTTP_URI: str = _rpc(
    "ETHEREUM_FULL_NODE_HTTP_URI",
    "https://ethereum-rpc.publicnode.com",
)
ETHEREUM_FULL_NODE_WS_URI: str = _rpc(
    "ETHEREUM_FULL_NODE_WS_URI",
    "wss://ethereum-rpc.publicnode.com",
)


def _require_live_node() -> None:
    """Skip a live-network test in the constrained (no-RPC, no-anvil) CI runner.

    Guards the root ``fork_*`` fixtures: any test that chains off them (bot_*, per-file
    bot/token/pool fixtures, …) skips transitively via ``pytest.skip``, so a fork/anvil
    test can never fail offline CI. Local dev (RPC + anvil reachable) runs them normally.
    """
    if is_offline():
        pytest.skip("requires live RPC/anvil (not available in offline CI)")


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
def recorded_pool_factory(request: pytest.FixtureRequest):
    """Factory for :class:`tests.golden.recorded_pool.RecordedPool` (T0 pool-state golden seam).

    Yields a callable ``bind(chain_id, block_number)`` returning a ``RecordedPool``
    bound to a per-test JSON file derived from ``request.node.nodeid``. Mode is driven
    by ``--golden-mode`` (replay by default, record to (re)populate the file against a
    pinned fork). See ``docs/architecture/golden-onchain-parity.md``.
    """
    mode: str = request.config.getoption("--golden-mode")
    root = Path(request.config.getoption("--golden-root"))
    rel = _nodeid_to_path(request.node.nodeid, GOLDEN_ROOT).relative_to(GOLDEN_ROOT)
    path = root / rel

    def bind(*, chain_id: int, block_number: int) -> RecordedPool:
        return RecordedPool(path=path, chain_id=chain_id, block=block_number, mode=mode)

    return bind


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
    fork = AnvilFork(chain_id=seed_catalog.CHAIN_ID, anvil_opts=["--accounts=0"])
    seed_catalog.seed(fork)
    yield fork
    fork.close()


@pytest.fixture
def fork_arbitrum_full() -> Generator[AnvilFork, None, None]:
    _require_live_node()
    fork = AnvilFork(
        fork_url=ARBITRUM_FULL_NODE_HTTP_URI,
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
    _require_live_node()

    fork = AnvilFork(
        fork_url=BASE_ARCHIVE_NODE_HTTP_URI,
        storage_caching=True,
        fork_block=block_number,
        anvil_opts=["--accounts=0", "--optimism"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_base_full() -> Generator[AnvilFork, None, None]:
    _require_live_node()
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
    _require_live_node()

    fork = AnvilFork(
        fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        storage_caching=True,
        fork_block=block_number,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


@pytest.fixture
def fork_mainnet_full() -> Generator[AnvilFork, None, None]:
    _require_live_node()
    fork = AnvilFork(
        fork_url=ETHEREUM_FULL_NODE_HTTP_URI,
        storage_caching=False,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


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
