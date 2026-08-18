"""
conftest.py — shared fixtures for cmd_executor tests.

Imports encoding helpers from conftest_shared.py and provides
common fixtures used across test files.
"""

from pathlib import Path
import pytest

from eth_utils.address import to_checksum_address
from .conftest_shared import WETH_DEPLOYMENT_WRAP_AMOUNT, enc_preamble, make_config
from ape.api.accounts import TestAccountAPI
from ape.contracts.base import ContractInstance
from ape.managers.project import ProjectManager
from ape_test.accounts import TestAccount

NATIVE_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")
ZERO_ADDRESS = to_checksum_address("0x0000000000000000000000000000000000000000")


def pytest_configure(config):
    # Wipe the gas results file at the start of each test session
    # so stale results from a previous run don't contaminate.
    Path(".gas-results").unlink(missing_ok=True)


# Common fixtures


@pytest.fixture
def owner_account(accounts: TestAccountAPI) -> TestAccount:
    return accounts[0]


@pytest.fixture
def usdc(project, owner_account: TestAccount) -> ContractInstance:
    return project.fake_erc20.deploy(
        "Fake USD Coin",
        "USDC",
        6,
        100_000_000,
        sender=owner_account,
    )


@pytest.fixture
def weth(project, owner_account) -> ContractInstance:
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
        sender=owner_account,
    )
    # Pre-warm the WETH balance and ERC6909 Vault on the V4PoolManager
    contract.initialize(value=1, sender=owner_account)
    return contract


@pytest.fixture
def run_executor(weth, executor, v4_pm):
    """Fixture that returns a callable to execute command streams with profit checks.

    By default, packs expected_value = pre-tx WETH+ETH balance into the config
    parameter so the contract asserts the executor doesn't lose value. This is
    the steady-state operating mode — production searchers always verify
    profitability.

    Usage::

        tx = run_executor(at, commands, owner)            # WETH+ETH profit check
        tx = run_executor(at, commands, owner, check_mode=2)  # ERC6909 profit check
        tx = run_executor(at, commands, owner, skip_profit=True)  # no check

    Args:
        at: AddressTable instance
        commands: Encoded command bytes (without preamble)
        owner: Transaction sender account
        check_mode: Profit check mode (default=1).
            1 = check WETH + ETH combined balance (default for most paths)
            2 = check ERC6909 WETH balance (V4V4V4 with MINT, saves ~4,900 gas)
        skip_profit: If True, pass config=0 (no on-chain check).
                     Use ONLY when the test legitimately reduces combined
                     WETH+ETH balance (bribes, withdrawals, ERC6909 mint,
                     V2 callbacks that pay more than received, etc.).
        expected_balance: Override the auto-computed pre-tx balance.
                         When provided, used as expected_value in the config word.
        **kwargs: Additional kwargs passed to executor.execute().

    Returns:
        ReceiptApi of the transaction.
    """
    def _run(at, commands, owner, *, skip_profit=False, check_mode=1, expected_balance=None, **kwargs):
        if skip_profit:
            config = 0
        elif expected_balance is not None:
            config = make_config(check_mode=check_mode, expected_value=expected_balance)
        else:
            if check_mode == 2:
                # ERC6909 WETH: read PM.balanceOf(executor, weth_id)
                weth_id = int(weth.address, 16)
                value = v4_pm.balanceOf(executor.address, weth_id)
            else:
                # WETH + ETH combined
                value = weth.balanceOf(executor) + executor.balance
            config = make_config(check_mode=check_mode, expected_value=value)

        raise_on_revert = kwargs.pop('raise_on_revert', True)
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
