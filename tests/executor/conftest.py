"""
conftest.py — shared fixtures for cmd_executor tests.

Imports encoding helpers from conftest_shared.py and provides
common fixtures used across test files.
"""

import os
from pathlib import Path

import pytest
from ape import networks
from ape.api.accounts import TestAccountAPI
from ape.contracts.base import ContractInstance
from ape.managers.project import ProjectManager
from ape_test.accounts import TestAccount
from ape_vyper.config import VyperConfig

# Pydantic 2.14 requires an explicit rebuild for ape-vyper's forward reference
# to Remapping, which is defined later in its config module.
VyperConfig.model_rebuild()

# Ape 0.8 selects networks through its API rather than a CLI --network flag.
# The offline recipe uses this to run standalone Foundry instead of mainnet-fork.
if os.environ.get("APE_ETHEREUM_NETWORK") == "local":
    networks.ethereum.local.set_default_provider("foundry")
    networks.ethereum.set_default_network("local")

from eth_utils.address import to_checksum_address
from hypothesis import settings

from .conftest_shared import WETH_DEPLOYMENT_WRAP_AMOUNT, enc_preamble, make_config

# pytest-xdist: pin one anvil port per worker.
# ape-foundry's "host: auto" makes the FIRST caller prefer 127.0.0.1:8545
# ("attempt the default port before anything else"). Concurrent workers
# start at the same time and collide there: the losers' anvil fails to
# bind, their provider then finds an anvil ALREADY listening on 8545 and
# re-attaches to it ("connecting to existing process"). The fork chain is
# now shared across workers — with the shared test mnemonic every worker
# sends from the same owner account, so concurrent txns fail with
# "nonce too low" (-32003). An explicit per-worker APE_FOUNDRY_HOST gives
# each worker its own anvil + fork; setdefault preserves an operator's
# explicit override. The value must be a full URI: a bare host:port makes
# Provider._port (yarl) fail to parse, which silently falls back to the
# default 8545 path and collapses the workers back onto one chain.
_xdist_worker = os.environ.get("PYTEST_XDIST_WORKER", "")
if _xdist_worker.startswith("gw"):
    os.environ.setdefault("APE_FOUNDRY_HOST", f"http://127.0.0.1:{8550 + int(_xdist_worker[2:])}")

# Hypothesis policy: no per-example wall-clock deadline. Fuzz tests here assert
# math properties, not timing; the 200ms default only fails spuriously under
# load. A test that someday asserts a real timing property opts back in with an
# explicit @settings(deadline=...), which overrides this profile.
settings.register_profile("default", deadline=None)
settings.load_profile("default")

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")


def pytest_configure(config):
    # Wipe the gas results file at the start of each test session
    # so stale results from a previous run don't contaminate.
    Path(".gas-results").unlink(missing_ok=True)


@pytest.fixture
def owner_account(accounts: TestAccountAPI) -> TestAccount:
    return accounts[0]


@pytest.fixture
def usdc(project: ProjectManager, owner_account: TestAccount) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake USD Coin",
        "USDC",
        6,
        100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def weth(project: ProjectManager, owner_account) -> ContractInstance:
    fake_weth: ContractInstance
    fake_weth = project.fake_weth.deploy(
        "Fake Wrapped Ether",
        "WETH",
        18,
        100_000_000,
        sender=owner_account,
    )

    # Credit enough ETH to cover any practical test's withdraw needs.
    fake_weth.balance = 1_000_000 * 10**18

    return fake_weth


@pytest.fixture
def wbtc(project, owner_account) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake Wrapped Bitcoin",
        "WBTC",
        8,
        100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def v4_pm(project, owner_account, weth, usdc) -> ContractInstance:
    """V4 PoolManager fake with WETH/USDC pool."""
    token0, token1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    return project.fake_uniswap_v4_pool_manager.deploy(
        token0,
        token1,
        sender=owner_account,
    )


@pytest.fixture
def executor(project, owner_account, weth, v4_pm, usdc, wbtc) -> ContractInstance:
    contract = project.cmd_executor.deploy(
        weth.address,
        v4_pm.address,
        usdc.address,
        wbtc.address,
        sender=owner_account,
    )
    # Pre-warm the WETH balance and ERC6909 Vault on the V4PoolManager
    contract.initialize(value=1, sender=owner_account)
    return contract


@pytest.fixture
def run_executor(executor):
    """Execute a command stream with the selected measured balance domain.

    ``profit_mode=1`` measures physical WETH + native ETH; ``profit_mode=2``
    measures PoolManager ERC6909 WETH claims. Other keyword arguments forward
    to ``executor.execute``.
    """

    def _run(at, commands, owner, *, profit_mode=1, **kwargs):
        config = make_config(profit_mode=profit_mode)

        raise_on_revert = kwargs.pop("raise_on_revert", True)
        tx = executor.execute(
            enc_preamble(at) + commands,
            config,
            sender=owner,
            raise_on_revert=False,
            **kwargs,
        )
        if raise_on_revert and tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
        return tx

    return _run
