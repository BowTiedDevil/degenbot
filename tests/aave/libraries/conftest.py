"""Fixtures for TokenMath property-based testing with Solidity wrapper contracts.

These fixtures deploy compiled Solidity wrapper contracts to a standalone Anvil
instance (no forking) since the contracts are stateless with pure functions only.
"""

import json
import pathlib
from collections.abc import Generator

import pytest

from degenbot.fork import AnvilFork
from degenbot.provider import AlloyProvider
from tests.helpers.w3_contract import make_contract


def _load_contract_artifact(artifact_path: pathlib.Path) -> dict:
    """Load a compiled contract artifact (ABI + bytecode)."""
    with pathlib.Path(artifact_path).open(encoding="utf-8") as f:
        return json.load(f)


def _deploy_contract(
    provider: AlloyProvider,
    artifact: dict,
    deployer_address: str,
) -> W3ContractCompat:
    """Deploy a contract from compiled artifact.

    Args:
        provider: AlloyProvider connected to Anvil
        artifact: Compiled contract artifact with 'abi' and 'bytecode'
        deployer_address: Address to deploy from (must have ETH)

    Returns:
        Deployed contract wrapper instance

    """
    bytecode = artifact["bytecode"]["object"]

    # Deploy via eth_sendTransaction (Anvil auto-signs for pre-funded accounts)
    tx_hash = provider.make_request(
        "eth_sendTransaction",
        [{"from": deployer_address, "data": "0x" + bytecode if not bytecode.startswith("0x") else bytecode}],
    )

    # Wait for receipt
    receipt = None
    for _ in range(30):
        receipt = provider.get_transaction_receipt(tx_hash)
        if receipt is not None:
            break
        import time
        time.sleep(0.1)

    if receipt is None:
        msg = f"Contract deployment failed: no receipt for {tx_hash}"
        raise RuntimeError(msg)

    contract_address = receipt["contractAddress"]

    # Return contract wrapper for read-only calls
    return make_contract(
        provider_url=provider.rpc_url,
        address=contract_address,
        abi=artifact["abi"],
    )


@pytest.fixture(scope="module")
def standalone_anvil() -> Generator[AnvilFork, None, None]:
    """Create a standalone Anvil instance (no forking) for pure contract testing.

    Much faster than forking since we don't need to sync state from a remote node.
    """
    fork = AnvilFork(
        fork_url=None,  # No forking - standalone mode
    )
    yield fork
    fork.close()


@pytest.fixture(scope="module")
def token_math_wrappers(
    standalone_anvil,
) -> dict[int, "Contract"]:
    """Deploy all TokenMath wrapper contracts for each test function.

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
    provider = standalone_anvil.provider

    # Use the first pre-funded Anvil account as deployer
    accounts = provider.make_request("eth_accounts", [])
    deployer = accounts[0]

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

        contract = _deploy_contract(provider, artifact, deployer)
        wrappers[revision] = contract

    return wrappers


@pytest.fixture(scope="module")
def token_math_wrapper_rev1(token_math_wrappers) -> "Contract":
    """Get the Rev 1 wrapper contract (half-up rounding)."""
    return token_math_wrappers[1]


@pytest.fixture(scope="module")
def token_math_wrapper_rev4(token_math_wrappers) -> "Contract":
    """Get the Rev 4 wrapper contract (floor/ceil rounding)."""
    return token_math_wrappers[4]


@pytest.fixture(scope="module")
def token_math_wrapper_rev9(token_math_wrappers) -> "Contract":
    """Get the Rev 9 wrapper contract (floor/ceil rounding, same as Rev4)."""
    return token_math_wrappers[9]
