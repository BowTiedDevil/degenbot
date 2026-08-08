"""Canonical seed catalog for the standalone-anvil test tier (T1).

A non-forking anvil (``AnvilFork(chain_id=31337)``) with **no upstream RPC**
can still exercise the provider/contract seams: we write precompiled deployed
bytecode at fixed addresses via ``AnvilFork.set_code`` and tune constructor
state via ``AnvilFork.set_storage``, then make real RPC calls against the
disposable chain.

Contracts are compiled once with ``forge build`` (tests/standalone_anvil/
foundry.toml) and the artifacts committed under tests/standalone_anvil/out.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING

from eth_utils import to_checksum_address

if TYPE_CHECKING:
    from degenbot.fork import AnvilFork

_ARTIFACTS = Path(__file__).resolve().parent / "out" / "Seed.sol"


# Fixed, memorable addresses the seeds land at (deterministic across runs).
# Computed via to_checksum_address so they are valid EIP-55 (web3 in the
# standalone-anvil tests enforces checksums strictly).
def _seed_addr(suffix: str) -> str:
    return to_checksum_address(f"0x{'00' * 19}{suffix}")


TOKEN = _seed_addr("aa")  # SimpleToken (WBTC-like)
EVENT_EMITTER = _seed_addr(
    "bc"
)  # EventEmitter (mixed-case checksum so get_logs address has both cases)
REVERTER = _seed_addr("cc")  # Reverter
CHAINLINK = _seed_addr("dd")  # MockChainlinkAggregator
FUNDED_EOA = _seed_addr("ee")  # funded EOA with balance

# Chainlink mock: WETH/USD ≈ 3720.38 USD, scaled to 8 decimals (matches the
# real WETH/USD aggregator's decimals used by the mainnet chainlink test).
_CHAINLINK_ANSWER = 372_038_000_000  # 3720.38 * 1e8
_CHAINLINK_DECIMALS = 8

# SimpleToken slot layout: name=0, symbol=1, decimals=2, totalSupply=3.
_TOKEN_DECIMALS = 8

# Standalone chain id (anvil default fresh chain).
CHAIN_ID = 31337


def _artifact(name: str) -> dict:
    return json.loads((_ARTIFACTS / f"{name}.json").read_text(encoding="utf-8"))


def deployed_bytecode(name: str) -> bytes:
    """Return the runtime bytecode of a compiled seed contract."""
    obj = _artifact(name)["deployedBytecode"]["object"]
    return bytes.fromhex(obj[2:])


def seed(fork: AnvilFork) -> None:
    """Write canonical bytecode + storage onto a standalone (non-forking) anvil."""
    fork.set_code(TOKEN, deployed_bytecode("SimpleToken"))
    fork.set_storage(TOKEN, 2, _TOKEN_DECIMALS)  # decimals

    fork.set_code(EVENT_EMITTER, deployed_bytecode("EventEmitter"))

    fork.set_code(REVERTER, deployed_bytecode("Reverter"))

    fork.set_code(CHAINLINK, deployed_bytecode("MockChainlinkAggregator"))
    fork.set_storage(CHAINLINK, 0, _CHAINLINK_ANSWER)  # answer
    fork.set_storage(CHAINLINK, 1, _CHAINLINK_DECIMALS)  # decimals

    fork.set_balance(FUNDED_EOA, 10**18)
