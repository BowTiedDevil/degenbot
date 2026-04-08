"""
Fixtures for TokenMath property-based testing with Solidity wrapper contracts.

These fixtures deploy compiled Solidity wrapper contracts to a standalone Anvil
instance (no forking) since the contracts are stateless with pure functions only.
"""

import json
import pathlib
from collections.abc import Generator
from typing import TYPE_CHECKING

import pytest

from degenbot.anvil_fork import AnvilFork

if TYPE_CHECKING:
    from web3 import Web3
    from web3.contract import Contract


def _load_contract_artifact(artifact_path: pathlib.Path) -> dict:
    """
    Load a compiled contract artifact (ABI + bytecode).
    """

    with pathlib.Path(artifact_path).open(encoding="utf-8") as f:
        return json.load(f)


def _deploy_contract(
    w3: "Web3",
    artifact: dict,
    deployer_address: str,
) -> "Contract":
    """
    Deploy a contract from compiled artifact.

    Args:
        w3: Web3 instance connected to Anvil
        artifact: Compiled contract artifact with 'abi' and 'bytecode'
        deployer_address: Address to deploy from (must have ETH)

    Returns:
        Deployed contract instance
    """

    bytecode = artifact["bytecode"]["object"]

    # Build and send deployment transaction
    contract_factory = w3.eth.contract(
        abi=artifact["abi"],
        bytecode=bytecode,
    )
    tx_hash = contract_factory.constructor().transact({"from": deployer_address})
    receipt = w3.eth.wait_for_transaction_receipt(tx_hash)

    # Return contract instance at deployed address
    return w3.eth.contract(
        address=receipt["contractAddress"],
        abi=artifact["abi"],
    )


@pytest.fixture(scope="module")
def standalone_anvil() -> Generator[AnvilFork, None, None]:
    """
    Create a standalone Anvil instance (no forking) for pure contract testing.

    Much faster than forking since we don't need to sync state from a remote node.
    """
    fork = AnvilFork(
        fork_url=None,  # No forking - standalone mode
        ipc_provider_kwargs={"timeout": None},
    )
    yield fork
    fork.close()


@pytest.fixture(scope="module")
def token_math_wrappers(
    standalone_anvil,
) -> dict[int, "Contract"]:
    """
    Deploy all TokenMath wrapper contracts for each test function.

    Contracts are stateless (pure functions only), so they can be safely reused
    across tests.

    Yields:
        Dictionary mapping revision numbers (1, 4, 9) to deployed contract instances.
        Each contract exposes TokenMath functions as external calls:

        - getCollateralMintScaledAmount(amount, index)
        - getCollateralBurnScaledAmount(amount, index)
        - getCollateralTransferScaledAmount(amount, index)
        - getCollateralBalance(scaledAmount, index)
        - getDebtMintScaledAmount(amount, index)
        - getDebtBurnScaledAmount(amount, index)
        - getDebtBalance(scaledAmount, index)

        Plus raw math functions (Rev4/9 only):
        - rayMul, rayMulFloor, rayMulCeil
        - rayDiv, rayDivFloor, rayDivCeil
        - wadMul, wadDiv

        Constants:
        - WAD() -> 1e18
        - RAY() -> 1e27

    Example:
        >>> def test_collateral_mint(token_math_wrappers):
        ...     wrapper = token_math_wrappers[4]  # Rev 4
        ...     result = wrapper.functions.getCollateralMintScaledAmount(
        ...         1000, 1000000000000000000000000000
        ...     ).call()
        ...     assert result == expected_value
    """
    w3 = standalone_anvil.w3

    # Use the first pre-funded Anvil account as deployer
    deployer = w3.eth.accounts[0]

    compiled_dir = pathlib.Path(__file__).parent / "contracts" / ".foundry" / "out"

    wrappers = {}
    revisions = [1, 4, 9]

    for revision in revisions:
        artifact_path = (
            compiled_dir
            / f"TestTokenMathWrapper_Rev{revision}.sol"
            / f"TestTokenMathWrapper_Rev{revision}.json"
        )
        artifact = _load_contract_artifact(artifact_path)

        contract = _deploy_contract(w3, artifact, deployer)
        wrappers[revision] = contract

    return wrappers


@pytest.fixture(scope="module")
def token_math_wrapper_rev1(token_math_wrappers) -> "Contract":
    """
    Get the Rev 1 wrapper contract (half-up rounding).
    """

    return token_math_wrappers[1]


@pytest.fixture(scope="module")
def token_math_wrapper_rev4(token_math_wrappers) -> "Contract":
    """
    Get the Rev 4 wrapper contract (floor/ceil rounding).
    """

    return token_math_wrappers[4]


@pytest.fixture(scope="module")
def token_math_wrapper_rev9(token_math_wrappers) -> "Contract":
    """
    Get the Rev 9 wrapper contract (floor/ceil rounding, same as Rev4).
    """

    return token_math_wrappers[9]
