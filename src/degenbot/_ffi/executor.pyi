"""Stub for the dynamically-created ``degenbot._ffi.executor`` submodule.

Created at runtime by ``add_executor_module`` in the PyO3 wrapper crate
(``degenbot-python/src/executor/mod.rs``). Holds the command-stream encoding
+ storage-slot helper functions over the ``degenbot-executor`` core crate.

The encode path lives in the Rust core (ADR-005):
``dispatch_profitable_py`` calls ``composers::encode_cmd_stream``, and the
candidate resolves its ``composers::PathInfo`` from a registered ``path_id``
via ``PyArbitrageEngine.path_info_for_core``. No Python ``PathInfo``
dataclass crosses this seam — ``path_infos`` returns plain dicts.
"""

from typing import Any

type WarmupDict = dict[str, dict[str, Any]]

def compute_simulation_warmup_slots(
    executor_address: str,
    weth_address: str,
    pool_manager_address: str,
) -> WarmupDict:
    """Compute ``eth_simulateV1`` ``stateDiff`` overrides.

    Replicates ``cmd_executor.initialize()``'s three warmed storage slots.

    Args:
        executor_address: The cmd_executor contract address.
        weth_address: The WETH9 contract address.
        pool_manager_address: The Uniswap V4 PoolManager address.

    Returns:
        A dict keyed by checksummed contract addresses, with ``stateDiff``
        sub-dicts mapping slot hex to 1-wei value hex, plus the executor's
        residual balance entry.

    """

def pack_config(
    check_mode: int = ...,
    expected_value: int = ...,
    bribe_bips: int = ...,
    bribe_recipient_idx: int = ...,
) -> int:
    """Pack the ``execute(commands, config)`` ABI ``config`` uint256."""

def pack_expected_balance(check_mode: int, expected_value: int) -> int:
    """Return a deprecated alias for ``pack_config``.

    Uses ``bribe_bips=0`` / ``bribe_recipient_idx=0``.
    """

def mapping_slot(base_slot: int, key: int) -> int:
    """Compute a Solidity mapping storage slot (``keccak256(pad(key,32) || pad(base,32))``)."""

def nested_mapping_slot(base_slot: int, key1: int, key2: int) -> int:
    """Compute a nested Solidity mapping storage slot."""

__all__ = [
    "compute_simulation_warmup_slots",
    "mapping_slot",
    "nested_mapping_slot",
    "pack_config",
    "pack_expected_balance",
]
