"""Standalone-anvil seed catalog regression (T1).

Runs anywhere the anvil binary is present (local dev + the anvil CI job) and is
skipped in the no-anvil CI job. Exercises the provider/contract seams against
the seeded non-forking chain with no upstream RPC.
"""

from degenbot.fork import AnvilFork
from tests.helpers.w3_contract import make_contract
from tests.standalone_anvil import seed as seed_catalog


def test_seeded_chainlink_aggregator(standalone_anvil: AnvilFork) -> None:
    agg = make_contract(
        standalone_anvil.http_url,
        seed_catalog.CHAINLINK,
        [
            {
                "constant": True,
                "inputs": [],
                "name": "latestAnswer",
                "outputs": [{"name": "", "type": "int256"}],
                "payable": False,
                "stateMutability": "view",
                "type": "function",
            }
        ],
    )
    assert agg.functions.latestAnswer().call() == seed_catalog._CHAINLINK_ANSWER


def test_seeded_chain_identity(standalone_anvil: AnvilFork) -> None:
    assert standalone_anvil.provider.get_chain_id() == seed_catalog.CHAIN_ID
    # seeded EOA carries balance
    assert standalone_anvil.provider.get_balance(seed_catalog.FUNDED_EOA) == 10**18
